//! `MastClient` over the daemon's unix socket (plan M8). Symmetric to
//! `mast-client-local`: same trait, different transport, so every client
//! binary prefers the daemon and falls back to an embedded engine.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use mast_client::{ClientError, LogStream, MastClient, OperationStream, PatchStream};
use mast_contract::{
    Action, EngineSnapshot, ErrorInfo, LogLine, OperationEvent, OperationId, ProjectId,
    SubscriptionItem, WorkspaceId,
};
use serde_json::{Value, json};
// Only the unix `connect` speaks the wire protocol; the non-unix stub refuses
// before any of this is reachable.
#[cfg(unix)]
use mast_contract::PROTOCOL_VERSION;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot};

struct Router {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, ErrorInfo>>>>,
    streams: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
}

pub struct IpcClient {
    writer: mpsc::Sender<String>,
    router: Arc<Router>,
    next_id: AtomicU64,
}

impl IpcClient {
    /// Windows adapter TODO: named pipes. Every caller treats `Err` as
    /// "no daemon" and falls back to an embedded engine.
    #[cfg(not(unix))]
    pub async fn connect(_path: &Path) -> Result<Self, ClientError> {
        Err(ClientError::Transport("daemon transport is unix-only".into()))
    }

    /// Connect + version-negotiate. `Err` means "no live daemon here" —
    /// callers fall back to an embedded engine.
    #[cfg(unix)]
    pub async fn connect(path: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|e| ClientError::Transport(format!("connect {}: {e}", path.display())))?;
        let (read_half, mut write_half) = stream.into_split();

        let (writer, mut writer_rx) = mpsc::channel::<String>(256);
        tokio::spawn(async move {
            while let Some(mut line) = writer_rx.recv().await {
                line.push('\n');
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let router = Arc::new(Router {
            pending: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
        });
        {
            let router = router.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(read_half).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                    if let Some(id) = value.get("id").and_then(Value::as_u64) {
                        if let Some(reply) = router.pending.lock().await.remove(&id) {
                            let result = match value.get("error") {
                                Some(error) => Err(serde_json::from_value(error.clone())
                                    .unwrap_or(ErrorInfo::Internal {
                                        message: error.to_string(),
                                    })),
                                None => {
                                    Ok(value.get("result").cloned().unwrap_or(Value::Null))
                                }
                            };
                            let _ = reply.send(result);
                        }
                    } else if let Some(stream) = value.get("stream").and_then(Value::as_u64) {
                        if value.get("end").and_then(Value::as_bool) == Some(true) {
                            router.streams.lock().await.remove(&stream);
                        } else if let Some(item) = value.get("item") {
                            let tx = router.streams.lock().await.get(&stream).cloned();
                            if let Some(tx) = tx {
                                // Blocking send preserves backpressure per stream.
                                let _ = tx.send(item.clone()).await;
                            }
                        }
                    }
                }
                // Connection gone: fail everything outstanding.
                router.pending.lock().await.clear();
                router.streams.lock().await.clear();
            });
        }

        let client = Self { writer, router, next_id: AtomicU64::new(1) };
        let negotiated = client
            .request("hello", json!({"protocolVersion": PROTOCOL_VERSION}))
            .await?;
        let daemon_version =
            negotiated.get("protocolVersion").and_then(Value::as_u64).unwrap_or(0) as u32;
        if daemon_version != PROTOCOL_VERSION {
            return Err(ClientError::Engine(ErrorInfo::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual: daemon_version,
            }));
        }
        Ok(client)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.router.pending.lock().await.insert(id, tx);
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        self.writer
            .send(line)
            .await
            .map_err(|_| ClientError::Transport("daemon connection closed".into()))?;
        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(ClientError::Engine(error)),
            Err(_) => Err(ClientError::Transport("daemon connection closed".into())),
        }
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, ClientError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value)
            .map_err(|e| ClientError::Transport(format!("bad {method} reply: {e}")))
    }

    /// Register a client-chosen stream id BEFORE the request — items can
    /// never race the response.
    async fn open_stream<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &str,
        mut params: Value,
    ) -> Result<futures::stream::BoxStream<'static, T>, ClientError> {
        let stream_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<Value>(64);
        self.router.streams.lock().await.insert(stream_id, tx);
        params["stream"] = json!(stream_id);
        if let Err(e) = self.request(method, params).await {
            self.router.streams.lock().await.remove(&stream_id);
            return Err(e);
        }
        let stream = tokio_stream_from(rx)
            .filter_map(|value| async move { serde_json::from_value::<T>(value).ok() })
            .boxed();
        Ok(stream)
    }
}

fn tokio_stream_from(rx: mpsc::Receiver<Value>) -> impl futures::Stream<Item = Value> + Send {
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|v| (v, rx)) })
}

#[async_trait::async_trait]
impl MastClient for IpcClient {
    async fn snapshot(&self) -> Result<EngineSnapshot, ClientError> {
        self.call("snapshot", json!({})).await
    }

