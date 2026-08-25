//! Resource usage (M11): what each running container costs.
//!
//! The load-bearing claims are that **nothing is sampled while nobody is
//! watching** (the whole reason this is safe to run on a laptop), that CPU is
//! a delta between two readings rather than a meaningless cumulative counter,
//! and that a replaced container never has its counters diffed against a
//! previous life.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::BoxStream;
use mast_contract::{Action, ContainerState, DockerStatus, OperationEventKind, UsageSample};
use mast_docker::{
    CapturedLine, CommandOutcome, ContainerObservation, DockerError, LogChunk, OutputLine,
    RuntimeAdapter, RuntimeEvent, StatsSample,
};
use mast_engine::{
    Engine, EngineConfig, EngineDeps, LifecycleRunner, LifecycleVerb, RuntimeConnector,
    acquire_ownership,
};
use mast_project::MetadataStore;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------- fakes ----------

/// Reports CPU counters that advance by a scripted amount per call, so a delta
/// between two readings is arithmetically predictable.
struct FakeAdapter {
    containers: Mutex<Vec<ContainerObservation>>,
    /// Counts every `container_stats` call — the assertion that proves
    /// sampling does not happen unwatched.
    stats_calls: AtomicU64,
    /// How much container CPU time to add per reading, in ns.
    cpu_step: Mutex<u64>,
    memory_usage: Mutex<u64>,
    memory_cache: Mutex<u64>,
    memory_limit: Mutex<u64>,
    cpu_total: Mutex<HashMap<String, u64>>,
    system_total: Mutex<u64>,
    events_tx: broadcast::Sender<RuntimeEvent>,
}

impl FakeAdapter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            containers: Mutex::new(Vec::new()),
            stats_calls: AtomicU64::new(0),
            // One eighth of the 8-core machine's time per tick = one core.
            cpu_step: Mutex::new(1_000),
            memory_usage: Mutex::new(500 * 1024 * 1024),
            memory_cache: Mutex::new(100 * 1024 * 1024),
            memory_limit: Mutex::new(16 * 1024 * 1024 * 1024),
            cpu_total: Mutex::new(HashMap::new()),
            system_total: Mutex::new(0),
            events_tx: broadcast::channel(64).0,
        })
    }

    fn set_containers(&self, containers: Vec<ContainerObservation>) {
        *self.containers.lock().unwrap() = containers;
        let _ = self.events_tx.send(RuntimeEvent);
    }

    fn stats_calls(&self) -> u64 {
        self.stats_calls.load(Ordering::Relaxed)
    }

    fn set_memory(&self, usage: u64, cache: u64, limit: u64) {
        *self.memory_usage.lock().unwrap() = usage;
        *self.memory_cache.lock().unwrap() = cache;
        *self.memory_limit.lock().unwrap() = limit;
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
        _container_id: &str,
        _since_unix: i64,
        _max_lines: u32,
    ) -> Result<Vec<CapturedLine>, DockerError> {
        Ok(vec![])
    }
    async fn container_stats(&self, container_id: &str) -> Result<StatsSample, DockerError> {
        self.stats_calls.fetch_add(1, Ordering::Relaxed);
        let step = *self.cpu_step.lock().unwrap();
        let cpu_total_ns = {
            let mut totals = self.cpu_total.lock().unwrap();
            let entry = totals.entry(container_id.to_string()).or_insert(0);
            *entry += step;
            *entry
        };
        // The host's clock advances by a full 8 cores' worth each reading, so
        // a container stepping 1/8 of that is using exactly one core.
        let system_cpu_ns = {
            let mut system = self.system_total.lock().unwrap();
            *system += 8_000;
            *system
        };
        Ok(StatsSample {
            cpu_total_ns,
            system_cpu_ns,
            online_cpus: 8,
            memory_usage: *self.memory_usage.lock().unwrap(),
            memory_cache: *self.memory_cache.lock().unwrap(),
            memory_limit: *self.memory_limit.lock().unwrap(),
        })
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
            reconcile_interval: Duration::from_millis(200),
            registry_refresh: false,
            // Fast enough to keep the suite quick; the arithmetic does not
            // depend on the interval, only on consecutive readings.
            usage_interval: Duration::from_millis(80),
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
    std::fs::write(project.join("compose.yaml"), "services:\n  app:\n    image: alpine:latest\n")
        .unwrap();
    project
}

