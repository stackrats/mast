//! Effect loops: everything with I/O lives here, outside the state-owning
//! core. Inputs are hints, inspection is truth (plan §3): docker events and
//! file changes only schedule a reconcile; the reconcile re-inspects and
//! diffs fresh observations into minimal patches.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use mast_contract::{DiscoveredProject, DockerStatus, PatchEvent, ServiceState};
use mast_docker::{
    BollardAdapter, ContainerObservation, DockerError, RuntimeAdapter, resolve_endpoint,
};
use tokio::sync::mpsc;

use crate::{Engine, RuntimeConnector, derive_status, map_container_state, map_health};

/// Production connector: ADR-0002 endpoint resolution + bollard, verified
/// with a ping before being handed to the engine.
pub struct RealConnector;

#[async_trait::async_trait]
impl RuntimeConnector for RealConnector {
    async fn connect(&self) -> Result<(Arc<dyn RuntimeAdapter>, DockerStatus), DockerError> {
        let endpoint = resolve_endpoint().await?;
        let adapter = BollardAdapter::connect(&endpoint)?;
        adapter.ping().await?;
        let status = DockerStatus {
            available: true,
            context_name: Some(endpoint.context_name.clone()),
            endpoint: Some(endpoint.host.clone()),
            error: None,
        };
        Ok((Arc::new(adapter), status))
    }
}

pub(crate) fn start(engine: Engine) {
    let (hint_tx, hint_rx) = mpsc::channel::<()>(64);
    *engine.inner.hint_tx.lock().unwrap() = Some(hint_tx.clone());
    tokio::spawn(docker_loop(engine.clone()));
    tokio::spawn(file_watcher_loop(engine.clone(), hint_tx));
    tokio::spawn(reconcile_loop(engine, hint_rx));
}

/// Maintain the daemon connection: resolve → connect → stream events (each
/// event is a hint); on stream end or failure, mark unavailable and retry
/// with backoff. Reconnect triggers a full reconcile (resync after outage).
async fn docker_loop(engine: Engine) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match engine.inner.deps.connector.connect().await {
            Ok((adapter, status)) => {
                backoff = Duration::from_secs(1);
                *engine.inner.adapter.lock().unwrap() = Some(adapter.clone());
                engine.update_docker_status(status);
                engine.hint();
                // First contact with the daemon: sweep share tunnels a
                // previous engine left publishing. A tunnel with no live
                // operation is unmanaged and invisible — and still public.
                engine.cleanup_orphan_shares().await;
                match adapter.events().await {
                    Ok(mut events) => {
                        while events.next().await.is_some() {
                            engine.hint();
                        }
                    }
                    Err(e) => tracing::warn!("docker event stream failed: {e}"),
                }
                // Stream ended: connection lost.
                *engine.inner.adapter.lock().unwrap() = None;
                engine.update_docker_status_unavailable("docker connection lost");
            }
            Err(e) => {
                *engine.inner.adapter.lock().unwrap() = None;
                engine.update_docker_status_unavailable(&e.to_string());
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(15));
    }
}

