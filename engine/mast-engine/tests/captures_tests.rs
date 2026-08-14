//! Log captures (M10): a container's last words, kept after the container is
//! gone.
//!
//! The load-bearing claims are that a death Mast did not cause is still
//! captured (the whole point — nobody was watching), that a deliberate
//! teardown captures exactly once rather than twice, that secrets never reach
//! a capture (unlike a live stream, a capture is written to disk and copyable),
//! and that captures outlive the process that took them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::BoxStream;
use mast_contract::{
    Action, CaptureReason, ContainerState, DockerStatus, OperationEventKind, ProjectId,
};
use mast_docker::{
    CapturedLine, CommandOutcome, ContainerObservation, DockerError, LogChunk, OutputLine,
    RuntimeAdapter, RuntimeEvent,
};
use mast_engine::{
    Engine, EngineConfig, EngineDeps, LifecycleRunner, LifecycleVerb, RuntimeConnector,
    acquire_ownership,
};
use mast_project::MetadataStore;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------- fakes ----------

struct FakeAdapter {
    containers: Mutex<Vec<ContainerObservation>>,
    tail: Mutex<Vec<CapturedLine>>,
    /// Every `container_log_tail` call, so a test can prove a capture was — or
    /// was not — attempted.
    tail_calls: Mutex<Vec<String>>,
    events_tx: broadcast::Sender<RuntimeEvent>,
}

