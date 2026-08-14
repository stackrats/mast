//! Docker-gated integration tests against the real daemon and the
//! `fixtures/sail-minimal` fixture (M2 verify criteria). Skipped cleanly when
//! no usable docker daemon is present. Each run uses a unique compose project
//! name (`mast-it-<nanos>`) and a startup janitor sweeps crash leftovers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use mast_contract::{Action, ContainerState, EngineSnapshot, OperationEventKind, ProjectStatus};
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
    mast_docker::run_command(&argv, cwd, &[], Duration::from_secs(180), 1024 * 1024)
        .await
        .expect("command ran")
}

/// Sweep leftovers from crashed previous runs (plan: startup janitor).
async fn janitor() {
    let out = sh(&["docker", "ps", "-aq", "--filter", "name=mast-it-"], None).await;
    let ids: Vec<&str> = out.stdout.split_whitespace().collect();
    if !ids.is_empty() {
        let mut argv = vec!["docker", "rm", "-f"];
        argv.extend(&ids);
        sh(&argv, None).await;
    }
    let nets = sh(&["docker", "network", "ls", "--filter", "name=mast-it-", "-q"], None).await;
    for net in nets.stdout.split_whitespace() {
        sh(&["docker", "network", "rm", net], None).await;
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
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.metadata().unwrap().permissions().mode();
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).unwrap();
            }
        }
    }
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{prefix}{nanos}")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sail-minimal")
}

