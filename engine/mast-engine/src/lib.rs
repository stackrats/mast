//! The engine composition root. M2 shape: the state-owning core performs zero
//! I/O (plan §3) — all Docker/fs/subprocess work happens in the effect loops
//! (`effects` module), which feed observations back through [`Engine::with_state`].
//! The snapshot/subscribe protocol, replay buffer, lagged-subscriber policy,
//! and cancellable-operation machinery carry over from M1 unchanged.

pub mod captures;
mod commands;
mod db_repair;
mod diagnostics;
mod effects;
mod history;
pub mod integrations;
mod lifecycle;
mod lock;
mod manifest;
mod catalog_ops;
mod ops;
mod supervise;
mod volumes;
mod php;
mod ports;
mod proxy;
mod project_ops;
mod share;
mod snapshot_ops;
mod workspace_ops;
mod redact;
pub mod usage;
pub mod workspace;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use mast_compose::{ComposeInvocation, ResolvedModel};
use mast_contract::{
    Action, ContainerState, DiscoveredProject, DockerStatus, EnginePatch,
    EngineSnapshot, ErrorInfo, HistoryEntry,
    IntegrationSettings, OperationEventKind, OperationId,
    PROTOCOL_VERSION, PatchEvent, ProjectId, ProjectStatus, ProjectSummary, ServiceHealth,
    ServiceState, SubscriptionItem, WorkspaceId, WorkspaceMember,
    WorkspaceSummary,
};
use mast_docker::{DockerError, RuntimeAdapter};
use mast_project::{MetadataStore, ProjectRecord, WorkspaceMemberRecord, WorkspaceRecord};
use tokio::sync::{broadcast, mpsc};

pub use effects::RealConnector;
pub use lifecycle::{LifecycleRunner, LifecycleVerb, RealLifecycleRunner, lifecycle_argv};
pub use lock::{Ownership, OwnershipLock, acquire_ownership};
pub(crate) use ops::OpHandle;
pub use redact::{REDACTED, Redactor};

pub(crate) fn internal_err(e: impl std::fmt::Display) -> ErrorInfo {
    ErrorInfo::Internal { message: e.to_string() }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Patches kept for subscriber catch-up; older gaps force a resync.
    pub replay_capacity: usize,
    /// Broadcast channel capacity; overrun for a slow subscriber triggers the
    /// explicit lagged policy (ResyncRequired), never silent drops.
    pub patch_channel_capacity: usize,
    /// Per-subscription delivery buffer (backpressure boundary).
    pub subscription_buffer: usize,
    /// Full reconcile cadence (event hints trigger earlier passes).
    pub reconcile_interval: Duration,
    /// Coalescing window for docker/file-event hints.
    pub hint_debounce: Duration,
    /// How long a workspace member may take to become Ready before its
    /// dependents are blocked and the workspace start fails.
    pub ready_timeout: Duration,
    /// Stable-running window for projects with no healthcheck and no
    /// reachable HTTP probe (last rung of the readiness ladder).
    pub ready_grace: Duration,
    /// Whether opening the catalog may refresh image tags from the registry in
    /// the background. Off in tests: the suite asserts on the offline
    /// fallback, and nothing here should need a network.
    pub registry_refresh: bool,
    /// How often to sample container CPU/memory while a client is watching.
    /// No sampling happens at all when nobody is subscribed.
    pub usage_interval: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            replay_capacity: 256,
            patch_channel_capacity: 1024,
            subscription_buffer: 64,
            reconcile_interval: Duration::from_secs(20),
            hint_debounce: Duration::from_millis(250),
            ready_timeout: Duration::from_secs(90),
            ready_grace: Duration::from_secs(2),
            registry_refresh: true,
            usage_interval: usage::USAGE_INTERVAL,
        }
    }
}

/// How the engine obtains a runtime connection. `RealConnector` resolves the
/// docker context (ADR-0002) and connects bollard; tests inject fakes.
#[async_trait::async_trait]
pub trait RuntimeConnector: Send + Sync {
    async fn connect(&self) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError>;
}

pub struct EngineDeps {
    pub connector: Arc<dyn RuntimeConnector>,
    pub store: MetadataStore,
    /// Real process environment (wins over `.env` per-key during resolution);
    /// injected so tests are hermetic.
    pub process_env: HashMap<String, String>,
    /// Executes lifecycle shell-outs; injected so tests need no docker.
    pub runner: Arc<dyn LifecycleRunner>,
    /// Result of the per-user mutation-ownership lock (plan §1).
    pub ownership: Ownership,
}

pub(crate) struct ProjectEntry {
    pub record: ProjectRecord,
    pub summary: ProjectSummary,
    pub invocation: Option<ComposeInvocation>,
    pub model: Option<ResolvedModel>,
    /// Rebuilt from the project's `.env` on every reconcile.
    pub redactor: Redactor,
    /// APP_PORT from `.env` (host-published Laravel port) — drives the HTTP
    /// `/up` readiness probe.
    pub app_port: Option<u16>,
    /// Host-side port-forward declarations from `.env` (key → port), for
    /// cross-project conflict detection inside workspaces.
    pub host_ports: Vec<(String, u16)>,
    /// Hash of the compose files' bytes when `model` was last resolved. An
    /// in-place edit (catalog add, retag, external editor) changes content
    /// without changing the invocation — the model must refresh then too.
    pub compose_fingerprint: Option<u64>,
    /// Commands the project's committed `mast.yml` contributes, refreshed on
    /// every reconcile. Kept beside the record's saved commands so anything
    /// rebuilding `summary.commands` off-reconcile (SetProjectCommands, the
    /// export) can merge without touching the filesystem.
    pub manifest: crate::manifest::Manifest,
}

pub(crate) struct EngineState {
    pub seq: u64,
    pub replay: VecDeque<EnginePatch>,
    pub docker: DockerStatus,
    pub integrations: IntegrationSettings,
    pub watched_directories: Vec<PathBuf>,
    pub discovered: Vec<DiscoveredProject>,
    pub projects: BTreeMap<String, ProjectEntry>,
    pub workspaces: Vec<WorkspaceRecord>,
    /// Every project's secrets in one redactor. History records commands that
    /// belong to no single project (resolution, probes) but can still echo a
    /// project's secrets, so those need the union, not a per-project view.
    pub redactor_all: Redactor,
}

