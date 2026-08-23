//! Docker-gated end-to-end proof of the database credential doctor: a real
//! MariaDB volume is initialized, `.env` drifts, diagnostics finds it, the
//! live reconcile fixes it without data loss, and — when the password itself
//! rotated away from root's — the volume recreate rebuilds from `.env`.
//!
//! Skipped cleanly without a docker daemon AND without a local `mariadb:11`
//! image — CI stays alpine-only; this runs where Sail developers live.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use mast_contract::{Action, EngineSnapshot, OperationEventKind, ProjectId};
use mast_engine::{
    Engine, EngineConfig, EngineDeps, RealConnector, RealLifecycleRunner, acquire_ownership,
};
use mast_project::MetadataStore;

const IMAGE: &str = "mariadb:11";

async fn sh(argv: &[&str], cwd: Option<&Path>) -> mast_docker::CommandOutput {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    mast_docker::run_command(&argv, cwd, &[], Duration::from_secs(180), 1024 * 1024)
        .await
        .expect("command ran")
}

async fn preconditions_met() -> bool {
    if std::env::var_os("MAST_SKIP_DOCKER_TESTS").is_some() {
        return false;
    }
    let docker = sh(&["docker", "info"], None).await.success();
    docker && sh(&["docker", "image", "inspect", IMAGE], None).await.success()
}

async fn janitor() {
    let out = sh(&["docker", "ps", "-aq", "--filter", "name=mast-it-"], None).await;
    let ids: Vec<&str> = out.stdout.split_whitespace().collect();
    if !ids.is_empty() {
        let mut argv = vec!["docker", "rm", "-f"];
        argv.extend(&ids);
        sh(&argv, None).await;
    }
    let vols = sh(&["docker", "volume", "ls", "-q", "--filter", "name=mast-it-"], None).await;
    for vol in vols.stdout.split_whitespace() {
        sh(&["docker", "volume", "rm", vol], None).await;
    }
    let nets = sh(&["docker", "network", "ls", "--filter", "name=mast-it-", "-q"], None).await;
    for net in nets.stdout.split_whitespace() {
        sh(&["docker", "network", "rm", net], None).await;
    }
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
        tokio::time::sleep(Duration::from_millis(100)).await;
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

/// Dispatch an action expected to FAIL; returns the failure message.
async fn run_action_expect_failure(engine: &Engine, action: Action) -> String {
    let id = engine.dispatch(action).unwrap();
    let mut events = engine.operation_events(id).unwrap();
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Completed => panic!("operation unexpectedly succeeded"),
            OperationEventKind::Failed { error } => return error,
            _ => {}
        }
    }
    panic!("operation stream ended without a terminal event");
}

/// `SELECT 1` inside the db container with explicit credentials; true when
/// the server accepts them.
async fn login_works(project: &Path, user: &str, password: &str, database: &str) -> bool {
    sh(
        &[
            "docker",
            "compose",
            "exec",
            "-T",
            "-e",
            &format!("MYSQL_PWD={password}"),
            "mariadb",
            "mariadb",
            "-u",
            user,
            "-D",
            database,
            "-N",
            "-e",
            "SELECT 1",
        ],
        Some(project),
    )
    .await
    .success()
}