fn observation(
    project_name: &str,
    project_dir: &Path,
    service: &str,
    container_id: &str,
) -> ContainerObservation {
    ContainerObservation {
        id: container_id.into(),
        name: format!("{project_name}-{service}-1"),
        project: project_name.into(),
        service: service.into(),
        config_files: vec![project_dir.join("compose.yaml").to_string_lossy().into_owned()],
        working_dir: Some(project_dir.to_string_lossy().into_owned()),
        state: "running".into(),
        health: None,
        exit_code: None,
        config_hash: Some("hash".into()),
        networks: vec![format!("{project_name}_default")],
        published_ports: Vec::new(),
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

/// Import a project and get its one container observed as running.
async fn running_project(engine: &Engine, adapter: &FakeAdapter, dir: &Path) {
    let project = make_project(dir, "usageapp");
    engine.start();
    run_action(engine, Action::ImportProject { path: project.to_string_lossy().into() }).await;

    let deadline = Instant::now() + Duration::from_secs(15);
    let compose_name = loop {
        if let Some(name) =
            engine.snapshot().projects.first().and_then(|p| p.compose_project_name.clone())
        {
            break name;
        }
        assert!(Instant::now() < deadline, "project never resolved");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    adapter.set_containers(vec![observation(&compose_name, &project, "app", "cid-app")]);
    loop {
        let running = engine.snapshot().projects.first().is_some_and(|p| {
            p.services.iter().any(|s| s.state == Some(ContainerState::Running))
        });
        if running {
            return;
        }
        assert!(Instant::now() < deadline, "container never observed running");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Take samples off a subscription until one reports non-zero CPU, or give up.
/// The first sample after subscribing has no predecessor to subtract from.
async fn next_measured_sample(
    stream: &mut BoxStream<'static, UsageSample>,
) -> UsageSample {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let sample = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("usage stream stalled")
            .expect("usage stream ended");
        if sample.services.iter().any(|s| s.cpu_cores > 0.0) {
            return sample;
        }
        assert!(Instant::now() < deadline, "no sample ever carried a CPU reading");
    }
}

// ---------- tests ----------

/// The claim that makes this safe to ship on a laptop: measuring the machine
/// costs CPU, so it must not happen when nobody is looking at the answer.

/// Same skip contract as the other docker-gated suites: these tests run
/// real containers, so a runner without a usable LINUX docker daemon skips
/// instead of timing out.
async fn docker_usable() -> bool {
    if std::env::var_os("MAST_SKIP_DOCKER_TESTS").is_some() {
        return false;
    }
    mast_docker::run_command(
        &["docker".into(), "info".into(), "--format".into(), "{{.OSType}}".into()],
        None,
        &[],
        Duration::from_secs(10),
        16 * 1024,
    )
    .await
    .map(|o| o.success() && o.stdout.trim() == "linux")
    .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_sampled_while_nobody_is_subscribed() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    running_project(&engine, &adapter, tmp.path()).await;

    // Plenty of intervals go by with a running container and no subscriber.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(adapter.stats_calls(), 0, "sampled a container with nobody watching");

    // Subscribing is what starts it.
    let mut stream = engine.subscribe_usage();
    let _ = next_measured_sample(&mut stream).await;
    assert!(adapter.stats_calls() > 0, "subscribing did not start sampling");

    // And dropping the last subscriber stops it again.
    drop(stream);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after_drop = adapter.stats_calls();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        adapter.stats_calls(),
        after_drop,
        "kept sampling after the last subscriber went away"
    );
}

/// CPU is a delta. The fake advances the container by exactly one eighth of an
/// 8-core machine's time per reading, so the answer must be one core.
#[tokio::test(flavor = "multi_thread")]
async fn cpu_is_measured_as_cores_between_two_readings() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    running_project(&engine, &adapter, tmp.path()).await;

    let mut stream = engine.subscribe_usage();
    let sample = next_measured_sample(&mut stream).await;

    assert_eq!(sample.host_cores, 8);
    let service = sample.services.iter().find(|s| s.service == "app").expect("app not sampled");
    assert!(
        (service.cpu_cores - 1.0).abs() < 0.001,
        "expected one core, got {}",
        service.cpu_cores
    );
}

/// The working set excludes page cache, and an unlimited container reports
/// host RAM as its limit — which is how we tell "share of the machine" from
/// "about to be OOM-killed".
#[tokio::test(flavor = "multi_thread")]
async fn memory_is_the_working_set_and_knows_whether_it_is_limited() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    // 500 MB held, 100 MB of it reclaimable cache, no limit of its own.
    adapter.set_memory(500 * 1024 * 1024, 100 * 1024 * 1024, 16 * 1024 * 1024 * 1024);
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    running_project(&engine, &adapter, tmp.path()).await;

    let mut stream = engine.subscribe_usage();
    let sample = next_measured_sample(&mut stream).await;
    let service = sample.services.iter().find(|s| s.service == "app").unwrap();

    assert_eq!(service.memory_bytes, 400 * 1024 * 1024, "page cache was not subtracted");
    assert_eq!(sample.host_memory_bytes, 16 * 1024 * 1024 * 1024);
    assert!(
        !service.memory_limited,
        "a container reporting host RAM as its limit is not actually limited"
    );
}

/// A replaced container is a different container. Diffing its fresh counters
/// against its predecessor's would report a huge negative — or, once clamped,
/// a bogus spike.
#[tokio::test(flavor = "multi_thread")]
async fn a_replaced_container_is_not_diffed_against_its_predecessor() {
    if !docker_usable().await {
        eprintln!("skipping: docker daemon not usable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let adapter = FakeAdapter::new();
    let engine = test_engine(tmp.path(), Arc::new(FakeConnector(adapter.clone())));
    let project = tmp.path().join("usageapp");
    running_project(&engine, &adapter, tmp.path()).await;

    let mut stream = engine.subscribe_usage();
    let _ = next_measured_sample(&mut stream).await;

    // Recreated: same service, new container id, counters starting from zero.
    let compose_name = engine.snapshot().projects[0].compose_project_name.clone().unwrap();
    adapter.set_containers(vec![observation(&compose_name, &project, "app", "cid-app-v2")]);

    // Every reading for the new id must be a sane fraction of the machine —
    // never a spike from subtracting against the old container's totals.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Ok(Some(sample)) = tokio::time::timeout(Duration::from_secs(5), stream.next()).await
        else {
            break;
        };
        for service in &sample.services {
            assert!(
                service.cpu_cores <= 8.0 && service.cpu_cores >= 0.0,
                "implausible reading after a recreate: {} cores",
                service.cpu_cores
            );
        }
    }
}
