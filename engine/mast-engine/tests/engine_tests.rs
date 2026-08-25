//! Engine behavior through the public API only, with a fake runtime adapter —
//! the same surface the desktop client uses. Real-docker coverage lives in
//! `sail_minimal.rs` (docker-gated).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::BoxStream;
use mast_contract::{
    Action, DockerStatus, EngineSnapshot, ErrorInfo, OperationEventKind, ProjectStatus,
    SubscriptionItem,
};
use mast_docker::{
    CapturedLine, CommandOutcome, ContainerObservation, DockerError, LogChunk, OutputLine,
    RuntimeAdapter, RuntimeEvent, StatsSample,
};
use mast_engine::{
    Engine, EngineConfig, EngineDeps, LifecycleRunner, LifecycleVerb, RealLifecycleRunner,
    RuntimeConnector, acquire_ownership,
};
use mast_project::MetadataStore;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct FakeAdapter {
    containers: Mutex<Vec<ContainerObservation>>,
    events_tx: broadcast::Sender<RuntimeEvent>,
}

impl FakeAdapter {
    fn new() -> Arc<Self> {
        Arc::new(Self { containers: Mutex::new(Vec::new()), events_tx: broadcast::channel(64).0 })
    }

    fn set_containers(&self, containers: Vec<ContainerObservation>) {
        *self.containers.lock().unwrap() = containers;
        let _ = self.events_tx.send(RuntimeEvent);
    }
}

#[async_trait::async_trait]
impl RuntimeAdapter for FakeAdapter {
    async fn ping(&self) -> Result<(), DockerError> {
        Ok(())
    }
    async fn list_compose_containers(&self) -> Result<Vec<ContainerObservation>, DockerError> {
        Ok(self.containers.lock().unwrap().clone())
    }
    async fn events(&self) -> Result<BoxStream<'static, RuntimeEvent>, DockerError> {
        let rx = self.events_tx.subscribe();
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => return Some((event, rx)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed())
    }

    async fn container_logs(
        &self,
        _container_id: &str,
        _tail: u32,
    ) -> Result<BoxStream<'static, LogChunk>, DockerError> {
        Ok(futures::stream::iter(vec![
            LogChunk { message: "fake log line".into(), stderr: false },
        ])
        .boxed())
    }

    async fn container_log_tail(
        &self,
        _container_id: &str,
        _since_unix: i64,
        _max_lines: u32,
    ) -> Result<Vec<CapturedLine>, DockerError> {
        Ok(vec![CapturedLine {
            at: Some("2026-08-12T14:22:03.000000000Z".into()),
            message: "fake captured line".into(),
            stderr: false,
        }])
    }

    async fn container_stats(&self, _container_id: &str) -> Result<StatsSample, DockerError> {
        Ok(StatsSample::default())
    }
}

type RunBehavior =
    Box<dyn Fn(&std::path::Path, LifecycleVerb) -> i32 + Send + Sync>;

/// Scripted lifecycle runner: emits a line, waits (cancellable), exits with a
/// per-call code decided by `behavior`. Records (project dir name, verb).
struct FakeRunner {
    delay: Duration,
    first_line: String,
    behavior: RunBehavior,
    calls: Mutex<Vec<(String, LifecycleVerb)>>,
}

impl FakeRunner {
    fn new(delay: Duration, exit: i32) -> Arc<Self> {
        Self::with_line(delay, exit, "pulling images")
    }

    fn with_line(delay: Duration, exit: i32, first_line: &str) -> Arc<Self> {
        Self::with_behavior(delay, first_line, Box::new(move |_, _| exit))
    }

    fn with_behavior(delay: Duration, first_line: &str, behavior: RunBehavior) -> Arc<Self> {
        Arc::new(Self {
            delay,
            first_line: first_line.to_string(),
            behavior,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn verbs(&self) -> Vec<LifecycleVerb> {
        self.calls.lock().unwrap().iter().map(|(_, v)| *v).collect()
    }

    fn call_names(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(n, _)| n.clone()).collect()
    }
}

#[async_trait::async_trait]
impl LifecycleRunner for FakeRunner {
    async fn run(
        &self,
        invocation: &mast_compose::ComposeInvocation,
        verb: LifecycleVerb,
        _service: Option<&str>,
        lines: tokio::sync::mpsc::Sender<OutputLine>,
        cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        let name = invocation
            .project_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.calls.lock().unwrap().push((name, verb));
        let _ = lines.send(OutputLine { line: self.first_line.clone(), stderr: false }).await;
        tokio::select! {
            _ = cancel.cancelled() => return Ok(CommandOutcome::Cancelled),
            _ = tokio::time::sleep(self.delay) => {}
        }
        let _ = lines.send(OutputLine { line: "warning: WWWUSER".into(), stderr: true }).await;
        Ok(CommandOutcome::Exited((self.behavior)(&invocation.project_dir, verb)))
    }
}

struct FakeConnector(Arc<FakeAdapter>);

#[async_trait::async_trait]
impl RuntimeConnector for FakeConnector {
    async fn connect(&self) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError> {
        Ok((
            self.0.clone(),
            DockerStatus {
                available: true,
                context_name: Some("fake".into()),
                endpoint: Some("unix:///fake.sock".into()),
                error: None,
            },
        ))
    }
}

struct DeadConnector;

#[async_trait::async_trait]
impl RuntimeConnector for DeadConnector {
    async fn connect(&self) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError> {
        Err(DockerError::Api("daemon unreachable".into()))
    }
}

fn test_engine_with(
    meta_dir: &Path,
    connector: Arc<dyn RuntimeConnector>,
    runner: Arc<dyn LifecycleRunner>,
) -> Engine {
    Engine::new(
        EngineConfig {
            hint_debounce: Duration::from_millis(30),
            reconcile_interval: Duration::from_secs(60),
            ready_grace: Duration::from_millis(200),
            registry_refresh: false,
            ..Default::default()
        },
        EngineDeps {
            connector,
            store: MetadataStore::open(meta_dir.join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner,
            ownership: acquire_ownership(Some(meta_dir.join("lock"))),
        },
    )
}

fn test_engine(meta_dir: &Path, connector: Arc<dyn RuntimeConnector>) -> Engine {
    test_engine_with(meta_dir, connector, Arc::new(RealLifecycleRunner))
}

async fn wait_until(
    engine: &Engine,
    what: &str,
    predicate: impl Fn(&EngineSnapshot) -> bool,
) -> EngineSnapshot {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snap = engine.snapshot();
        if predicate(&snap) {
            return snap;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {snap:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Dispatch an action and wait for its terminal event, asserting completion.
async fn run_action(engine: &Engine, action: Action) {
    let id = engine.dispatch(action).unwrap();
    let mut events = engine.operation_events(id).unwrap();
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Completed => return,
            OperationEventKind::Failed { error } => panic!("operation failed: {error}"),
            OperationEventKind::Cancelled => panic!("operation cancelled"),
            _ => {}
        }
    }
    panic!("operation event stream ended without terminal event");
}

fn make_project(dir: &Path, name: &str) -> std::path::PathBuf {
    let project = dir.join(name);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n",
    )
    .unwrap();
    project
}

fn observation(project_name: &str, project_dir: &Path, service: &str, state: &str) -> ContainerObservation {
    ContainerObservation {
        id: format!("cid-{service}"),
        name: format!("{project_name}-{service}-1"),
        project: project_name.into(),
        service: service.into(),
        config_files: vec![project_dir.join("compose.yaml").to_string_lossy().into_owned()],
        working_dir: Some(project_dir.to_string_lossy().into_owned()),
        state: state.into(),
        health: None,
        exit_code: None,
        config_hash: Some("hash".into()),
        networks: vec![format!("{project_name}_default")],
        published_ports: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn import_observe_and_reflect_terminal_changes() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "observeapp");
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();

    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let project_summary = &snap.projects[0];
    let compose_name = project_summary.compose_project_name.clone().unwrap();
    assert!(!project_summary.services.is_empty(), "model services should be declared");
    assert_eq!(project_summary.status, ProjectStatus::Stopped);

    // Simulate `docker compose up` from a terminal: containers appear + event.
    let canonical = project.canonicalize().unwrap();
    let started = Instant::now();
    adapter.set_containers(vec![observation(&compose_name, &canonical, "app", "running")]);
    let snap = wait_until(&engine, "project running", |s| {
        s.projects[0].status == ProjectStatus::Running
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "reflection took {:?}",
        started.elapsed()
    );
    let service = &snap.projects[0].services[0];
    assert_eq!(service.name, "app");
    assert!(service.container_id.is_some());

    // Containers from a same-name project in a DIFFERENT directory must not
    // associate (ADR cross-check).
    let elsewhere = tmp.path().join("elsewhere");
    adapter.set_containers(vec![observation(&compose_name, &elsewhere, "app", "running")]);
    wait_until(&engine, "foreign container ignored", |s| {
        s.projects[0].status == ProjectStatus::Stopped
    })
    .await;

    // Terminal `docker compose down`: containers vanish.
    adapter.set_containers(vec![]);
    wait_until(&engine, "project stopped", |s| s.projects[0].status == ProjectStatus::Stopped)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_unavailable_is_surfaced_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path(), Arc::new(DeadConnector));
    engine.start();
    let snap = wait_until(&engine, "docker unavailable", |s| {
        !s.docker.available && s.docker.error.is_some()
    })
    .await;
    assert!(snap.docker.error.unwrap().contains("daemon unreachable"));
}

#[tokio::test(flavor = "multi_thread")]
async fn compose_file_deletion_degrades_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "degrade");
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter)));
    engine.start();

    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;

    std::fs::remove_file(project.join("compose.yaml")).unwrap();
    run_action(&engine, Action::RefreshNow).await;
    let snap = wait_until(&engine, "resolution error", |s| {
        s.projects.first().is_some_and(|p| p.resolution_error.is_some())
    })
    .await;
    assert_eq!(snap.projects.len(), 1, "project must stay listed");
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_lists_candidates_until_imported() {
    let tmp = tempfile::tempdir().unwrap();
    let code = tmp.path().join("code");
    std::fs::create_dir(&code).unwrap();
    let project = make_project(&code, "candidate");
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter)));
    engine.start();

    run_action(&engine, Action::AddWatchedDirectory { path: code.to_string_lossy().into() }).await;
    wait_until(&engine, "candidate discovered", |s| {
        s.discovered.iter().any(|d| d.name == "candidate")
    })
    .await;

    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "candidate promoted", |s| {
        s.discovered.is_empty() && s.projects.len() == 1
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_leak_into_events_patches_or_journal() {
    const SECRET: &str = "ultrasecretdbpw99";
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "secretive");
    std::fs::write(
        project.join(".env"),
        format!("APP_NAME=secretive\nDB_PASSWORD={SECRET}\n"),
    )
    .unwrap();
    // The runner echoes the secret, like a chatty compose/db container would.
    let runner =
        FakeRunner::with_line(Duration::from_millis(20), 0, &format!("mysql pw is {SECRET}"));
    let adapter = FakeAdapter::new();
    let engine =
        test_engine_with(tmp.path(), Arc::new(FakeConnector(adapter)), runner);
    engine.start();

    let mut patches = engine.subscribe(Some(0));
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;

    let id = project_id(&engine);
    let op = engine.dispatch(Action::StartProject { id }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut saw_redacted_output = false;
    while let Some(event) = events.next().await {
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains(SECRET), "operation event leaked the secret: {json}");
        if let OperationEventKind::Output { line, .. } = &event.kind
            && line.contains(mast_engine::REDACTED)
        {
            saw_redacted_output = true;
        }
        if event.kind.is_terminal() {
            break;
        }
    }
    assert!(saw_redacted_output);

    // Snapshot and every patch emitted so far must be clean too.
    let snapshot_json = serde_json::to_string(&engine.snapshot()).unwrap();
    assert!(!snapshot_json.contains(SECRET));
    let mut drained = 0;
    while drained < 50 {
        match tokio::time::timeout(Duration::from_millis(200), patches.next()).await {
            Ok(Some(item)) => {
                let json = serde_json::to_string(&item).unwrap();
                assert!(!json.contains(SECRET), "patch leaked the secret: {json}");
                drained += 1;
            }
            _ => break,
        }
    }
    assert!(drained > 0, "expected some patches");

    // Nothing persisted under the metadata dir may contain the secret.
    for entry in std::fs::read_dir(tmp.path().join("meta")).unwrap().flatten() {
        if entry.path().is_file() {
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !content.contains(SECRET),
                "persisted file {} leaked the secret",
                entry.path().display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_operation_is_noticed_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "crashy");
    // Simulate the previous session: project imported, op journaled, then crash.
    let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
    let record = store.import_project(&project).unwrap();
    store
        .journal_push(mast_project::OperationJournalEntry {
            operation: 3,
            project_id: record.id.clone(),
            verb: "start".into(),
            started_unix: 1,
        })
        .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter)));
    engine.start();
    let snap = wait_until(&engine, "crash notice", |s| {
        s.projects.first().is_some_and(|p| p.warnings.iter().any(|w| w.contains("interrupted")))
    })
    .await;
    assert!(snap.projects[0].warnings.iter().any(|w| w.contains("start")));
    // The journal was consumed — a second restart would be clean.
    let store2 = MetadataStore::open(tmp.path().join("meta")).unwrap();
    assert!(store2.load_journal().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn journal_entry_lives_only_while_operation_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new(Duration::from_secs(5), 0);
    let (engine, _adapter) = imported_resolved_project(&tmp, runner).await;
    let id = project_id(&engine);
    let store = MetadataStore::open(tmp.path().join("meta")).unwrap();

    let op = engine.dispatch(Action::StartProject { id }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let journal = store.load_journal().unwrap();
        if journal.len() == 1 {
            assert_eq!(journal[0].verb, "start");
            break;
        }
        assert!(Instant::now() < deadline, "journal entry never appeared");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    engine.cancel(op).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !store.load_journal().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "journal entry never cleared");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn vendorless_sail_clone_surfaces_bootstrap_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("freshclone");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("composer.json"),
        r#"{"require": {"laravel/sail": "^1.26"}}"#,
    )
    .unwrap();
    std::fs::write(project.join("docker-compose.yml"), "services:\n  app:\n    image: alpine\n")
        .unwrap();
    std::fs::write(project.join(".env.example"), "APP_NAME=x\n").unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter)));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "bootstrap warnings", |s| {
        s.projects.first().is_some_and(|p| p.warnings.len() >= 2)
    })
    .await;
    let warnings = &snap.projects[0].warnings;
    assert!(warnings.iter().any(|w| w.contains("composer install")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains(".env is missing")), "{warnings:?}");
    // Not detected as a *usable* sail project: lifecycle would run bare compose.
    assert!(!snap.projects[0].is_sail);
}