fn real_engine(meta_dir: &Path) -> Engine {
    let engine = Engine::new(
        EngineConfig {
            hint_debounce: Duration::from_millis(100),
            reconcile_interval: Duration::from_secs(60),
            ..Default::default()
        },
        EngineDeps {
            connector: Arc::new(RealConnector),
            store: MetadataStore::open(meta_dir.join("meta")).unwrap(),
            process_env: HashMap::new(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: acquire_ownership(Some(meta_dir.join("lock"))),
        },
    );
    engine.start();
    engine
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
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {snap:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run_action(engine: &Engine, action: Action) {
    let id = engine.dispatch(action).unwrap();
    let mut events = engine.operation_events(id).unwrap();
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Completed => return,
            OperationEventKind::Failed { error } => panic!("operation failed: {error}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sail_minimal_core_observation_loop() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    janitor().await;

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join(unique_name("mast-it-sail"));
    copy_dir(&fixture_dir(), &project);

    let engine = real_engine(tmp.path());
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;

    // Resolution goes through the REAL vendored sail script
    // (SAIL_SKIP_CHECKS=1 sail config, ADR-0001).
    let snap = wait_until(&engine, "sail project resolved", Duration::from_secs(30), |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let summary = &snap.projects[0];
    assert!(summary.is_sail);
    let compose_name = summary.compose_project_name.clone().unwrap();
    assert_eq!(compose_name, project.file_name().unwrap().to_string_lossy());
    let declared: Vec<&str> = summary.services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(declared, vec!["laravel.test", "redis"]);
    assert_eq!(summary.status, ProjectStatus::Stopped);

    // Terminal parity: plain `docker compose up -d`, exactly as a developer
    // would run it. The UI must reflect it within ~a second of completion.
    let up = sh(&["docker", "compose", "up", "-d"], Some(&project)).await;
    assert!(up.success(), "compose up failed: {}", up.stderr);
    let reflected = Instant::now();
    let snap = wait_until(&engine, "running after terminal up", Duration::from_secs(10), |s| {
        s.projects[0].status == ProjectStatus::Running
    })
    .await;
    let latency = reflected.elapsed();
    assert!(latency < Duration::from_secs(3), "reflection took {latency:?}");
    for service in &snap.projects[0].services {
        assert_eq!(service.state, Some(ContainerState::Running), "{}", service.name);
        assert!(service.container_id.is_some());
    }

    // Terminal stop → exited containers → project Stopped, still associated.
    let stop = sh(&["docker", "compose", "stop"], Some(&project)).await;
    assert!(stop.success(), "compose stop failed: {}", stop.stderr);
    let snap = wait_until(&engine, "stopped after terminal stop", Duration::from_secs(10), |s| {
        s.projects[0].status == ProjectStatus::Stopped
    })
    .await;
    assert!(
        snap.projects[0].services.iter().any(|s| s.state == Some(ContainerState::Exited)),
        "exited containers should stay associated"
    );

    sh(&["docker", "compose", "down", "-v", "--remove-orphans"], Some(&project)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_file_and_profile_project_associates_correctly() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    janitor().await;

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join(unique_name("mast-it-multi"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n    command: ['sleep', '600']\n",
    )
    .unwrap();
    // Cross-family override (ADR-0001) + a profile-gated service.
    std::fs::write(
        project.join("docker-compose.override.yml"),
        "services:\n  extra:\n    image: alpine:latest\n    command: ['sleep', '600']\n    profiles: [debug]\n",
    )
    .unwrap();

    let engine = real_engine(tmp.path());
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "resolved", Duration::from_secs(30), |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;

    let up = sh(&["docker", "compose", "--profile", "debug", "up", "-d"], Some(&project)).await;
    assert!(up.success(), "compose up failed: {}", up.stderr);
    let snap = wait_until(&engine, "profile container observed", Duration::from_secs(10), |s| {
        s.projects[0]
            .services
            .iter()
            .filter(|svc| svc.state == Some(ContainerState::Running))
            .count()
            == 2
    })
    .await;
    let names: Vec<&str> = snap.projects[0].services.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"app") && names.contains(&"extra"), "services: {names:?}");

    sh(
        &["docker", "compose", "--profile", "debug", "down", "-v", "--remove-orphans"],
        Some(&project),
    )
    .await;
}

/// M3 verify: the full core loop through engine operations — start via
/// dispatch (streamed output), observe Running, follow real container logs,
/// stop via dispatch, observe Stopped.
#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_operations_and_log_streaming_end_to_end() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    janitor().await;

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join(unique_name("mast-it-lc"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        "services:\n  app:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"i=0; while true; do echo tick $i; i=$((i+1)); sleep 0.2; done\"]\n",
    )
    .unwrap();

    let engine = real_engine(tmp.path());
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    wait_until(&engine, "resolved", Duration::from_secs(30), |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let id = engine.snapshot().projects[0].id.clone();

    // Start through the engine: streamed output + Completed.
    let op = engine.dispatch(Action::StartProject { id: id.clone() }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    let mut output_lines = 0;
    loop {
        let event = events.next().await.expect("op stream");
        match event.kind {
            OperationEventKind::Output { .. } => output_lines += 1,
            OperationEventKind::Completed => break,
            OperationEventKind::Failed { error } => panic!("start failed: {error}"),
            _ => {}
        }
    }
    assert!(output_lines > 0, "compose up should stream output");
    wait_until(&engine, "running after engine start", Duration::from_secs(15), |s| {
        s.projects[0].status == ProjectStatus::Running
    })
    .await;

    // Follow real container logs.
    let mut logs = engine.service_logs(&id, "app", 10).await.unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.len() < 3 && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), logs.next()).await {
            Ok(Some(line)) => {
                assert_eq!(line.service, "app");
                seen.push(line.message);
            }
            _ => break,
        }
    }
    assert!(seen.len() >= 3, "expected streamed log lines, got {seen:?}");
    assert!(seen.iter().any(|l| l.contains("tick")), "{seen:?}");

    // Stop through the engine.
    let op = engine.dispatch(Action::StopProject { id: id.clone() }).unwrap();
    let mut events = engine.operation_events(op).unwrap();
    loop {
        let event = events.next().await.expect("op stream");
        match event.kind {
            OperationEventKind::Completed => break,
            OperationEventKind::Failed { error } => panic!("stop failed: {error}"),
            _ => {}
        }
    }
    wait_until(&engine, "stopped after engine stop", Duration::from_secs(15), |s| {
        s.projects[0].status == ProjectStatus::Stopped
    })
    .await;

    // M10: the stop above captured the container's tail before running, so the
    // output that was streaming a moment ago is still readable now that the
    // stream itself has ended.
    let captures = engine.log_captures(10).await.unwrap();
    let capture = captures
        .iter()
        .find(|c| c.service == "app")
        .expect("stopping a project captures its containers first");
    assert_eq!(capture.reason, mast_contract::CaptureReason::Teardown { verb: "stop".into() });
    assert!(
        capture.lines.iter().any(|l| l.message.contains("tick")),
        "capture should hold the container's own output: {:?}",
        capture.lines
    );
    // Docker's --timestamps prefix is split off, not left in the message.
    assert!(
        capture.lines.iter().all(|l| !l.message.starts_with("20")),
        "timestamps leaked into the message text: {:?}",
        capture.lines
    );
    assert!(capture.lines.iter().any(|l| l.at.is_some()), "no line carried a timestamp");

    // `down -v` removes the container, which is what destroys the log. The
    // capture is on disk, so it survives — including into a fresh engine.
    sh(&["docker", "compose", "down", "-v", "--remove-orphans"], Some(&project)).await;
    let reopened = real_engine(tmp.path());
    assert!(
        reopened
            .log_captures(10)
            .await
            .unwrap()
            .iter()
            .any(|c| c.lines.iter().any(|l| l.message.contains("tick"))),
        "captures must outlive both the container and the engine"
    );
}

