//! Docker-gated M6 verify: real workspace start against the daemon —
//! B waits for A's *health* (A has a healthcheck), order is observable in the
//! operation output, stop reverses. Uses unique mast-it-* names + janitor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use mast_contract::{
    Action, EngineSnapshot, OperationEventKind, ProjectStatus, WorkspaceMember,
};
use mast_engine::{
    Engine, EngineConfig, EngineDeps, RealConnector, RealLifecycleRunner, acquire_ownership,
};
use mast_project::MetadataStore;

async fn docker_usable() -> bool {
    if std::env::var_os("MAST_SKIP_DOCKER_TESTS").is_some() {
        return false;
    }
    mast_docker::run_command(
        &["docker".into(), "info".into()],
        None,
        &[],
        Duration::from_secs(10),
        16 * 1024,
    )
    .await
    .map(|o| o.success())
    .unwrap_or(false)
}

async fn sh(argv: &[&str], cwd: Option<&Path>) {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let _ = mast_docker::run_command(&argv, cwd, &[], Duration::from_secs(180), 1024 * 1024)
        .await;
}

async fn janitor() {
    let out = mast_docker::run_command(
        &["docker".into(), "ps".into(), "-aq".into(), "--filter".into(), "name=mast-it-".into()],
        None,
        &[],
        Duration::from_secs(30),
        64 * 1024,
    )
    .await
    .unwrap();
    let ids: Vec<&str> = out.stdout.split_whitespace().collect();
    if !ids.is_empty() {
        let mut argv = vec!["docker", "rm", "-f"];
        argv.extend(&ids);
        sh(&argv, None).await;
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

async fn wait_until(
    engine: &Engine,
    what: &str,
    timeout: Duration,
    predicate: impl Fn(&EngineSnapshot) -> bool,
) -> EngineSnapshot {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = engine.snapshot();
        if predicate(&snap) {
            return snap;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_start_waits_for_health_and_orders_members() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    janitor().await;

    let tmp = tempfile::tempdir().unwrap();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sail-multiproject");
    let mut project_dirs = HashMap::new();
    for name in ["a", "b", "c"] {
        let dir = tmp.path().join(format!("mast-it-ws{name}-{nanos}"));
        copy_dir(&fixture.join(name), &dir);
        project_dirs.insert(name, dir);
    }

    let engine = Engine::new(
        EngineConfig {
            hint_debounce: Duration::from_millis(100),
            reconcile_interval: Duration::from_secs(60),
            ready_timeout: Duration::from_secs(60),
            ..Default::default()
        },
        EngineDeps {
            connector: Arc::new(RealConnector),
            store: MetadataStore::open(tmp.path().join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: acquire_ownership(Some(tmp.path().join("lock"))),
        },
    );
    engine.start();

    for dir in project_dirs.values() {
        let op = engine
            .dispatch(Action::ImportProject { path: dir.to_string_lossy().into() })
            .unwrap();
        let mut events = engine.operation_events(op).unwrap();
        while let Some(e) = events.next().await {
            if e.kind.is_terminal() {
                break;
            }
        }
    }
    let snap = wait_until(&engine, "all resolved", Duration::from_secs(30), |s| {
        s.projects.len() == 3 && s.projects.iter().all(|p| p.compose_project_name.is_some())
    })
    .await;
    let id_of = |needle: &str| {
        snap.projects
            .iter()
            .find(|p| p.name.contains(&format!("ws{needle}")))
            .map(|p| p.id.clone())
            .unwrap()
    };
    let (a, b, c) = (id_of("a"), id_of("b"), id_of("c"));

    let members = vec![
        WorkspaceMember { project: a.clone(), depends_on: vec![] },
        WorkspaceMember { project: b.clone(), depends_on: vec![a.clone()] },
        WorkspaceMember { project: c.clone(), depends_on: vec![b.clone()] },
    ];
    let op = engine
        .dispatch(Action::SaveWorkspace { id: None, name: "it-suite".into(), members })
        .unwrap();
    let mut events = engine.operation_events(op).unwrap();
    while let Some(e) = events.next().await {
        if e.kind.is_terminal() {
            break;
        }
    }
    let ws = engine.snapshot().workspaces[0].id.clone();

    // Start: collect output, must complete, and the order must show A ready
    // (healthcheck passed) before B starts.
    let op = engine.dispatch(Action::StartWorkspace { id: ws.clone() }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut lines = Vec::new();
    let mut failed = None;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => lines.push(line),
            OperationEventKind::Failed { error } => {
                failed = Some(error);
                break;
            }
            kind if kind.is_terminal() => break,
            _ => {}
        }
    }
    assert!(failed.is_none(), "workspace start failed: {failed:?}\nlines: {lines:#?}");
    let idx = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in {lines:#?}"))
    };
    let a_ready = idx("ready");
    let b_start = lines
        .iter()
        .position(|l| l.contains("wsb") && l.contains("start"))
        .expect("b start line");
    assert!(a_ready < b_start, "B must start after A is ready: {lines:#?}");

    wait_until(&engine, "workspace running", Duration::from_secs(30), |s| {
        s.workspaces[0].status == ProjectStatus::Running
    })
    .await;

    // Stop reverses and settles.
    let op = engine.dispatch(Action::StopWorkspace { id: ws }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    while let Some(e) = events.next().await {
        if e.kind.is_terminal() {
            break;
        }
    }
    wait_until(&engine, "workspace stopped", Duration::from_secs(30), |s| {
        s.workspaces[0].status == ProjectStatus::Stopped
    })
    .await;

    for dir in project_dirs.values() {
        sh(&["docker", "compose", "down", "-v", "--remove-orphans"], Some(dir)).await;
    }
}