#[tokio::test(flavor = "multi_thread")]
async fn protocol_gap_still_forces_resync() {
    let tmp = tempfile::tempdir().unwrap();
    let dir_a = tmp.path().join("a");
    std::fs::create_dir(&dir_a).unwrap();
    let engine = Engine::new(
        EngineConfig { replay_capacity: 2, ..Default::default() },
        EngineDeps {
            connector: Arc::new(DeadConnector),
            store: MetadataStore::open(tmp.path().join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: acquire_ownership(Some(tmp.path().join("lock"))),
        },
    );
    // No start(): generate patches purely via actions.
    for _ in 0..3 {
        run_action(&engine, Action::AddWatchedDirectory { path: dir_a.to_string_lossy().into() })
            .await;
        run_action(&engine, Action::RemoveWatchedDirectory { path: dir_a.to_string_lossy().into() })
            .await;
    }
    assert!(engine.snapshot().seq >= 5);
    let mut stream = engine.subscribe(Some(1));
    assert!(matches!(stream.next().await, Some(SubscriptionItem::ResyncRequired)));

    // Tail subscription stays contiguous.
    let mut tail = engine.subscribe(None);
    run_action(&engine, Action::AddWatchedDirectory { path: dir_a.to_string_lossy().into() }).await;
    match tail.next().await.unwrap() {
        SubscriptionItem::Patch { patch } => assert_eq!(patch.seq, engine.snapshot().seq),
        other => panic!("expected patch, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_operation_still_streams_and_cancels() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path(), Arc::new(DeadConnector));
    let id = engine
        .dispatch(Action::StartFakeOperation {
            project: mast_contract::ProjectId("any".into()),
        })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    loop {
        let event = events.next().await.unwrap();
        if matches!(event.kind, OperationEventKind::Progress { .. }) {
            break;
        }
    }
    engine.cancel(id).unwrap();
    loop {
        let event = events.next().await.unwrap();
        if event.kind.is_terminal() {
            assert!(matches!(event.kind, OperationEventKind::Cancelled));
            break;
        }
    }
    // State untouched by the fake op.
    assert_eq!(engine.snapshot().projects.len(), 0);
}

async fn imported_resolved_project(
    tmp: &tempfile::TempDir,
    runner: Arc<dyn LifecycleRunner>,
) -> (Engine, Arc<FakeAdapter>) {
    let project = make_project(tmp.path(), "lcapp");
    let adapter = FakeAdapter::new();
    let engine = test_engine_with(tmp.path(), Arc::new(FakeConnector(adapter.clone())), runner);
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    (engine, adapter)
}

fn project_id(engine: &Engine) -> mast_contract::ProjectId {
    engine.snapshot().projects[0].id.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_streams_output_completes_and_releases_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new(Duration::from_millis(50), 0);
    let (engine, _adapter) = imported_resolved_project(&tmp, runner.clone()).await;
    let id = project_id(&engine);

    let op = engine.dispatch(Action::StartProject { id: id.clone() }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut kinds = Vec::new();
    while let Some(event) = events.next().await {
        let terminal = event.kind.is_terminal();
        kinds.push(event.kind);
        if terminal {
            break;
        }
    }
    assert!(matches!(kinds.first(), Some(OperationEventKind::Started)));
    assert!(matches!(kinds.last(), Some(OperationEventKind::Completed)));
    let outputs: Vec<(&str, bool)> = kinds
        .iter()
        .filter_map(|k| match k {
            OperationEventKind::Output { line, stderr } => Some((line.as_str(), *stderr)),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, vec![("pulling images", false), ("warning: WWWUSER", true)]);

    // Status was optimistically set to Starting during the op.
    // Lock released: a follow-up verb dispatches fine.
    let op2 = engine.dispatch(Action::StopProject { id }).unwrap();
    let mut events2 = engine.operation_events(op2).unwrap();
    while let Some(event) = events2.next().await {
        if event.kind.is_terminal() {
            break;
        }
    }
    assert_eq!(runner.verbs(), vec![LifecycleVerb::Up, LifecycleVerb::Stop]);
}

/// Dispatch and wait for the terminal event, returning the Failed error if
/// the operation failed (None on Completed/Cancelled).
async fn run_action_capture_failure(engine: &Engine, action: Action) -> Option<String> {
    let id = engine.dispatch(action).unwrap();
    let mut events = engine.operation_events(id).unwrap();
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Failed { error } => return Some(error),
            kind if kind.is_terminal() => return None,
            _ => {}
        }
    }
    None
}

struct WorkspaceRig {
    engine: Engine,
    runner: Arc<FakeRunner>,
    ids: std::collections::HashMap<String, mast_contract::ProjectId>,
}

/// Three projects wsa/wsb/wsc; the runner marks a project's container running
/// in the fake adapter when its Up succeeds, so readiness follows reality.
/// `envs` writes a `.env` into the named project before import.
async fn workspace_rig(
    tmp: &tempfile::TempDir,
    fail_on: Option<&'static str>,
    envs: &[(&str, String)],
) -> WorkspaceRig {
    let adapter = FakeAdapter::new();
    let mut dirs = Vec::new();
    for name in ["wsa", "wsb", "wsc"] {
        let dir = make_project(tmp.path(), name).canonicalize().unwrap();
        if let Some((_, env)) = envs.iter().find(|(n, _)| *n == name) {
            std::fs::write(dir.join(".env"), env).unwrap();
        }
        dirs.push((name, dir));
    }
    let behavior_adapter = adapter.clone();
    let behavior_dirs = dirs.clone();
    let runner = FakeRunner::with_behavior(
        Duration::from_millis(30),
        "up",
        Box::new(move |dir, verb| {
            let name =
                dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if Some(name.as_str()) == fail_on && verb == LifecycleVerb::Up {
                return 1;
            }
            if verb == LifecycleVerb::Up {
                // Container appears — normalized compose name == dir name.
                let canonical = behavior_dirs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, p)| p.clone())
                    .unwrap();
                let mut all = behavior_adapter.containers.lock().unwrap().clone();
                all.push(observation(&name, &canonical, "app", "running"));
                behavior_adapter.set_containers(all);
            }
            0
        }),
    );
    let engine = test_engine_with(tmp.path(), Arc::new(FakeConnector(adapter)), runner.clone());
    engine.start();
    for (_, dir) in &dirs {
        run_action(&engine, Action::ImportProject { path: dir.to_string_lossy().into() }).await;
    }
    wait_until(&engine, "all resolved", |s| {
        s.projects.len() == 3 && s.projects.iter().all(|p| p.compose_project_name.is_some())
    })
    .await;
    let ids = engine
        .snapshot()
        .projects
        .iter()
        .map(|p| (p.name.clone(), p.id.clone()))
        .collect();
    WorkspaceRig { engine, runner, ids }
}

fn chain_members(rig: &WorkspaceRig) -> Vec<mast_contract::WorkspaceMember> {
    // wsb depends on wsa; wsc depends on wsb.
    vec![
        mast_contract::WorkspaceMember { project: rig.ids["wsa"].clone(), depends_on: vec![] },
        mast_contract::WorkspaceMember {
            project: rig.ids["wsb"].clone(),
            depends_on: vec![rig.ids["wsa"].clone()],
        },
        mast_contract::WorkspaceMember {
            project: rig.ids["wsc"].clone(),
            depends_on: vec![rig.ids["wsb"].clone()],
        },
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_starts_in_dependency_order_and_stops_reversed() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(&tmp, None, &[]).await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "suite".into(), members: chain_members(&rig) },
    )
    .await;
    let snap = rig.engine.snapshot();
    assert_eq!(snap.workspaces.len(), 1);
    assert!(snap.workspaces[0].graph_error.is_none());
    let ws = snap.workspaces[0].id.clone();

    let failure =
        run_action_capture_failure(&rig.engine, Action::StartWorkspace { id: ws.clone() }).await;
    assert_eq!(failure, None);
    assert_eq!(rig.runner.call_names(), vec!["wsa", "wsb", "wsc"], "start order");
    wait_until(&rig.engine, "workspace running", |s| {
        s.workspaces[0].status == ProjectStatus::Running
    })
    .await;

    let failure =
        run_action_capture_failure(&rig.engine, Action::StopWorkspace { id: ws }).await;
    assert_eq!(failure, None);
    assert_eq!(
        rig.runner.call_names(),
        vec!["wsa", "wsb", "wsc", "wsc", "wsb", "wsa"],
        "stop reverses the order"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_member_failure_blocks_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(&tmp, Some("wsa"), &[]).await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "suite".into(), members: chain_members(&rig) },
    )
    .await;
    let ws = rig.engine.snapshot().workspaces[0].id.clone();

    let failure =
        run_action_capture_failure(&rig.engine, Action::StartWorkspace { id: ws }).await;
    let failure = failure.expect("start must fail");
    assert!(failure.contains("dependents blocked"), "{failure}");
    assert_eq!(rig.runner.call_names(), vec!["wsa"], "wsb/wsc never started");
}

