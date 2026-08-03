//! Effect history (M9): what the user is shown about what Mast did.
//!
//! The load-bearing claims are that coverage is automatic (a shell-out nobody
//! remembered to record still shows up), that user actions are distinguishable
//! from Mast's own upkeep, and that secrets never reach the record — the
//! history is copyable, so a leak here is a leak into a clipboard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use mast_contract::{
    Action, DockerStatus, HistoryDetail, HistoryEntry, HistoryOrigin, HistoryOutcome,
    OperationEventKind, ProjectId,
};
use mast_docker::{DockerError, RuntimeAdapter};
use mast_engine::{
    Engine, EngineConfig, EngineDeps, RealLifecycleRunner, RuntimeConnector, acquire_ownership,
};
use mast_project::MetadataStore;

struct DeadConnector;

#[async_trait::async_trait]
impl RuntimeConnector for DeadConnector {
    async fn connect(&self) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError> {
        Err(DockerError::Api("daemon unreachable".into()))
    }
}

fn test_engine(meta_dir: &std::path::Path) -> Engine {
    Engine::new(
        EngineConfig {
            hint_debounce: Duration::from_millis(30),
            reconcile_interval: Duration::from_secs(60),
            registry_refresh: false,
            ..Default::default()
        },
        EngineDeps {
            connector: Arc::new(DeadConnector),
            store: MetadataStore::open(meta_dir.join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: acquire_ownership(Some(meta_dir.join("lock"))),
        },
    )
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

/// Poll this engine's history until `predicate` matches, returning the most
/// recent match — tests that probe repeatedly want the newest attempt.
async fn wait_for_history(
    engine: &Engine,
    what: &str,
    predicate: impl Fn(&HistoryEntry) -> bool,
) -> HistoryEntry {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(found) = engine.history_recent().into_iter().rfind(&predicate) {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for history entry: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn argv_of(entry: &HistoryEntry) -> Vec<String> {
    match &entry.detail {
        HistoryDetail::Command { argv, .. } => argv.clone(),
        other => panic!("expected a command, got {other:?}"),
    }
}

fn make_project(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let project = dir.join(name);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("compose.yaml"), "services:\n  app:\n    image: alpine:latest\n")
        .unwrap();
    project
}

/// Nobody threads a recorder through `run_command`; the observer hook means a
/// shell-out anywhere in the workspace is recorded anyway — as background,
/// since no user action claimed it.
#[tokio::test(flavor = "multi_thread")]
async fn unclaimed_shell_outs_are_recorded_as_background() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path());

    let argv: Vec<String> = ["bash", "-c", "echo mast-history-probe"].map(String::from).to_vec();
    let out = mast_docker::run_command(&argv, None, &[], Duration::from_secs(10), 64 * 1024)
        .await
        .unwrap();
    assert!(out.success());

    let entry = wait_for_history(&engine, "the probe", |entry| {
        argv_of(entry).iter().any(|arg| arg.contains("mast-history-probe"))
    })
    .await;
    assert_eq!(entry.origin, HistoryOrigin::Background);
    assert_eq!(entry.outcome, HistoryOutcome::Exited { status: 0 });
    assert_eq!(entry.project, None);
    assert!(entry.output.iter().any(|line| line.contains("mast-history-probe")), "{entry:?}");
    assert!(entry.duration_ms.is_some());
}

/// A failing command keeps its exit status and its output, which is the whole
/// point: the user can see why it failed without reproducing it.
#[tokio::test(flavor = "multi_thread")]
async fn failures_keep_their_status_and_output() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path());

    let argv: Vec<String> = ["bash", "-c", "echo mast-history-boom >&2; exit 3"]
        .map(String::from)
        .to_vec();
    let _ = mast_docker::run_command(&argv, None, &[], Duration::from_secs(10), 64 * 1024).await;

    let entry = wait_for_history(&engine, "the failing probe", |entry| {
        argv_of(entry).iter().any(|arg| arg.contains("mast-history-boom"))
    })
    .await;
    assert_eq!(entry.outcome, HistoryOutcome::Exited { status: 3 });
    assert!(entry.output.iter().any(|line| line.contains("mast-history-boom")), "{entry:?}");
}

/// Config writes are effects too — a history that only listed subprocesses
/// would understate what Mast changed on the machine.
#[tokio::test(flavor = "multi_thread")]
async fn env_writes_are_recorded_against_their_action() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "envapp");
    let engine = test_engine(tmp.path());
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let id = ProjectId(engine.snapshot().projects[0].id.0.clone());

    run_action(
        &engine,
        Action::SetEnvVar { id: id.clone(), key: "APP_PORT".into(), value: "8123".into() },
    )
    .await;

    let entry = wait_for_history(&engine, "the env write", |entry| {
        matches!(&entry.detail, HistoryDetail::FileWrite { path, .. } if path.ends_with(".env"))
    })
    .await;
    assert_eq!(entry.origin, HistoryOrigin::User);
    assert_eq!(entry.outcome, HistoryOutcome::Applied);
    assert_eq!(entry.project, Some(id));
    // The label names the action and the project, not the file.
    assert!(entry.label.contains("envapp"), "{}", entry.label);
    assert!(entry.label.contains("APP_PORT"), "{}", entry.label);
    let HistoryDetail::FileWrite { summary, .. } = &entry.detail else { unreachable!() };
    assert!(summary.iter().any(|line| line.contains("APP_PORT=8123")), "{summary:?}");
}

/// A secret set in one project must not surface in a command that belongs to
/// no project — history is copyable, and the union redactor is what stops it.
#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_the_record() {
    let tmp = tempfile::tempdir().unwrap();
    let project = make_project(tmp.path(), "secretapp");
    std::fs::write(project.join(".env"), "DB_PASSWORD=hunter2secretvalue\n").unwrap();
    let engine = test_engine(tmp.path());
    engine.start();
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;

    // The redactor is rebuilt on reconcile; wait until it knows the secret by
    // probing through a recorded command.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let argv: Vec<String> = ["bash", "-c", "echo tag-hunter2secretvalue-tag"]
            .map(String::from)
            .to_vec();
        let _ = mast_docker::run_command(&argv, None, &[], Duration::from_secs(10), 64 * 1024)
            .await;
        let entry = wait_for_history(&engine, "the echo", |entry| {
            entry.output.iter().any(|line| line.contains("tag-"))
        })
        .await;
        if entry.output.iter().all(|line| !line.contains("hunter2secretvalue")) {
            // The argv and the derived label carried the secret too, and must
            // be redacted the same way.
            assert!(
                argv_of(&entry).iter().all(|arg| !arg.contains("hunter2secretvalue")),
                "{entry:?}"
            );
            assert!(!entry.label.contains("hunter2secretvalue"), "{entry:?}");
            return;
        }
        assert!(Instant::now() < deadline, "secret was still visible in history: {entry:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Two engines in one process both keep recording: registering an observer
/// must never switch an earlier engine's history off.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_engine_does_not_silence_the_first() {
    let tmp = tempfile::tempdir().unwrap();
    let first = test_engine(&tmp.path().join("one"));
    let second = test_engine(&tmp.path().join("two"));

    let argv: Vec<String> =
        ["bash", "-c", "echo mast-history-two-engines"].map(String::from).to_vec();
    mast_docker::run_command(&argv, None, &[], Duration::from_secs(10), 64 * 1024).await.unwrap();

    for engine in [&first, &second] {
        wait_for_history(engine, "the shared probe", |entry| {
            argv_of(entry).iter().any(|arg| arg.contains("mast-history-two-engines"))
        })
        .await;
    }
}