pub(crate) struct Inner {
    pub config: EngineConfig,
    pub deps: EngineDeps,
    pub state: Mutex<EngineState>,
    patches_tx: broadcast::Sender<EnginePatch>,
    pub(crate) ops: Mutex<HashMap<u64, Arc<OpHandle>>>,
    pub(crate) next_op: AtomicU64,
    pub adapter: Mutex<Option<Arc<dyn RuntimeAdapter>>>,
    pub hint_tx: Mutex<Option<mpsc::Sender<()>>>,
    /// Per-project operation locks: no concurrent lifecycle ops on a project.
    pub(crate) busy_projects: Mutex<HashSet<String>>,
    /// Interrupted operations found in the journal at startup (crash
    /// recovery); surfaced as project warnings until the next lifecycle op.
    pub(crate) crash_notices: Mutex<HashMap<String, String>>,
    /// Effect history ring (M9), oldest first. Deliberately outside
    /// `EngineState`: it is not state that patches describe, and routing it
    /// through the patch stream would evict real patches from the replay
    /// window (see the history section of the contract).
    pub(crate) history: Mutex<VecDeque<HistoryEntry>>,
    history_tx: broadcast::Sender<HistoryEntry>,
    pub(crate) next_history: AtomicU64,
    /// Start instants for in-flight commands, keyed by history id.
    pub(crate) history_started: Mutex<HashMap<u64, std::time::Instant>>,
    /// Live feed of log captures (M10). Like history this is deliberately not
    /// state: a capture is an event with a body, and the database — not the
    /// replay window — is its record.
    pub(crate) captures_tx: broadcast::Sender<mast_contract::LogCapture>,
    /// Container names of share tunnels this engine is currently running —
    /// what the orphan sweep must NOT touch.
    pub(crate) live_shares: Mutex<HashSet<String>>,
    /// When each container was last captured, for repeat suppression.
    pub(crate) captures_seen: Mutex<HashMap<String, u64>>,
    /// Live resource usage (M11). Subscribing to this is what starts the
    /// sampler; the loop makes no docker calls while `receiver_count()` is 0.
    pub(crate) usage_tx: broadcast::Sender<mast_contract::UsageSample>,
    /// Last stats reading per container, so the next one can be a delta.
    pub(crate) usage_prev: Mutex<HashMap<String, mast_docker::StatsSample>>,
    /// Context for an operation's commands, keyed by operation id. Set when
    /// the action is dispatched, consumed by the task that runs it.
    pub(crate) op_contexts: Mutex<HashMap<u64, history::CommandContext>>,
    /// Keeps this engine's command observer alive — the registry in
    /// `mast-docker` only holds a weak reference, so a dropped engine stops
    /// being notified.
    pub(crate) command_observer: Mutex<Option<Arc<dyn mast_docker::CommandObserver>>>,
}

#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

fn commands_to_contract(record: &ProjectRecord) -> Vec<mast_contract::ProjectCommand> {
    record
        .commands
        .iter()
        .map(|c| mast_contract::ProjectCommand {
            name: c.name.clone(),
            command: c.command.clone(),
            auto_start: c.auto_start,
            cwd: c.cwd.clone(),
            after: c.after.clone(),
            ready_when: c.ready_when.clone(),
            auto_restart: c.auto_restart,
            restart_when_changed: c.restart_when_changed.clone(),
            from_manifest: false,
        })
        .collect()
}

fn initial_summary(record: &ProjectRecord) -> ProjectSummary {
    ProjectSummary {
        id: ProjectId(record.id.clone()),
        name: record.display_name.clone(),
        path: record.path.to_string_lossy().into_owned(),
        status: ProjectStatus::Stopped,
        compose_project_name: None,
        is_sail: record.is_sail,
        services: Vec::new(),
        resolution_error: None,
        warnings: Vec::new(),
        commands: commands_to_contract(record),
        processes: Vec::new(),
        git_branch: None,
        git_dirty: None,
        app_url: None,
        php: None,
        share_url: None,
        share_dashboard_url: None,
        local_domain: record.local_domain.clone(),
    }
}