/// Start a workspace and collect (failure, output lines).
async fn start_workspace_collect(
    engine: &Engine,
    ws: mast_contract::WorkspaceId,
) -> (Option<String>, Vec<String>) {
    let id = engine.dispatch(Action::StartWorkspace { id: ws }).unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut lines = Vec::new();
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => lines.push(line),
            OperationEventKind::Failed { error } => return (Some(error), lines),
            kind if kind.is_terminal() => return (None, lines),
            _ => {}
        }
    }
    (Some("stream ended".into()), lines)
}

/// Tiny scripted HTTP server: serves the nth request with statuses[n]
/// (clamped to the last). Returns (port, hit counter).
async fn spawn_up_server(statuses: Vec<u16>) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let status = *statuses.get(n).unwrap_or_else(|| statuses.last().unwrap());
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = sock.write_all(response.as_bytes()).await;
        }
    });
    (port, hits)
}

#[tokio::test(flavor = "multi_thread")]
async fn http_up_probe_gates_dependent_start() {
    use std::sync::atomic::Ordering;
    // wsa's app answers 503 twice, then 200 — wsb must wait for the 200.
    let (port, hits) = spawn_up_server(vec![503, 503, 200]).await;
    let tmp = tempfile::tempdir().unwrap();
    let rig =
        workspace_rig(&tmp, None, &[("wsa", format!("APP_PORT={port}\n"))]).await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "probe".into(), members: chain_members(&rig) },
    )
    .await;
    let ws = rig.engine.snapshot().workspaces[0].id.clone();

    let (failure, lines) = start_workspace_collect(&rig.engine, ws).await;
    assert_eq!(failure, None, "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("http /up")), "{lines:#?}");
    assert!(hits.load(Ordering::SeqCst) >= 3, "probe should have retried through the 503s");
    assert_eq!(rig.runner.call_names(), vec!["wsa", "wsb", "wsc"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn absent_up_endpoint_falls_back_to_grace() {
    let (port, _hits) = spawn_up_server(vec![404]).await;
    let tmp = tempfile::tempdir().unwrap();
    let rig =
        workspace_rig(&tmp, None, &[("wsa", format!("APP_PORT={port}\n"))]).await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "graceful".into(), members: chain_members(&rig) },
    )
    .await;
    let ws = rig.engine.snapshot().workspaces[0].id.clone();

    let (failure, lines) = start_workspace_collect(&rig.engine, ws).await;
    assert_eq!(failure, None, "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("/up not served")), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("stable-running grace")), "{lines:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_take_report_remove_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(&tmp, None, &[]).await;

    // Make wsa a real git repo so refs are captured.
    let wsa_dir = tmp.path().join("wsa");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&wsa_dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
    };
    let git_available = git(&["init", "-q"]).map(|o| o.status.success()).unwrap_or(false);
    if git_available {
        git(&["add", "-A"]).unwrap();
        git(&["commit", "-q", "-m", "init"]).unwrap();
    }

    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "snappy".into(), members: chain_members(&rig) },
    )
    .await;
    let ws = rig.engine.snapshot().workspaces[0].id.clone();

    run_action(&rig.engine, Action::TakeSnapshot { workspace: ws.clone(), name: "before".into() })
        .await;
    let snapshots = rig.engine.list_snapshots(&ws).await.unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].name, "before");
    assert_eq!(snapshots[0].members.len(), 3);
    let wsa_member =
        snapshots[0].members.iter().find(|m| m.project_name == "wsa").unwrap();
    assert!(!wsa_member.file_hashes.is_empty());
    if git_available {
        assert!(wsa_member.git_commit.is_some());
        assert_eq!(wsa_member.git_dirty, Some(false));
    }

    // Untouched → clean report.
    let report = rig.engine.snapshot_report(&snapshots[0].id).await.unwrap();
    assert!(report.clean, "{report:?}");

    // Change wsa's compose file → the report names it; others stay clean.
    let compose = wsa_dir.join("compose.yaml");
    let mut content = std::fs::read_to_string(&compose).unwrap();
    content.push_str("# drifted\n");
    std::fs::write(&compose, content).unwrap();
    let report = rig.engine.snapshot_report(&snapshots[0].id).await.unwrap();
    assert!(!report.clean);
    let wsa_delta = report.deltas.iter().find(|d| d.project_name == "wsa").unwrap();
    assert!(wsa_delta.changes.iter().any(|c| c.contains("compose.yaml changed")), "{wsa_delta:?}");
    if git_available {
        assert!(wsa_delta.changes.iter().any(|c| c.contains("dirty")), "{wsa_delta:?}");
    }
    assert!(report.deltas.iter().filter(|d| d.project_name != "wsa").all(|d| d.changes.is_empty()));

    run_action(&rig.engine, Action::RemoveSnapshot { id: snapshots[0].id.clone() }).await;
    assert!(rig.engine.list_snapshots(&ws).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn network_attach_preview_and_apply_roundtrip() {
    // Uses the real compose CLI for the write transaction's validation gate.
    let cli_ok = mast_docker::run_command(
        &["docker".into(), "compose".into(), "version".into()],
        None,
        &[],
        Duration::from_secs(5),
        4096,
    )
    .await
    .map(|o| o.success())
    .unwrap_or(false);
    if !cli_ok {
        eprintln!("skipping: docker compose CLI unavailable");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(&tmp, None, &[]).await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "My Net".into(), members: chain_members(&rig) },
    )
    .await;
    let ws = rig.engine.snapshot().workspaces[0].id.clone();
    let wsa = rig.ids["wsa"].clone();

    // Preview: services listed, file untouched.
    let preview = rig.engine.network_attach_preview(&ws, &wsa).await.unwrap();
    assert!(!preview.no_op);
    assert!(preview.summary.iter().any(|s| s.contains("attach service app to mast-mynet")));
    assert!(preview.after.contains("mast-mynet"));
    let on_disk = std::fs::read_to_string(&preview.file).unwrap();
    assert_eq!(on_disk, preview.before, "preview must not write");

    // Apply through the transaction; file now matches the preview.
    run_action(&rig.engine, Action::AttachNetwork { workspace: ws.clone(), project: wsa.clone() })
        .await;
    let applied = std::fs::read_to_string(&preview.file).unwrap();
    assert_eq!(applied, preview.after);
    assert!(applied.contains("networks: [default, mast-mynet]"));
    assert!(applied.contains("external: true"));

    // Second attach is a clean no-op.
    let again = rig.engine.network_attach_preview(&ws, &wsa).await.unwrap();
    assert!(again.no_op);
    run_action(&rig.engine, Action::AttachNetwork { workspace: ws, project: wsa }).await;
    assert_eq!(std::fs::read_to_string(&preview.file).unwrap(), applied);
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_project_host_port_conflicts_are_surfaced() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(
        &tmp,
        None,
        &[
            ("wsa", "APP_PORT=8080\nFORWARD_DB_PORT=3306\n".into()),
            ("wsb", "APP_PORT=8080\n".into()),
            ("wsc", "APP_PORT=8099\n".into()),
        ],
    )
    .await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "clash".into(), members: chain_members(&rig) },
    )
    .await;
    let snap = wait_until(&rig.engine, "port conflict warning", |s| {
        s.workspaces.first().is_some_and(|w| !w.warnings.is_empty())
    })
    .await;
    let warnings = &snap.workspaces[0].warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("8080"));
    assert!(warnings[0].contains("wsa") && warnings[0].contains("wsb"), "{warnings:?}");
    assert!(!warnings[0].contains("wsc"));
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_cycle_is_a_diagnostic_and_refuses_start() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(&tmp, None, &[]).await;
    let members = vec![
        mast_contract::WorkspaceMember {
            project: rig.ids["wsa"].clone(),
            depends_on: vec![rig.ids["wsb"].clone()],
        },
        mast_contract::WorkspaceMember {
            project: rig.ids["wsb"].clone(),
            depends_on: vec![rig.ids["wsa"].clone()],
        },
    ];
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "cyclic".into(), members },
    )
    .await;
    let snap = rig.engine.snapshot();
    let ws = &snap.workspaces[0];
    assert!(ws.graph_error.as_deref().is_some_and(|e| e.contains("cycle")), "{ws:?}");
    // Errors name projects, never id hashes.
    assert!(ws.graph_error.as_deref().is_some_and(|e| e.contains("wsa") && e.contains("wsb")));

    let failure =
        run_action_capture_failure(&rig.engine, Action::StartWorkspace { id: ws.id.clone() })
            .await;
    assert!(failure.is_some_and(|e| e.contains("cycle")));
    assert!(rig.runner.call_names().is_empty(), "nothing may start");
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_project_prunes_it_from_every_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = workspace_rig(&tmp, None, &[]).await;
    run_action(
        &rig.engine,
        Action::SaveWorkspace { id: None, name: "suite".into(), members: chain_members(&rig) },
    )
    .await;
    let wsb = rig.ids["wsb"].clone();

    run_action(&rig.engine, Action::RemoveProject { id: wsb.clone() }).await;

    let snap = rig.engine.snapshot();
    let ws = &snap.workspaces[0];
    assert_eq!(
        ws.members.iter().map(|m| m.project.clone()).collect::<Vec<_>>(),
        vec![rig.ids["wsa"].clone(), rig.ids["wsc"].clone()],
        "{ws:?}"
    );
    assert!(
        ws.members.iter().all(|m| !m.depends_on.contains(&wsb)),
        "dangling dependency: {ws:?}"
    );
    assert!(ws.graph_error.is_none(), "{ws:?}");

    // The prune is persisted, not just in-memory.
    let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
    let saved = store.load_workspaces().unwrap();
    assert!(
        saved[0].members.iter().all(|m| m.project_id != wsb.0
            && !m.depends_on.contains(&wsb.0)),
        "{saved:?}"
    );

    // Losing every member leaves an empty workspace: deleting it is the
    // user's call, not a side effect.
    for name in ["wsa", "wsc"] {
        run_action(&rig.engine, Action::RemoveProject { id: rig.ids[name].clone() }).await;
    }
    let snap = rig.engine.snapshot();
    assert_eq!(snap.workspaces.len(), 1);
    assert!(snap.workspaces[0].members.is_empty(), "{:?}", snap.workspaces[0]);
}

