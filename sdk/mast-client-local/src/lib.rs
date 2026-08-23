//! In-process adapter: `MastClient` over a directly-owned `mast-engine`.
//! The ONLY crate that links the engine into a client binary.

use mast_client::{ClientError, LogStream, MastClient, OperationStream, PatchStream};
use mast_contract::{Action, EngineSnapshot, OperationId, ProjectId};
use mast_engine::Engine;

pub struct LocalClient {
    engine: Engine,
}

impl LocalClient {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }
}

#[async_trait::async_trait]
impl MastClient for LocalClient {
    async fn snapshot(&self) -> Result<EngineSnapshot, ClientError> {
        Ok(self.engine.snapshot())
    }

    async fn subscribe(&self, after_seq: Option<u64>) -> Result<PatchStream, ClientError> {
        Ok(self.engine.subscribe(after_seq))
    }

    async fn dispatch(&self, action: Action) -> Result<OperationId, ClientError> {
        self.engine.dispatch(action).map_err(ClientError::from)
    }

    async fn operation_events(&self, id: OperationId) -> Result<OperationStream, ClientError> {
        self.engine.operation_events(id).map_err(ClientError::from)
    }

    async fn cancel(&self, id: OperationId) -> Result<(), ClientError> {
        self.engine.cancel(id).map_err(ClientError::from)
    }

    async fn service_logs(
        &self,
        project: ProjectId,
        service: String,
        tail: u32,
    ) -> Result<LogStream, ClientError> {
        self.engine.service_logs(&project, &service, tail).await.map_err(ClientError::from)
    }

    async fn env_report(
        &self,
        project: ProjectId,
    ) -> Result<mast_contract::EnvReport, ClientError> {
        self.engine.env_report(&project).await.map_err(ClientError::from)
    }

    async fn laravel_log(
        &self,
        project: ProjectId,
    ) -> Result<mast_contract::LaravelLogReport, ClientError> {
        self.engine.laravel_log(&project).await.map_err(ClientError::from)
    }

    async fn php_extensions(&self, project: ProjectId) -> Result<Vec<String>, ClientError> {
        self.engine.php_extensions(&project).await.map_err(ClientError::from)
    }

    async fn proxy_ca(&self) -> Result<Option<mast_contract::ProxyCa>, ClientError> {
        Ok(self.engine.export_proxy_ca().await)
    }

    async fn history_recent(&self) -> Result<Vec<mast_contract::HistoryEntry>, ClientError> {
        Ok(self.engine.history_recent())
    }

    async fn subscribe_history(&self) -> Result<mast_client::HistoryStream, ClientError> {
        Ok(self.engine.subscribe_history())
    }

    async fn log_captures(
        &self,
        limit: u32,
    ) -> Result<Vec<mast_contract::LogCapture>, ClientError> {
        self.engine.log_captures(limit).await.map_err(ClientError::from)
    }

    async fn subscribe_log_captures(&self) -> Result<mast_client::CaptureStream, ClientError> {
        Ok(self.engine.subscribe_log_captures())
    }

    async fn subscribe_usage(&self) -> Result<mast_client::UsageStream, ClientError> {
        Ok(self.engine.subscribe_usage())
    }

    async fn network_attach_preview(
        &self,
        workspace: mast_contract::WorkspaceId,
        project: ProjectId,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.engine
            .network_attach_preview(&workspace, &project)
            .await
            .map_err(ClientError::from)
    }

    async fn list_snapshots(
        &self,
        workspace: mast_contract::WorkspaceId,
    ) -> Result<Vec<mast_contract::WorkspaceSnapshot>, ClientError> {
        self.engine.list_snapshots(&workspace).await.map_err(ClientError::from)
    }

    async fn snapshot_report(
        &self,
        snapshot_id: String,
    ) -> Result<mast_contract::SnapshotReport, ClientError> {
        self.engine.snapshot_report(&snapshot_id).await.map_err(ClientError::from)
    }

    async fn run_diagnostics(&self) -> Result<mast_contract::DiagnosticReport, ClientError> {
        self.engine.run_diagnostics().await.map_err(ClientError::from)
    }

    async fn repair_preview(
        &self,
        repair: String,
        arg: Option<String>,
        project: Option<mast_contract::ProjectId>,
    ) -> Result<mast_contract::RepairPlan, ClientError> {
        self.engine
            .repair_preview(&repair, arg.as_deref(), project.as_ref())
            .await
            .map_err(ClientError::from)
    }

    async fn diagnostics_history(&self) -> Result<mast_contract::DiagnosticsHistory, ClientError> {
        self.engine.diagnostics_history().await.map_err(ClientError::from)
    }

    async fn catalog(
        &self,
        project: mast_contract::ProjectId,
    ) -> Result<Vec<mast_contract::CatalogEntry>, ClientError> {
        self.engine.catalog(&project).await.map_err(ClientError::from)
    }