impl FakeAdapter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            containers: Mutex::new(Vec::new()),
            tail: Mutex::new(vec![CapturedLine {
                at: Some("2026-08-12T14:22:03.000000000Z".into()),
                message: "worker exiting".into(),
                stderr: true,
            }]),
            tail_calls: Mutex::new(Vec::new()),
            events_tx: broadcast::channel(64).0,
        })
    }

    fn set_containers(&self, containers: Vec<ContainerObservation>) {
        *self.containers.lock().unwrap() = containers;
        let _ = self.events_tx.send(RuntimeEvent);
    }

    fn set_tail(&self, lines: Vec<CapturedLine>) {
        *self.tail.lock().unwrap() = lines;
    }

    fn tail_calls(&self) -> usize {
        self.tail_calls.lock().unwrap().len()
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
        Ok(futures::stream::empty().boxed())
    }
    async fn container_log_tail(
        &self,
        container_id: &str,
        _since_unix: i64,
        _max_lines: u32,
    ) -> Result<Vec<CapturedLine>, DockerError> {
        self.tail_calls.lock().unwrap().push(container_id.to_string());
        Ok(self.tail.lock().unwrap().clone())
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

/// Exits 0 without doing anything — the teardown tests care about when the
/// capture happens relative to the command, not about the command.
struct FakeRunner;

#[async_trait::async_trait]
impl LifecycleRunner for FakeRunner {
    async fn run(
        &self,
        _invocation: &mast_compose::ComposeInvocation,
        _verb: LifecycleVerb,
        _service: Option<&str>,
        _lines: tokio::sync::mpsc::Sender<OutputLine>,
        _cancel: CancellationToken,
    ) -> Result<CommandOutcome, String> {
        Ok(CommandOutcome::Exited(0))
    }
}

// ---------- harness ----------

fn test_engine(meta_dir: &Path, connector: Arc<dyn RuntimeConnector>) -> Engine {
    Engine::new(
        EngineConfig {
            hint_debounce: Duration::from_millis(30),
            // Short so a missed docker-event hint costs a tick, not a test.
            reconcile_interval: Duration::from_millis(200),
            registry_refresh: false,
            ..Default::default()
        },
        EngineDeps {
            connector,
            store: MetadataStore::open(meta_dir.join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner: Arc::new(FakeRunner),
            ownership: acquire_ownership(Some(meta_dir.join("lock"))),
        },
    )
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

fn observation(
    project_name: &str,
    project_dir: &Path,
    service: &str,
    state: &str,
) -> ContainerObservation {
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
    }
}

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

/// Import a project and wait until it resolves and its container is observed
/// as running — the state every capture test starts from.
async fn running_project(engine: &Engine, adapter: &FakeAdapter, dir: &Path) -> ProjectId {
    let project = make_project(dir, "captureapp");
    engine.start();
    run_action(engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;

    let compose_name = wait_for(engine, "project resolved", |e| {
        e.snapshot().projects.first().and_then(|p| p.compose_project_name.clone())
    })
    .await;
    adapter.set_containers(vec![observation(&compose_name, &project, "app", "running")]);
    wait_for(engine, "container observed running", |e| {
        e.snapshot()
            .projects
            .first()
            .filter(|p| {
                p.services.iter().any(|s| s.state == Some(ContainerState::Running))
            })
            .map(|p| p.id.clone())
    })
    .await
}

async fn wait_for<T>(engine: &Engine, what: &str, f: impl Fn(&Engine) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = f(engine) {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_captures(engine: &Engine, what: &str, count: usize) -> Vec<mast_contract::LogCapture> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let captures = engine.log_captures(50).await.unwrap();
        if captures.len() >= count {
            return captures;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: have {} of {count}",
            captures.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ---------- tests ----------

/// The headline case: nobody asked Mast to stop this container and nobody was
/// tailing it. Without a capture, the output that explains the exit is gone
/// the moment compose recreates it.
#[tokio::test(flavor = "multi_thread")]
async fn a_container_that_dies_on_its_own_is_captured() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project_dir = tmp.path().join("captureapp");
    let _ = running_project(&engine, &adapter, tmp.path()).await;

    let compose_name =
        engine.snapshot().projects[0].compose_project_name.clone().unwrap();
    let mut dead = observation(&compose_name, &project_dir, "app", "exited");
    dead.exit_code = Some(137);
    adapter.set_containers(vec![dead]);

    let captures = wait_for_captures(&engine, "the crash capture", 1).await;
    assert_eq!(captures[0].service, "app");
    assert_eq!(captures[0].reason, CaptureReason::Exited { status: Some(137) });
    assert_eq!(captures[0].lines[0].message, "worker exiting");
    assert!(captures[0].lines[0].stderr);
    assert_eq!(captures[0].window_secs, 60);
}

/// Health is a transition, not a condition: a container that stays unhealthy
/// across many reconciles must not refill the tab with the same lines.
#[tokio::test(flavor = "multi_thread")]
async fn unhealthy_captures_once_not_every_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project_dir = tmp.path().join("captureapp");
    let _ = running_project(&engine, &adapter, tmp.path()).await;

    let compose_name =
        engine.snapshot().projects[0].compose_project_name.clone().unwrap();
    let unhealthy = || {
        let mut o = observation(&compose_name, &project_dir, "app", "running");
        o.health = Some("unhealthy".into());
        o
    };
    adapter.set_containers(vec![unhealthy()]);
    let captures = wait_for_captures(&engine, "the unhealthy capture", 1).await;
    assert_eq!(captures[0].reason, CaptureReason::Unhealthy);

    // Several more passes over the same still-unhealthy container.
    for _ in 0..4 {
        adapter.set_containers(vec![unhealthy()]);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert_eq!(
        engine.log_captures(50).await.unwrap().len(),
        1,
        "an ongoing unhealthy condition captured more than once"
    );
}

/// A stop captures before the command runs — that is the only moment the
/// container is guaranteed to still exist. The reconcile that then observes
/// the stop must not record the same lines a second time.
#[tokio::test(flavor = "multi_thread")]
async fn a_deliberate_stop_captures_exactly_once() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project_dir = tmp.path().join("captureapp");
    let id = running_project(&engine, &adapter, tmp.path()).await;

    run_action(&engine, Action::StopProject { id: id.clone() }).await;
    let captures = wait_for_captures(&engine, "the teardown capture", 1).await;
    assert_eq!(captures[0].reason, CaptureReason::Teardown { verb: "stop".into() });

    // Now let observation catch up with what the stop did.
    let compose_name =
        engine.snapshot().projects[0].compose_project_name.clone().unwrap();
    let mut dead = observation(&compose_name, &project_dir, "app", "exited");
    dead.exit_code = Some(0);
    adapter.set_containers(vec![dead]);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        engine.log_captures(50).await.unwrap().len(),
        1,
        "the reconcile after a stop re-captured what the teardown already had"
    );
}

/// A container replaced behind Mast's back took its log with it. There is
/// nothing to read, so there must be no attempt and no empty row.
#[tokio::test(flavor = "multi_thread")]
async fn a_container_replaced_outside_mast_is_not_captured() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project_dir = tmp.path().join("captureapp");
    let _ = running_project(&engine, &adapter, tmp.path()).await;
    let calls_before = adapter.tail_calls();

    let compose_name =
        engine.snapshot().projects[0].compose_project_name.clone().unwrap();
    let mut replaced = observation(&compose_name, &project_dir, "app", "exited");
    replaced.id = "cid-app-recreated".into();
    replaced.exit_code = Some(1);
    adapter.set_containers(vec![replaced]);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(adapter.tail_calls(), calls_before, "read a log that no longer exists");
    assert!(engine.log_captures(50).await.unwrap().is_empty());
}

/// Unlike a live stream, a capture is written to disk and rendered with a copy
/// button. A secret that reaches one reaches a clipboard and a file.
#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_a_capture() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    adapter.set_tail(vec![CapturedLine {
        at: None,
        message: "SQLSTATE: access denied using password s3cr3t-value-here".into(),
        stderr: true,
    }]);
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));

    let project = make_project(tmp.path(), "captureapp");
    std::fs::write(project.join(".env"), "DB_PASSWORD=s3cr3t-value-here\n").unwrap();
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let compose_name = wait_for(&engine, "project resolved", |e| {
        e.snapshot().projects.first().and_then(|p| p.compose_project_name.clone())
    })
    .await;
    adapter.set_containers(vec![observation(&compose_name, &project, "app", "running")]);
    let id = wait_for(&engine, "container observed running", |e| {
        e.snapshot()
            .projects
            .first()
            .filter(|p| p.services.iter().any(|s| s.state == Some(ContainerState::Running)))
            .map(|p| p.id.clone())
    })
    .await;

    run_action(&engine, Action::CaptureServiceLogs { id, service: "app".into() }).await;
    let captures = wait_for_captures(&engine, "the manual capture", 1).await;

    let line = &captures[0].lines[0].message;
    assert!(!line.contains("s3cr3t-value-here"), "a secret reached a capture: {line}");
    assert!(line.contains("access denied"), "redaction ate the message: {line}");

    // And not into the file either — the db is the thing that outlives us.
    let db = std::fs::read(tmp.path().join("meta").join("captures.db")).unwrap();
    assert!(
        !String::from_utf8_lossy(&db).contains("s3cr3t-value-here"),
        "a secret was written to captures.db"
    );
}