/// A store written by an older build can already hold dangling members.
#[tokio::test(flavor = "multi_thread")]
async fn startup_heals_workspace_members_whose_project_vanished() {
    let tmp = tempfile::tempdir().unwrap();
    let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
    let kept = store.import_project(&make_project(tmp.path(), "kept")).unwrap();
    store
        .save_workspaces(&[mast_project::WorkspaceRecord {
            id: "ws-1".into(),
            name: "stale".into(),
            members: vec![
                mast_project::WorkspaceMemberRecord {
                    project_id: kept.id.clone(),
                    depends_on: vec!["9ebd976911b107ab".into()],
                },
                mast_project::WorkspaceMemberRecord {
                    project_id: "9ebd976911b107ab".into(),
                    depends_on: vec![],
                },
            ],
        }])
        .unwrap();

    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(FakeAdapter::new())));
    let snap = engine.snapshot();
    let ws = &snap.workspaces[0];
    assert_eq!(ws.members.len(), 1, "{ws:?}");
    assert_eq!(ws.members[0].project.0, kept.id);
    assert!(ws.members[0].depends_on.is_empty(), "{ws:?}");
    // Healed on disk too, so it stays fixed.
    let reopened = MetadataStore::open(tmp.path().join("meta")).unwrap();
    assert_eq!(reopened.load_workspaces().unwrap()[0].members.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_lifecycle_ops_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new(Duration::from_secs(5), 0);
    let (engine, _adapter) = imported_resolved_project(&tmp, runner).await;
    let id = project_id(&engine);

    let first = engine.dispatch(Action::StartProject { id: id.clone() });
    assert!(first.is_ok());
    let second = engine.dispatch(Action::StopProject { id: id.clone() });
    assert!(matches!(second, Err(ErrorInfo::Conflict { .. })), "got {second:?}");
    engine.cancel(first.unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_lifecycle_op_emits_cancelled_and_releases_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new(Duration::from_secs(30), 0);
    let (engine, _adapter) = imported_resolved_project(&tmp, runner).await;
    let id = project_id(&engine);

    let op = engine.dispatch(Action::StartProject { id: id.clone() }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    // Wait until the runner is actually going.
    loop {
        let event = events.next().await.unwrap();
        if matches!(event.kind, OperationEventKind::Output { .. }) {
            break;
        }
    }
    engine.cancel(op).unwrap();
    let mut terminal = None;
    while let Some(event) = events.next().await {
        if event.kind.is_terminal() {
            terminal = Some(event.kind);
            break;
        }
    }
    assert!(matches!(terminal, Some(OperationEventKind::Cancelled)));
    // Lock released after cancellation.
    assert!(engine.dispatch(Action::StopProject { id }).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_lifecycle_reports_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let runner = FakeRunner::new(Duration::from_millis(10), 17);
    let (engine, _adapter) = imported_resolved_project(&tmp, runner).await;
    let id = project_id(&engine);

    let op = engine.dispatch(Action::StartProject { id }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut failure = None;
    while let Some(event) = events.next().await {
        if let OperationEventKind::Failed { error } = &event.kind {
            failure = Some(error.clone());
            break;
        }
        if event.kind.is_terminal() {
            break;
        }
    }
    let failure = failure.expect("expected Failed event");
    assert!(failure.contains("17"), "{failure}");
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_on_unresolved_project_is_invalid_input() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "unresolved");
    let engine = test_engine(tmp.path(), Arc::new(DeadConnector));
    // No start(): the import records the project but nothing resolves it.
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let id = project_id(&engine);
    let result = engine.dispatch(Action::StartProject { id });
    assert!(matches!(result, Err(ErrorInfo::InvalidInput { .. })), "got {result:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn integrations_persist_and_launch_context_flows() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "toolapp");
    let engine = test_engine(tmp.path(), Arc::new(DeadConnector));
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;

    // Configure `true` as the terminal: exists everywhere, exits instantly.
    run_action(
        &engine,
        Action::SetIntegrations {
            integrations: mast_contract::IntegrationSettings {
                terminal: Some("true".into()),
                editor: Some("true".into()),
                auto_port_remap: true,
            },
        },
    )
    .await;
    assert_eq!(engine.snapshot().integrations.terminal.as_deref(), Some("true"));
    // Persisted: a fresh store sees it.
    let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
    assert_eq!(store.load_settings().unwrap().terminal.as_deref(), Some("true"));

    let id = project_id(&engine);
    run_action(&engine, Action::OpenTerminal { id: id.clone() }).await;
    run_action(&engine, Action::OpenInEditor { id: id.clone() }).await;

    // Shell into a service with no container → Failed with NotFound message.
    let op = engine
        .dispatch(Action::ShellIntoContainer { id, service: "app".into() })
        .unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut failure = None;
    while let Some(event) = events.next().await {
        if let OperationEventKind::Failed { error } = &event.kind {
            failure = Some(error.clone());
            break;
        }
        if event.kind.is_terminal() {
            break;
        }
    }
    assert!(failure.unwrap().contains("running container"), "expected container NotFound");
}

#[tokio::test(flavor = "multi_thread")]
async fn env_report_and_env_edits_flow() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "envapp");
    std::fs::write(
        project.join(".env"),
        "APP_PORT=8080\nDB_HOST=mariadb\nDB_PASSWORD=hunter2\nQUEUE_CONNECTION=redis\n",
    )
    .unwrap();
    std::fs::write(project.join(".env.example"), "APP_PORT=80\nDB_HOST=db\nMAIL_HOST=mailpit\n")
        .unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter)));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let id = project_id(&engine);

    let report = engine.env_report(&id).await.unwrap();
    assert!(report.env_exists && report.example_exists);
    let password = report.entries.iter().find(|e| e.key == "DB_PASSWORD").unwrap();
    assert!(password.secret);
    assert!(!password.in_example);
    assert_eq!(report.missing_from_env, vec!["MAIL_HOST".to_string()]);
    // compose.yaml declares only `app` — DB_HOST=mariadb is not a service,
    // and redis-backed queue has no redis host/service.
    assert!(report.findings.iter().any(|f| f.key.as_deref() == Some("DB_HOST")));
    assert!(report.findings.iter().any(|f| f.key.as_deref() == Some("QUEUE_CONNECTION")));

    // Edit precisely: one line changes, secrets survive verbatim.
    run_action(&engine, Action::SetEnvVar { id: id.clone(), key: "APP_PORT".into(), value: "9090".into() })
        .await;
    run_action(&engine, Action::RemoveEnvVar { id: id.clone(), key: "QUEUE_CONNECTION".into() })
        .await;
    let content = std::fs::read_to_string(project.join(".env")).unwrap();
    assert_eq!(content, "APP_PORT=9090\nDB_HOST=mariadb\nDB_PASSWORD=hunter2\n");
    // Backup captured under the metadata dir.
    let backups = tmp.path().join("meta").join("backups");
    assert!(std::fs::read_dir(&backups).unwrap().count() >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_engine_refuses_mutations_but_still_observes() {
    let tmp = tempfile::tempdir().unwrap();
    let lock_dir = tmp.path().join("shared-lock");
    // First instance owns the lock…
    let _owner = acquire_ownership(Some(lock_dir.clone()));
    // …second instance (same lock dir) comes up read-only.
    let engine = Engine::new(
        EngineConfig::default(),
        EngineDeps {
            connector: Arc::new(DeadConnector),
            store: MetadataStore::open(tmp.path().join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: acquire_ownership(Some(lock_dir)),
        },
    );
    assert!(engine.snapshot().read_only);
    let denied = engine.dispatch(Action::ImportProject { path: "/tmp".into() });
    assert!(matches!(denied, Err(ErrorInfo::ReadOnly { owner_pid: Some(_) })), "got {denied:?}");
    // Non-mutating actions still work.
    assert!(engine.dispatch(Action::RefreshNow).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_find_bootstrap_issues_and_safe_repairs_fix_them() {
    let tmp = tempfile::tempdir().unwrap();
    // A vendor-less sail-flavored clone with only .env.example: the M4
    // detection states, now with one-click M7 repairs.
    let project = make_project(tmp.path(), "clinic");
    std::fs::write(
        project.join("composer.json"),
        r#"{"require": {"php": "^8.2", "laravel/framework": "^12.0"}, "require-dev": {"laravel/sail": "^1.41"}}"#,
    )
    .unwrap();
    std::fs::write(project.join(".env.example"), "APP_NAME=clinic\nWWWUSER=\nWWWGROUP=\n")
        .unwrap();
    // A public disk with no public/storage link — the post-clone state.
    std::fs::create_dir_all(project.join("storage/app/public")).unwrap();
    std::fs::create_dir_all(project.join("public")).unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();

    let report = engine.run_diagnostics().await.unwrap();
    assert!(report.checks_run > 0);
    let env_missing = report
        .findings
        .iter()
        .find(|f| f.check == "env-missing")
        .expect("missing .env must be found");
    assert_eq!(env_missing.project_name.as_deref(), Some("clinic"));
    assert_eq!(env_missing.repair.as_ref().unwrap().id, "copy-env-example");
    let vendor = report
        .findings
        .iter()
        .find(|f| f.check == "vendor-missing")
        .expect("vendor-less clone must be found");
    assert_eq!(vendor.repair.as_ref().unwrap().id, "composer-install");

    // The composer repair previews the exact containerized argv. The official
    // composer image ships its own PHP, so no per-project image series is
    // involved — the retired laravelsail/phpXX-composer images stopped at 8.4.
    let plan = engine
        .repair_preview("composer-install", None, Some(&pid))
        .await
        .unwrap();
    assert!(plan.summary[0].contains("composer:latest"), "{:?}", plan.summary);
    assert!(!plan.summary[0].contains("laravelsail"), "{:?}", plan.summary);
    assert!(plan.summary[0].contains("install --ignore-platform-reqs"));

    // copy-env-example: preview shows the example content, apply creates .env.
    let plan = engine.repair_preview("copy-env-example", None, Some(&pid)).await.unwrap();
    assert!(!plan.no_op);
    assert!(plan.file_preview.as_ref().unwrap().after.contains("APP_NAME=clinic"));
    run_action(
        &engine,
        Action::ApplyRepair { repair: "copy-env-example".into(), arg: None, project: Some(pid.clone()) },
    )
    .await;
    assert!(project.join(".env").is_file());

    // With .env in place the WWWUSER parity check engages (empty != uid) and
    // its repair rewrites .env through the transactional writer. The copied
    // .env has no APP_KEY, and the public disk is unlinked — both surface too.
    let report = engine.run_diagnostics().await.unwrap();
    assert!(report.findings.iter().any(|f| f.check == "wwwuser-parity"));
    let app_key = report
        .findings
        .iter()
        .find(|f| f.check == "app-key-missing")
        .expect("empty APP_KEY must be found");
    assert_eq!(app_key.repair.as_ref().unwrap().id, "generate-app-key");
    let storage = report
        .findings
        .iter()
        .find(|f| f.check == "storage-link")
        .expect("unlinked public disk must be found");
    assert_eq!(storage.repair.as_ref().unwrap().id, "storage-link");
    run_action(
        &engine,
        Action::ApplyRepair { repair: "set-wwwuser".into(), arg: None, project: Some(pid.clone()) },
    )
    .await;
    run_action(
        &engine,
        Action::ApplyRepair {
            repair: "generate-app-key".into(),
            arg: None,
            project: Some(pid.clone()),
        },
    )
    .await;
    run_action(
        &engine,
        Action::ApplyRepair { repair: "storage-link".into(), arg: None, project: Some(pid.clone()) },
    )
    .await;
    let env = std::fs::read_to_string(project.join(".env")).unwrap();
    assert!(env.contains("APP_KEY=base64:"), "{env}");
    let link = std::fs::read_link(project.join("public/storage")).unwrap();
    assert_eq!(link, std::path::PathBuf::from("../storage/app/public"));
    let uid = String::from_utf8(
        std::process::Command::new("id").arg("-u").output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let env = std::fs::read_to_string(project.join(".env")).unwrap();
    assert!(env.contains(&format!("WWWUSER={uid}")), "{env}");

    // Every repaired finding is gone on the next run.
    let report = engine.run_diagnostics().await.unwrap();
    assert!(
        !report.findings.iter().any(|f| {
            matches!(
                f.check.as_str(),
                "env-missing" | "wwwuser-parity" | "app-key-missing" | "storage-link"
            )
        }),
        "{:?}",
        report.findings
    );

    // Every run and repair is in the audit history, newest first.
    let history = engine.diagnostics_history().await.unwrap();
    assert_eq!(history.runs.len(), 3);
    assert_eq!(history.repairs.len(), 4);
    assert!(history.repairs.iter().all(|r| r.outcome == "applied"));
    assert_eq!(history.repairs[0].repair, "storage-link");
    assert_eq!(history.repairs[0].risk, "safe");
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_add_and_three_way_remove() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "catapp");
    std::fs::write(project.join(".env"), "APP_NAME=catapp\n").unwrap();
    let original = std::fs::read_to_string(project.join("compose.yaml")).unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();

    let entries = engine.catalog(&pid).await.unwrap();
    assert!(entries.len() >= 5);
    assert!(entries.iter().all(|e| !e.installed));

    // Preview then apply the add; the file gains redis and .env gains
    // REDIS_HOST through the transactional writers.
    let preview = engine.catalog_preview(&pid, "redis", false).await.unwrap();
    assert!(preview.after.contains("redis:alpine"));
    assert!(preview.summary.iter().any(|s| s.contains("REDIS_HOST")));
    run_action(
        &engine,
        Action::AddCatalogService { id: pid.clone(), service: "redis".into() },
    )
    .await;
    let edited = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(edited.contains("  redis:\n    image: 'redis:alpine'"), "{edited}");
    let env = std::fs::read_to_string(project.join(".env")).unwrap();
    assert!(env.contains("REDIS_HOST=redis"), "{env}");
    assert!(
        engine.catalog(&pid).await.unwrap().iter().any(|e| e.id == "redis" && e.installed)
    );

    // Three-way removal restores the compose file byte-exactly.
    run_action(
        &engine,
        Action::RemoveCatalogService { id: pid.clone(), service: "redis".into() },
    )
    .await;
    assert_eq!(std::fs::read_to_string(project.join("compose.yaml")).unwrap(), original);

    // A customized block refuses removal instead of destroying the edit.
    run_action(
        &engine,
        Action::AddCatalogService { id: pid.clone(), service: "redis".into() },
    )
    .await;
    let customized = std::fs::read_to_string(project.join("compose.yaml"))
        .unwrap()
        .replace("image: 'redis:alpine'", "image: 'redis:7.2'");
    std::fs::write(project.join("compose.yaml"), customized).unwrap();
    let denied = engine.catalog_preview(&pid, "redis", true).await;
    assert!(matches!(denied, Err(ErrorInfo::InvalidInput { ref message }) if message.contains("customized")), "{denied:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn project_commands_persist_run_and_refuse_sail_without_vendor() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "cmdapp");
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project listed", |s| !s.projects.is_empty()).await;
    let pid = snap.projects[0].id.clone();

    let commands = vec![
        mast_contract::ProjectCommand {
            name: "touch".into(),
            command: "touch marker.txt".into(),
            auto_start: true, cwd: None,
        },
        mast_contract::ProjectCommand {
            name: "dev".into(),
            command: "sail npm run dev".into(),
            auto_start: false, cwd: None,
        },
    ];
    run_action(
        &engine,
        Action::SetProjectCommands { id: pid.clone(), commands: commands.clone() },
    )
    .await;
    // Persisted on the record and visible on the summary patch stream.
    let snap = engine.snapshot();
    assert_eq!(snap.projects[0].commands, commands);
    let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
    assert_eq!(store.load_projects().unwrap()[0].commands.len(), 2);

    // Run executes in the project dir with streamed output; no shell involved.
    run_action(&engine, Action::RunProjectCommand { id: pid.clone(), name: "touch".into() }).await;
    assert!(project.join("marker.txt").is_file());

    // A sail-prefixed command on an unbootstrapped project refuses cleanly.
    let id = engine
        .dispatch(Action::RunProjectCommand { id: pid.clone(), name: "dev".into() })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut failed = None;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Failed { error } => {
                failed = Some(error);
                break;
            }
            OperationEventKind::Completed | OperationEventKind::Cancelled => break,
            _ => {}
        }
    }
    let error = failed.expect("sail command without vendor must fail");
    assert!(error.contains("vendor/bin/sail"), "{error}");

    // Unknown names are NotFound, not silent successes.
    let denied = engine
        .dispatch(Action::RunProjectCommand { id: pid.clone(), name: "nope".into() })
        .unwrap();
    let mut events = engine.operation_events(denied).unwrap();
    let mut saw_failure = false;
    while let Some(event) = events.next().await {
        if let OperationEventKind::Failed { error } = event.kind {
            assert!(error.contains("nope"), "{error}");
            saw_failure = true;
            break;
        }
    }
    assert!(saw_failure);

    // Duplicate names are rejected up front.
    let id = engine
        .dispatch(Action::SetProjectCommands {
            id: pid.clone(),
            commands: vec![
                mast_contract::ProjectCommand {
                    name: "x".into(),
                    command: "true".into(),
                    auto_start: false, cwd: None,
                },
                mast_contract::ProjectCommand {
                    name: "x".into(),
                    command: "false".into(),
                    auto_start: false, cwd: None,
                },
            ],
        })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut saw_dup = false;
    while let Some(event) = events.next().await {
        if let OperationEventKind::Failed { error } = event.kind {
            assert!(error.contains("duplicate"), "{error}");
            saw_dup = true;
            break;
        }
    }
    assert!(saw_dup);
}

#[tokio::test(flavor = "multi_thread")]
async fn command_cwd_targets_a_sibling_directory_and_sail_refuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "backend");
    // The sibling repo the command should actually run in.
    let sibling = tmp.path().join("frontend");
    std::fs::create_dir_all(&sibling).unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project listed", |s| !s.projects.is_empty()).await;
    let pid = snap.projects[0].id.clone();

    run_action(
        &engine,
        Action::SetProjectCommands {
            id: pid.clone(),
            commands: vec![
                mast_contract::ProjectCommand {
                    name: "mark".into(),
                    command: "touch here.txt".into(),
                    auto_start: false,
                    cwd: Some("../frontend".into()),
                },
                mast_contract::ProjectCommand {
                    name: "gone".into(),
                    command: "touch nowhere.txt".into(),
                    auto_start: false,
                    cwd: Some("../does-not-exist".into()),
                },
                mast_contract::ProjectCommand {
                    name: "dev".into(),
                    command: "sail npm run dev".into(),
                    auto_start: false,
                    cwd: Some("../frontend".into()),
                },
            ],
        },
    )
    .await;

    // Relative cwd resolves against the project and the command runs THERE.
    run_action(&engine, Action::RunProjectCommand { id: pid.clone(), name: "mark".into() }).await;
    assert!(sibling.join("here.txt").is_file());
    assert!(!project.join("here.txt").exists());

    let failure = |name: &str| {
        let engine = engine.clone();
        let pid = pid.clone();
        let name = name.to_string();
        async move {
            let id = engine
                .dispatch(Action::RunProjectCommand { id: pid, name })
                .unwrap();
            let mut events = engine.operation_events(id).unwrap();
            while let Some(event) = events.next().await {
                match event.kind {
                    OperationEventKind::Failed { error } => return error,
                    OperationEventKind::Completed | OperationEventKind::Cancelled => break,
                    _ => {}
                }
            }
            panic!("expected a failure");
        }
    };
    // A missing directory names itself instead of running somewhere else.
    let error = failure("gone").await;
    assert!(error.contains("does not exist"), "{error}");
    // sail only works from the project root; the combination is refused.
    let error = failure("dev").await;
    assert!(error.contains("project root"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_project_validates_before_touching_the_network() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();

    let expect_failure = |action: Action, needle: &'static str| {
        let engine = engine.clone();
        async move {
            let id = engine.dispatch(action).unwrap();
            let mut events = engine.operation_events(id).unwrap();
            while let Some(event) = events.next().await {
                if let OperationEventKind::Failed { error } = event.kind {
                    assert!(error.contains(needle), "expected {needle:?} in {error:?}");
                    return;
                }
            }
            panic!("operation did not fail");
        }
    };

    expect_failure(
        Action::CreateProject {
            parent: tmp.path().to_string_lossy().into(),
            name: "Bad Name!".into(),
            php: "84".into(),
            services: vec![],
        },
        "lowercase",
    )
    .await;
    expect_failure(
        Action::CreateProject {
            parent: tmp.path().to_string_lossy().into(),
            name: "app".into(),
            php: "79".into(),
            services: vec![],
        },
        "unsupported PHP",
    )
    .await;
    expect_failure(
        Action::CreateProject {
            parent: tmp.path().to_string_lossy().into(),
            name: "app".into(),
            php: "86".into(),
            services: vec![],
        },
        "unsupported PHP",
    )
    .await;
    expect_failure(
        Action::CreateProject {
            parent: tmp.path().to_string_lossy().into(),
            name: "app".into(),
            php: "85".into(),
            services: vec!["cassandra".into()],
        },
        "unknown Sail service",
    )
    .await;
    // 8.5 is Sail's default runtime, and rustfs and mongodb are services it
    // installs natively — all three must clear validation and fail later, on
    // the missing parent.
    expect_failure(
        Action::CreateProject {
            parent: tmp.path().join("nope").to_string_lossy().into(),
            name: "app".into(),
            php: "85".into(),
            services: vec!["rustfs".into(), "mongodb".into()],
        },
        "is not a directory",
    )
    .await;
    std::fs::create_dir(tmp.path().join("taken")).unwrap();
    expect_failure(
        Action::CreateProject {
            parent: tmp.path().to_string_lossy().into(),
            name: "taken".into(),
            php: "84".into(),
            services: vec![],
        },
        "already exists",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn git_chips_reflect_branch_and_dirtiness() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "gitapp");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "clean git chip", |s| {
        s.projects.first().is_some_and(|p| {
            p.git_branch.as_deref() == Some("main") && p.git_dirty == Some(false)
        })
    })
    .await;
    assert_eq!(snap.projects[0].git_branch.as_deref(), Some("main"));

    // Drift the tree; the next reconcile flips the dirty flag.
    std::fs::write(project.join("scratch.txt"), "wip").unwrap();
    engine.dispatch(Action::RefreshNow).unwrap();
    wait_until(&engine, "dirty git chip", |s| {
        s.projects.first().is_some_and(|p| p.git_dirty == Some(true))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn app_processes_detected_from_composer_and_env() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "procapp");
    std::fs::write(
        project.join("composer.json"),
        r#"{"require":{"laravel/framework":"^12","laravel/reverb":"^1"}}"#,
    )
    .unwrap();
    std::fs::write(project.join(".env"), "QUEUE_CONNECTION=redis\n").unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "processes detected", |s| {
        s.projects.first().is_some_and(|p| !p.processes.is_empty())
    })
    .await;
    let ids: Vec<&str> = snap.projects[0].processes.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["reverb", "queue", "schedule"]);
    // No app container running → nothing can be running.
    assert!(snap.projects[0].processes.iter().all(|p| !p.running));
}

