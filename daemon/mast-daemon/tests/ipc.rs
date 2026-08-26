#![cfg(unix)]

//! The M8 verify criterion: the SAME client suite runs against the local
//! and IPC clients — the transport must be invisible.

use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use mast_client::MastClient;
use mast_client_ipc::IpcClient;
use mast_client_local::LocalClient;
use mast_contract::{
    Action, BUILD_VERSION, DockerStatus, OperationEventKind, PROTOCOL_VERSION, ProjectId,
    SubscriptionItem,
};
use mast_docker::{DockerError, RuntimeAdapter};
use mast_engine::{Engine, EngineConfig, EngineDeps, RealLifecycleRunner, RuntimeConnector};
use mast_project::MetadataStore;

struct NoDocker;

#[async_trait::async_trait]
impl RuntimeConnector for NoDocker {
    async fn connect(&self) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError> {
        Err(DockerError::Api("no docker in this test".into()))
    }
}

fn test_engine(dir: &Path) -> Engine {
    Engine::new(
        EngineConfig::default(),
        EngineDeps {
            connector: Arc::new(NoDocker),
            store: MetadataStore::open(dir.join("meta")).unwrap(),
            process_env: Default::default(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: mast_engine::acquire_ownership(Some(dir.join("lock"))),
        },
    )
}

/// The transport-agnostic exercise: snapshot, subscribe→patch, fake
/// operation event replay to terminal, cancellation.
async fn client_suite(client: &dyn MastClient, watch_dir: &str) {
    let snapshot = client.snapshot().await.unwrap();
    assert_eq!(snapshot.protocol_version, PROTOCOL_VERSION);
    assert!(!snapshot.read_only);

    // Subscribe FIRST (protocol rule), then provoke a patch.
    let mut patches = client.subscribe(None).await.unwrap();
    let op = client
        .dispatch(Action::AddWatchedDirectory { path: watch_dir.into() })
        .await
        .unwrap();
    let mut events = client.operation_events(op).await.unwrap();
    while let Some(event) = events.next().await {
        if matches!(event.kind, OperationEventKind::Completed) {
            break;
        }
        assert!(
            !matches!(event.kind, OperationEventKind::Failed { .. }),
            "watch-directory op failed"
        );
    }
    let patch = tokio::time::timeout(std::time::Duration::from_secs(5), patches.next())
        .await
        .expect("no patch arrived")
        .expect("patch stream ended");
    assert!(matches!(patch, SubscriptionItem::Patch { .. }));

    // Fake operation: full replay to a terminal event.
    let op = client
        .dispatch(Action::StartFakeOperation { project: ProjectId("fake".into()) })
        .await
        .unwrap();
    let mut events = client.operation_events(op).await.unwrap();
    let mut saw_started = false;
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.next())
            .await
            .expect("operation stream stalled")
            .expect("operation stream ended early");
        match event.kind {
            OperationEventKind::Started => saw_started = true,
            OperationEventKind::Completed => break,
            OperationEventKind::Failed { error } => panic!("fake op failed: {error}"),
            _ => {}
        }
    }
    assert!(saw_started, "event replay must include Started");

    // Cancellation confirms over the same stream.
    let op = client
        .dispatch(Action::StartFakeOperation { project: ProjectId("fake".into()) })
        .await
        .unwrap();
    let mut events = client.operation_events(op).await.unwrap();
    client.cancel(op).await.unwrap();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.next())
            .await
            .expect("cancel stream stalled")
            .expect("cancel stream ended early");
        if matches!(event.kind, OperationEventKind::Cancelled) {
            break;
        }
    }

    // The side channels: a request/response and a subscription each. There is
    // no docker here, so the interesting assertion is that both round-trip at
    // all — a mistyped method name on the wire compiles perfectly and fails
    // only here.
    assert!(client.history_recent().await.unwrap().iter().all(|e| e.id > 0));
    let _history: mast_client::HistoryStream = client.subscribe_history().await.unwrap();
    assert!(client.log_captures(10).await.unwrap().is_empty());
    let _captures: mast_client::CaptureStream = client.subscribe_log_captures().await.unwrap();
    let _usage: mast_client::UsageStream = client.subscribe_usage().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn same_suite_local_and_ipc() {
    let tmp = tempfile::tempdir().unwrap();

    // Local transport.
    let engine = test_engine(tmp.path());
    engine.start();
    let watch_local = tmp.path().join("watch-local");
    std::fs::create_dir_all(&watch_local).unwrap();
    client_suite(&LocalClient::new(engine.clone()), watch_local.to_str().unwrap()).await;

    // IPC transport against the same engine.
    let socket = tmp.path().join("daemon.sock");
    {
        let engine = engine.clone();
        let socket = socket.clone();
        tokio::spawn(async move {
            let _ = mast_daemon::serve(engine, &socket).await;
        });
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let ipc = loop {
        match IpcClient::connect(&socket).await {
            Ok(client) => break client,
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(e) => panic!("daemon never came up: {e}"),
        }
    };
    let watch_ipc = tmp.path().join("watch-ipc");
    std::fs::create_dir_all(&watch_ipc).unwrap();
    client_suite(&ipc, watch_ipc.to_str().unwrap()).await;

    // Socket is private to the user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "socket must be 0600");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn protocol_mismatch_is_refused() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path());
    engine.start();
    let socket = tmp.path().join("daemon.sock");
    {
        let engine = engine.clone();
        let socket = socket.clone();
        tokio::spawn(async move {
            let _ = mast_daemon::serve(engine, &socket).await;
        });
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(b"{\"id\":1,\"method\":\"hello\",\"params\":{\"protocolVersion\":999}}\n")
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(&mut stream).read_line(&mut line).await.unwrap();
    assert!(line.contains("protocolMismatch") || line.contains("ProtocolMismatch"), "{line}");
}

/// The gap the protocol version cannot see: same framing, different DTOs.
/// Both a build from another minor and a build too old to announce one at all
/// must be turned away at `hello` — with `versionMismatch`, not with a
/// missing-field error several calls later.
#[tokio::test(flavor = "multi_thread")]
async fn build_version_mismatch_is_refused() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path());
    engine.start();
    let socket = tmp.path().join("daemon.sock");
    {
        let engine = engine.clone();
        let socket = socket.clone();
        tokio::spawn(async move {
            let _ = mast_daemon::serve(engine, &socket).await;
        });
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    async fn handshake(socket: &Path, params: &str) -> String {
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        stream
            .write_all(format!("{{\"id\":1,\"method\":\"hello\",\"params\":{params}}}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).await.unwrap();
        line
    }

    // A build from a different minor: framing agrees, payloads do not.
    let line = handshake(
        &socket,
        &format!("{{\"protocolVersion\":{PROTOCOL_VERSION},\"version\":\"99.99.0\"}}"),
    )
    .await;
    assert!(line.contains("versionMismatch"), "{line}");
    assert!(line.contains("99.99.0"), "the peer's version belongs in the error: {line}");

    // A build predating the versioned handshake announces no version at all.
    // Unknown is not compatible — it is refused, and named as old rather than
    // reported as an empty string.
    let line =
        handshake(&socket, &format!("{{\"protocolVersion\":{PROTOCOL_VERSION}}}")).await;
    assert!(line.contains("versionMismatch"), "{line}");
    assert!(line.contains("older than"), "{line}");

    // Our own build is, necessarily, compatible with itself: the check must
    // not be so strict that the shipped pair cannot talk.
    let line = handshake(
        &socket,
        &format!("{{\"protocolVersion\":{PROTOCOL_VERSION},\"version\":\"{BUILD_VERSION}\"}}"),
    )
    .await;
    assert!(!line.contains("error"), "{line}");
    assert!(line.contains(BUILD_VERSION), "the reply must carry the daemon's build: {line}");
}

/// A patch-level difference is deliberately NOT a mismatch: bug-fix releases
/// never move a wire shape, and refusing them would make every point release
/// a forced lockstep upgrade of both installs.
#[tokio::test(flavor = "multi_thread")]
async fn patch_level_difference_is_accepted() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let tmp = tempfile::tempdir().unwrap();
    let engine = test_engine(tmp.path());
    engine.start();
    let socket = tmp.path().join("daemon.sock");
    {
        let engine = engine.clone();
        let socket = socket.clone();
        tokio::spawn(async move {
            let _ = mast_daemon::serve(engine, &socket).await;
        });
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Same major.minor as this build, an implausible patch component.
    let peer = format!("{}.99", mast_contract::wire_compat_key(BUILD_VERSION));
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(
            format!(
                "{{\"id\":1,\"method\":\"hello\",\"params\":{{\"protocolVersion\":{PROTOCOL_VERSION},\"version\":\"{peer}\"}}}}\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(&mut stream).read_line(&mut line).await.unwrap();
    assert!(!line.contains("error"), "patch drift must be allowed: {line}");
}