/// The reason captures are on disk at all: a container that died while Mast
/// was closed is exactly the one nobody can explain afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn captures_outlive_the_engine_that_took_them() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();

    {
        let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
        let id = running_project(&engine, &adapter, tmp.path()).await;
        run_action(&engine, Action::CaptureServiceLogs { id, service: "app".into() }).await;
        wait_for_captures(&engine, "the manual capture", 1).await;
    }

    // A second engine over the same metadata directory — a restarted app.
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let captures = engine.log_captures(50).await.unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].lines[0].message, "worker exiting");
}

/// Captures are on disk, so clearing them has to be a real delete rather than
/// emptying a view that repopulates on the next read.
#[tokio::test(flavor = "multi_thread")]
async fn clearing_removes_them_for_good() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let id = running_project(&engine, &adapter, tmp.path()).await;

    run_action(&engine, Action::CaptureServiceLogs { id, service: "app".into() }).await;
    wait_for_captures(&engine, "the manual capture", 1).await;

    run_action(&engine, Action::ClearLogCaptures).await;
    assert!(engine.log_captures(50).await.unwrap().is_empty());
}

/// A subscriber sees captures as they happen — the tab must not need a poll.
#[tokio::test(flavor = "multi_thread")]
async fn subscribers_see_captures_live() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let id = running_project(&engine, &adapter, tmp.path()).await;

    let mut stream = engine.subscribe_log_captures();
    run_action(&engine, Action::CaptureServiceLogs { id, service: "app".into() }).await;

    let capture = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("no capture arrived on the subscription")
        .expect("capture stream ended");
    assert_eq!(capture.reason, CaptureReason::Manual);
    assert!(capture.id > 0, "a broadcast capture carries its stored id");
}

