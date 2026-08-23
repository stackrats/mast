//! The `MastClient` trait — the ONLY surface clients program against.
//!
//! Depends on `mast-contract` and nothing else. `mast-client-local` adapts the
//! in-process engine; `mast-client-ipc` (daemon milestone) will adapt JSON-RPC.
//! The same client test suite must pass against both.

use futures::stream::BoxStream;
use mast_contract::{
    Action, CatalogEntry, DiagnosticReport, DiagnosticsHistory, EngineSnapshot, EnvReport, LaravelLogReport,
    ErrorInfo, FileEditPreview, HistoryEntry, LogCapture, LogLine, OperationEvent, OperationId,
    ProjectId, RepairPlan, SnapshotReport, SubscriptionItem, UsageSample, WorkspaceId,
    WorkspaceSnapshot,
};

pub type PatchStream = BoxStream<'static, SubscriptionItem>;
pub type OperationStream = BoxStream<'static, OperationEvent>;
pub type LogStream = BoxStream<'static, LogLine>;
pub type HistoryStream = BoxStream<'static, HistoryEntry>;
pub type CaptureStream = BoxStream<'static, LogCapture>;
pub type UsageStream = BoxStream<'static, UsageSample>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Engine(#[from] ErrorInfo),
    #[error("transport error: {0}")]
    Transport(String),
}

/// Race-free consumption protocol (plan §2): call [`MastClient::subscribe`]
/// FIRST (buffering items), then [`MastClient::snapshot`], then discard
/// buffered patches with `seq <= snapshot.seq` and apply the rest in order.
/// On a seq gap or a [`SubscriptionItem::ResyncRequired`], refetch the
/// snapshot and resubscribe.
#[async_trait::async_trait]
pub trait MastClient: Send + Sync {
    async fn snapshot(&self) -> Result<EngineSnapshot, ClientError>;

    /// Stream patches with `seq > after_seq`. `None` follows from the current
    /// tip. The stream ends after `ResyncRequired`.
    async fn subscribe(&self, after_seq: Option<u64>) -> Result<PatchStream, ClientError>;

    /// Dispatch a mutation; returns the operation handle immediately.
    async fn dispatch(&self, action: Action) -> Result<OperationId, ClientError>;

    /// Full event history of the operation from its first event (late
    /// subscribers replay), then live until a terminal event.
    async fn operation_events(&self, id: OperationId) -> Result<OperationStream, ClientError>;

    /// Request cancellation; the operation confirms via a `Cancelled` event.
    async fn cancel(&self, id: OperationId) -> Result<(), ClientError>;

    /// Follow a service's container logs (last `tail` lines first). Delivered
    /// on a dedicated stream, never through the patch store (plan §3).
    async fn service_logs(
        &self,
        project: ProjectId,
        service: String,
        tail: u32,
    ) -> Result<LogStream, ClientError>;

    /// Env editor payload (entries, example diff, validation findings). On
    /// demand only — values may be secrets and stay out of the patch store.
    async fn env_report(&self, project: ProjectId) -> Result<EnvReport, ClientError>;

    /// The tail of `storage/logs/laravel.log`, parsed and grouped (newest
    /// first). On demand only — log bodies routinely carry user data.
    async fn laravel_log(&self, project: ProjectId) -> Result<LaravelLogReport, ClientError>;

    /// Recent effect history, oldest first: every command Mast spawned and
    /// every config file it wrote, with outcomes.
    async fn history_recent(&self) -> Result<Vec<HistoryEntry>, ClientError>;

    /// Live effect history. Entries arrive on creation and again on
    /// completion; consumers upsert by `HistoryEntry::id`. Like container logs
    /// this is a dedicated stream — its volume would evict real state patches
    /// from the replay window.
    async fn subscribe_history(&self) -> Result<HistoryStream, ClientError>;

    /// Stored log captures, newest first — the tail of a container's output
    /// read at the moment it went down. Unlike history these outlive the
    /// process: they are on disk, because a container that dies while Mast is
    /// closed is exactly the one nobody can explain afterwards.
    async fn log_captures(&self, limit: u32) -> Result<Vec<LogCapture>, ClientError>;

    /// Live log captures. Append-only — a capture is never revised after it is
    /// written, so consumers only prepend.
    async fn subscribe_log_captures(&self) -> Result<CaptureStream, ClientError>;

    /// Live CPU and memory per running service. **Subscribing is what starts
    /// the engine sampling** — it makes no docker calls while nobody is
    /// listening, so a client that stops caring should drop the stream.
    /// There is no backlog method: a sample is worthless a minute later.
    async fn subscribe_usage(&self) -> Result<UsageStream, ClientError>;

    /// Preview attaching a workspace member to the shared network (apply via
    /// `Action::AttachNetwork`).
    async fn network_attach_preview(
        &self,
        workspace: WorkspaceId,
        project: ProjectId,
    ) -> Result<FileEditPreview, ClientError>;

    /// Snapshots for a workspace, newest first.
    async fn list_snapshots(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<WorkspaceSnapshot>, ClientError>;

    /// Current state vs a snapshot — a report, never an automatic restore.
    async fn snapshot_report(&self, snapshot_id: String) -> Result<SnapshotReport, ClientError>;

    /// Run the full applicable check set; the run is recorded in history.
    async fn run_diagnostics(&self) -> Result<DiagnosticReport, ClientError>;

    /// What a repair would do — shown before consent (apply via
    /// `Action::ApplyRepair`).
    async fn repair_preview(
        &self,
        repair: String,
        arg: Option<String>,
        project: Option<ProjectId>,
    ) -> Result<RepairPlan, ClientError>;

    /// Recent diagnostic runs and applied repairs (audit trail).
    async fn diagnostics_history(&self) -> Result<DiagnosticsHistory, ClientError>;

    /// The service catalog with per-project installed flags.
    async fn catalog(&self, project: ProjectId) -> Result<Vec<CatalogEntry>, ClientError>;

    /// Preview a catalog add (or three-way removal) as a full file diff
    /// (apply via `Action::AddCatalogService` / `RemoveCatalogService`).
    async fn catalog_preview(
        &self,
        project: ProjectId,
        service: String,
        remove: bool,
    ) -> Result<FileEditPreview, ClientError>;

    /// Preview removing ANY service by its compose key (apply via
    /// `Action::RemoveService` — no three-way baseline).
    async fn service_remove_preview(
        &self,
        project: ProjectId,
        service: String,
    ) -> Result<FileEditPreview, ClientError>;

    /// Preview retagging a service's image (apply via
    /// `Action::SetServiceImage`).
    async fn service_image_preview(
        &self,
        project: ProjectId,
        service: String,
        image: String,
    ) -> Result<FileEditPreview, ClientError>;

    /// Preview adding a user-described service (apply via
    /// `Action::AddCustomService`).
    async fn custom_service_preview(
        &self,
        project: ProjectId,
        spec: mast_contract::CustomServiceSpec,
    ) -> Result<FileEditPreview, ClientError>;
}