    async fn catalog_preview(
        &self,
        project: mast_contract::ProjectId,
        service: String,
        remove: bool,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.engine.catalog_preview(&project, &service, remove).await.map_err(ClientError::from)
    }

    async fn service_remove_preview(
        &self,
        project: mast_contract::ProjectId,
        service: String,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.engine.service_remove_preview(&project, &service).await.map_err(ClientError::from)
    }

    async fn service_image_preview(
        &self,
        project: mast_contract::ProjectId,
        service: String,
        image: String,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.engine
            .service_image_preview(&project, &service, &image)
            .await
            .map_err(ClientError::from)
    }

    async fn custom_service_preview(
        &self,
        project: mast_contract::ProjectId,
        spec: mast_contract::CustomServiceSpec,
    ) -> Result<mast_contract::FileEditPreview, ClientError> {
        self.engine.custom_service_preview(&project, &spec).await.map_err(ClientError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use mast_contract::{DockerStatus, OperationEventKind, PatchEvent, SubscriptionItem};
    use mast_docker::{ContainerObservation, DockerError, RuntimeAdapter, RuntimeEvent};
    use mast_engine::{EngineConfig, EngineDeps, RuntimeConnector};
    use std::collections::HashMap;
    use std::sync::Arc;

    struct NoDocker;

    #[async_trait::async_trait]
    impl RuntimeConnector for NoDocker {
        async fn connect(
            &self,
        ) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError> {
            Err(DockerError::Api("test: no docker".into()))
        }
    }

    // Referenced only so the trait stays object-safe for test fakes elsewhere.
    #[allow(dead_code)]
    struct NoAdapter;

    #[async_trait::async_trait]
    impl RuntimeAdapter for NoAdapter {
        async fn ping(&self) -> Result<(), DockerError> {
            Ok(())
        }
        async fn list_compose_containers(
            &self,
        ) -> Result<Vec<ContainerObservation>, DockerError> {
            Ok(vec![])
        }
        async fn events(&self) -> Result<BoxStream<'static, RuntimeEvent>, DockerError> {
            Ok(futures::stream::empty().boxed())
        }
        async fn container_logs(
            &self,
            _container_id: &str,
            _tail: u32,
        ) -> Result<BoxStream<'static, mast_docker::LogChunk>, DockerError> {
            Ok(futures::stream::empty().boxed())
        }
        async fn container_log_tail(
            &self,
            _container_id: &str,
            _since_unix: i64,
            _max_lines: u32,
        ) -> Result<Vec<mast_docker::CapturedLine>, DockerError> {
            Ok(vec![])
        }
        async fn container_stats(
            &self,
            _container_id: &str,
        ) -> Result<mast_docker::StatsSample, DockerError> {
            Ok(mast_docker::StatsSample::default())
        }
    }

    /// The consumption protocol exercised through `dyn MastClient`, exactly as
    /// a remote client would: subscribe first, snapshot second, discard
    /// overlap, then follow live patches. This test is the seed of the shared
    /// suite that must also pass against `mast-client-ipc` in the daemon
    /// milestone.
    #[tokio::test(flavor = "multi_thread")]
    async fn full_protocol_flow_through_trait_object() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = tmp.path().join("code");
        std::fs::create_dir(&directory).unwrap();
        let engine = mast_engine::Engine::new(
            EngineConfig::default(),
            EngineDeps {
                connector: Arc::new(NoDocker),
                store: mast_project::MetadataStore::open(tmp.path().join("meta")).unwrap(),
                process_env: HashMap::new(),
                runner: Arc::new(mast_engine::RealLifecycleRunner),
                ownership: mast_engine::acquire_ownership(Some(tmp.path().join("lock"))),
            },
        );
        let client: Arc<dyn MastClient> = Arc::new(LocalClient::new(engine));

        let mut stream = client.subscribe(Some(0)).await.unwrap();
        let snap = client.snapshot().await.unwrap();
        assert_eq!(snap.protocol_version, mast_contract::PROTOCOL_VERSION);

        let op = client
            .dispatch(Action::AddWatchedDirectory { path: directory.to_string_lossy().into() })
            .await
            .unwrap();
        let mut events = client.operation_events(op).await.unwrap();
        let mut completed = false;
        while let Some(event) = events.next().await {
            if event.kind.is_terminal() {
                assert!(matches!(event.kind, OperationEventKind::Completed));
                completed = true;
                break;
            }
        }
        assert!(completed);

        // The watched-directory patch arrives in order with no gaps.
        match stream.next().await.unwrap() {
            SubscriptionItem::Patch { patch } => {
                assert_eq!(patch.seq, snap.seq + 1);
                assert!(matches!(patch.event, PatchEvent::WatchedDirectoriesChanged { .. }));
            }
            SubscriptionItem::ResyncRequired => panic!("unexpected resync"),
        }
    }
}