impl Engine {
    /// Load persisted state and build the engine. No I/O beyond the metadata
    /// store; effect loops start with [`Engine::start`].
    pub fn new(config: EngineConfig, deps: EngineDeps) -> Self {
        let records = deps.store.load_projects().unwrap_or_default();
        let settings = deps.store.load_settings().unwrap_or_default();
        let mut workspaces = deps.store.load_workspaces().unwrap_or_default();
        // Heal membership left dangling by an older build; read-only sessions
        // prune in memory only.
        let known: HashSet<&str> = records.iter().map(|r| r.id.as_str()).collect();
        let mut healed = false;
        for ws in workspaces.iter_mut() {
            let before = ws.members.len();
            ws.members.retain(|m| known.contains(m.project_id.as_str()));
            healed |= ws.members.len() != before;
            for member in ws.members.iter_mut() {
                let before = member.depends_on.len();
                member.depends_on.retain(|d| known.contains(d.as_str()));
                healed |= member.depends_on.len() != before;
            }
        }
        if healed && !deps.ownership.is_read_only() {
            let _ = deps.store.save_workspaces(&workspaces);
        }
        // Crash recovery (plan M4): operations still journaled at startup were
        // interrupted; surface them, then let inspection settle actual state.
        let interrupted = deps.store.load_journal().unwrap_or_default();
        let crash_notices: HashMap<String, String> = interrupted
            .iter()
            .map(|entry| {
                (
                    entry.project_id.clone(),
                    format!(
                        "A {} operation was interrupted (crash or forced quit); current \
                         state has been re-inspected.",
                        entry.verb
                    ),
                )
            })
            .collect();
        if !interrupted.is_empty() {
            let _ = deps.store.save_journal(&[]);
        }
        let projects = records
            .into_iter()
            .map(|record| {
                let entry = ProjectEntry {
                    summary: initial_summary(&record),
                    record,
                    invocation: None,
                    model: None,
                    redactor: Redactor::default(),
                    app_port: None,
                    host_ports: Vec::new(),
                    compose_fingerprint: None,
                    manifest: manifest::Manifest::default(),
                };
                (entry.record.id.clone(), entry)
            })
            .collect();
        let (patches_tx, _) = broadcast::channel(config.patch_channel_capacity.max(1));
        let (history_tx, _) = broadcast::channel(256);
        let (captures_tx, _) = broadcast::channel(64);
        let (usage_tx, _) = broadcast::channel(8);
        let engine = Self {
            inner: Arc::new(Inner {
                config,
                deps,
                state: Mutex::new(EngineState {
                    seq: 0,
                    replay: VecDeque::new(),
                    docker: DockerStatus::default(),
                    integrations: IntegrationSettings {
                        terminal: settings.terminal.clone(),
                        editor: settings.editor.clone(),
                        browser: settings.browser.clone(),
                        auto_port_remap: settings.auto_port_remap,
                    },
                    // Heal pre-strip_verbatim stores: `\\?\C:\…` entries
                    // display broken and never match a remove.
                    watched_directories: settings
                        .watched_directories
                        .into_iter()
                        .map(mast_compose::strip_verbatim)
                        .collect(),
                    discovered: Vec::new(),
                    projects,
                    workspaces,
                    redactor_all: Redactor::default(),
                }),
                patches_tx,
                ops: Mutex::new(HashMap::new()),
                next_op: AtomicU64::new(0),
                adapter: Mutex::new(None),
                hint_tx: Mutex::new(None),
                busy_projects: Mutex::new(HashSet::new()),
                crash_notices: Mutex::new(crash_notices),
                history: Mutex::new(VecDeque::new()),
                history_tx,
                next_history: AtomicU64::new(0),
                history_started: Mutex::new(HashMap::new()),
                captures_tx,
                live_shares: Mutex::new(HashSet::new()),
                captures_seen: Mutex::new(HashMap::new()),
                usage_tx,
                usage_prev: Mutex::new(HashMap::new()),
                op_contexts: Mutex::new(HashMap::new()),
                command_observer: Mutex::new(None),
            }),
        };
        engine.install_command_observer();
        engine
    }

    pub fn read_only(&self) -> bool {
        self.inner.deps.ownership.is_read_only()
    }

    /// Spawn the effect loops (docker connection/events, file watcher,
    /// reconcile). Requires a tokio runtime.
    pub fn start(&self) {
        effects::start(self.clone());
        self.usage_loop();
    }

    /// Mutate state and emit the corresponding patches under one lock, so
    /// broadcast order always equals seq order. The closure mutates state
    /// directly and pushes one event per semantic change it made.
    pub(crate) fn with_state<R>(
        &self,
        f: impl FnOnce(&mut EngineState, &mut Vec<PatchEvent>) -> R,
    ) -> R {
        let mut st = self.inner.state.lock().unwrap();
        let mut events = Vec::new();
        let result = f(&mut st, &mut events);
        for event in events {
            st.seq += 1;
            let patch = EnginePatch { seq: st.seq, event };
            st.replay.push_back(patch.clone());
            while st.replay.len() > self.inner.config.replay_capacity {
                st.replay.pop_front();
            }
            let _ = self.inner.patches_tx.send(patch);
        }
        result
    }

    pub(crate) fn hint(&self) {
        if let Some(tx) = self.inner.hint_tx.lock().unwrap().as_ref() {
            let _ = tx.try_send(());
        }
    }

    pub fn snapshot(&self) -> EngineSnapshot {
        let st = self.inner.state.lock().unwrap();
        EngineSnapshot {
            protocol_version: PROTOCOL_VERSION,
            seq: st.seq,
            read_only: self.read_only(),
            docker: st.docker.clone(),
            integrations: st.integrations.clone(),
            watched_directories: st
                .watched_directories
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            discovered: st.discovered.clone(),
            projects: st.projects.values().map(|e| e.summary.clone()).collect(),
            workspaces: workspace_summaries(&st),
        }
    }

    /// Stream patches with `seq > after_seq` (`None` = from the current tip).
    /// Ends after emitting `ResyncRequired` (gap, ahead-of-engine, or lag).
    pub fn subscribe(&self, after_seq: Option<u64>) -> BoxStream<'static, SubscriptionItem> {
        // Subscribe to live patches BEFORE reading state so nothing can slip
        // between the replay snapshot and the live stream.
        let live_rx = self.inner.patches_tx.subscribe();
        let (buffered, gap, after) = {
            let st = self.inner.state.lock().unwrap();
            let after = after_seq.unwrap_or(st.seq);
            let oldest = st.replay.front().map(|p| p.seq);
            let gap = after > st.seq
                || (after < st.seq && oldest.is_none_or(|oldest| after + 1 < oldest));
            let buffered: Vec<EnginePatch> = if gap {
                Vec::new()
            } else {
                st.replay.iter().filter(|p| p.seq > after).cloned().collect()
            };
            (buffered, gap, after)
        };

