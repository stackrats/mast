//! M6 verify: 30+ container event-storm load test. A burst of container
//! starts/stops must not flood the patch stream (debounced reconcile
//! coalesces hints) and observation must converge to the truth.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use mast_contract::{Action, EngineSnapshot, OperationEventKind, ProjectStatus};
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

async fn sh(argv: &[&str], cwd: Option<&Path>) -> mast_docker::CommandOutput {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    mast_docker::run_command(&argv, cwd, &[], Duration::from_secs(300), 1024 * 1024)
        .await
        .expect("command ran")
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn thirty_container_storm_converges_without_patch_flood() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let project = tmp.path().join(format!("mast-it-storm-{nanos}"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n    command: [\"sleep\", \"600\"]\n",
    )
    .unwrap();

    let engine = Engine::new(
        EngineConfig {
            hint_debounce: Duration::from_millis(150),
            reconcile_interval: Duration::from_secs(60),
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

    let op = engine
        .dispatch(Action::ImportProject { path: project.to_string_lossy().into() })
        .unwrap();
    let mut events = engine.operation_events(op).unwrap();
    while let Some(e) = events.next().await {
        if matches!(e.kind, OperationEventKind::Completed | OperationEventKind::Failed { .. }) {
            break;
        }
    }
    wait_until(&engine, "resolved", Duration::from_secs(30), |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let seq_before = engine.snapshot().seq;

    // The storm: 32 replicas coming up at once (~32 create + 32 start events).
    let up = sh(&["docker", "compose", "up", "-d", "--scale", "app=32"], Some(&project)).await;
    assert!(up.success(), "scale up failed: {}", up.stderr);

    let snap = wait_until(&engine, "storm converged to running", Duration::from_secs(30), |s| {
        s.projects[0].status == ProjectStatus::Running
    })
    .await;
    assert!(!snap.projects[0].services.is_empty());

    // Kill everything at once: another ~32-event burst.
    let kill = sh(&["docker", "compose", "kill"], Some(&project)).await;
    assert!(kill.success(), "kill failed: {}", kill.stderr);
    wait_until(&engine, "storm converged to stopped", Duration::from_secs(30), |s| {
        s.projects[0].status == ProjectStatus::Stopped
    })
    .await;

    // Coalescing proof: ~128 daemon events must not mean ~128 patches. Each
    // debounced reconcile emits at most one ProjectUpdated.
    let seq_after = engine.snapshot().seq;
    let patches = seq_after - seq_before;
    assert!(patches < 60, "patch stream flooded: {patches} patches for the storm");

    sh(&["docker", "compose", "down", "-v", "--remove-orphans"], Some(&project)).await;
}
