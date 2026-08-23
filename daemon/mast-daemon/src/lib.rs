//! The daemon transport (plan M8): the engine served over a unix socket so
//! multiple clients (desktop, CLI) share ONE engine — one mutation owner,
//! one observation loop, no read-only second instances.
//!
//! Wire format: newline-delimited JSON (compact serde_json never emits raw
//! newlines).
//! - `{"id":n,"method":"…","params":…}` → `{"id":n,"result":…}` or
//!   `{"id":n,"error":<ErrorInfo>}`
//! - Stream traffic: `{"stream":k,"item":…}` — the CLIENT chooses `k` and
//!   registers its receiver before sending the request, so no item can race
//!   the response. A `{"stream":k,"end":true}` marks stream end.
//! - The first request MUST be `hello {"protocolVersion":N}`: exact-match
//!   negotiation against the frozen contract v1.

#![cfg_attr(not(unix), allow(unused))]

use std::path::{Path, PathBuf};

use futures::StreamExt;
use mast_contract::{Action, ErrorInfo, OperationId, ProjectId, PROTOCOL_VERSION, WorkspaceId};
use mast_engine::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// Per-user socket path: `$XDG_RUNTIME_DIR/mast/daemon.sock`, falling back
/// to `/tmp/mast-<uid>` (created 0700 either way).
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid is always safe.
            let uid = unsafe { libc_getuid() };
            PathBuf::from(format!("/tmp/mast-{uid}"))
        });
    base.join("mast").join("daemon.sock")
}

// Tiny shim so this crate needs no libc dependency.
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Bind and serve forever. Refuses to clobber a LIVE socket (another daemon
/// answering); silently replaces a stale one (bind would fail otherwise
/// after a crash — the flock ownership lock is the real mutual exclusion).
#[cfg(unix)]
pub async fn serve(engine: Engine, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        set_mode(dir, 0o700)?;
    }
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "another mast daemon is already serving this socket",
                ));
            }
            Err(_) => std::fs::remove_file(path)?, // stale leftover
        }
    }
    let listener = UnixListener::bind(path)?;
    set_mode(path, 0o600)?;
    tracing::info!("mast daemon listening on {}", path.display());
    loop {
        let (stream, _addr) = listener.accept().await?;
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(engine, stream).await {
                tracing::debug!("ipc connection ended: {e}");
            }
        });
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[derive(Deserialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

#[cfg(unix)]
async fn handle_connection(engine: Engine, stream: UnixStream) -> std::io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // Single writer task: responses and stream items interleave safely.
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let writer = tokio::spawn(async move {
        while let Some(mut line) = rx.recv().await {
            line.push('\n');
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut greeted = false;
    while let Some(line) = lines.next_line().await? {
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                tracing::debug!("bad ipc request: {e}");
                continue;
            }
        };
        let id = request.id;
        if !greeted {
            if request.method != "hello" {
                send(&tx, error_response(id, ErrorInfo::ProtocolMismatch {
                    expected: PROTOCOL_VERSION,
                    actual: 0,
                }))
                .await;
                break;
            }
            let actual = request.params.get("protocolVersion").and_then(Value::as_u64).unwrap_or(0)
                as u32;
            if actual != PROTOCOL_VERSION {
                send(&tx, error_response(id, ErrorInfo::ProtocolMismatch {
                    expected: PROTOCOL_VERSION,
                    actual,
                }))
                .await;
                break;
            }
            greeted = true;
            send(&tx, json!({"id": id, "result": {"protocolVersion": PROTOCOL_VERSION}})).await;
            continue;
        }
        let reply = handle_request(&engine, &tx, &request.method, request.params).await;
        match reply {
            Ok(result) => send(&tx, json!({"id": id, "result": result})).await,
            Err(error) => send(&tx, error_response(id, error)).await,
        }
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

fn error_response(id: u64, error: ErrorInfo) -> Value {
    json!({"id": id, "error": error})
}

async fn send(tx: &mpsc::Sender<String>, value: Value) {
    let _ = tx.send(value.to_string()).await;
}

fn param<T: serde::de::DeserializeOwned>(params: &Value, key: &str) -> Result<T, ErrorInfo> {
    serde_json::from_value(params.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| ErrorInfo::InvalidInput { message: format!("bad param {key}: {e}") })
}

fn ok<T: serde::Serialize>(value: T) -> Result<Value, ErrorInfo> {
    serde_json::to_value(value).map_err(|e| ErrorInfo::Internal { message: e.to_string() })
}

/// Forward any stream to the writer as `{"stream":k,"item":…}` lines.
fn pump<T: serde::Serialize + Send + 'static>(
    tx: mpsc::Sender<String>,
    stream_id: u64,
    mut stream: futures::stream::BoxStream<'static, T>,
) {
    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            let Ok(item) = serde_json::to_value(&item) else { break };
            let line = json!({"stream": stream_id, "item": item}).to_string();
            if tx.send(line).await.is_err() {
                return; // client hung up — drop the stream
            }
        }
        let _ = tx.send(json!({"stream": stream_id, "end": true}).to_string()).await;
    });
}