        let (tx, out_rx) =
            mpsc::channel::<SubscriptionItem>(self.inner.config.subscription_buffer.max(1));
        tokio::spawn(async move {
            if gap {
                let _ = tx.send(SubscriptionItem::ResyncRequired).await;
                return;
            }
            let mut last = after;
            for patch in buffered {
                last = patch.seq;
                if tx.send(SubscriptionItem::Patch { patch }).await.is_err() {
                    return;
                }
            }
            let mut live_rx = live_rx;
            loop {
                match live_rx.recv().await {
                    Ok(patch) => {
                        if patch.seq <= last {
                            continue; // already delivered via replay
                        }
                        if patch.seq != last + 1 {
                            let _ = tx.send(SubscriptionItem::ResyncRequired).await;
                            return;
                        }
                        last = patch.seq;
                        if tx.send(SubscriptionItem::Patch { patch }).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = tx.send(SubscriptionItem::ResyncRequired).await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .boxed()
    }

    pub fn dispatch(&self, action: Action) -> Result<OperationId, ErrorInfo> {
        // Plan §1: observation converges across instances; mutation does not.
        // Local tool launches are allowed read-only — they change nothing.
        let mutating = !matches!(
            action,
            Action::RefreshNow
                | Action::StartFakeOperation { .. }
                | Action::OpenTerminal { .. }
                | Action::ShellIntoContainer { .. }
                | Action::OpenInEditor { .. }
                | Action::RevealInFileManager { .. }
                | Action::OpenInBrowser { .. }
                | Action::OpenUrl { .. }
                | Action::OpenHostsFile
                | Action::RevealPath { .. }
                | Action::OpenProjectFile { .. }
                | Action::OpenTinker { .. }
                | Action::OpenDbUrl { .. }
        );
        if mutating && self.read_only() {
            return Err(ErrorInfo::ReadOnly {
                owner_pid: self.inner.deps.ownership.owner_pid(),
            });
        }
        match &action {
            Action::StartProject { id } => {
                return self.dispatch_lifecycle(id.clone(), LifecycleVerb::Up, None);
            }
            Action::StopProject { id } => {
                return self.dispatch_lifecycle(id.clone(), LifecycleVerb::Stop, None);
            }
            Action::RestartProject { id } => {
                return self.dispatch_lifecycle(id.clone(), LifecycleVerb::Restart, None);
            }
            Action::RebuildProject { id } => {
                return self.dispatch_lifecycle(id.clone(), LifecycleVerb::Rebuild, None);
            }
            Action::RemoveOrphanContainer { id, service } => {
                return self.dispatch_remove_orphan(id.clone(), service.clone());
            }
            Action::StartService { id, service } => {
                return self.dispatch_lifecycle(id.clone(), LifecycleVerb::Up, Some(service.clone()));
            }
            Action::StopService { id, service } => {
                return self.dispatch_lifecycle(
                    id.clone(),
                    LifecycleVerb::Stop,
                    Some(service.clone()),
                );
            }
            Action::RestartService { id, service } => {
                return self.dispatch_lifecycle(
                    id.clone(),
                    LifecycleVerb::Restart,
                    Some(service.clone()),
                );
            }
            Action::RebuildService { id, service } => {
                return self.dispatch_lifecycle(
                    id.clone(),
                    LifecycleVerb::Rebuild,
                    Some(service.clone()),
                );
            }
            Action::SnapshotServiceData { id, service } => {
                return self.dispatch_volume_snapshot(id.clone(), service.clone());
            }
            Action::RestoreServiceData { id, group } => {
                return self.dispatch_volume_restore(id.clone(), group.clone());
            }
            Action::SetPhpVersion { id, service, series } => {
                return self.dispatch_php_switch(id.clone(), service.clone(), series.clone());
            }
            Action::SetNodeVersion { id, service, major } => {
                return self.dispatch_node_switch(id.clone(), service.clone(), major.clone());
            }
            Action::SetPhpExtensions { id, service, extensions } => {
                return self.dispatch_php_extensions(
                    id.clone(),
                    service.clone(),
                    extensions.clone(),
                );
            }
            Action::ShareProject { id } => {
                return self.dispatch_share(id.clone());
            }
            Action::SetLocalDomain { id, domain } => {
                return self.dispatch_set_local_domain(id.clone(), domain.clone());
            }
            _ => {}
        }
        let (id, handle) = self.new_operation();
        // Name the action once, here, so every command it spawns is
        // attributable in history without each op re-deriving a label.
        {
            let (label, project) = {
                let st = self.inner.state.lock().unwrap();
                history::describe_action(&action, |id| {
                    st.projects.get(id).map(|e| e.summary.name.clone())
                })
            };
            self.inner.op_contexts.lock().unwrap().insert(
                id.0,
                history::CommandContext { label, project, operation: Some(id) },
            );
        }
        match action {
            Action::StartFakeOperation { project: _ } => {
                self.run_fake_operation(id, handle);
            }
            Action::ImportProject { path } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    engine.import_project_at(PathBuf::from(&path)).await?;
                    Ok(())
                });
            }
            Action::RemoveProject { id: project_id } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let pid = project_id.0.clone();
                    let engine2 = engine.clone();
                    let removed = tokio::task::spawn_blocking(move || {
                        engine2.inner.deps.store.remove_project(&pid)
                    })
                    .await
                    .map_err(internal_err)?
                    .map_err(internal_err)?;
                    if !removed {
                        return Err(ErrorInfo::NotFound {
                            what: format!("project {}", project_id.0),
                        });
                    }
                    let workspaces = engine.with_state(|st, events| {
                        if st.projects.remove(&project_id.0).is_some() {
                            events.push(PatchEvent::ProjectRemoved { id: project_id.clone() });
                        }
                        let mut touched = false;
                        for ws in st.workspaces.iter_mut() {
                            let before = ws.members.len();
                            ws.members.retain(|m| m.project_id != project_id.0);
                            touched |= ws.members.len() != before;
                            for member in ws.members.iter_mut() {
                                let before = member.depends_on.len();
                                member.depends_on.retain(|d| *d != project_id.0);
                                touched |= member.depends_on.len() != before;
                            }
                        }
                        if touched {
                            events.push(PatchEvent::WorkspacesChanged {
                                workspaces: workspace_summaries(st),
                            });
                            Some(st.workspaces.clone())
                        } else {
                            None
                        }
                    });
                    if let Some(workspaces) = workspaces {
                        engine
                            .inner
                            .deps
                            .store
                            .save_workspaces(&workspaces)
                            .map_err(internal_err)?;
                    }
                    engine.hint();
                    Ok(())
                });
            }
            Action::AddWatchedDirectory { path } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let directory = PathBuf::from(&path);
                    if !directory.is_dir() {
                        return Err(ErrorInfo::InvalidInput {
                            message: format!("not a directory: {path}"),
                        });
                    }
                    let directory =
                        mast_compose::strip_verbatim(directory.canonicalize().unwrap_or(directory));
                    engine.update_watched_directories(|directories| {
                        if !directories.contains(&directory) {
                            directories.push(directory.clone());
                            true
                        } else {
                            false
                        }
                    })?;
                    engine.hint();
                    Ok(())
                });
            }
            Action::RemoveWatchedDirectory { path } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let directory = PathBuf::from(&path);
                    let directory =
                        mast_compose::strip_verbatim(directory.canonicalize().unwrap_or(directory));
                    engine.update_watched_directories(|directories| {
                        let before = directories.len();
                        directories.retain(|f| f != &directory);
                        directories.len() != before
                    })?;
                    engine.hint();
                    Ok(())
                });
            }
            Action::RefreshNow => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    engine.hint();
                    Ok(())
                });
            }
            Action::CaptureServiceLogs { id: project_id, service } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let request = engine
                        .capture_request(&project_id, &service, mast_contract::CaptureReason::Manual)
                        .ok_or_else(|| ErrorInfo::NotFound {
                            what: format!("no container for service {service}"),
                        })?;
                    // A manual capture is the one case where the user is
                    // watching for the result, so bypass repeat suppression:
                    // asking twice means they want a second look.
                    engine.run_capture_forced(request).await;
                    Ok(())
                });
            }
            Action::ClearLogCaptures => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    engine.clear_log_captures().await?;
                    Ok(())
                });
            }
            Action::ExportProjectManifest { id: project } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.export_project_manifest(&h, id, &project).await
                });
            }
            Action::RemoveServiceDataSnapshot { group } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.remove_volume_snapshot(&h, id, &group).await
                });
            }
            Action::SaveWorkspace { id: ws_id, name, members } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    if members.is_empty() {
                        return Err(ErrorInfo::InvalidInput {
                            message: "a workspace needs at least one member".into(),
                        });
                    }
                    {
                        let st = engine.inner.state.lock().unwrap();
                        for member in &members {
                            if !st.projects.contains_key(&member.project.0) {
                                return Err(ErrorInfo::NotFound {
                                    what: format!("project {}", member.project.0),
                                });
                            }
                        }
                    }
                    let record = WorkspaceRecord {
                        id: ws_id.map(|w| w.0).unwrap_or_else(|| {
                            format!(
                                "ws-{:x}",
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_nanos())
                                    .unwrap_or(0)
                            )
                        }),
                        name,
                        members: members
                            .iter()
                            .map(|m| WorkspaceMemberRecord {
                                project_id: m.project.0.clone(),
                                depends_on: m.depends_on.iter().map(|d| d.0.clone()).collect(),
                            })
                            .collect(),
                    };
                    engine.mutate_workspaces(|list| {
                        list.retain(|w| w.id != record.id);
                        list.push(record);
                    })
                });
            }
            Action::RemoveWorkspace { id: ws_id } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    engine.mutate_workspaces(|list| list.retain(|w| w.id != ws_id.0))
                });
            }
            Action::StartWorkspace { id: ws_id } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.run_workspace(&h, id, &ws_id, LifecycleVerb::Up).await
                });
            }
            Action::StopWorkspace { id: ws_id } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.run_workspace(&h, id, &ws_id, LifecycleVerb::Stop).await
                });
            }
            Action::TakeSnapshot { workspace, name } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let member_ids: Vec<String> = {
                        let st = engine.inner.state.lock().unwrap();
                        let record = st
                            .workspaces
                            .iter()
                            .find(|w| w.id == workspace.0)
                            .ok_or(ErrorInfo::NotFound {
                                what: format!("workspace {}", workspace.0),
                            })?;
                        record.members.iter().map(|m| m.project_id.clone()).collect()
                    };
                    let mut members = Vec::new();
                    for member in &member_ids {
                        if let Some(state) = engine.capture_member_state(member).await {
                            members.push(state);
                        }
                    }
                    let taken_unix = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    engine
                        .inner
                        .deps
                        .store
                        .push_snapshot(mast_project::SnapshotRecord {
                            id: format!("snap-{taken_unix}-{}", workspace.0),
                            workspace_id: workspace.0.clone(),
                            name,
                            taken_unix,
                            members,
                        })
                        .map_err(internal_err)
                });
            }
            Action::RemoveSnapshot { id: snap_id } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let removed = engine
                        .inner
                        .deps
                        .store
                        .remove_snapshot(&snap_id)
                        .map_err(internal_err)?;
                    if removed {
                        Ok(())
                    } else {
                        Err(ErrorInfo::NotFound { what: format!("snapshot {snap_id}") })
                    }
                });
            }
            Action::AttachNetwork { workspace, project } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let (network, invocation, file) =
                        engine.network_attach_context(&workspace, &project)?;
                    let source = tokio::task::spawn_blocking({
                        let file = file.clone();
                        move || std::fs::read_to_string(file)
                    })
                    .await
                    .map_err(internal_err)?
                    .map_err(internal_err)?;
                    let plan = mast_compose::plan_network_attach(&source, &network)
                        .map_err(|message| ErrorInfo::InvalidInput { message })?;
                    if plan.edits.is_empty() {
                        return Ok(()); // already attached everywhere
                    }
                    let backups = engine.inner.deps.store.backups_dir();
                    mast_compose::apply_compose_edit(
                        &invocation,
                        &file,
                        &plan.edits,
                        Some(&backups),
                    )
                    .await
                    .map_err(|e| match e {
                        mast_compose::ComposeEditError::ConflictExternalEdit => {
                            ErrorInfo::Conflict { message: e.to_string() }
                        }
                        other => ErrorInfo::InvalidInput { message: other.to_string() },
                    })?;
                    engine.hint();
                    Ok(())
                });
            }
            Action::ApplyRepair { repair, arg, project } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.apply_repair(&h, id, &repair, arg.as_deref(), project.as_ref()).await
                });
            }
            Action::AddCatalogService { id: project, service } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.apply_catalog(&h, id, &project, &service, false).await
                });
            }
            Action::RemoveCatalogService { id: project, service } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.apply_catalog(&h, id, &project, &service, true).await
                });
            }
            Action::AddCustomService { id: project, spec } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.apply_custom_service(&h, id, &project, &spec).await
                });
            }
            Action::SetServiceImage { id: project, service, image } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.apply_service_image(&h, id, &project, &service, &image).await
                });
            }
            Action::RemoveService { id: project, service } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let (invocation, file) = engine.catalog_context(&project)?;
                    let source = tokio::task::spawn_blocking({
                        let file = file.clone();
                        move || std::fs::read_to_string(file)
                    })
                    .await
                    .map_err(internal_err)?
                    .map_err(internal_err)?;
                    let plan = mast_compose::catalog::plan_service_remove(&source, &service)
                        .map_err(|message| ErrorInfo::InvalidInput { message })?;
                    let backups = engine.inner.deps.store.backups_dir();
                    mast_compose::apply_compose_edit(&invocation, &file, &plan.edits, Some(&backups))
                        .await
                        .map_err(|e| match e {
                            mast_compose::ComposeEditError::ConflictExternalEdit => {
                                ErrorInfo::Conflict { message: e.to_string() }
                            }
                            other => ErrorInfo::InvalidInput { message: other.to_string() },
                        })?;
                    engine.hint();
                    Ok(())
                });
            }
            Action::CreateProject { parent, name, php, services } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.create_project(&h, id, &parent, &name, &php, &services).await
                });
            }
            Action::CloneProject { url, parent, name } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.clone_project(&h, id, &url, &parent, &name).await
                });
            }
            Action::StartProcess { id: project, process } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.start_process(&h, id, &project, &process).await
                });
            }
            Action::StopProcess { id: project, process } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.stop_process(&h, id, &project, &process).await
                });
            }
            Action::SetProjectCommands { id: project, commands } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    // Clients send the merged list back; entries the manifest
                    // contributed live in mast.yml, not app data, so they are
                    // dropped here rather than persisted as copies.
                    let commands: Vec<_> =
                        commands.into_iter().filter(|c| !c.from_manifest).collect();
                    let mut names = std::collections::HashSet::new();
                    for c in &commands {
                        if c.name.trim().is_empty() || c.command.trim().is_empty() {
                            return Err(ErrorInfo::InvalidInput {
                                message: "command name and command line must be non-empty".into(),
                            });
                        }
                        if !names.insert(c.name.clone()) {
                            return Err(ErrorInfo::InvalidInput {
                                message: format!("duplicate command name \"{}\"", c.name),
                            });
                        }
                        // A bad glob would otherwise surface as a watch that
                        // silently never fires. Refuse it here, in the dialog.
                        for pattern in &c.restart_when_changed {
                            if let Err(e) = glob::Pattern::new(pattern) {
                                return Err(ErrorInfo::InvalidInput {
                                    message: format!(
                                        "\"{}\": \"{pattern}\" is not a glob pattern ({e})",
                                        c.name
                                    ),
                                });
                            }
                        }
                    }
                    let records: Vec<mast_project::ProjectCommandRecord> = commands
                        .iter()
                        .map(|c| mast_project::ProjectCommandRecord {
                            name: c.name.clone(),
                            command: c.command.clone(),
                            auto_start: c.auto_start,
                            cwd: c.cwd.clone().filter(|w| !w.trim().is_empty()),
                            after: c.after.clone().filter(|a| !a.trim().is_empty()),
                            ready_when: c.ready_when.clone().filter(|r| !r.trim().is_empty()),
                            auto_restart: c.auto_restart,
                            restart_when_changed: c
                                .restart_when_changed
                                .iter()
                                .map(|p| p.trim().to_string())
                                .filter(|p| !p.is_empty())
                                .collect(),
                        })
                        .collect();
                    let all = engine.with_state(|st, events| {
                        if let Some(entry) = st.projects.get_mut(&project.0) {
                            // A dependency that cannot be satisfied is worse
                            // than no dependency: the command simply never
                            // starts, and the only symptom is a chip that
                            // stays grey. Refuse it here, where there is a
                            // dialog to show the reason in — and check the
                            // merged view, since `after` may point at a
                            // manifest command.
                            let merged =
                                crate::manifest::merged(&entry.manifest.commands, &commands);
                            if let Err(message) = crate::commands::check_order(&merged) {
                                return Err(ErrorInfo::InvalidInput { message });
                            }
                            entry.record.commands = records.clone();
                            entry.summary.commands = merged;
                            events.push(PatchEvent::ProjectUpdated {
                                project: entry.summary.clone(),
                            });
                        }
                        Ok(st.projects.values().map(|e| e.record.clone()).collect::<Vec<_>>())
                    })?;
                    engine
                        .inner
                        .deps
                        .store
                        .save_projects(&all)
                        .map_err(internal_err)
                });
            }
            Action::RunProjectCommand { id: project, name } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    engine.run_project_command(&h, id, &project, &name).await
                });
            }
            Action::OpenTerminal { id: project } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let (path, terminal) = engine.project_launch_context(&project)?;
                    integrations::open_terminal(terminal.as_deref(), &path, None)
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::ShellIntoContainer { id: project, service } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let (path, terminal) = engine.project_launch_context(&project)?;
                    let container_id = {
                        let st = engine.inner.state.lock().unwrap();
                        st.projects
                            .get(&project.0)
                            .and_then(|e| {
                                e.summary
                                    .services
                                    .iter()
                                    .find(|s| s.name == service)
                                    .and_then(|s| s.container_id.clone())
                            })
                            .ok_or(ErrorInfo::NotFound {
                                what: format!("running container for service {service}"),
                            })?
                    };
                    let command = integrations::container_shell_command(&container_id);
                    integrations::open_terminal(terminal.as_deref(), &path, Some(&command))
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::OpenInEditor { id: project } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let path = engine.project_path(&project)?;
                    let editor = engine.inner.state.lock().unwrap().integrations.editor.clone();
                    integrations::open_editor(editor.as_deref(), &path, None)
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::OpenTinker { id: project } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let (path, terminal) = engine.project_launch_context(&project)?;
                    // Tinker lives in the app service's container; the other
                    // services have no artisan to speak of.
                    let app_service = crate::project_ops::app_service_of(&path);
                    let container_id = {
                        let st = engine.inner.state.lock().unwrap();
                        st.projects
                            .get(&project.0)
                            .and_then(|e| {
                                e.summary
                                    .services
                                    .iter()
                                    .find(|s| s.name == app_service)
                                    .and_then(|s| s.container_id.clone())
                            })
                            .ok_or(ErrorInfo::InvalidInput {
                                message: "the app container is not running — start the \
                                          project first"
                                    .into(),
                            })?
                    };
                    let command = integrations::container_tinker_command(&container_id);
                    integrations::open_terminal(terminal.as_deref(), &path, Some(&command))
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::OpenDbUrl { url } => {
                self.spawn_operation(id, handle, async move {
                    integrations::open_db_url(&url)
                        .map_err(|message| ErrorInfo::InvalidInput { message })
                });
            }
            Action::RevealInFileManager { id: project } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let path = engine.project_path(&project)?;
                    integrations::reveal_in_file_manager(&path)
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::OpenInBrowser { id: project } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    // The reconciled summary is the same address the UI shows,
                    // so the button never opens something else than its label.
                    let (url, browser) = {
                        let st = engine.inner.state.lock().unwrap();
                        let url = st
                            .projects
                            .get(&project.0)
                            .and_then(|e| e.summary.app_url.clone())
                            .ok_or(ErrorInfo::NotFound {
                                what: "APP_URL for this project".to_string(),
                            })?;
                        (url, st.integrations.browser.clone())
                    };
                    integrations::open_in_browser(browser.as_deref(), &url)
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::OpenUrl { url } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let browser =
                        engine.inner.state.lock().unwrap().integrations.browser.clone();
                    integrations::open_in_browser(browser.as_deref(), &url)
                        .map_err(|message| ErrorInfo::InvalidInput { message })
                });
            }
            Action::OpenHostsFile => {
                self.spawn_operation(id, handle, async move {
                    integrations::open_path(std::path::Path::new(proxy::hosts_file_path()))
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::OpenProjectFile { id: project, file, line } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let root = engine.project_path(&project)?;
                    let relative = std::path::Path::new(&file);
                    let escapes = relative.is_absolute()
                        || relative
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir));
                    if escapes {
                        return Err(ErrorInfo::InvalidInput {
                            message: format!("{file} is not a path inside the project"),
                        });
                    }
                    let target = root.join(relative);
                    if !target.is_file() {
                        return Err(ErrorInfo::InvalidInput {
                            message: format!("{} does not exist", target.display()),
                        });
                    }
                    let editor = engine.inner.state.lock().unwrap().integrations.editor.clone();
                    integrations::open_editor(editor.as_deref(), &target, line)
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::RevealPath { path } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let candidate = PathBuf::from(&path);
                    let known = {
                        let st = engine.inner.state.lock().unwrap();
                        st.watched_directories.contains(&candidate)
                            || st.projects.values().any(|e| e.record.path == candidate)
                    };
                    if !known {
                        return Err(ErrorInfo::InvalidInput {
                            message: format!(
                                "{path} is not a watched directory or project — nothing to open"
                            ),
                        });
                    }
                    integrations::open_path(&candidate)
                        .map_err(|message| ErrorInfo::Internal { message })
                });
            }
            Action::ClearLaravelLog { id: project } => {
                let engine = self.clone();
                let h = handle.clone();
                self.spawn_operation(id, handle, async move {
                    let path = {
                        let st = engine.inner.state.lock().unwrap();
                        st.projects
                            .get(&project.0)
                            .ok_or(ErrorInfo::NotFound {
                                what: format!("project {}", project.0),
                            })?
                            .record
                            .path
                            .join("storage/logs/laravel.log")
                    };
                    let cleared = tokio::task::spawn_blocking(move || {
                        if !path.is_file() {
                            return Ok(false);
                        }
                        // Truncate in place: running PHP workers keep their
                        // open handle and append to the same (now empty) file.
                        std::fs::write(&path, "").map(|()| true)
                    })
                    .await
                    .map_err(internal_err)?
                    .map_err(internal_err)?;
                    engine.emit_op(
                        &h,
                        id,
                        OperationEventKind::Output {
                            line: if cleared {
                                "laravel.log cleared".into()
                            } else {
                                "no laravel.log to clear".into()
                            },
                            stderr: false,
                        },
                    );
                    Ok(())
                });
            }
            Action::SetEnvVar { id: project, key: env_key, value } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let path = engine.project_path(&project)?.join(".env");
                    let backups = engine.inner.deps.store.backups_dir();
                    let summary = vec![format!(
                        "{env_key}={}",
                        if mast_laravel::is_secret_key(&env_key) { REDACTED } else { &value }
                    )];
                    let result = tokio::task::spawn_blocking({
                        let path = path.clone();
                        move || {
                            mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                                f.set(&env_key, &value)
                            })
                        }
                    })
                    .await
                    .map_err(internal_err)?
                    .map_err(env_write_error);
                    engine.record_config_write(&path, summary, &result);
                    result?;
                    engine.hint();
                    Ok(())
                });
            }
            Action::RemoveEnvVar { id: project, key: env_key } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    let path = engine.project_path(&project)?.join(".env");
                    let backups = engine.inner.deps.store.backups_dir();
                    let summary = vec![format!("removed {env_key}")];
                    let result = tokio::task::spawn_blocking({
                        let path = path.clone();
                        move || {
                            mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                                f.remove(&env_key).map(|_| ())
                            })
                        }
                    })
                    .await
                    .map_err(internal_err)?
                    .map_err(env_write_error);
                    engine.record_config_write(&path, summary, &result);
                    result?;
                    engine.hint();
                    Ok(())
                });
            }
            Action::SetIntegrations { integrations: new_settings } => {
                let engine = self.clone();
                self.spawn_operation(id, handle, async move {
                    engine.with_state(|st, events| {
                        if st.integrations != new_settings {
                            st.integrations = new_settings.clone();
                            events.push(PatchEvent::IntegrationsChanged {
                                integrations: new_settings.clone(),
                            });
                        }
                    });
                    engine.persist_settings()
                });
            }
            Action::StartProject { .. }
            | Action::StopProject { .. }
            | Action::RestartProject { .. }
            | Action::RebuildProject { .. }
            | Action::RemoveOrphanContainer { .. }
            | Action::StartService { .. }
            | Action::StopService { .. }
            | Action::RestartService { .. }
            | Action::RebuildService { .. }
            | Action::SetPhpVersion { .. }
            | Action::SetNodeVersion { .. }
            | Action::SetPhpExtensions { .. }
            | Action::ShareProject { .. }
            | Action::SetLocalDomain { .. }
            | Action::SnapshotServiceData { .. }
            | Action::RestoreServiceData { .. } => unreachable!("handled above"),
        }
        Ok(id)
    }

    fn persist_settings(&self) -> Result<(), ErrorInfo> {
        let settings = {
            let st = self.inner.state.lock().unwrap();
            mast_project::Settings {
                watched_directories: st.watched_directories.clone(),
                terminal: st.integrations.terminal.clone(),
                editor: st.integrations.editor.clone(),
                browser: st.integrations.browser.clone(),
                auto_port_remap: st.integrations.auto_port_remap,
            }
        };
        self.inner
            .deps
            .store
            .save_settings(&settings)
            .map_err(internal_err)
    }

    fn update_watched_directories(
        &self,
        mutate: impl FnOnce(&mut Vec<PathBuf>) -> bool,
    ) -> Result<(), ErrorInfo> {
        let changed = self.with_state(|st, events| {
            let changed = mutate(&mut st.watched_directories);
            if changed {
                events.push(PatchEvent::WatchedDirectoriesChanged {
                    directories: st
                        .watched_directories
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect(),
                });
            }
            changed.then(|| st.watched_directories.clone())
        });
        if changed.is_some() {
            self.persist_settings()?;
        }
        Ok(())
    }

}