    async fn subscribe(&self, after_seq: Option<u64>) -> Result<PatchStream, ClientError> {
        self.open_stream::<SubscriptionItem>("subscribe", json!({"afterSeq": after_seq})).await
    }

    async fn dispatch(&self, action: Action) -> Result<OperationId, ClientError> {
        self.call("dispatch", json!({"action": action})).await
    }

    async fn operation_events(&self, id: OperationId) -> Result<OperationStream, ClientError> {
        self.open_stream::<OperationEvent>("operationEvents", json!({"operation": id})).await
    }

    async fn cancel(&self, id: OperationId) -> Result<(), ClientError> {
        self.request("cancel", json!({"operation": id})).await.map(|_| ())
    }

    async fn service_logs(
        &self,
        project: ProjectId,
        service: String,
        tail: u32,
    ) -> Result<LogStream, ClientError> {
        self.open_stream::<LogLine>(
            "serviceLogs",
            json!({"project": project, "service": service, "tail": tail}),
        )
        .await
    }

    async fn env_report(
        &self,
        project: ProjectId,
    ) -> Result<mast_contract::EnvReport, ClientError> {
        self.call("envReport", json!({"project": project})).await
    }

    async fn laravel_log(
        &self,
        project: ProjectId,
    ) -> Result<mast_contract::LaravelLogReport, ClientError> {
        self.call("laravelLog", json!({"project": project})).await
    }

    async fn proxy_ca(&self) -> Result<Option<mast_contract::ProxyCa>, ClientError> {
        self.call("proxyCa", json!({})).await
    }

    async fn history_recent(&self) -> Result<Vec<mast_contract::HistoryEntry>, ClientError> {
        self.call("historyRecent", json!({})).await
    }

    async fn subscribe_history(&self) -> Result<mast_client::HistoryStream, ClientError> {
        self.open_stream::<mast_contract::HistoryEntry>("subscribeHistory", json!({})).await
    }

    async fn log_captures(
        &self,
        limit: u32,
    ) -> Result<Vec<mast_contract::LogCapture>, ClientError> {
        self.call("logCaptures", json!({"limit": limit})).await
    }

    async fn subscribe_log_captures(&self) -> Result<mast_client::CaptureStream, ClientError> {
        self.open_stream::<mast_contract::LogCapture>("subscribeLogCaptures", json!({})).await
    }

    async fn subscribe_usage(&self) -> Result<mast_client::UsageStream, ClientError> {
        self.open_stream::<mast_contract::UsageSample>("subscribeUsage", json!({})).await
    }

    async fn network_attach_preview(
        &self,
        workspace: WorkspaceId,
        project: ProjectId,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.call("networkAttachPreview", json!({"workspace": workspace, "project": project}))
            .await
    }

    async fn list_snapshots(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<mast_contract::WorkspaceSnapshot>, ClientError> {
        self.call("listSnapshots", json!({"workspace": workspace})).await
    }

    async fn snapshot_report(
        &self,
        snapshot_id: String,
    ) -> Result<mast_contract::SnapshotReport, ClientError> {
        self.call("snapshotReport", json!({"snapshot": snapshot_id})).await
    }

    async fn run_diagnostics(&self) -> Result<mast_contract::DiagnosticReport, ClientError> {
        self.call("runDiagnostics", json!({})).await
    }

    async fn repair_preview(
        &self,
        repair: String,
        arg: Option<String>,
        project: Option<ProjectId>,
    ) -> Result<mast_contract::RepairPlan, ClientError> {
        self.call("repairPreview", json!({"repair": repair, "arg": arg, "project": project}))
            .await
    }

    async fn diagnostics_history(
        &self,
    ) -> Result<mast_contract::DiagnosticsHistory, ClientError> {
        self.call("diagnosticsHistory", json!({})).await
    }

    async fn catalog(
        &self,
        project: ProjectId,
    ) -> Result<Vec<mast_contract::CatalogEntry>, ClientError> {
        self.call("catalog", json!({"project": project})).await
    }

    async fn catalog_preview(
        &self,
        project: ProjectId,
        service: String,
        remove: bool,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.call(
            "catalogPreview",
            json!({"project": project, "service": service, "remove": remove}),
        )
        .await
    }

    async fn service_remove_preview(
        &self,
        project: ProjectId,
        service: String,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.call("serviceRemovePreview", json!({"project": project, "service": service})).await
    }

    async fn service_image_preview(
        &self,
        project: ProjectId,
        service: String,
        image: String,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.call(
            "serviceImagePreview",
            json!({"project": project, "service": service, "image": image}),
        )
        .await
    }

    async fn custom_service_preview(
        &self,
        project: ProjectId,
        spec: mast_contract::CustomServiceSpec,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.call("customServicePreview", json!({"project": project, "spec": spec})).await
    }
}