/// Watch watched-directories and imported project roots (non-recursive) for
/// compose-file/.env changes; every debounced burst is one hint. The watcher
/// is rebuilt when the watched path set changes.
async fn file_watcher_loop(engine: Engine, hint_tx: mpsc::Sender<()>) {
    use notify::RecursiveMode;
    let mut watched: Vec<PathBuf> = Vec::new();
    // Kept alive across rebuilds; replaced (dropping the old watcher) when
    // the watched path set changes.
    let mut _debouncer: Option<
        notify_debouncer_full::Debouncer<notify::RecommendedWatcher, notify_debouncer_full::RecommendedCache>,
    > = None;
    loop {
        let desired: Vec<PathBuf> = {
            let st = engine.inner.state.lock().unwrap();
            let mut paths = st.watched_directories.clone();
            paths.extend(st.projects.values().map(|e| e.record.path.clone()));
            paths.sort();
            paths.dedup();
            paths
        };
        if desired != watched {
            let tx = hint_tx.clone();
            match notify_debouncer_full::new_debouncer(
                Duration::from_millis(200),
                None,
                move |result: notify_debouncer_full::DebounceEventResult| {
                    if result.is_ok() {
                        let _ = tx.try_send(());
                    }
                },
            ) {
                Ok(mut new_debouncer) => {
                    let mut ok = true;
                    for path in &desired {
                        if let Err(e) = new_debouncer.watch(path, RecursiveMode::NonRecursive) {
                            tracing::warn!("cannot watch {}: {e}", path.display());
                            ok = false;
                        }
                    }
                    if ok || !desired.is_empty() {
                        _debouncer = Some(new_debouncer);
                        watched = desired;
                    }
                }
                Err(e) => tracing::warn!("file watcher unavailable: {e}"),
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Debounced hint consumer + periodic full reconcile.
async fn reconcile_loop(engine: Engine, mut hint_rx: mpsc::Receiver<()>) {
    reconcile(&engine).await;
    loop {
        tokio::select! {
            received = hint_rx.recv() => {
                if received.is_none() {
                    return;
                }
                // Coalesce the burst.
                tokio::time::sleep(engine.inner.config.hint_debounce).await;
                while hint_rx.try_recv().is_ok() {}
                reconcile(&engine).await;
            }
            _ = tokio::time::sleep(engine.inner.config.reconcile_interval) => {
                reconcile(&engine).await;
            }
        }
    }
}

impl Engine {
    pub(crate) fn update_docker_status(&self, status: DockerStatus) {
        self.with_state(|st, events| {
            if st.docker != status {
                st.docker = status.clone();
                events.push(PatchEvent::DockerStatusChanged { status });
            }
        });
    }

    pub(crate) fn update_docker_status_unavailable(&self, error: &str) {
        self.with_state(|st, events| {
            if st.docker.available || st.docker.error.as_deref() != Some(error) {
                st.docker.available = false;
                st.docker.error = Some(error.to_string());
                events.push(PatchEvent::DockerStatusChanged { status: st.docker.clone() });
            }
        });
    }
}

/// One reconcile pass: discovery scan, per-project resolution refresh,
/// observation, association, diff → patches.
async fn reconcile(engine: &Engine) {
    // ---- discovery (blocking fs) ----
    let directories: Vec<PathBuf> =
        engine.inner.state.lock().unwrap().watched_directories.clone();
    let candidates = tokio::task::spawn_blocking(move || mast_project::scan_directories(&directories))
        .await
        .unwrap_or_default();

    // ---- per-project resolution (blocking fs + offline `config` subprocess) ----
    let project_dirs: Vec<(String, PathBuf)> = {
        let st = engine.inner.state.lock().unwrap();
        st.projects.values().map(|e| (e.record.id.clone(), e.record.path.clone())).collect()
    };
    let process_env = engine.inner.deps.process_env.clone();
    let mut resolutions: HashMap<String, Result<mast_compose::ComposeInvocation, String>> =
        HashMap::new();
    let mut warnings: HashMap<String, Vec<String>> = HashMap::new();
    let mut redactors: HashMap<String, crate::Redactor> = HashMap::new();
    // (probe port, declared host-port forwards, compose port → `.env` key)
    // per project.
    type PortInfo = (Option<u16>, Vec<(String, u16)>, BTreeMap<u16, String>);
    let mut app_ports: HashMap<String, PortInfo> = HashMap::new();
    // (branch, dirty) per project — display-only git chips (M8).
    let mut git_infos: HashMap<String, (Option<String>, Option<bool>)> = HashMap::new();
    // (detected app processes, APP_SERVICE) per project (M8.5).
    type ProcessInfo = (Vec<&'static mast_laravel::processes::ProcessDef>, String);
    let mut process_infos: HashMap<String, ProcessInfo> = HashMap::new();
    // Browsable address from .env, per project.
    let mut app_urls: HashMap<String, Option<String>> = HashMap::new();
    // Sail PHP runtime info (current series + vendored alternatives).
    let mut php_infos: HashMap<String, Option<mast_contract::PhpVersionInfo>> = HashMap::new();
    // Content hash of the compose files at resolution time — the model must
    // refresh on in-place edits, which change no part of the invocation.
    let mut fingerprints: HashMap<String, Option<u64>> = HashMap::new();
    for (id, dir) in &project_dirs {
        let env = process_env.clone();
        let dir = dir.clone();
        let (resolved, fingerprint, project_warnings, redactor, ports, git, procs, app_url, php) =
            tokio::task::spawn_blocking(move || {
                let resolved =
                    mast_compose::resolve_invocation(&dir, &env).map_err(|e| e.to_string());
                let fingerprint = resolved.as_ref().ok().map(|inv| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    for file in &inv.files {
                        file.path.hash(&mut h);
                        std::fs::read(&file.path).unwrap_or_default().hash(&mut h);
                    }
                    h.finish()
                });
                // Which key moves which published port, read from the compose
                // source — the resolved model only carries the numbers.
                let port_keys: BTreeMap<u16, String> = resolved
                    .as_ref()
                    .map(|inv| {
                        inv.files
                            .iter()
                            .filter_map(|f| std::fs::read_to_string(&f.path).ok())
                            .flat_map(|text| mast_compose::published_port_env_keys(&text))
                            .filter(|(_, key)| mast_laravel::is_host_port_key(key))
                            .collect()
                    })
                    .unwrap_or_default();
                let redactor = crate::Redactor::from_env_file(&dir.join(".env"));
                let env = mast_compose::parse_env_file(&dir.join(".env"));
                let app_port = env.get("APP_PORT").and_then(|v| v.parse::<u16>().ok());
                let mut host_ports: Vec<(String, u16)> = env
                    .iter()
                    .filter(|(k, _)| mast_laravel::is_host_port_key(k))
                    .filter_map(|(k, v)| v.parse::<u16>().ok().map(|p| (k.clone(), p)))
                    .collect();
                host_ports.sort();
                let composer = std::fs::read_to_string(dir.join("composer.json")).ok();
                let detected =
                    mast_laravel::processes::detect_processes(composer.as_deref(), &env);
                let app_service = env
                    .get("APP_SERVICE")
                    .cloned()
                    .unwrap_or_else(|| "laravel.test".to_string());
                (
                    resolved,
                    fingerprint,
                    mast_project::project_warnings(&dir),
                    redactor,
                    (app_port, host_ports, port_keys),
                    git_info(&dir),
                    (detected, app_service),
                    mast_laravel::app_url(&env),
                    crate::php::php_info(&dir),
                )
            })
            .await
            .unwrap_or_else(|e| {
                (
                    Err(e.to_string()),
                    None,
                    Vec::new(),
                    crate::Redactor::default(),
                    (None, Vec::new(), BTreeMap::new()),
                    (None, None),
                    (Vec::new(), String::new()),
                    None,
                    None,
                )
            });
        resolutions.insert(id.clone(), resolved);
        fingerprints.insert(id.clone(), fingerprint);
        warnings.insert(id.clone(), project_warnings);
        redactors.insert(id.clone(), redactor);
        app_ports.insert(id.clone(), ports);
        git_infos.insert(id.clone(), git);
        process_infos.insert(id.clone(), procs);
        app_urls.insert(id.clone(), app_url);
        php_infos.insert(id.clone(), php);
    }

    // Refresh resolved models where the invocation OR the file content
    // changed (subprocess). Content matters: a catalog add, a retag, or an
    // external edit rewrites the file without touching the invocation.
    let mut models: HashMap<String, Result<mast_compose::ResolvedModel, String>> = HashMap::new();
    for (id, resolution) in &resolutions {
        if let Ok(invocation) = resolution {
            let fingerprint = fingerprints.get(id).copied().flatten();
            let needs_model = {
                let st = engine.inner.state.lock().unwrap();
                st.projects.get(id).is_none_or(|e| {
                    e.invocation.as_ref() != Some(invocation)
                        || e.model.is_none()
                        || e.compose_fingerprint != fingerprint
                })
            };
            if needs_model {
                models.insert(
                    id.clone(),
                    mast_compose::resolve_model(invocation).await.map_err(|e| e.to_string()),
                );
            }
        }
    }

    // ---- observation ----
    let adapter = engine.inner.adapter.lock().unwrap().clone();
    let observations = match adapter {
        Some(adapter) => match adapter.list_compose_containers().await {
            Ok(list) => Some(list),
            Err(e) => {
                tracing::warn!("container listing failed: {e}");
                None
            }
        },
        None => None,
    };

    // App-process running states (M8.5): one in-container cmdline scan per
    // project that has detected processes and a running app container.
    let mut process_scans: HashMap<String, String> = HashMap::new();
    if let Some(observations) = &observations {
        for (id, dir) in &project_dirs {
            let Some((detected, app_service)) = process_infos.get(id) else { continue };
            if detected.is_empty() {
                continue;
            }
            let dir_str = dir.to_string_lossy();
            let matched: Vec<&ContainerObservation> = observations
                .iter()
                .filter(|o| o.state == "running" && observation_belongs_to(o, &dir_str))
                .collect();
            let container = matched
                .iter()
                .find(|o| o.service == *app_service)
                .or_else(|| matched.iter().find(|o| o.service.contains("app")))
                .or_else(|| if matched.len() == 1 { Some(&matched[0]) } else { None });
            let Some(container) = container else { continue };
            let argv: Vec<String> = [
                "docker",
                "exec",
                container.id.as_str(),
                "sh",
                "-c",
                mast_laravel::processes::scan_script(),
            ]
            .map(String::from)
            .into();
            if let Ok(out) =
                mast_docker::run_command(&argv, None, &[], Duration::from_secs(5), 256 * 1024)
                    .await
                && out.success()
            {
                process_scans.insert(id.clone(), out.stdout);
            }
        }
    }

    // ---- fold everything into state, emitting minimal patches ----
    // The fold also notices services that died since the last pass. It runs
    // under the state lock and so cannot await, so it only *decides*: the
    // reads happen after, off the lock.
    let capture_requests = engine.with_state(|st, events| {
        let mut capture_requests: Vec<crate::captures::CaptureRequest> = Vec::new();
        // A project Mast is currently operating on had its capture taken by
        // the teardown itself, with the container still alive.
        let busy = engine.inner.busy_projects.lock().unwrap().clone();
        let imported: Vec<PathBuf> =
            st.projects.values().map(|e| e.record.path.clone()).collect();
        let discovered: Vec<DiscoveredProject> = candidates
            .iter()
            .filter(|c| !imported.contains(&c.path))
            .map(|c| DiscoveredProject {
                path: c.path.to_string_lossy().into_owned(),
                name: c.name.clone(),
                is_sail: c.is_sail,
            })
            .collect();
        if st.discovered != discovered {
            st.discovered = discovered.clone();
            events.push(PatchEvent::DiscoveryChanged { discovered });
        }

        if !redactors.is_empty() {
            st.redactor_all = crate::Redactor::union(redactors.values());
        }
        let crash_notices = engine.inner.crash_notices.lock().unwrap().clone();
        for entry in st.projects.values_mut() {
            let id = entry.record.id.clone();
            if let Some(redactor) = redactors.get(&id) {
                entry.redactor = redactor.clone();
            }
            if let Some((app_port, _, _)) = app_ports.get(&id) {
                entry.app_port = *app_port;
            }
            match resolutions.get(&id) {
                Some(Ok(invocation)) => {
                    if entry.invocation.as_ref() != Some(invocation) {
                        entry.invocation = Some(invocation.clone());
                    }
                    match models.get(&id) {
                        Some(Ok(model)) => {
                            entry.model = Some(model.clone());
                            entry.compose_fingerprint =
                                fingerprints.get(&id).copied().flatten();
                            entry.summary.resolution_error = None;
                        }
                        Some(Err(e)) => {
                            entry.summary.resolution_error = Some(entry.redactor.redact(e));
                        }
                        None => entry.summary.resolution_error = None,
                    }
                }
                Some(Err(e)) => {
                    entry.invocation = None;
                    entry.model = None;
                    entry.summary.resolution_error = Some(entry.redactor.redact(e));
                }
                None => {}
            }

            // Ports must be merged after the model update above: `.env` gives
            // the actionable key names, the resolved model gives the ports
            // compose actually publishes.
            if let Some((_, env_ports, port_keys)) = app_ports.get(&id) {
                entry.host_ports = merge_host_ports(env_ports, port_keys, entry.model.as_ref());
            }

            let mut summary = entry.summary.clone();
            summary.compose_project_name =
                entry.invocation.as_ref().map(|i| i.project_name.clone());
            summary.is_sail = entry
                .invocation
                .as_ref()
                .map(|i| i.is_sail())
                .unwrap_or(entry.record.is_sail);
            summary.warnings = warnings.get(&id).cloned().unwrap_or_default();
            if entry.invocation.as_ref().is_some_and(|i| i.both_base_families) {
                summary.warnings.push(
                    "Both compose.yaml and docker-compose.yml families exist — compose picks \
                     compose.yaml and only warns on stderr."
                        .to_string(),
                );
            }
            if let Some(notice) = crash_notices.get(&id) {
                summary.warnings.push(notice.clone());
            }
            if let Some((branch, dirty)) = git_infos.get(&id) {
                summary.git_branch = branch.clone();
                summary.git_dirty = *dirty;
            }
            if let Some(url) = app_urls.get(&id) {
                summary.app_url = url.clone();
            }
            if let Some(php) = php_infos.get(&id) {
                summary.php = php.clone();
            }
            if let Some((detected, _)) = process_infos.get(&id) {
                summary.processes = detected
                    .iter()
                    .map(|def| mast_contract::ProcessState {
                        id: def.id.to_string(),
                        title: def.title.to_string(),
                        running: process_scans
                            .get(&id)
                            .is_some_and(|scan| {
                                mast_laravel::processes::scan_shows(scan, def.pattern)
                            }),
                    })
                    .collect();
            }

            if let Some(observations) = &observations {
                let project_dir = entry.record.path.to_string_lossy().into_owned();
                let matched: Vec<&ContainerObservation> = observations
                    .iter()
                    .filter(|o| {
                        summary.compose_project_name.as_deref() == Some(o.project.as_str())
                            && observation_belongs_to(o, &project_dir)
                    })
                    .collect();
                let services = build_services(entry.model.as_ref(), &matched);
                if !busy.contains(&id) {
                    collect_deaths(
                        &id,
                        &summary.name,
                        &entry.summary.services,
                        &services,
                        &matched,
                        &mut capture_requests,
                    );
                }
                summary.services = services;
                summary.status = derive_status(&summary.services);
            }

            if entry.summary != summary {
                entry.summary = summary.clone();
                events.push(PatchEvent::ProjectUpdated { project: summary });
            }
        }

        // Workspace statuses derive from members; emit when they changed.
        if !events.is_empty() && !st.workspaces.is_empty() {
            events.push(PatchEvent::WorkspacesChanged {
                workspaces: crate::workspace_summaries(st),
            });
        }
        capture_requests
    });
    // Off the lock: read each dead container's tail before anything removes
    // it. Detached, because a reconcile must not wait on docker logs.
    //
    // Only the instance that owns mutation writes them. Observation converges
    // across instances (plan §1), so a second Mast reconciles the same deaths
    // and would file the same captures into the same shared database.
    if !engine.read_only() {
        engine.spawn_captures(capture_requests);
    }
}

/// Diff one project's services across a reconcile and record which deaths are
/// worth a post-mortem.
///
/// A changed container id means the container was replaced without Mast asking
/// — its log went with it, so there is nothing to read and nothing to record.
///
/// A service with no previous observation is the interesting case rather than
/// the boring one: it is what a container that died while Mast was closed
/// looks like on the first reconcile after it opens. Treating "unseen" as
/// "was alive" makes that a death like any other. It cannot spam, because the
/// capture window bounds what can be read — a container that died last week
/// yields no lines, and a capture with no lines is not recorded.
fn collect_deaths(
    project_id: &str,
    project_name: &str,
    previous: &[ServiceState],
    current: &[ServiceState],
    observed: &[&ContainerObservation],
    out: &mut Vec<crate::captures::CaptureRequest>,
) {
    use mast_contract::{ContainerState, ServiceHealth};

    for new in current {
        let Some(container_id) = new.container_id.clone() else {
            continue;
        };
        // Only a previous observation *of this same container* says anything
        // about how it got here. A service declared by the model but not yet
        // observed carries `container_id: None`, which is a first sighting,
        // not a replacement — and a different id is a replacement, whose log
        // is already gone.
        let old = previous
            .iter()
            .find(|s| s.name == new.name)
            .filter(|s| s.container_id.is_some());
        if let Some(old) = old
            && old.container_id.as_deref() != Some(container_id.as_str())
        {
            continue;
        }
        let exit_code = observed.iter().find(|o| o.id == container_id).and_then(|o| o.exit_code);
        // Never seen alive: assume it was, so whatever state it is in now
        // reads as the transition into it.
        let was_running = old.map(|o| o.state == Some(ContainerState::Running)).unwrap_or(true);
        let previous_health = old.map(|o| o.health).unwrap_or(ServiceHealth::Unknown);
        if let Some(reason) = crate::captures::death_reason(
            was_running,
            new.state.as_ref(),
            previous_health,
            new.health,
            exit_code,
        ) {
            out.push(crate::captures::CaptureRequest {
                project: mast_contract::ProjectId(project_id.to_string()),
                project_name: project_name.to_string(),
                service: new.name.clone(),
                container_id,
                reason,
            });
        }
    }
}

/// Branch + dirty via shell git (plan risk §4: gix stays display-only behind
/// a fallback; shell git is that fallback and already proven by snapshots).
/// One subprocess per project per reconcile; (None, None) outside a repo.
fn git_info(dir: &std::path::Path) -> (Option<String>, Option<bool>) {
    let output = std::process::Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(["status", "--porcelain", "--branch"])
        .output();
    let Ok(output) = output else { return (None, None) };
    if !output.status.success() {
        return (None, None);
    }
    parse_git_status(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `git status --porcelain --branch` output into (branch, dirty).
fn parse_git_status(text: &str) -> (Option<String>, Option<bool>) {
    let mut lines = text.lines();
    let branch = lines.next().and_then(|header| {
        let header = header.strip_prefix("## ")?;
        if header.starts_with("HEAD") {
            return Some("detached".to_string());
        }
        // "main...origin/main [ahead 1]" → "main"; "No commits yet on main".
        let header = header.strip_prefix("No commits yet on ").unwrap_or(header);
        Some(header.split("...").next().unwrap_or(header).to_string())
    });
    let dirty = Some(lines.next().is_some());
    (branch, dirty)
}

/// Host ports this project publishes, labelled for the collision warnings and
/// for the remapper, which can only move a port it can name a key for.
///
/// The `.env` scan alone is not enough: Sail publishes its Vite port as
/// `'${VITE_PORT:-5173}:${VITE_PORT:-5173}'`, so two projects that never set
/// `VITE_PORT` both claim 5173 with nothing in either `.env` to compare. The
/// resolved model carries the post-interpolation ports, so it is the source of
/// truth; the label comes from whichever key governs the port — `.env` first
/// (it is definitive), then the compose default (`${VITE_PORT:-5173}` names
/// the key even when `.env` is silent), and only then the service name, which
/// says where the port comes from but not how to move it.
fn merge_host_ports(
    env_ports: &[(String, u16)],
    port_keys: &BTreeMap<u16, String>,
    model: Option<&mast_compose::ResolvedModel>,
) -> Vec<(String, u16)> {
    let mut ports = env_ports.to_vec();
    if let Some(model) = model {
        for service in &model.services {
            for port in &service.published_ports {
                if !ports.iter().any(|(_, known)| known == port) {
                    let label =
                        port_keys.get(port).cloned().unwrap_or_else(|| service.name.clone());
                    ports.push((label, *port));
                }
            }
        }
    }
    ports.sort();
    ports.dedup();
    ports
}

/// ADR-0002/0001 association cross-check: same project name is not enough —
/// the container must actually come from this directory (guards against
/// same-name projects in different directories).
fn observation_belongs_to(observation: &ContainerObservation, project_dir: &str) -> bool {
    observation.working_dir.as_deref() == Some(project_dir)
        || observation.config_files.iter().any(|f| f.starts_with(project_dir))
}

/// Union of declared services (resolved model) and observed containers.
fn build_services(
    model: Option<&mast_compose::ResolvedModel>,
    observed: &[&ContainerObservation],
) -> Vec<ServiceState> {
    let mut services: Vec<ServiceState> = Vec::new();
    if let Some(model) = model {
        for declared in &model.services {
            services.push(ServiceState {
                name: declared.name.clone(),
                container_id: None,
                state: None,
                health: mast_contract::ServiceHealth::Unknown,
            });
        }
    }
    for container in observed {
        let state = Some(map_container_state(&container.state));
        let health = map_health(container.health.as_deref());
        if let Some(existing) = services.iter_mut().find(|s| s.name == container.service) {
            existing.container_id = Some(container.id.clone());
            existing.state = state;
            existing.health = health;
        } else {
            services.push(ServiceState {
                name: container.service.clone(),
                container_id: Some(container.id.clone()),
                state,
                health,
            });
        }
    }
    services.sort_by(|a, b| a.name.cmp(&b.name));
    services
}

#[cfg(test)]
mod tests {
    use super::{merge_host_ports, parse_git_status};

    fn model(services: &[(&str, &[u16])]) -> mast_compose::ResolvedModel {
        mast_compose::ResolvedModel {
            name: "demo".into(),
            services: services
                .iter()
                .map(|(name, ports)| mast_compose::ResolvedService {
                    name: (*name).into(),
                    image: None,
                    aliases: Vec::new(),
                    published_ports: ports.to_vec(),
                    volumes: Vec::new(),
                })
                .collect(),
            external_networks: Vec::new(),
        }
    }

    fn keys(pairs: &[(u16, &str)]) -> std::collections::BTreeMap<u16, String> {
        pairs.iter().map(|(port, key)| (*port, (*key).to_string())).collect()
    }

    #[test]
    fn host_ports_include_compose_defaults_absent_from_env() {
        // Sail's `'${VITE_PORT:-5173}:${VITE_PORT:-5173}'` with no VITE_PORT
        // in .env — the case that let two workspace members collide silently.
        // The compose default still names the key that moves it.
        let env = vec![("APP_PORT".to_string(), 8081)];
        let merged = merge_host_ports(
            &env,
            &keys(&[(80, "APP_PORT"), (5173, "VITE_PORT")]),
            Some(&model(&[("laravel.test", &[8081, 5173])])),
        );
        assert_eq!(merged, vec![("APP_PORT".into(), 8081), ("VITE_PORT".into(), 5173)]);
    }

    #[test]
    fn a_port_no_key_governs_is_labelled_by_its_service() {
        // A hand-written `- '9000:9000'`: nothing in `.env` moves it.
        let merged =
            merge_host_ports(&[], &keys(&[(80, "APP_PORT")]), Some(&model(&[("minio", &[9000])])));
        assert_eq!(merged, vec![("minio".into(), 9000)]);
    }

    #[test]
    fn env_key_labels_win_over_service_names() {
        let env = vec![("FORWARD_DB_PORT".to_string(), 33061)];
        let merged = merge_host_ports(&env, &keys(&[]), Some(&model(&[("mariadb", &[33061])])));
        assert_eq!(merged, vec![("FORWARD_DB_PORT".into(), 33061)]);
    }

    #[test]
    fn host_ports_survive_a_missing_model() {
        let env = vec![("APP_PORT".to_string(), 8081)];
        assert_eq!(merge_host_ports(&env, &keys(&[]), None), env);
    }

    #[test]
    fn git_status_parsing_covers_branch_shapes() {
        assert_eq!(
            parse_git_status("## main...origin/main [ahead 1]\n"),
            (Some("main".into()), Some(false))
        );
        assert_eq!(parse_git_status("## feature/x\n M src/a.rs\n"), (
            Some("feature/x".into()),
            Some(true)
        ));
        assert_eq!(
            parse_git_status("## No commits yet on main\n?? new.txt\n"),
            (Some("main".into()), Some(true))
        );
        assert_eq!(
            parse_git_status("## HEAD (no branch)\n"),
            (Some("detached".into()), Some(false))
        );
        assert_eq!(parse_git_status(""), (None, Some(false)));
    }
}