/// Build workspace summaries from records + current project statuses.
pub(crate) fn workspace_summaries(st: &EngineState) -> Vec<WorkspaceSummary> {
    st.workspaces
        .iter()
        .map(|record| {
            let members: Vec<WorkspaceMember> = record
                .members
                .iter()
                .map(|m| WorkspaceMember {
                    project: ProjectId(m.project_id.clone()),
                    depends_on: m.depends_on.iter().map(|d| ProjectId(d.clone())).collect(),
                })
                .collect();
            let graph: Vec<(String, Vec<String>)> = record
                .members
                .iter()
                .map(|m| (m.project_id.clone(), m.depends_on.clone()))
                .collect();
            // Humanize graph errors: users see project names, not id hashes.
            let graph_error = workspace::topo_layers(&graph).err().map(|mut err| {
                for m in &record.members {
                    if let Some(entry) = st.projects.get(&m.project_id) {
                        err = err.replace(&m.project_id, &entry.summary.name);
                    }
                }
                err
            });
            let statuses: Vec<ProjectStatus> = record
                .members
                .iter()
                .filter_map(|m| st.projects.get(&m.project_id).map(|e| e.summary.status))
                .collect();
            let status = if statuses.is_empty()
                || statuses.iter().all(|s| *s == ProjectStatus::Stopped)
            {
                ProjectStatus::Stopped
            } else if statuses.iter().all(|s| *s == ProjectStatus::Running) {
                ProjectStatus::Running
            } else if statuses.contains(&ProjectStatus::Starting) {
                ProjectStatus::Starting
            } else {
                ProjectStatus::Degraded
            };
            // Cross-project env validation (M6): host ports published by more
            // than one member collide at `up` time — surface it up front.
            let mut port_users: BTreeMap<u16, Vec<String>> = BTreeMap::new();
            for member in &record.members {
                if let Some(entry) = st.projects.get(&member.project_id) {
                    for (key, port) in &entry.host_ports {
                        port_users
                            .entry(*port)
                            .or_default()
                            .push(format!("{} ({key})", entry.summary.name));
                    }
                }
            }
            let warnings: Vec<String> = port_users
                .into_iter()
                .filter(|(_, users)| users.len() > 1)
                .map(|(port, users)| {
                    format!("host port {port} is published by {}", users.join(" and "))
                })
                .collect();

            WorkspaceSummary {
                id: WorkspaceId(record.id.clone()),
                name: record.name.clone(),
                members,
                status,
                graph_error,
                warnings,
            }
        })
        .collect()
}