/// A service running a tag the offline fallback table has never heard of must
/// still see that tag in its own picker.
///
/// The regression: a Sail project on `mariadb:11.4` was offered `12, 11,
/// 10.11`, so the dropdown's value matched no option and rendered blank. The
/// list now comes from the registry, but this has to hold without a network
/// too — which is exactly what this test runs under.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_tag_is_offered_even_when_the_fallback_table_lacks_it() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "ltsapp");
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n  mariadb:\n    image: 'mariadb:11.4'\n",
    )
    .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();

    let entry = engine
        .catalog(&pid)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == "mariadb")
        .expect("mariadb entry");
    assert_eq!(entry.installed_image.as_deref(), Some("mariadb:11.4"));
    assert!(
        entry.versions.contains(&"11.4".to_string()),
        "the running tag must be offered back: {:?}",
        entry.versions
    );
    // And it sorts where it belongs rather than being tacked on the end.
    let newest = entry.versions.first().expect("a non-empty list");
    assert!(newest.starts_with("12"), "newest first: {:?}", entry.versions);
}

/// A repo we cannot query and do not pin gets no dropdown at all — an
/// invented list would offer tags that cannot be pulled.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_repo_offers_no_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "odd");
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n  mailpit:\n    image: 'axllent/mailpit:latest'\n",
    )
    .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();

    let entry = engine
        .catalog(&pid)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == "mailpit")
        .expect("mailpit entry");
    assert!(entry.installed);
    assert!(entry.versions.is_empty(), "{:?}", entry.versions);
}