async fn wait_for_login(project: &Path, user: &str, password: &str, database: &str) {
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        if login_works(project, user, password, database).await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {user}@{database} to authenticate"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn write_env(project: &Path, database: &str, user: &str, password: &str) {
    std::fs::write(
        project.join(".env"),
        format!(
            "APP_NAME=dbdoc\nDB_CONNECTION=mariadb\nDB_HOST=mariadb\nDB_PORT=3306\n\
             DB_DATABASE={database}\nDB_USERNAME={user}\nDB_PASSWORD={password}\n"
        ),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn env_drift_is_found_reconciled_live_and_recreated_when_root_is_lost() {
    if !preconditions_met().await {
        eprintln!("skipping: docker daemon or {IMAGE} image not available");
        return;
    }
    janitor().await;

    let tmp = tempfile::tempdir().unwrap();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let project = tmp.path().join(format!("mast-it-dbdoc{nanos}"));
    std::fs::create_dir_all(&project).unwrap();
    // Sail's mariadb stub shape: creds interpolated from .env, data in a
    // named volume — the init-once trap in its natural habitat.
    std::fs::write(
        project.join("compose.yaml"),
        format!(
            "services:\n  mariadb:\n    image: '{IMAGE}'\n    environment:\n      \
             MYSQL_ROOT_PASSWORD: '${{DB_PASSWORD}}'\n      MYSQL_DATABASE: \
             '${{DB_DATABASE}}'\n      MYSQL_USER: '${{DB_USERNAME}}'\n      MYSQL_PASSWORD: \
             '${{DB_PASSWORD}}'\n    volumes:\n      - 'dbdata:/var/lib/mysql'\nvolumes:\n  \
             dbdata:\n    driver: local\n"
        ),
    )
    .unwrap();
    write_env(&project, "app", "sail", "secret1");

    let engine = real_engine(tmp.path());
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", Duration::from_secs(30), |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = ProjectId(snap.projects[0].id.0.clone());

    run_action(&engine, Action::StartProject { id: pid.clone() }).await;
    wait_for_login(&project, "sail", "secret1", "app").await;

    // Matching credentials: the probe runs and finds nothing to report.
    let report = engine.run_diagnostics().await.unwrap();
    assert!(
        !report.findings.iter().any(|f| f.check == "db-credentials"),
        "{:?}",
        report.findings
    );

    // The user renames database and user in .env; the volume stays as
    // initialized. Password unchanged, so root access survives → the live
    // reconcile is offered.
    write_env(&project, "app2", "sail2", "secret1");
    let report = engine.run_diagnostics().await.unwrap();
    let finding = report
        .findings
        .iter()
        .find(|f| f.check == "db-credentials")
        .expect("drifted creds must be found");
    let repair = finding.repair.as_ref().unwrap();
    assert_eq!(repair.id, "db-reconcile");
    assert_eq!(repair.arg.as_deref(), Some("mariadb"));

    // The preview shows the statements with the password masked.
    let plan = engine.repair_preview("db-reconcile", Some("mariadb"), Some(&pid)).await.unwrap();
    let joined = plan.summary.join("\n");
    assert!(joined.contains("CREATE USER IF NOT EXISTS 'sail2'@'%'"), "{joined}");
    assert!(!joined.contains("secret1"), "password leaked into preview: {joined}");

    run_action(
        &engine,
        Action::ApplyRepair {
            repair: "db-reconcile".into(),
            arg: Some("mariadb".into()),
            project: Some(pid.clone()),
        },
    )
    .await;
    assert!(login_works(&project, "sail2", "secret1", "app2").await);
    // The old database is untouched — reconcile never destroys data.
    assert!(login_works(&project, "root", "secret1", "app").await);
    let report = engine.run_diagnostics().await.unwrap();
    assert!(!report.findings.iter().any(|f| f.check == "db-credentials"));

    // Now the password itself rotates: root's stays at the init-time value,
    // so no admin login works and only the destructive path remains.
    write_env(&project, "app2", "sail2", "secret2");
    let report = engine.run_diagnostics().await.unwrap();
    let finding = report
        .findings
        .iter()
        .find(|f| f.check == "db-credentials")
        .expect("rotated password must be found");
    assert_eq!(finding.repair.as_ref().unwrap().id, "db-recreate-volume");

    let plan =
        engine.repair_preview("db-recreate-volume", Some("mariadb"), Some(&pid)).await.unwrap();
    let joined = plan.summary.join("\n");
    assert!(joined.contains("_dbdata"), "preview must name the doomed volume: {joined}");
    assert!(joined.contains("lost"), "{joined}");

    run_action(
        &engine,
        Action::ApplyRepair {
            repair: "db-recreate-volume".into(),
            arg: Some("mariadb".into()),
            project: Some(pid.clone()),
        },
    )
    .await;
    wait_for_login(&project, "sail2", "secret2", "app2").await;
    let report = engine.run_diagnostics().await.unwrap();
    assert!(
        !report.findings.iter().any(|f| f.check == "db-credentials"),
        "{:?}",
        report.findings
    );

    sh(&["docker", "compose", "down", "-v", "--remove-orphans"], Some(&project)).await;
}

/// The version guard: retagging a database service against an initialized
/// volume warns on in-place upgrades, refuses guaranteed crash-loops, and the
/// diagnostics scan catches a mismatched tag while the project is STOPPED —
/// before the failed start, which is the whole point.
#[tokio::test(flavor = "multi_thread")]
async fn retagging_a_database_is_guarded_by_what_its_volume_holds() {
    const OLD: &str = "mariadb:10.6";
    if !preconditions_met().await
        || !sh(&["docker", "image", "inspect", OLD], None).await.success()
        || !sh(&["docker", "image", "inspect", "alpine:latest"], None).await.success()
    {
        eprintln!("skipping: docker daemon or {OLD}/alpine images not available");
        return;
    }
    janitor().await;

    let tmp = tempfile::tempdir().unwrap();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let project = tmp.path().join(format!("mast-it-dbver{nanos}"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("compose.yaml"),
        format!(
            "services:\n  mariadb:\n    image: '{OLD}'\n    environment:\n      \
             MYSQL_ROOT_PASSWORD: '${{DB_PASSWORD}}'\n      MYSQL_DATABASE: \
             '${{DB_DATABASE}}'\n      MYSQL_USER: '${{DB_USERNAME}}'\n      MYSQL_PASSWORD: \
             '${{DB_PASSWORD}}'\n    volumes:\n      - 'dbdata:/var/lib/mysql'\nvolumes:\n  \
             dbdata:\n    driver: local\n"
        ),
    )
    .unwrap();
    write_env(&project, "app", "sail", "secret1");

    let engine = real_engine(tmp.path());
    run_action(&engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;
    let snap = wait_until(&engine, "project resolved", Duration::from_secs(30), |s| {
        s.projects.first().is_some_and(|p| p.compose_project_name.is_some())
    })
    .await;
    let pid = ProjectId(snap.projects[0].id.0.clone());

    // Initialize the volume under the old major, then stop everything.
    run_action(&engine, Action::StartProject { id: pid.clone() }).await;
    wait_for_login(&project, "sail", "secret1", "app").await;
    run_action(&engine, Action::StopProject { id: pid.clone() }).await;

    // Matching tag: the scan is silent.
    let report = engine.run_diagnostics().await.unwrap();
    assert!(
        !report.findings.iter().any(|f| f.check == "db-volume-version"),
        "{:?}",
        report.findings
    );

    // A downgrade preview says so, and the apply refuses outright.
    let preview =
        engine.service_image_preview(&pid, "mariadb", "mariadb:10.5").await.unwrap();
    assert!(
        preview.summary.iter().any(|s| s.contains("WILL NOT START")),
        "{:?}",
        preview.summary
    );
    let error = run_action_expect_failure(
        &engine,
        Action::SetServiceImage {
            id: pid.clone(),
            service: "mariadb".into(),
            image: "mariadb:10.5".into(),
        },
    )
    .await;
    assert!(error.contains("downgrade"), "{error}");
    let file = std::fs::read_to_string(project.join("compose.yaml")).unwrap();
    assert!(file.contains(OLD), "refused retag must leave the file alone: {file}");

    // An upgrade is allowed with a back-up-first warning…
    let preview = engine.service_image_preview(&pid, "mariadb", "mariadb:11").await.unwrap();
    assert!(
        preview.summary.iter().any(|s| s.contains("MARIADB_AUTO_UPGRADE")),
        "{:?}",
        preview.summary
    );
    run_action(
        &engine,
        Action::SetServiceImage {
            id: pid.clone(),
            service: "mariadb".into(),
            image: "mariadb:11".into(),
        },
    )
    .await;

    // …and with the tag now ahead of the volume, diagnostics flags the
    // stopped project before its next start. The scan reads the resolved
    // model, which refreshes on the debounced post-retag reconcile — poll
    // the report until it lands.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (title, detail) = loop {
        let report = engine.run_diagnostics().await.unwrap();
        if let Some(f) = report.findings.iter().find(|f| f.check == "db-volume-version") {
            break (f.title.clone(), f.detail.clone());
        }
        assert!(
            Instant::now() < deadline,
            "stopped-project version mismatch never surfaced: {:?}",
            report.findings
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert!(title.contains("upgrade its data"), "{title}");
    assert!(detail.contains("10.6"), "{detail}");

    sh(&["docker", "compose", "down", "-v", "--remove-orphans"], Some(&project)).await;
}