/// A container that died while Mast was closed — the case that motivated
/// persisting captures in the first place. Reconcile has no previous
/// observation to diff against, so the first sighting is already a corpse;
/// "never seen alive" must not read as "nothing happened".
#[tokio::test(flavor = "multi_thread")]
async fn a_death_mast_never_saw_happen_is_still_captured() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project = make_project(tmp.path(), "captureapp");

    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let compose_name = wait_for(&engine, "project resolved", |e| {
        e.snapshot().projects.first().and_then(|p| p.compose_project_name.clone())
    })
    .await;

    let mut dead = observation(&compose_name, &project, "app", "exited");
    dead.exit_code = Some(1);
    adapter.set_containers(vec![dead]);

    let captures = wait_for_captures(&engine, "the capture of a death from downtime", 1).await;
    assert_eq!(captures[0].reason, CaptureReason::Exited { status: Some(1) });
    assert_eq!(captures[0].lines[0].message, "worker exiting");
}

/// A container whose window holds nothing — it died long before the app
/// opened — must not leave an empty row. This is what keeps "assume it was
/// alive" from filling the tab on every startup with every stopped container.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_dead_container_leaves_no_empty_row() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    // Nothing within the capture window — what docker returns for a container
    // that stopped hours ago.
    adapter.set_tail(vec![]);
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project = make_project(tmp.path(), "captureapp");

    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let compose_name = wait_for(&engine, "project resolved", |e| {
        e.snapshot().projects.first().and_then(|p| p.compose_project_name.clone())
    })
    .await;

    let mut dead = observation(&compose_name, &project, "app", "exited");
    dead.exit_code = Some(0);
    adapter.set_containers(vec![dead]);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        engine.log_captures(50).await.unwrap().is_empty(),
        "a capture with no lines was recorded"
    );
}

/// A capture attempt that found nothing must not start the suppression
/// window. A container can go unhealthy while quiet and then crash noisily,
/// and the noisy one is the capture worth having.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_attempt_does_not_suppress_the_next_one() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    adapter.set_tail(vec![]); // quiet container
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project_dir = tmp.path().join("captureapp");
    let _ = running_project(&engine, &adapter, tmp.path()).await;
    let compose_name = engine.snapshot().projects[0].compose_project_name.clone().unwrap();

    // Goes unhealthy with nothing to say — no capture recorded.
    let mut sick = observation(&compose_name, &project_dir, "app", "running");
    sick.health = Some("unhealthy".into());
    adapter.set_containers(vec![sick]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(engine.log_captures(50).await.unwrap().is_empty());

    // Then dies, loudly — well inside the 30s suppression window.
    adapter.set_tail(vec![CapturedLine {
        at: None,
        message: "OOM killed".into(),
        stderr: true,
    }]);
    let mut dead = observation(&compose_name, &project_dir, "app", "exited");
    dead.exit_code = Some(137);
    adapter.set_containers(vec![dead]);

    let captures = wait_for_captures(&engine, "the capture after a quiet attempt", 1).await;
    assert_eq!(captures[0].lines[0].message, "OOM killed");
}

/// Repeat suppression has to outlive the process, because the store does.
/// Stopping a project and reopening Mast a moment later must not record the
/// same container's last words twice.
#[tokio::test(flavor = "multi_thread")]
async fn suppression_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let project_dir = tmp.path().join("captureapp");

    let compose_name = {
        let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
        let id = running_project(&engine, &adapter, tmp.path()).await;
        run_action(&engine, Action::StopProject { id }).await;
        wait_for_captures(&engine, "the teardown capture", 1).await;
        engine.snapshot().projects[0].compose_project_name.clone().unwrap()
    };

    // A fresh engine, whose in-memory suppression window is empty, meets the
    // same container — now exited, still inside the capture window.
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    engine.start();
    let mut dead = observation(&compose_name, &project_dir, "app", "exited");
    dead.exit_code = Some(0);
    adapter.set_containers(vec![dead]);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        engine.log_captures(50).await.unwrap().len(),
        1,
        "restarting the app re-captured a container already on record"
    );
}