/// Changing a service's version is a retag of one scalar, previewed and
/// applied through the same write transaction as every other compose edit.
#[tokio::test(flavor = "multi_thread")]
async fn service_image_retag_previews_and_applies() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "verapp");
    std::fs::write(project.join(".env"), "APP_NAME=verapp\n").unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();
    run_action(&engine, Action::AddCatalogService { id: pid.clone(), service: "redis".into() })
        .await;

    // The catalog reports what redis actually runs, plus the tags on offer for
    // that repo — never a list borrowed from a different repo.
    let entry = engine
        .catalog(&pid)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == "redis")
        .expect("redis entry");
    assert_eq!(entry.installed_image.as_deref(), Some("redis:alpine"));
    assert!(entry.versions.contains(&"8".to_string()), "{:?}", entry.versions);
    // The tag it was installed with has to be offered back, or the picker
    // opens on a value missing from its own list.
    assert!(entry.versions.contains(&"alpine".to_string()), "{:?}", entry.versions);

    let preview = engine.service_image_preview(&pid, "redis", "redis:8").await.unwrap();
    assert!(!preview.no_op);
    assert!(preview.after.contains("image: 'redis:8'"), "{}", preview.after);
    assert!(preview.summary.iter().any(|s| s.contains("rebuild")), "{:?}", preview.summary);

    // Re-selecting the tag it already runs is a no-op, not an empty write.
    let same = engine.service_image_preview(&pid, "redis", "redis:alpine").await.unwrap();
    assert!(same.no_op, "{:?}", same.summary);

    run_action(
        &engine,
        Action::SetServiceImage {
            id: pid.clone(),
            service: "redis".into(),
            image: "redis:8".into(),
        },
    )
    .await;
    let edited = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(edited.contains("image: 'redis:8'"), "{edited}");
    assert!(!edited.contains("redis:alpine"), "{edited}");

    // A service with no image: (the built app) cannot be retagged.
    let denied = engine.service_image_preview(&pid, "nope", "redis:8").await;
    assert!(denied.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_a_project_moves_a_host_port_that_is_already_taken() {
    // Only ports the resolved model publishes are moved, and resolving the
    // model shells out to `docker compose config` (no daemon needed).
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Something else on this machine is already listening — the exact
    // situation `up` would fail on with "bind: address already in use".
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let busy = squatter.local_addr().unwrap().port();

    let project = make_project(tmp.path(), "portapp");
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n    ports:\n      - '${APP_PORT:-80}:80'\n",
    )
    .unwrap();
    // APP_URL pins the same port explicitly (the bootstrap-template shape) —
    // it must move together with APP_PORT or the Browser button goes stale.
    std::fs::write(
        project.join(".env"),
        format!("APP_PORT={busy}\nAPP_URL=http://localhost:{busy}\nAPP_KEY=base64:x\n"),
    )
    .unwrap();

    let adapter = FakeAdapter::new();
    let runner = FakeRunner::new(Duration::from_millis(20), 0);
    let engine = test_engine_with(tmp.path(), Arc::new(FakeConnector(adapter)), runner);
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    // The reconcile has read .env and resolved the model once the app URL
    // reflects APP_PORT and the declared service is listed.
    wait_until(&engine, "env read and model resolved", |s| {
        s.projects.first().is_some_and(|p| {
            p.app_url.as_deref() == Some(&format!("http://localhost:{busy}"))
                && p.services.iter().any(|svc| svc.name == "app")
        })
    })
    .await;

    let id = project_id(&engine);
    let op = engine.dispatch(Action::StartProject { id }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut moved_line = None;
    while let Some(event) = events.next().await {
        if let OperationEventKind::Output { line, .. } = &event.kind
            && line.contains("already in use")
        {
            moved_line = Some(line.clone());
        }
        if event.kind.is_terminal() {
            break;
        }
    }

    let env = std::fs::read_to_string(project.join(".env")).unwrap();
    assert!(!env.contains(&format!("APP_PORT={busy}")), "the busy port survived: {env}");
    assert!(env.contains(&format!("APP_PORT={}", busy + 1)), "{env}");
    // The pinned APP_URL followed the move.
    assert!(env.contains(&format!("APP_URL=http://localhost:{}", busy + 1)), "{env}");
    // Untouched keys keep their bytes.
    assert!(env.contains("APP_KEY=base64:x"), "{env}");
    let moved = moved_line.expect("the move is reported in the operation output");
    assert!(moved.contains("APP_PORT") && moved.contains(&(busy + 1).to_string()), "{moved}");

    // With the setting off, the same start leaves .env alone.
    drop(squatter);
    let squatter2 = std::net::TcpListener::bind(("127.0.0.1", busy + 1)).unwrap();
    run_action(
        &engine,
        Action::SetIntegrations {
            integrations: mast_contract::IntegrationSettings {
                terminal: None,
                editor: None,
                auto_port_remap: false,
            },
        },
    )
    .await;
    let before = std::fs::read_to_string(project.join(".env")).unwrap();
    run_action(&engine, Action::StartProject { id: project_id(&engine) }).await;
    assert_eq!(std::fs::read_to_string(project.join(".env")).unwrap(), before);
    drop(squatter2);
}

/// A failing operation whose output carries a known error signature ends
/// with a plain-language explanation before the Failed event — the GPG/PPA
/// build-failure wave, port squatters, and version-locked volumes read as
/// sentences instead of scrollback.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn failing_operations_explain_known_error_signatures() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "sigapp");
    std::fs::write(
        project.join("fail-like-a-gpg-outage.sh"),
        "#!/bin/sh\necho 'gpg: keyserver receive failed: Server indicated a failure' >&2\nexit 1\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            project.join("fail-like-a-gpg-outage.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project listed", |s| !s.projects.is_empty()).await;
    let pid = snap.projects[0].id.clone();
    run_action(
        &engine,
        Action::SetProjectCommands {
            id: pid.clone(),
            commands: vec![mast_contract::ProjectCommand {
                name: "build".into(),
                command: "./fail-like-a-gpg-outage.sh".into(),
                auto_start: false, cwd: None,
            }],
        },
    )
    .await;

    let id = engine
        .dispatch(Action::RunProjectCommand { id: pid.clone(), name: "build".into() })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut lines: Vec<String> = Vec::new();
    let mut failed = false;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => lines.push(line),
            OperationEventKind::Failed { .. } => {
                failed = true;
                break;
            }
            OperationEventKind::Completed | OperationEventKind::Cancelled => break,
            _ => {}
        }
    }
    assert!(failed, "the command must fail");
    let cause = lines
        .iter()
        .position(|l| l.starts_with("likely cause:") && l.contains("GPG keyserver"))
        .unwrap_or_else(|| panic!("no explanation in {lines:?}"));
    assert!(
        lines[cause + 1].starts_with("  fix:"),
        "advice must follow the cause: {lines:?}"
    );

    // A signature that maps to a repair also emits a FixAvailable event —
    // the failure carries its own Fix button, offer and project attached.
    std::fs::write(
        project.join("fail-like-a-port-clash.sh"),
        "#!/bin/sh\necho 'Error starting userland proxy: bind: address already in use' >&2\nexit 1\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            project.join("fail-like-a-port-clash.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    run_action(
        &engine,
        Action::SetProjectCommands {
            id: pid.clone(),
            commands: vec![mast_contract::ProjectCommand {
                name: "clash".into(),
                command: "./fail-like-a-port-clash.sh".into(),
                auto_start: false, cwd: None,
            }],
        },
    )
    .await;
    let id = engine
        .dispatch(Action::RunProjectCommand { id: pid.clone(), name: "clash".into() })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut fix = None;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::FixAvailable { repair, project } => fix = Some((repair, project)),
            OperationEventKind::Failed { .. } => break,
            OperationEventKind::Completed | OperationEventKind::Cancelled => break,
            _ => {}
        }
    }
    let (repair, fix_project) = fix.expect("port clash must carry a fix offer");
    assert_eq!(repair.id, "reassign-ports");
    assert_eq!(fix_project, pid);
}