fn env_write_error(e: mast_laravel::EnvWriteError) -> ErrorInfo {
    match e {
        mast_laravel::EnvWriteError::Conflict => {
            ErrorInfo::Conflict { message: e.to_string() }
        }
        mast_laravel::EnvWriteError::Env(inner) => {
            ErrorInfo::InvalidInput { message: inner.to_string() }
        }
        other => ErrorInfo::Internal { message: other.to_string() },
    }
}

// ---------- observation → contract mapping helpers (used by effects) ----------

pub(crate) fn map_container_state(raw: &str) -> ContainerState {
    match raw {
        "created" => ContainerState::Created,
        "running" => ContainerState::Running,
        "restarting" => ContainerState::Restarting,
        "paused" => ContainerState::Paused,
        "exited" => ContainerState::Exited,
        "dead" => ContainerState::Dead,
        "removing" => ContainerState::Removing,
        _ => ContainerState::Unknown,
    }
}

pub(crate) fn map_health(raw: Option<&str>) -> ServiceHealth {
    match raw {
        Some("healthy") => ServiceHealth::Healthy,
        Some("unhealthy") => ServiceHealth::Unhealthy,
        Some("starting") => ServiceHealth::Starting,
        _ => ServiceHealth::Unknown,
    }
}

pub(crate) fn derive_status(services: &[ServiceState]) -> ProjectStatus {
    let running = services
        .iter()
        .filter(|s| matches!(s.state, Some(ContainerState::Running)))
        .count();
    if running == 0 {
        ProjectStatus::Stopped
    } else if running == services.len() {
        ProjectStatus::Running
    } else {
        ProjectStatus::Degraded
    }
}