async fn handle_request(
    engine: &Engine,
    tx: &mpsc::Sender<String>,
    method: &str,
    params: Value,
) -> Result<Value, ErrorInfo> {
    match method {
        "snapshot" => ok(engine.snapshot()),
        "subscribe" => {
            let stream_id: u64 = param(&params, "stream")?;
            let after_seq: Option<u64> = param(&params, "afterSeq").unwrap_or(None);
            pump(tx.clone(), stream_id, engine.subscribe(after_seq));
            ok(json!({}))
        }
        "dispatch" => {
            let action: Action = param(&params, "action")?;
            ok(engine.dispatch(action)?)
        }
        "operationEvents" => {
            let stream_id: u64 = param(&params, "stream")?;
            let operation: OperationId = param(&params, "operation")?;
            pump(tx.clone(), stream_id, engine.operation_events(operation)?);
            ok(json!({}))
        }
        "cancel" => {
            let operation: OperationId = param(&params, "operation")?;
            engine.cancel(operation)?;
            ok(json!({}))
        }
        "serviceLogs" => {
            let stream_id: u64 = param(&params, "stream")?;
            let project: ProjectId = param(&params, "project")?;
            let service: String = param(&params, "service")?;
            let tail: u32 = param(&params, "tail")?;
            pump(tx.clone(), stream_id, engine.service_logs(&project, &service, tail).await?);
            ok(json!({}))
        }
        "envReport" => {
            let project: ProjectId = param(&params, "project")?;
            ok(engine.env_report(&project).await?)
        }
        "laravelLog" => {
            let project: ProjectId = param(&params, "project")?;
            ok(engine.laravel_log(&project).await?)
        }
        "proxyCa" => ok(engine.export_proxy_ca().await),
        "phpRuntime" => {
            let project: ProjectId = param(&params, "project")?;
            ok(engine.php_runtime(&project).await?)
        }
        "historyRecent" => ok(engine.history_recent()),
        "subscribeHistory" => {
            let stream_id: u64 = param(&params, "stream")?;
            pump(tx.clone(), stream_id, engine.subscribe_history());
            ok(json!({}))
        }
        "logCaptures" => {
            let limit: u32 = param(&params, "limit")?;
            ok(engine.log_captures(limit).await?)
        }
        "subscribeLogCaptures" => {
            let stream_id: u64 = param(&params, "stream")?;
            pump(tx.clone(), stream_id, engine.subscribe_log_captures());
            ok(json!({}))
        }
        "subscribeUsage" => {
            let stream_id: u64 = param(&params, "stream")?;
            pump(tx.clone(), stream_id, engine.subscribe_usage());
            ok(json!({}))
        }
        "networkAttachPreview" => {
            let workspace: WorkspaceId = param(&params, "workspace")?;
            let project: ProjectId = param(&params, "project")?;
            ok(engine.network_attach_preview(&workspace, &project).await?)
        }
        "listSnapshots" => {
            let workspace: WorkspaceId = param(&params, "workspace")?;
            ok(engine.list_snapshots(&workspace).await?)
        }
        "snapshotReport" => {
            let snapshot: String = param(&params, "snapshot")?;
            ok(engine.snapshot_report(&snapshot).await?)
        }
        "runDiagnostics" => ok(engine.run_diagnostics().await?),
        "repairPreview" => {
            let repair: String = param(&params, "repair")?;
            let arg: Option<String> = param(&params, "arg").unwrap_or(None);
            let project: Option<ProjectId> = param(&params, "project").unwrap_or(None);
            ok(engine.repair_preview(&repair, arg.as_deref(), project.as_ref()).await?)
        }
        "diagnosticsHistory" => ok(engine.diagnostics_history().await?),
        "catalog" => {
            let project: ProjectId = param(&params, "project")?;
            ok(engine.catalog(&project).await?)
        }
        "catalogPreview" => {
            let project: ProjectId = param(&params, "project")?;
            let service: String = param(&params, "service")?;
            let remove: bool = param(&params, "remove")?;
            ok(engine.catalog_preview(&project, &service, remove).await?)
        }
        "serviceRemovePreview" => {
            let project: ProjectId = param(&params, "project")?;
            let service: String = param(&params, "service")?;
            ok(engine.service_remove_preview(&project, &service).await?)
        }
        "serviceImagePreview" => {
            let project: ProjectId = param(&params, "project")?;
            let service: String = param(&params, "service")?;
            let image: String = param(&params, "image")?;
            ok(engine.service_image_preview(&project, &service, &image).await?)
        }
        "customServicePreview" => {
            let project: ProjectId = param(&params, "project")?;
            let spec: mast_contract::CustomServiceSpec = param(&params, "spec")?;
            ok(engine.custom_service_preview(&project, &spec).await?)
        }
        other => Err(ErrorInfo::InvalidInput { message: format!("unknown method {other}") }),
    }
}

/// Windows adapter TODO: named pipes with ACLs. Until then the daemon
/// transport is unix-only; clients fall back to embedded engines.
#[cfg(not(unix))]
pub async fn serve(_engine: Engine, _path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "daemon transport is unix-only"))
}