/// The PHP switch: refuses series that are not vendored, rewrites build
/// context and image tag TOGETHER through the write transaction, and only
/// then rebuilds — so a failed build leaves a coherent file that `up` or a
/// rebuild can still make good on, never a context/tag mismatch.
#[tokio::test(flavor = "multi_thread")]
async fn php_switch_rewrites_context_and_tag_together_before_building() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("phpapp");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  laravel.test:\n    build:\n      context: './vendor/laravel/sail/runtimes/8.3'\n      dockerfile: Dockerfile\n    image: 'sail-8.3/app'\n",
    )
    .unwrap();
    // Two vendored runtimes, deliberately WITHOUT Dockerfiles: the build step
    // must fail, proving the compose edit lands first and stays coherent.
    for series in ["8.3", "8.4"] {
        std::fs::create_dir_all(project.join("vendor/laravel/sail/runtimes").join(series))
            .unwrap();
    }

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    // The picker's data arrives on the summary via reconcile.
    let snap = wait_until(&engine, "php info on summary", |s| {
        s.projects
            .first()
            .is_some_and(|p| p.php.is_some() && p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();
    let php = snap.projects[0].php.as_ref().unwrap();
    assert_eq!(php.service, "laravel.test");
    assert_eq!(php.current, "8.3");
    assert_eq!(php.available, vec!["8.3", "8.4"]);

    // A series that is not vendored is refused before anything is touched.
    let id = engine
        .dispatch(Action::SetPhpVersion {
            id: pid.clone(),
            service: "laravel.test".into(),
            series: "9.9".into(),
        })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut error = None;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Failed { error: e } => {
                error = Some(e);
                break;
            }
            OperationEventKind::Completed | OperationEventKind::Cancelled => break,
            _ => {}
        }
    }
    let error = error.expect("unvendored series must fail");
    assert!(error.contains("not vendored"), "{error}");
    let untouched = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(untouched.contains("runtimes/8.3"), "{untouched}");

    // A vendored switch edits BOTH fields, then fails at the build (no
    // Dockerfile here) — the file must already be coherent on 8.4.
    let id = engine
        .dispatch(Action::SetPhpVersion {
            id: pid.clone(),
            service: "laravel.test".into(),
            series: "8.4".into(),
        })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut failed = false;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Failed { .. } => {
                failed = true;
                break;
            }
            OperationEventKind::Completed | OperationEventKind::Cancelled => break,
            _ => {}
        }
    }
    assert!(failed, "the Dockerfile-less build must fail");
    let switched = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(switched.contains("context: './vendor/laravel/sail/runtimes/8.4'"), "{switched}");
    assert!(switched.contains("image: 'sail-8.4/app'"), "{switched}");
    assert!(!switched.contains("8.3"), "{switched}");
}

/// The Node switch pins `build.args.NODE_VERSION` (existing args intact)
/// BEFORE the rebuild, refuses garbage majors without touching the file,
/// and the summary's effective Node follows the override.
#[tokio::test(flavor = "multi_thread")]
async fn node_switch_pins_the_build_arg_before_building() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("nodeapp");
    std::fs::create_dir_all(project.join("vendor/laravel/sail/runtimes/8.3")).unwrap();
    // No Dockerfile on purpose: the build step must fail, proving the compose
    // edit lands first — and no real `sail-8.3/app` image ever gets clobbered.
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  laravel.test:\n    build:\n      context: './vendor/laravel/sail/runtimes/8.3'\n      dockerfile: Dockerfile\n      args:\n        WWWGROUP: '${WWWGROUP}'\n    image: 'sail-8.3/app'\n",
    )
    .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "php info on summary", |s| {
        s.projects
            .first()
            .is_some_and(|p| p.php.is_some() && p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();
    let php = snap.projects[0].php.as_ref().unwrap();
    assert_eq!(php.node, None, "no Dockerfile ARG and no override yet");
    assert!(php.node_available.contains(&"20".to_string()), "{:?}", php.node_available);

    // Garbage majors are refused at dispatch — before the op lock, before
    // any operation exists, before anything is touched.
    let denied = engine.dispatch(Action::SetNodeVersion {
        id: pid.clone(),
        service: "laravel.test".into(),
        major: "v20".into(),
    });
    match denied {
        Err(mast_contract::ErrorInfo::InvalidInput { message }) => {
            assert!(message.contains("not a Node major"), "{message}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert!(
        !std::fs::read_to_string(project.join("compose.yaml")).unwrap().contains("NODE_VERSION"),
        "refusal must leave the file alone"
    );

    // The real switch: the arg lands (existing args intact), the Dockerfile-
    // less build then fails — a coherent file a plain rebuild makes good on.
    let id = engine
        .dispatch(Action::SetNodeVersion {
            id: pid.clone(),
            service: "laravel.test".into(),
            major: "20".into(),
        })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    while let Some(event) = events.next().await {
        if event.kind.is_terminal() {
            break;
        }
    }
    let edited = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(edited.contains("NODE_VERSION: '20'"), "{edited}");
    assert!(edited.contains("WWWGROUP: '${WWWGROUP}'"), "{edited}");

    // The effective Node on the summary follows the override.
    wait_until(&engine, "node override on summary", |s| {
        s.projects
            .first()
            .is_some_and(|p| p.php.as_ref().is_some_and(|php| php.node.as_deref() == Some("20")))
    })
    .await;
}

/// The stale `public/hot` trap: a killed Vite dev server leaves the file
/// behind, Blade keeps rendering dev-server URLs, and `npm run build`
/// changes nothing. Diagnostics finds it, the repair deletes it — and
/// refuses while a live dev server owns the file.
#[tokio::test(flavor = "multi_thread")]
async fn stale_vite_hot_file_is_found_and_removed_but_a_live_one_is_respected() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "hotapp");
    std::fs::create_dir_all(project.join("public")).unwrap();
    // A port that was just free: bind, read it, drop the listener.
    let stale_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    std::fs::write(project.join("public/hot"), format!("http://127.0.0.1:{stale_port}"))
        .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project listed", |s| !s.projects.is_empty()).await;
    let pid = snap.projects[0].id.clone();

    let report = engine.run_diagnostics().await.unwrap();
    let finding = report
        .findings
        .iter()
        .find(|f| f.check == "vite-hot-stale")
        .expect("stale hot file must be found");
    assert!(finding.detail.contains("changes nothing"), "{}", finding.detail);
    assert_eq!(finding.repair.as_ref().unwrap().id, "remove-hot-file");

    run_action(
        &engine,
        Action::ApplyRepair { repair: "remove-hot-file".into(), arg: None, project: Some(pid.clone()) },
    )
    .await;
    assert!(!project.join("public/hot").exists());
    let report = engine.run_diagnostics().await.unwrap();
    assert!(!report.findings.iter().any(|f| f.check == "vite-hot-stale"));

    // A hot file whose dev server IS listening is not stale: no finding, and
    // the repair refuses rather than desyncing a healthy dev setup.
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let live_port = live.local_addr().unwrap().port();
    std::fs::write(project.join("public/hot"), format!("http://127.0.0.1:{live_port}")).unwrap();
    let report = engine.run_diagnostics().await.unwrap();
    assert!(!report.findings.iter().any(|f| f.check == "vite-hot-stale"));
    let id = engine
        .dispatch(Action::ApplyRepair {
            repair: "remove-hot-file".into(),
            arg: None,
            project: Some(pid.clone()),
        })
        .unwrap();
    let mut events = engine.operation_events(id).unwrap();
    let mut failed = None;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Failed { error } => {
                failed = Some(error);
                break;
            }
            OperationEventKind::Completed | OperationEventKind::Cancelled => break,
            _ => {}
        }
    }
    let error = failed.expect("removing a live hot file must refuse");
    assert!(error.contains("not stale"), "{error}");
    assert!(project.join("public/hot").exists(), "the live file must survive");
    drop(live);
}

/// The Xdebug doctor: an old published compose file that never passes
/// XDEBUG_MODE is the fatal first rung; once the mode flows, a missing
/// host-gateway mapping on Linux is an Error with a one-click compose
/// repair through the write transaction.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn xdebug_doctor_finds_missing_wiring_and_repairs_host_gateway() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("xdbg");
    std::fs::create_dir_all(&project).unwrap();
    // Sail-flavored via composer.json; compose file predates Xdebug wiring.
    std::fs::write(
        project.join("composer.json"),
        r#"{"require-dev": {"laravel/sail": "^1.41"}}"#,
    )
    .unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  laravel.test:\n    image: 'alpine:latest'\n",
    )
    .unwrap();
    std::fs::write(project.join(".env"), "APP_SERVICE=laravel.test\nSAIL_XDEBUG_MODE=develop,debug\n")
        .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();

    let report = engine.run_diagnostics().await.unwrap();
    let f = report
        .findings
        .iter()
        .find(|f| f.check == "xdebug")
        .expect("unwired XDEBUG_MODE must be found");
    assert!(f.title.contains("never reaches the container"), "{}", f.title);

    // The mode now flows, but the file still lacks the Linux host mapping.
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  laravel.test:\n    image: 'alpine:latest'\n    environment:\n      XDEBUG_MODE: '${SAIL_XDEBUG_MODE:-off}'\n",
    )
    .unwrap();
    let report = engine.run_diagnostics().await.unwrap();
    let f = report
        .findings
        .iter()
        .find(|f| f.check == "xdebug" && f.title.contains("client_host"))
        .expect("missing host-gateway must be found");
    let repair = f.repair.as_ref().unwrap();
    assert_eq!(repair.id, "add-host-gateway");
    assert_eq!(repair.arg.as_deref(), Some("laravel.test"));

    // Preview shows the exact insertion; apply lands it via the transaction.
    let plan = engine
        .repair_preview("add-host-gateway", Some("laravel.test"), Some(&pid))
        .await
        .unwrap();
    let preview = plan.file_preview.expect("compose repair must show the diff");
    assert!(preview.after.contains("host.docker.internal:host-gateway"), "{}", preview.after);
    run_action(
        &engine,
        Action::ApplyRepair {
            repair: "add-host-gateway".into(),
            arg: Some("laravel.test".into()),
            project: Some(pid.clone()),
        },
    )
    .await;
    let edited = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(edited.contains("extra_hosts:"), "{edited}");
    assert!(edited.contains("host.docker.internal:host-gateway"), "{edited}");
    let report = engine.run_diagnostics().await.unwrap();
    assert!(
        !report.findings.iter().any(|f| f.check == "xdebug" && f.title.contains("client_host")),
        "{:?}",
        report.findings
    );
}

/// Stub drift: a pre-2023 compose file still running MailHog migrates to
/// Mailpit in one repair — compose transaction plus the .env updates.
#[tokio::test(flavor = "multi_thread")]
async fn mailhog_migrates_to_mailpit_in_one_repair() {
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("mailapp");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  laravel.test:\n    image: 'alpine:latest'\n  mailhog:\n    image: 'mailhog/mailhog:latest'\n    ports:\n      - '1025:1025'\n",
    )
    .unwrap();
    std::fs::write(project.join(".env"), "MAIL_MAILER=smtp\nMAIL_HOST=mailhog\nMAIL_PORT=1025\n")
        .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();

    // The scan reads the resolved model; poll until the finding lands.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let repair = loop {
        let report = engine.run_diagnostics().await.unwrap();
        if let Some(f) = report.findings.iter().find(|f| f.check == "stub-drift") {
            break f.repair.clone().expect("mailhog drift must offer the migration");
        }
        assert!(std::time::Instant::now() < deadline, "stub-drift never surfaced");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(repair.id, "migrate-mailpit");
    assert_eq!(repair.arg.as_deref(), Some("mailhog"));

    let plan = engine.repair_preview("migrate-mailpit", Some("mailhog"), Some(&pid)).await.unwrap();
    let preview = plan.file_preview.expect("migration must show the file diff");
    assert!(preview.after.contains("mailpit"), "{}", preview.after);
    assert!(!preview.after.contains("mailhog"), "{}", preview.after);

    run_action(
        &engine,
        Action::ApplyRepair {
            repair: "migrate-mailpit".into(),
            arg: Some("mailhog".into()),
            project: Some(pid.clone()),
        },
    )
    .await;
    let compose = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(compose.contains("axllent/mailpit"), "{compose}");
    assert!(!compose.contains("mailhog"), "{compose}");
    let env = std::fs::read_to_string(project.join(".env")).unwrap();
    assert!(env.contains("MAIL_HOST=mailpit"), "{env}");
}

/// `OpenUrl` powers the share dialog's Open/Dashboard buttons — and must
/// refuse anything that is not plain http(s), since the URL travels through
/// the generic action pipe.
#[tokio::test(flavor = "multi_thread")]
async fn open_url_refuses_non_http_schemes() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    for url in ["javascript:alert(1)", "file:///etc/passwd", "ftp://x", "http://"] {
        let id = engine.dispatch(Action::OpenUrl { url: url.into() }).unwrap();
        let mut events = engine.operation_events(id).unwrap();
        let mut failed = false;
        while let Some(event) = events.next().await {
            match event.kind {
                OperationEventKind::Failed { .. } => {
                    failed = true;
                    break;
                }
                OperationEventKind::Completed | OperationEventKind::Cancelled => break,
                _ => {}
            }
        }
        assert!(failed, "{url} must be refused");
    }
}

/// Sharing tunnels the RUNNING app; a stopped project is refused up front
/// with the reason, not a dead tunnel.
#[tokio::test(flavor = "multi_thread")]
async fn share_refuses_a_stopped_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "shareapp");
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project listed", |s| !s.projects.is_empty()).await;
    let denied = engine.dispatch(Action::ShareProject { id: snap.projects[0].id.clone() });
    match denied {
        Err(mast_contract::ErrorInfo::InvalidInput { message }) => {
            assert!(message.contains("start the project"), "{message}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

/// `docker compose config` runs without a daemon, but the CLI still has to be
/// installed; tests that need a resolved model skip without it.
async fn compose_cli_available() -> bool {
    mast_docker::run_command(
        &["docker".into(), "compose".into(), "version".into()],
        None,
        &[],
        Duration::from_secs(5),
        4096,
    )
    .await
    .map(|o| o.success())
    .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_flag_a_running_container_detached_from_its_network() {
    // The wreckage a half-failed start leaves behind: the container reports
    // "running" but sits on no network and publishes none of its ports —
    // and since its config-hash still matches, only a force-recreate helps.
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "adrift");
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n    ports:\n      - '18099:80'\n",
    )
    .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = snap.projects[0].id.clone();
    let compose_name = snap.projects[0].compose_project_name.clone().unwrap();
    let canonical = project.canonicalize().unwrap();

    let mut adrift = observation(&compose_name, &canonical, "app", "running");
    adrift.networks = Vec::new();
    adapter.set_containers(vec![adrift]);
    wait_until(&engine, "project running", |s| s.projects[0].status == ProjectStatus::Running)
        .await;

    let report = engine.run_diagnostics().await.unwrap();
    let f = report
        .findings
        .iter()
        .find(|f| f.check == "detached-containers")
        .expect("the detached container must be found");
    let repair = f.repair.as_ref().unwrap();
    assert_eq!(repair.id, "recreate-service");
    assert_eq!(repair.arg.as_deref(), Some("app"));

    // The preview names the exact compose command, scoped to the service.
    let plan = engine.repair_preview("recreate-service", Some("app"), Some(&pid)).await.unwrap();
    assert!(
        plan.summary[0].contains("up -d --force-recreate --no-deps app"),
        "{:?}",
        plan.summary
    );

    // A service the model does not declare is refused — the arg travels
    // through the UI before it lands in an argv.
    assert!(engine.repair_preview("recreate-service", Some("nope"), Some(&pid)).await.is_err());

    // The same container attached to its network raises nothing, even
    // though the summary's published ports happen to be empty.
    adapter.set_containers(vec![observation(&compose_name, &canonical, "app", "running")]);
    wait_until(&engine, "project still running", |s| {
        s.projects[0].status == ProjectStatus::Running
    })
    .await;
    let report = engine.run_diagnostics().await.unwrap();
    assert!(
        report.findings.iter().all(|f| f.check != "detached-containers"),
        "attached container flagged: {:?}",
        report.findings.iter().map(|f| &f.check).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_app_url_is_found_and_repaired() {
    // The bootstrap-template trap: APP_URL pins :8000 while a port remap
    // moved APP_PORT to 8082 — the Browser button opens a dead address.
    if !compose_cli_available().await {
        eprintln!("skipping: docker compose CLI not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "staleurl");
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n    ports:\n      - '${APP_PORT:-80}:80'\n",
    )
    .unwrap();
    std::fs::write(project.join(".env"), "APP_PORT=8082\nAPP_URL=http://localhost:8000\n")
        .unwrap();

    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter)));
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "env read and model resolved", |s| {
        s.projects.first().is_some_and(|p| {
            p.compose_project_name.is_some()
                && p.app_url.as_deref() == Some("http://localhost:8000")
        })
    })
    .await;
    let pid = project_id(&engine);

    let report = engine.run_diagnostics().await.unwrap();
    let f = report
        .findings
        .iter()
        .find(|f| f.check == "stale-app-url")
        .expect("the stale APP_URL must be found");
    assert_eq!(f.repair.as_ref().unwrap().id, "fix-app-url");

    // A scoped run carries only this project's findings and stays out of
    // recorded history; an unknown project is refused.
    let scoped = engine.run_diagnostics_scoped(Some(&pid)).await.unwrap();
    assert!(scoped.findings.iter().any(|f| f.check == "stale-app-url"));
    assert!(scoped.findings.iter().all(|f| f.project.as_ref() == Some(&pid)), "{scoped:?}");
    assert!(
        engine
            .run_diagnostics_scoped(Some(&mast_contract::ProjectId("nope".into())))
            .await
            .is_err()
    );

    // Preview shows the exact .env edit; applying makes it and nothing else.
    let plan = engine.repair_preview("fix-app-url", None, Some(&pid)).await.unwrap();
    assert!(!plan.no_op);
    let preview = plan.file_preview.as_ref().unwrap();
    assert!(preview.after.contains("APP_URL=http://localhost:8082"), "{}", preview.after);
    run_action(
        &engine,
        Action::ApplyRepair { repair: "fix-app-url".into(), arg: None, project: Some(pid.clone()) },
    )
    .await;
    let env = std::fs::read_to_string(project.join(".env")).unwrap();
    assert!(env.contains("APP_URL=http://localhost:8082"), "{env}");
    assert!(env.contains("APP_PORT=8082"), "{env}");

    // Healed: the report no longer carries the finding, and a second apply
    // is a clean no-op.
    let report = engine.run_diagnostics().await.unwrap();
    assert!(report.findings.iter().all(|f| f.check != "stale-app-url"));
    let plan = engine.repair_preview("fix-app-url", None, Some(&pid)).await.unwrap();
    assert!(plan.no_op);
}
