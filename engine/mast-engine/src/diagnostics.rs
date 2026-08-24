//! Diagnostics wiring (plan §8): the engine gathers `DiagCtx` facts (docker
//! probes, filesystem inspection — all effect-context work), hands them to
//! the pure check set in `mast-diagnostics`, and applies repairs through the
//! same transactional machinery every other mutation uses. Every run and
//! every applied repair lands in the rusqlite history db.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mast_contract::{
    DiagSeverity, DiagnosticFinding, DiagnosticReport, DiagnosticRunSummary, DiagnosticsHistory,
    ErrorInfo, FileEditPreview, ProjectId, RepairAuditEntry, RepairOffer, RepairPlan, RepairRisk,
};
use mast_diagnostics::{
    DiagCtx, DiagnosticsDb, Finding, ProjectFacts, RepairSpec, RiskTier, Severity, SocketFacts,
    SystemFacts, REPAIR_COMPOSER_INSTALL, REPAIR_COPY_ENV_EXAMPLE, REPAIR_CREATE_NETWORK,
    REPAIR_CHOWN_STORAGE, REPAIR_CONFIG_CLEAR, REPAIR_DB_RECONCILE, REPAIR_DB_RECREATE,
    REPAIR_DOCKER_GROUP, REPAIR_GENERATE_APP_KEY, REPAIR_HOSTS_ENTRY, REPAIR_NODE_INSTALL,
    REPAIR_ARTISAN_MIGRATE, REPAIR_DISCONNECT_STALE, REPAIR_FIX_APP_URL,
    REPAIR_INSTALL_CERTUTIL, REPAIR_REASSIGN_PORTS, REPAIR_RECREATE_SERVICE,
    REPAIR_TRUST_PROXY_CA,
    REPAIR_ADD_HOST_GATEWAY, REPAIR_MIGRATE_MAILPIT, REPAIR_NORMALIZE_ENV_EOL, REPAIR_REMOVE_HOT,
    REPAIR_SAIL_INSTALL, REPAIR_SET_PROJECT_NAME, REPAIR_SET_WWWUSER, REPAIR_STORAGE_LINK,
};
use mast_docker::run_command;

use crate::{Engine, OperationEventKind, OperationId, workspace_summaries};

pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const PROBE_CAP: usize = 64 * 1024;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn severity_to_contract(s: Severity) -> DiagSeverity {
    match s {
        Severity::Info => DiagSeverity::Info,
        Severity::Warning => DiagSeverity::Warning,
        Severity::Error => DiagSeverity::Error,
    }
}

fn risk_to_contract(r: RiskTier) -> RepairRisk {
    match r {
        RiskTier::Safe => RepairRisk::Safe,
        RiskTier::Caution => RepairRisk::Caution,
        RiskTier::HighRisk => RepairRisk::HighRisk,
    }
}

pub(crate) fn offer_to_contract(spec: RepairSpec) -> RepairOffer {
    RepairOffer {
        id: spec.id.to_string(),
        title: spec.title,
        risk: risk_to_contract(spec.risk),
        description: spec.description,
        arg: spec.arg,
    }
}

/// The official Composer image. It ships PHP itself, so Mast never needs PHP
/// on the host, and it tracks current PHP (8.5 today) — unlike the
/// `laravelsail/phpXX-composer` images the docs used to point at, which are no
/// longer documented and stop at 8.4.
pub(crate) const COMPOSER_IMAGE: &str = "composer:latest";

/// `docker run` prefix for [`COMPOSER_IMAGE`], mounting `dir` as the workdir.
///
/// `entrypoint` overrides the image's own (which is `composer`) — pass `php`
/// to run Artisan. `COMPOSER_HOME` is redirected because the default cache
/// path is not writable once `-u` drops us to the host user.
fn composer_run(dir: &Path, uid: u32, gid: u32, entrypoint: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = ["docker", "run", "--rm", "-u"].map(String::from).into();
    argv.push(format!("{uid}:{gid}"));
    argv.extend(["-e".into(), "COMPOSER_HOME=/tmp".into(), "-v".into()]);
    argv.push(format!("{}:/var/www/html", dir.display()));
    argv.extend(["-w".into(), "/var/www/html".into()]);
    if let Some(entrypoint) = entrypoint {
        argv.extend(["--entrypoint".into(), entrypoint.into()]);
    }
    argv.push(COMPOSER_IMAGE.into());
    argv
}

/// Containerized `composer install`.
pub(crate) fn composer_install_argv(dir: &Path, uid: u32, gid: u32) -> Vec<String> {
    let mut argv = composer_run(dir, uid, gid, None);
    argv.extend(["install", "--ignore-platform-reqs"].map(String::from));
    argv
}

/// Containerized `composer require laravel/sail --dev`.
pub(crate) fn sail_require_argv(dir: &Path, uid: u32, gid: u32) -> Vec<String> {
    let mut argv = composer_run(dir, uid, gid, None);
    argv.extend(["require", "laravel/sail", "--dev", "--ignore-platform-reqs"].map(String::from));
    argv
}

/// Containerized `php artisan sail:install --with=…` (non-interactive).
pub(crate) fn sail_install_argv(dir: &Path, uid: u32, gid: u32) -> Vec<String> {
    let mut argv = composer_run(dir, uid, gid, Some("php"));
    argv.extend(
        ["artisan", "sail:install", "--with=mysql,redis,mailpit", "--no-interaction"]
            .map(String::from),
    );
    argv
}

/// Node installs run *inside* the app container rather than in a throwaway
/// image the way composer's do. Two reasons: the container already carries
/// npm, pnpm, yarn and bun (Sail's runtimes install them from PHP 8.2 on), and
/// native modules have to be compiled against the Node that will load them.
/// The cost is that the stack must be up — unlike `composer install`, which is
/// what makes a vendor-less clone runnable in the first place.
pub(crate) fn node_install_argv(
    invocation: &mast_compose::ComposeInvocation,
    dir: &Path,
    manager: mast_project::PackageManager,
    frozen: bool,
) -> Vec<String> {
    let tail = manager.install_argv(frozen);
    match &invocation.runner {
        // Terminal parity: the same line the developer would type.
        mast_compose::Runner::Sail { script } => {
            let mut argv = vec![script.to_string_lossy().into_owned()];
            argv.extend(tail);
            argv
        }
        mast_compose::Runner::DockerCompose => crate::project_ops::compose_exec_argv(
            invocation,
            &crate::project_ops::app_service_of(dir),
            &tail,
        ),
    }
}

#[cfg(unix)]
pub(crate) fn uid_gid() -> (u32, u32) {
    // SAFETY: getuid/getgid are always safe to call.
    unsafe { (libc::getuid(), libc::getgid()) }
}

/// Windows adapter TODO: WWWUSER parity is a unix-permissions concern.
#[cfg(not(unix))]
pub(crate) fn uid_gid() -> (u32, u32) {
    (0, 0)
}

fn unix_socket_path(endpoint: &str) -> Option<&str> {
    endpoint.strip_prefix("unix://")
}

fn socket_facts(path: &str) -> SocketFacts {
    let exists = Path::new(path).exists();
    #[cfg(unix)]
    let writable = exists && {
        let c_path = std::ffi::CString::new(path).ok();
        // SAFETY: access() with a valid NUL-terminated path is safe.
        c_path.is_some_and(|p| unsafe { libc::access(p.as_ptr(), libc::W_OK) } == 0)
    };
    #[cfg(not(unix))]
    let writable = exists;
    SocketFacts { path: path.to_string(), exists, writable }
}

#[cfg(unix)]
fn free_bytes(path: &str) -> Option<u64> {
    let c_path = std::ffi::CString::new(path).ok()?;
    // SAFETY: statvfs with a valid path pointer and zeroed out-struct.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            Some(stat.f_bavail as u64 * stat.f_frsize as u64)
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
fn free_bytes(_path: &str) -> Option<u64> {
    None
}

struct ProjectSeed {
    id: String,
    name: String,
    path: PathBuf,
    resolution_error: Option<String>,
    /// (service name, aliases) from the resolved model.
    services: Vec<(String, Vec<String>)>,
    /// The compose project name comes only from the directory basename —
    /// nothing pins it (-p, env, .env, name:).
    name_from_dir: bool,
    host_ports: Vec<(String, u16)>,
    external_networks: Vec<String>,
    running: bool,
}

/// Filesystem half of a project's facts — runs in `spawn_blocking`. The
/// second value is the database the async half should probe, when there is
/// one (`facts.db` stays `None` until that probe fills it).
/// `APP_URL` pins a port the project does not publish while `APP_PORT`
/// names another — the Browser button opens a refused connection. A URL
/// port that *is* published (a proxy or second service, say) is a deliberate
/// arrangement, not staleness.
fn app_url_mismatch(
    env: &std::collections::HashMap<String, String>,
    host_ports: &[(String, u16)],
) -> Option<(u16, u16)> {
    let url_port = env.get("APP_URL").and_then(|v| mast_laravel::explicit_port(v))?;
    let app_port = env.get("APP_PORT").and_then(|v| v.trim().parse::<u16>().ok())?;
    if url_port == app_port || host_ports.iter().any(|(_, port)| *port == url_port) {
        return None;
    }
    Some((url_port, app_port))
}

/// The edit `fix-app-url` will make — recomputed fresh from the file, since
/// the report may be stale by the time the user consents. `None` when
/// APP_URL and APP_PORT already agree (or either says nothing).
fn app_url_rewrite(file: &mast_laravel::EnvFile) -> Option<String> {
    let url = file.get("APP_URL")?.value.clone();
    let from = mast_laravel::explicit_port(&url)?;
    let to = file.get("APP_PORT")?.value.trim().parse::<u16>().ok()?;
    if from == to {
        return None;
    }
    mast_laravel::rewrite_explicit_port(&url, from, to)
}

fn inspect_project(seed: ProjectSeed) -> (ProjectFacts, Option<crate::db_repair::DbProbeTarget>) {
    let env_path = seed.path.join(".env");
    let env_present = env_path.is_file();
    let env = mast_compose::parse_env_file(&env_path);
    let service_names: Vec<String> = seed
        .services
        .iter()
        .flat_map(|(name, aliases)| std::iter::once(name.clone()).chain(aliases.iter().cloned()))
        .collect();
    let mut db_target = None;
    let pairs: Vec<(String, String)> = if env_present {
        let src = std::fs::read_to_string(&env_path).unwrap_or_default();
        mast_laravel::EnvFile::parse(&src)
            .entries()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect()
    } else {
        Vec::new()
    };
    let env_error_count = if env_present {
        db_target = crate::db_repair::resolve_db_target(&pairs, &seed.services);
        mast_laravel::validate(&pairs, &service_names)
            .iter()
            .filter(|f| f.severity == mast_laravel::Severity::Error)
            .count()
    } else {
        0
    };
    let sail_flavored = mast_project::is_sail_flavored(&seed.path);
    let node = mast_project::inspect_node_project(&seed.path);
    let is_laravel = std::fs::read_to_string(seed.path.join("composer.json"))
        .map(|c| c.contains("laravel/framework"))
        .unwrap_or(false);
    let url_mismatch = app_url_mismatch(&env, &seed.host_ports);
    let facts = ProjectFacts {
        sail_flavored,
        is_laravel,
        has_compose: mast_project::has_compose_file(&seed.path),
        has_vendor: seed.path.join("vendor").is_dir(),
        package_manager: node.as_ref().map(|n| n.manager.as_str().to_string()),
        node_lockfile: node.as_ref().is_some_and(|n| n.frozen),
        has_node_modules: node.as_ref().is_some_and(|n| n.has_node_modules),
        conflicting_lockfiles: node
            .as_ref()
            .map(|n| n.conflicting_lockfiles.clone())
            .unwrap_or_default(),
        env_present,
        env_example_present: seed.path.join(".env.example").is_file(),
        app_key_empty: env_present
            && env.get("APP_KEY").is_none_or(|v| v.trim().is_empty()),
        storage_link_missing: seed.path.join("storage/app/public").is_dir()
            && seed.path.join("public").is_dir()
            // symlink_metadata: a dangling link still counts as present —
            // replacing it is not this repair's call.
            && std::fs::symlink_metadata(seed.path.join("public/storage")).is_err(),
        wwwuser: env.get("WWWUSER").cloned(),
        wwwgroup: env.get("WWWGROUP").cloned(),
        env_error_count,
        id: seed.id,
        name: seed.name,
        host_ports: seed.host_ports,
        running: seed.running,
        external_networks: seed.external_networks,
        resolution_error: seed.resolution_error,
        db: None,
        db_versions: Vec::new(),
        app_url_mismatch: url_mismatch,
        config_cached: seed.path.join("bootstrap/cache/config.php").is_file(),
        dotted_services: seed
            .services
            .iter()
            .filter(|(name, _)| name.contains('.'))
            .map(|(name, _)| name.clone())
            .collect(),
        sail_builds: crate::php::sail_build_facts(&seed.path),
        available_runtimes: crate::php::available_runtimes(&seed.path),
        foreign_owned: if is_laravel {
            foreign_owned_scan(&seed.path, uid_gid().0)
        } else {
            None
        },
        drifted_services: Vec::new(),
        detached_services: Vec::new(),
        compose_name: None, // filled from the resolved model in gather
        dir_basename: seed
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        name_from_dir: seed.name_from_dir,
        php_modules: None, // filled by the gather probe
        cli_server_workers: env
            .get("PHP_CLI_SERVER_WORKERS")
            .is_some_and(|v| !v.trim().is_empty()),
        redis_client: env.get("REDIS_CLIENT").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
        app_service: env
            .get("APP_SERVICE")
            .cloned()
            .unwrap_or_else(|| "laravel.test".to_string()),
        app_reachability: None, // filled by the gather probe
        vite_hot: std::fs::read_to_string(seed.path.join("public/hot"))
            .ok()
            .and_then(|contents| mast_laravel::vite::parse_hot_file(&contents))
            .map(|hot| mast_diagnostics::ViteHotFacts {
                dev_server_listening: dev_server_listening(&hot),
                url: hot.url,
            }),
        env_crlf: env_present
            && std::fs::read(&env_path)
                .map(|bytes| bytes.windows(2).any(|w| w == b"\r\n"))
                .unwrap_or(false),
        xdebug: if sail_flavored { xdebug_facts(&seed.path, &pairs, &env) } else { None },
        service_images: Vec::new(), // filled from the resolved model in gather
    };
    (facts, db_target)
}

/// The Xdebug doctor's per-project facts (no docker involved; the
/// extension probe is the async half's job).
fn xdebug_facts(
    dir: &Path,
    pairs: &[(String, String)],
    env: &std::collections::HashMap<String, String>,
) -> Option<mast_diagnostics::XdebugFacts> {
    let xdebug = mast_laravel::xdebug::xdebug_env(pairs)?;
    let mut compose_passes_mode = false;
    let mut compose_has_host_gateway = false;
    for name in [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
        "compose.override.yaml",
        "compose.override.yml",
        "docker-compose.override.yaml",
        "docker-compose.override.yml",
    ] {
        if let Ok(source) = std::fs::read_to_string(dir.join(name)) {
            compose_passes_mode |= source.contains("XDEBUG_MODE");
            compose_has_host_gateway |= source.contains("host-gateway");
        }
    }
    // The IDE's listener lives on THIS machine whatever client_host says —
    // that variable is how the container finds us, not where the IDE binds.
    let ide_listening = xdebug.wants_debug().then(|| {
        use std::net::{SocketAddr, TcpStream};
        ["127.0.0.1", "[::1]"].iter().any(|host| {
            format!("{host}:{}", xdebug.client_port)
                .parse::<SocketAddr>()
                .is_ok_and(|addr| {
                    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
                })
        })
    });
    Some(mast_diagnostics::XdebugFacts {
        app_service: env
            .get("APP_SERVICE")
            .cloned()
            .unwrap_or_else(|| "laravel.test".to_string()),
        env: xdebug,
        compose_passes_mode,
        compose_has_host_gateway,
        ide_listening,
        extension_loaded: None,
    })
}

/// Bounded ownership scan over the paths Laravel must write (`storage/`,
/// `bootstrap/cache`): anything owned by a uid other than the user's is the
/// #81 permission trap in its cured-too-late form. Depth- and entry-capped so
/// a giant storage/ cannot stall gathering; a truncated count still proves
/// the problem exists.
#[cfg(unix)]
fn foreign_owned_scan(dir: &Path, uid: u32) -> Option<(usize, String, u32)> {
    use std::os::unix::fs::MetadataExt;
    if uid == 0 {
        return None; // root owns everything it looks at — the scan is meaningless
    }
    let mut count = 0usize;
    let mut example: Option<(String, u32)> = None;
    let mut budget = 2000usize;
    let mut note = |path: &Path, owner: u32| {
        count += 1;
        if example.is_none() {
            let rel = path.strip_prefix(dir).unwrap_or(path);
            example = Some((rel.display().to_string(), owner));
        }
    };
    let mut stack: Vec<(PathBuf, usize)> = Vec::new();
    for root in ["storage", "bootstrap/cache"] {
        let path = dir.join(root);
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.uid() != uid {
            note(&path, meta.uid());
        }
        if meta.is_dir() {
            stack.push((path, 0));
        }
    }
    while let Some((path, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else { continue };
        for entry in entries.flatten() {
            if budget == 0 {
                return example.map(|(path, owner)| (count, path, owner));
            }
            budget -= 1;
            let child = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&child) else { continue };
            if meta.uid() != uid {
                note(&child, meta.uid());
            }
            if meta.is_dir() && depth < 4 {
                stack.push((child, depth + 1));
            }
        }
    }
    example.map(|(path, owner)| (count, path, owner))
}

/// Windows adapter TODO: unix ownership semantics do not apply.
#[cfg(not(unix))]
fn foreign_owned_scan(_dir: &Path, _uid: u32) -> Option<(usize, String, u32)> {
    None
}

/// Current per-service config hashes (`config --hash "*"`), through the
/// read-only runner rule: `SAIL_SKIP_CHECKS=1 sail` for Sail projects, bare
/// compose otherwise. Compared against running containers'
/// `com.docker.compose.config-hash` labels — ADR-0001's drift signal.
async fn config_hashes(
    invocation: &mast_compose::ComposeInvocation,
) -> Option<std::collections::HashMap<String, String>> {
    let (argv, env) =
        crate::db_repair::scoped_compose_argv(invocation, &["config", "--hash", "*"]);
    let out = run_command(&argv, Some(&invocation.project_dir), &env, PROBE_TIMEOUT, PROBE_CAP)
        .await
        .ok()
        .filter(|o| o.success())?;
    Some(
        out.stdout
            .lines()
            .filter_map(|line| {
                let mut cols = line.split_whitespace();
                Some((cols.next()?.to_string(), cols.next()?.to_string()))
            })
            .collect(),
    )
}

/// Does anything answer at the hot file's address? `None` when the host is
/// not loopback — a remote dev server is not ours to judge from here.
pub(crate) fn dev_server_listening(hot: &mast_laravel::vite::HotFile) -> Option<bool> {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    if !hot.loopback {
        return None;
    }
    let host = hot.host.trim_start_matches('[').trim_end_matches(']');
    // Wildcard binds answer on the loopback address.
    let host = if host == "0.0.0.0" || host == "::" { "127.0.0.1" } else { host };
    let addrs: Vec<SocketAddr> = (host, hot.port).to_socket_addrs().ok()?.collect();
    Some(
        addrs
            .iter()
            .any(|addr| TcpStream::connect_timeout(addr, Duration::from_millis(250)).is_ok()),
    )
}

/// The containerized chown for [`REPAIR_CHOWN_STORAGE`]: mounts the project
/// and touches ONLY the paths Laravel must own. `None` when neither target
/// exists.
fn chown_storage_argv(dir: &Path, uid: u32, gid: u32) -> Option<Vec<String>> {
    let targets: Vec<String> = ["storage", "bootstrap/cache"]
        .iter()
        .filter(|rel| dir.join(rel).is_dir())
        .map(|rel| format!("/mast-fix/{rel}"))
        .collect();
    if targets.is_empty() {
        return None;
    }
    let mut argv: Vec<String> =
        ["docker", "run", "--rm", "-v"].map(String::from).into();
    argv.push(format!("{}:/mast-fix", dir.display()));
    argv.extend(["alpine:latest".into(), "chown".into(), "-R".into()]);
    argv.push(format!("{uid}:{gid}"));
    argv.extend(targets);
    Some(argv)
}

impl Engine {
    /// Gather everything the checks read. `scope` narrows the gather to one
    /// project id: only its facts and probes, and no workspace-graph issues
    /// (those are not any single project's).
    async fn gather_diag_ctx(&self, scope: Option<&str>) -> DiagCtx {
        let (docker, seeds, invocations, db_metas, publishing, workspace_issues, docker_host_env) = {
            let st = self.inner.state.lock().unwrap();
            let seeds: Vec<ProjectSeed> = st
                .projects
                .values()
                .filter(|e| scope.is_none_or(|id| e.record.id == id))
                .map(|e| ProjectSeed {
                    id: e.record.id.clone(),
                    name: e.summary.name.clone(),
                    path: e.record.path.clone(),
                    resolution_error: e.summary.resolution_error.clone(),
                    services: e
                        .model
                        .as_ref()
                        .map(|m| {
                            m.services
                                .iter()
                                .map(|s| (s.name.clone(), s.aliases.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    name_from_dir: e
                        .invocation
                        .as_ref()
                        .is_none_or(|i| i.name_source == mast_compose::NameSource::DirBasename),
                    host_ports: e.host_ports.clone(),
                    external_networks: e
                        .model
                        .as_ref()
                        .map(|m| m.external_networks.clone())
                        .unwrap_or_default(),
                    running: e.summary.status != mast_contract::ProjectStatus::Stopped,
                })
                .collect();
            let invocations: std::collections::HashMap<String, mast_compose::ComposeInvocation> =
                st.projects
                    .values()
                    .filter_map(|e| e.invocation.clone().map(|i| (e.record.id.clone(), i)))
                    .collect();
            let db_metas: Vec<crate::db_repair::DbServiceMeta> = st
                .projects
                .values()
                .filter(|e| scope.is_none_or(|id| e.record.id == id))
                .filter_map(|e| {
                    let model = e.model.as_ref()?;
                    Some(crate::db_repair::DbServiceMeta {
                        project_id: e.record.id.clone(),
                        compose_name: model.name.clone(),
                        services: model
                            .services
                            .iter()
                            .filter_map(|s| {
                                s.image
                                    .clone()
                                    .map(|image| (s.name.clone(), image, s.volumes.clone()))
                            })
                            .collect(),
                    })
                })
                .collect();
            // Which of each project's services publish host ports, per the
            // resolved model — the detached-container scan needs to know a
            // container *should* be reachable before calling it wreckage.
            let publishing: std::collections::HashMap<
                String,
                std::collections::BTreeSet<String>,
            > = st
                .projects
                .values()
                .filter_map(|e| {
                    let model = e.model.as_ref()?;
                    Some((
                        e.record.id.clone(),
                        model
                            .services
                            .iter()
                            .filter(|s| !s.published_ports.is_empty())
                            .map(|s| s.name.clone())
                            .collect(),
                    ))
                })
                .collect();
            let issues = if scope.is_some() {
                Vec::new()
            } else {
                workspace_summaries(&st)
                    .into_iter()
                    .filter_map(|w| w.graph_error.map(|g| (w.name, g)))
                    .collect()
            };
            let docker_host_env = self
                .inner
                .deps
                .process_env
                .get("DOCKER_HOST")
                .is_some_and(|v| !v.is_empty());
            (st.docker.clone(), seeds, invocations, db_metas, publishing, issues, docker_host_env)
        };

        let compose_version = {
            let argv: Vec<String> =
                ["docker", "compose", "version", "--short"].map(String::from).into();
            run_command(&argv, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                .await
                .ok()
                .filter(|o| o.success())
                .map(|o| o.stdout.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        // Security options, data root and server version in one round trip.
        let (rootless, docker_root, docker_server_version) = {
            let argv: Vec<String> = [
                "docker",
                "info",
                "--format",
                "{{.SecurityOptions}}\t{{.DockerRootDir}}\t{{.ServerVersion}}",
            ]
            .map(String::from)
            .into();
            match run_command(&argv, None, &[], PROBE_TIMEOUT, PROBE_CAP).await {
                Ok(out) if out.success() => {
                    let line = out.stdout.lines().next().unwrap_or_default();
                    let mut cols = line.split('\t');
                    let security = cols.next().unwrap_or_default();
                    let root = cols.next().unwrap_or_default();
                    let server = cols.next().unwrap_or_default().trim();
                    (
                        Some(security.contains("rootless")),
                        Some(root.trim().to_string()),
                        (!server.is_empty()).then(|| server.to_string()),
                    )
                }
                _ => (None, None, None),
            }
        };

        let docker_networks = if docker.available {
            let argv: Vec<String> =
                ["docker", "network", "ls", "--format", "{{.Name}}"].map(String::from).into();
            run_command(&argv, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                .await
                .ok()
                .filter(|o| o.success())
                .map(|o| o.stdout.lines().map(|l| l.trim().to_string()).collect())
        } else {
            None
        };

        // Only meaningful once the trust repair has filled the system store
        // — before that, the whole trust step is still ahead and the HTTPS
        // dialog owns the story.
        let proxy_nss_gap = if self.proxy_ca_trusted().await {
            self.proxy_nss_gap().await
        } else {
            None
        };

        let endpoint = docker.endpoint.clone();
        let (uid, gid) = uid_gid();
        let socket = endpoint.as_deref().and_then(unix_socket_path).map(socket_facts);
        // Disk facts only make sense for a local daemon.
        let disk_free_bytes = match (&socket, &docker_root) {
            (Some(_), Some(root)) if !root.is_empty() && Path::new(root).exists() => {
                free_bytes(root)
            }
            _ => None,
        };
        let snap_docker = docker_root.as_deref().is_some_and(|r| r.contains("/var/snap"))
            || endpoint.as_deref().is_some_and(|e| e.contains("/snap"));
        let selinux_enforcing = std::fs::read_to_string("/sys/fs/selinux/enforce")
            .map(|s| s.trim() == "1")
            .unwrap_or(false);

        let mut inspected = tokio::task::spawn_blocking(move || {
            seeds.into_iter().map(inspect_project).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();

        // Service images from the resolved model (needs no docker) — feeds
        // the stub-drift check.
        for meta in &db_metas {
            if let Some((facts, _)) = inspected.iter_mut().find(|(f, _)| f.id == meta.project_id)
            {
                facts.service_images = meta
                    .services
                    .iter()
                    .map(|(service, image, _)| (service.clone(), image.clone()))
                    .collect();
                facts.compose_name = Some(meta.compose_name.clone());
            }
        }

        // Live credential probes — only where there is something to probe:
        // a running project whose DB_HOST names a service in its model.
        if docker.available {
            for (facts, target) in &mut inspected {
                let Some(target) = target.take() else { continue };
                if !facts.running {
                    continue;
                }
                let Some(invocation) = invocations.get(&facts.id) else { continue };
                facts.db = crate::db_repair::probe_db(invocation, &target).await;
            }
            // Extension probe: one `php -m` per running project that needs
            // it — an Xdebug mode is requested, or a redis service exists
            // with phpredis expected. Feeds both checks from one exec.
            for (facts, _) in &mut inspected {
                if !facts.running {
                    continue;
                }
                let Some(invocation) = invocations.get(&facts.id) else { continue };
                let wants_redis_check = facts
                    .redis_client
                    .as_deref()
                    .is_none_or(|client| client == "phpredis")
                    && facts.service_images.iter().any(|(_, image)| {
                        image.contains("redis") || image.contains("valkey")
                    });
                if facts.xdebug.is_none() && !wants_redis_check {
                    continue;
                }
                let app_service = facts.app_service.clone();
                let argv = crate::db_repair::exec_env_argv(
                    invocation,
                    &app_service,
                    &[],
                    &["php".into(), "-m".into()],
                );
                if let Ok(out) =
                    run_command(&argv, Some(&invocation.project_dir), &[], PROBE_TIMEOUT, PROBE_CAP)
                        .await
                    && out.success()
                {
                    facts.php_modules = Some(
                        out.stdout
                            .lines()
                            .map(|l| l.trim().to_ascii_lowercase())
                            .filter(|l| !l.is_empty() && !l.starts_with('['))
                            .collect(),
                    );
                }
                if let (Some(modules), Some(xdebug)) =
                    (facts.php_modules.clone(), facts.xdebug.as_mut())
                {
                    xdebug.extension_loaded = Some(modules.iter().any(|m| m == "xdebug"));
                }
            }
            // Reachability doctor: a "Running" badge can hide a dead app
            // server (the ready probe falls back to a grace timeout). Probe
            // the app's published port from the host; when it refuses, probe
            // again from INSIDE the app container to split "not serving"
            // from "published port not reaching the host".
            for (facts, _) in &mut inspected {
                if !facts.running || !facts.sail_flavored {
                    continue;
                }
                let Some(invocation) = invocations.get(&facts.id) else { continue };
                let host_port = facts
                    .host_ports
                    .iter()
                    .find(|(label, _)| label == "APP_PORT")
                    .or_else(|| {
                        facts.host_ports.iter().find(|(label, _)| *label == facts.app_service)
                    })
                    .map(|(_, port)| *port);
                let Some(host_port) = host_port else { continue };
                let connect = |addr: &'static str| async move {
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        tokio::net::TcpStream::connect((addr, host_port)),
                    )
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false)
                };
                // Both families: docker can publish v6-only (the remap
                // blind-spot lesson).
                let host_ok = connect("127.0.0.1").await || connect("::1").await;
                let inner_ok = if host_ok {
                    None
                } else {
                    // Sail's container-side app port is 80; `-f` makes curl
                    // fail on HTTP >= 400, and exit 22 still means something
                    // IS serving — reachability is what's on trial here.
                    let tail: Vec<String> =
                        ["curl", "-sf", "-o", "/dev/null", "--max-time", "3", "http://localhost:80/"]
                            .map(String::from)
                            .to_vec();
                    let argv = crate::project_ops::compose_exec_argv(
                        invocation,
                        &facts.app_service,
                        &tail,
                    );
                    match run_command(
                        &argv,
                        Some(&invocation.project_dir),
                        &[],
                        PROBE_TIMEOUT,
                        PROBE_CAP,
                    )
                    .await
                    {
                        Ok(out) if out.success() || out.status == 22 => Some(true),
                        // 127: no curl in the image — unknowable, not "down".
                        Ok(out) if out.status == 127 => None,
                        Ok(_) => Some(false),
                        Err(_) => None,
                    }
                };
                facts.app_reachability =
                    Some(mast_diagnostics::AppReachability { host_port, host_ok, inner_ok });
            }
            // Volume-vs-image version scan — deliberately including stopped
            // projects: the point is warning before the crash-loop.
            let mut version_issues = crate::db_repair::scan_db_versions(&db_metas).await;
            for (facts, _) in &mut inspected {
                if let Some(issues) = version_issues.remove(&facts.id) {
                    facts.db_versions = issues;
                }
            }
            // Config drift: containers created under an older resolved
            // config than the files+.env now produce.
            let adapter = self.inner.adapter.lock().unwrap().clone();
            let observations = match adapter {
                Some(adapter) => adapter.list_compose_containers().await.ok(),
                None => None,
            };
            if let Some(observations) = observations {
                for meta in &db_metas {
                    let running: Vec<_> = observations
                        .iter()
                        .filter(|o| o.project == meta.compose_name && o.state == "running")
                        .collect();
                    if running.is_empty() {
                        continue;
                    }
                    // Detached wreckage first — it does not depend on the
                    // config-hash probe below, and a detached container's
                    // hash still matches anyway.
                    let mut detached: Vec<String> = running
                        .iter()
                        .filter(|o| {
                            o.networks.is_empty()
                                && o.published_ports.is_empty()
                                && publishing
                                    .get(&meta.project_id)
                                    .is_some_and(|svcs| svcs.contains(&o.service))
                        })
                        .map(|o| o.service.clone())
                        .collect();
                    detached.sort();
                    detached.dedup();
                    if let Some((facts, _)) =
                        inspected.iter_mut().find(|(f, _)| f.id == meta.project_id)
                    {
                        facts.detached_services = detached;
                    }
                    let Some(invocation) = invocations.get(&meta.project_id) else { continue };
                    let Some(current) = config_hashes(invocation).await else { continue };
                    let mut drifted: Vec<String> = running
                        .iter()
                        .filter_map(|o| {
                            let label = o.config_hash.as_ref()?;
                            let now = current.get(&o.service)?;
                            (label != now).then(|| o.service.clone())
                        })
                        .collect();
                    drifted.sort();
                    drifted.dedup();
                    if let Some((facts, _)) =
                        inspected.iter_mut().find(|(f, _)| f.id == meta.project_id)
                    {
                        facts.drifted_services = drifted;
                    }
                }
            }
        }
        let projects: Vec<ProjectFacts> = inspected.into_iter().map(|(facts, _)| facts).collect();

        DiagCtx {
            system: SystemFacts {
                docker_connected: docker.available,
                docker_error: docker.error.clone(),
                endpoint,
                context_name: docker.context_name.clone(),
                docker_host_env,
                compose_version,
                docker_server_version,
                linux: cfg!(target_os = "linux"),
                socket,
                rootless,
                snap_docker,
                disk_free_bytes,
                selinux_enforcing,
                uid,
                gid,
                proxy_nss_gap,
            },
            projects,
            docker_networks,
            workspace_issues,
        }
    }

    fn finding_to_contract(&self, f: Finding) -> DiagnosticFinding {
        let project_name = f.project.as_ref().and_then(|id| {
            let st = self.inner.state.lock().unwrap();
            st.projects.get(id).map(|e| e.summary.name.clone())
        });
        DiagnosticFinding {
            check: f.check.to_string(),
            severity: severity_to_contract(f.severity),
            title: f.title,
            detail: f.detail,
            project: f.project.map(ProjectId),
            project_name,
            repair: f.repair.map(offer_to_contract),
        }
    }

    /// Run every applicable check and record the run in history. Failure to
    /// record is logged, never fatal — the report is the point.
    pub async fn run_diagnostics(&self) -> Result<DiagnosticReport, ErrorInfo> {
        self.run_diagnostics_scoped(None).await
    }

    /// Run diagnostics for one project (or everything, `None`). A scoped run
    /// gathers only that project's facts — no probes into the neighbours —
    /// keeps only its findings, and stays out of history: the recorded trend
    /// is "the full set passed", not a partial look.
    pub async fn run_diagnostics_scoped(
        &self,
        project: Option<&ProjectId>,
    ) -> Result<DiagnosticReport, ErrorInfo> {
        if let Some(project) = project {
            let st = self.inner.state.lock().unwrap();
            if !st.projects.contains_key(&project.0) {
                return Err(ErrorInfo::NotFound { what: format!("project {}", project.0) });
            }
        }
        let ctx = self.gather_diag_ctx(project.map(|p| p.0.as_str())).await;
        let (checks_run, mut findings) = mast_diagnostics::run_all(&ctx);
        let taken_unix = now_unix();

        match project {
            Some(project) => {
                findings.retain(|f| f.project.as_deref() == Some(project.0.as_str()));
            }
            None => {
                let db_path = self.inner.deps.store.diagnostics_db_path();
                let for_history = findings.clone();
                let recorded = tokio::task::spawn_blocking(move || {
                    DiagnosticsDb::open(&db_path)?.record_run(taken_unix, checks_run, &for_history)
                })
                .await;
                if let Ok(Err(e)) = recorded {
                    tracing::warn!("failed to record diagnostics run: {e}");
                }
            }
        }

        Ok(DiagnosticReport {
            taken_unix,
            checks_run: checks_run as u32,
            findings: findings.into_iter().map(|f| self.finding_to_contract(f)).collect(),
        })
    }

    pub async fn diagnostics_history(&self) -> Result<DiagnosticsHistory, ErrorInfo> {
        let db_path = self.inner.deps.store.diagnostics_db_path();
        tokio::task::spawn_blocking(move || {
            let db = DiagnosticsDb::open(&db_path)
                .map_err(crate::internal_err)?;
            let runs = db
                .recent_runs(20)
                .map_err(crate::internal_err)?
                .into_iter()
                .map(|r| DiagnosticRunSummary {
                    id: r.id,
                    taken_unix: r.taken_unix,
                    checks_run: r.checks_run,
                    errors: r.errors,
                    warnings: r.warnings,
                    infos: r.infos,
                })
                .collect();
            let repairs = db
                .recent_repairs(20)
                .map_err(crate::internal_err)?
                .into_iter()
                .map(|r| RepairAuditEntry {
                    applied_unix: r.applied_unix,
                    repair: r.repair,
                    project_name: r.project_name,
                    risk: r.risk,
                    outcome: r.outcome,
                })
                .collect();
            Ok(DiagnosticsHistory { runs, repairs })
        })
        .await
        .map_err(crate::internal_err)?
    }

    /// The Chromium half of Linux trust: add the proxy CA to `~/.pki/nssdb`
    /// via certutil. Best-effort — an unavailable certutil only means a
    /// browser warning remains, and the outcome line says so plainly. On
    /// macOS the keychain covers those browsers, so this does nothing.
    async fn nss_add_proxy_ca(
        &self,
        handle: &std::sync::Arc<crate::OpHandle>,
        op: OperationId,
        crt_str: &str,
    ) {
        let nssdb = if cfg!(target_os = "macos") {
            None
        } else {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".pki/nssdb"))
        };
        let Some(nssdb) = nssdb else { return };
        let _ = tokio::fs::create_dir_all(&nssdb).await;
        let db = format!("sql:{}", nssdb.display());
        let add: Vec<String> = [
            "certutil", "-d", db.as_str(), "-A", "-t", "C,,", "-n",
            crate::proxy::NSS_NICKNAME, "-i", crt_str,
        ]
        .map(String::from)
        .into();
        let nss = run_command(&add, None, &[], PROBE_TIMEOUT, PROBE_CAP).await;
        let (line, stderr) = match nss {
            Ok(o) if o.success() => (
                "added to ~/.pki/nssdb — Chromium-family browsers (Chrome, Vivaldi, \
                 Brave, Edge) trust it after a full restart"
                    .to_string(),
                false,
            ),
            Ok(o) => (
                format!(
                    "could not add to ~/.pki/nssdb ({}) — Chromium-family browsers \
                     will keep warning",
                    o.stderr.trim().lines().next().unwrap_or("certutil failed")
                ),
                true,
            ),
            Err(_) => (
                "certutil is not installed, so the NSS store Chromium-family browsers \
                 (Chrome, Vivaldi, Brave, Edge) read was NOT updated — they will keep \
                 warning. The \"Install certutil\" fix installs it and finishes this \
                 step."
                    .to_string(),
                true,
            ),
        };
        self.emit_op(handle, op, OperationEventKind::Output { line, stderr });
    }

    /// What `artisan-migrate` needs: the invocation and the app service
    /// (`APP_SERVICE`, default laravel.test) read fresh from `.env` and
    /// validated against the resolved model before it lands in an argv.
    fn migrate_target(
        &self,
        project: &ProjectId,
    ) -> Result<(mast_compose::ComposeInvocation, String, PathBuf, crate::Redactor), ErrorInfo>
    {
        let (invocation, path, redactor, services) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            let invocation =
                entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
                    message: "the project's compose invocation is not resolved".into(),
                })?;
            let services: Vec<String> = entry
                .model
                .as_ref()
                .map(|m| m.services.iter().map(|s| s.name.clone()).collect())
                .unwrap_or_default();
            (invocation, entry.record.path.clone(), entry.redactor.clone(), services)
        };
        let src = std::fs::read_to_string(path.join(".env")).unwrap_or_default();
        let service = mast_laravel::EnvFile::parse(&src)
            .get("APP_SERVICE")
            .map(|e| e.value.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "laravel.test".to_string());
        if !services.iter().any(|s| s == &service) {
            return Err(ErrorInfo::InvalidInput {
                message: format!("app service \"{service}\" is not in the compose model"),
            });
        }
        Ok((invocation, service, path, redactor))
    }

    /// Validate a `recreate-service` arg and gather what its compose command
    /// needs. The arg travels through the UI, so every named service must
    /// exist in the resolved model before it lands in an argv.
    fn recreate_targets(
        &self,
        project: &ProjectId,
        arg: Option<&str>,
    ) -> Result<
        (Vec<String>, mast_compose::ComposeInvocation, PathBuf, crate::Redactor),
        ErrorInfo,
    > {
        let services: Vec<String> =
            arg.unwrap_or_default().split_whitespace().map(str::to_string).collect();
        if services.is_empty() {
            return Err(ErrorInfo::InvalidInput {
                message: "recreate-service needs the service name(s)".into(),
            });
        }
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let invocation = entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
            message: "the project's compose invocation is not resolved".into(),
        })?;
        let known: std::collections::BTreeSet<&str> = entry
            .model
            .as_ref()
            .map(|m| m.services.iter().map(|s| s.name.as_str()).collect())
            .unwrap_or_default();
        if let Some(unknown) = services.iter().find(|s| !known.contains(s.as_str())) {
            return Err(ErrorInfo::InvalidInput {
                message: format!("\"{unknown}\" is not a service of this project"),
            });
        }
        Ok((services, invocation, entry.record.path.clone(), entry.redactor.clone()))
    }

    /// What a repair will do, before consent. Env-file repairs return a full
    /// before/after preview; command repairs describe their exact argv.
    pub async fn repair_preview(
        &self,
        repair: &str,
        arg: Option<&str>,
        project: Option<&ProjectId>,
    ) -> Result<RepairPlan, ErrorInfo> {
        let spec = mast_diagnostics::repair_spec(repair, arg)
            .ok_or_else(|| ErrorInfo::InvalidInput { message: format!("unknown repair {repair}") })?;
        let offer = offer_to_contract(spec);
        let (uid, gid) = uid_gid();

        match repair {
            REPAIR_SET_WWWUSER => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let env_path = path.join(".env");
                    let before = std::fs::read_to_string(&env_path)
                        .map_err(crate::internal_err)?;
                    let mut file = mast_laravel::EnvFile::parse(&before);
                    let mut summary = Vec::new();
                    for (key, value) in [
                        ("WWWUSER".to_string(), uid.to_string()),
                        ("WWWGROUP".to_string(), gid.to_string()),
                    ] {
                        if file.get(&key).map(|e| e.value.clone()) != Some(value.clone()) {
                            file.set(&key, &value)
                                .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
                            summary.push(format!("set {key}={value}"));
                        }
                    }
                    let no_op = summary.is_empty();
                    if no_op {
                        summary.push("already matching your uid/gid".into());
                    }
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: env_path.to_string_lossy().into_owned(),
                            after: file.to_string(),
                            before,
                            summary: summary.clone(),
                            no_op,
                        }),
                        summary,
                        no_op,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_COPY_ENV_EXAMPLE => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let env_path = path.join(".env");
                    let example_path = path.join(".env.example");
                    let no_op = env_path.exists();
                    let after = std::fs::read_to_string(&example_path)
                        .map_err(|_| ErrorInfo::InvalidInput {
                            message: "no .env.example to copy from".into(),
                        })?;
                    let summary = if no_op {
                        vec![".env already exists — nothing to do".to_string()]
                    } else {
                        vec!["create .env as a copy of .env.example".to_string()]
                    };
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: env_path.to_string_lossy().into_owned(),
                            before: String::new(),
                            after,
                            summary: summary.clone(),
                            no_op,
                        }),
                        summary,
                        no_op,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_COMPOSER_INSTALL => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let argv = composer_install_argv(&path, uid, gid);
                    Ok(RepairPlan {
                        summary: vec![
                            format!("run: {}", argv.join(" ")),
                            format!("in: {}", path.display()),
                            format!(
                                "{COMPOSER_IMAGE} carries its own PHP — nothing is installed on \
                                 the host"
                            ),
                        ],
                        repair: offer,
                        file_preview: None,
                        no_op: false,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_NODE_INSTALL => {
                let project = self.require_project(project)?;
                let (invocation, dir, _) = self.process_context(project)?;
                let (manager, frozen) = self.node_install_target(&dir, arg)?;
                let argv = node_install_argv(&invocation, &dir, manager, frozen);
                Ok(RepairPlan {
                    summary: vec![
                        format!("run: {}", argv.join(" ")),
                        format!("in: {}", dir.display()),
                        if frozen {
                            format!(
                                "the committed lockfile is honoured — {} refuses to \
                                 re-resolve it",
                                manager.as_str()
                            )
                        } else {
                            "no lockfile is committed, so this will write one".to_string()
                        },
                        "runs in the app container, which must be running".into(),
                    ],
                    repair: offer,
                    file_preview: None,
                    no_op: false,
                })
            }
            REPAIR_CREATE_NETWORK => {
                let net = arg.ok_or_else(|| ErrorInfo::InvalidInput {
                    message: "create-network needs a network name".into(),
                })?;
                Ok(RepairPlan {
                    summary: vec![format!("run: docker network create {net}")],
                    repair: offer,
                    file_preview: None,
                    no_op: false,
                })
            }
            REPAIR_DISCONNECT_STALE => {
                let packed = arg.ok_or_else(|| ErrorInfo::InvalidInput {
                    message: "disconnect-stale-endpoints needs a network name".into(),
                })?;
                let mut parts = packed.split_whitespace();
                let net = parts.next().unwrap_or_default().to_string();
                let container = parts.next();
                let mut summary = vec![
                    format!("inspect network {net} for endpoint records"),
                    "force-disconnect every record whose container no longer exists".into(),
                ];
                if let Some(container) = container {
                    summary.push(format!(
                        "remove leftover container {} if it exists and is not running",
                        &container[..container.len().min(12)]
                    ));
                }
                summary.push(
                    "remove every STOPPED container with the network's compose project \
                     label — what a successful `down` would have removed (volumes stay)"
                        .into(),
                );
                summary.push("running containers are never touched".into());
                Ok(RepairPlan { summary, repair: offer, file_preview: None, no_op: false })
            }
            REPAIR_FIX_APP_URL => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let env_path = path.join(".env");
                    let before =
                        std::fs::read_to_string(&env_path).map_err(crate::internal_err)?;
                    let mut file = mast_laravel::EnvFile::parse(&before);
                    let rewrite = app_url_rewrite(&file);
                    let (summary, no_op) = match &rewrite {
                        Some(to) => (vec![format!("set APP_URL={to}")], false),
                        None => (
                            vec!["APP_URL already agrees with APP_PORT — nothing to do".into()],
                            true,
                        ),
                    };
                    if let Some(to) = &rewrite {
                        file.set("APP_URL", to)
                            .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
                    }
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: env_path.to_string_lossy().into_owned(),
                            after: file.to_string(),
                            before,
                            summary: summary.clone(),
                            no_op,
                        }),
                        summary,
                        no_op,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_ARTISAN_MIGRATE => {
                let project = self.require_project(project)?;
                let (invocation, service, path, _) = self.migrate_target(project)?;
                let tail: Vec<String> =
                    ["php", "artisan", "migrate", "--force"].map(String::from).into();
                let argv = crate::db_repair::exec_env_argv(&invocation, &service, &[], &tail);
                Ok(RepairPlan {
                    summary: vec![
                        format!("run: {}", argv.join(" ")),
                        format!("in: {}", path.display()),
                        "applies this project's own migrations — nothing is dropped or \
                         rolled back"
                            .into(),
                        "runs in the app container, which must be running".into(),
                    ],
                    repair: offer,
                    file_preview: None,
                    no_op: false,
                })
            }
            REPAIR_RECREATE_SERVICE => {
                let project = self.require_project(project)?;
                let (services, invocation, path, _) = self.recreate_targets(project, arg)?;
                let mut tail: Vec<&str> = vec!["up", "-d", "--force-recreate", "--no-deps"];
                tail.extend(services.iter().map(String::as_str));
                let (argv, _) = crate::db_repair::scoped_compose_argv(&invocation, &tail);
                Ok(RepairPlan {
                    summary: vec![
                        format!("run: {}", argv.join(" ")),
                        format!("in: {}", path.display()),
                        "replaces exactly these containers — named volumes and their data \
                         are untouched"
                            .into(),
                    ],
                    repair: offer,
                    file_preview: None,
                    no_op: false,
                })
            }
            REPAIR_REASSIGN_PORTS => {
                let project = self.require_project(project)?;
                let env_path = self.project_path(project)?.join(".env");
                let (remaps, notes, before, after) =
                    self.preview_port_remap(project, crate::ports::RemapMode::BoundOrClaimed).await?;
                let no_op = remaps.is_empty();
                let mut summary: Vec<String> =
                    remaps.iter().map(|r| format!("set {}={} (was {})", r.key, r.to, r.from)).collect();
                summary.extend(notes);
                if no_op && summary.is_empty() {
                    summary.push("no host port is in the way — nothing to move".into());
                }
                Ok(RepairPlan {
                    repair: offer,
                    file_preview: Some(FileEditPreview {
                        file: env_path.to_string_lossy().into_owned(),
                        before,
                        after,
                        summary: summary.clone(),
                        no_op,
                    }),
                    summary,
                    no_op,
                })
            }
            REPAIR_SAIL_INSTALL => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    Ok(RepairPlan {
                        summary: vec![
                            format!("run: {}", sail_require_argv(&path, uid, gid).join(" ")),
                            format!("then: {}", sail_install_argv(&path, uid, gid).join(" ")),
                            "sail:install writes compose.yaml with mysql, redis and \
                             mailpit (editable afterwards via the Services card)"
                                .into(),
                        ],
                        repair: offer,
                        file_preview: None,
                        no_op: false,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_GENERATE_APP_KEY => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let src = std::fs::read_to_string(path.join(".env"))
                        .map_err(|_| ErrorInfo::InvalidInput {
                            message: "no .env file — create one first".into(),
                        })?;
                    let file = mast_laravel::EnvFile::parse(&src);
                    let no_op =
                        file.get("APP_KEY").is_some_and(|e| !e.value.trim().is_empty());
                    let summary = if no_op {
                        vec!["APP_KEY is already set — nothing to do".to_string()]
                    } else {
                        vec![
                            "generate a fresh base64: key (32 random bytes) and write \
                             APP_KEY to .env"
                                .into(),
                            "the key is minted at apply time and never leaves this machine"
                                .into(),
                        ]
                    };
                    Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_STORAGE_LINK => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let no_op = std::fs::symlink_metadata(path.join("public/storage")).is_ok();
                    let summary = if no_op {
                        vec!["public/storage already exists — nothing to do".to_string()]
                    } else {
                        vec![
                            "create symlink public/storage → ../storage/app/public".into(),
                            "relative, so it resolves on the host and inside the container"
                                .into(),
                        ]
                    };
                    Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_CHOWN_STORAGE => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let argv = chown_storage_argv(&path, uid, gid);
                    let scan = foreign_owned_scan(&path, uid);
                    let no_op = scan.is_none();
                    let summary = match (no_op, argv) {
                        (true, _) => {
                            vec!["everything is already owned by you — nothing to do".into()]
                        }
                        (false, Some(argv)) => vec![
                            format!("run: {}", argv.join(" ")),
                            "only storage/ and bootstrap/cache are touched".into(),
                            "runs as root in a throwaway container — no sudo on the host"
                                .into(),
                        ],
                        (false, None) => vec!["neither storage/ nor bootstrap/cache exists".into()],
                    };
                    Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_SET_PROJECT_NAME => {
                let path = self.project_path(self.require_project(project)?)?;
                let name = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "set-project-name needs the name to pin".into(),
                    })?
                    .to_string();
                tokio::task::spawn_blocking(move || {
                    let env_path = path.join(".env");
                    let before = std::fs::read_to_string(&env_path).map_err(|_| {
                        ErrorInfo::InvalidInput { message: "no .env file — create one first".into() }
                    })?;
                    let mut file = mast_laravel::EnvFile::parse(&before);
                    let no_op = file
                        .get("COMPOSE_PROJECT_NAME")
                        .is_some_and(|e| e.value.trim() == name);
                    let summary = if no_op {
                        vec![format!("COMPOSE_PROJECT_NAME is already {name} — nothing to do")]
                    } else {
                        file.set("COMPOSE_PROJECT_NAME", &name)
                            .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
                        vec![
                            format!("set COMPOSE_PROJECT_NAME={name} in .env"),
                            "apply while the project is STOPPED — the new name is a new \
                             compose identity; old containers/volumes stay under the old \
                             name"
                                .into(),
                        ]
                    };
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: env_path.to_string_lossy().into_owned(),
                            after: file.to_string(),
                            before,
                            summary: summary.clone(),
                            no_op,
                        }),
                        summary,
                        no_op,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_MIGRATE_MAILPIT => {
                let project = self.require_project(project)?;
                let service = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "migrate-mailpit needs the MailHog service name".into(),
                    })?
                    .to_string();
                let (_invocation, file) = self.catalog_context(project)?;
                tokio::task::spawn_blocking(move || {
                    let before = std::fs::read_to_string(&file).map_err(crate::internal_err)?;
                    let plan = mast_compose::catalog::plan_mailpit_migration(&before, &service)
                        .map_err(|message| ErrorInfo::InvalidInput { message })?;
                    let after = mast_yaml_edit::apply_all(&before, &plan.edits)
                        .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: file.to_string_lossy().into_owned(),
                            before,
                            after,
                            summary: plan.summary.clone(),
                            no_op: false,
                        }),
                        summary: plan.summary,
                        no_op: false,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_ADD_HOST_GATEWAY => {
                let project = self.require_project(project)?;
                let service = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "add-host-gateway needs the app service name".into(),
                    })?
                    .to_string();
                let (_invocation, file) = self.catalog_context(project)?;
                tokio::task::spawn_blocking(move || {
                    let before = std::fs::read_to_string(&file).map_err(crate::internal_err)?;
                    let (edits, summary) =
                        mast_compose::sail::plan_add_host_gateway(&before, &service)
                            .map_err(|message| ErrorInfo::InvalidInput { message })?;
                    let after = mast_yaml_edit::apply_all(&before, &edits)
                        .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: file.to_string_lossy().into_owned(),
                            before,
                            after,
                            summary: summary.clone(),
                            no_op: false,
                        }),
                        summary,
                        no_op: false,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_NORMALIZE_ENV_EOL => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let env_path = path.join(".env");
                    let crlf = std::fs::read(&env_path)
                        .map(|bytes| bytes.windows(2).any(|w| w == b"\r\n"))
                        .unwrap_or(false);
                    let no_op = !crlf;
                    let summary = if no_op {
                        vec![".env already has Unix line endings — nothing to do".into()]
                    } else {
                        vec![
                            "rewrite every CRLF line ending in .env to LF".into(),
                            "values are untouched; a timestamped backup is kept".into(),
                        ]
                    };
                    Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_REMOVE_HOT => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let hot_path = path.join("public/hot");
                    let hot = std::fs::read_to_string(&hot_path)
                        .ok()
                        .and_then(|c| mast_laravel::vite::parse_hot_file(&c));
                    let no_op = !hot_path.exists();
                    let summary = match &hot {
                        _ if no_op => vec!["public/hot is already gone — nothing to do".into()],
                        Some(hot) if dev_server_listening(hot) == Some(true) => vec![
                            format!("a dev server IS listening at {} — applying will refuse", hot.url),
                            "stop the dev server first, or leave the file alone".into(),
                        ],
                        Some(hot) => vec![
                            format!("delete public/hot (points at {})", hot.url),
                            "Blade serves built assets again immediately".into(),
                        ],
                        None => vec!["delete public/hot".into()],
                    };
                    Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_CONFIG_CLEAR => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let cache = path.join("bootstrap/cache/config.php");
                    let no_op = !cache.is_file();
                    let summary = if no_op {
                        vec!["no cached configuration — nothing to do".to_string()]
                    } else {
                        vec![
                            format!("delete {}", cache.display()),
                            "Laravel reads .env again on the next request".into(),
                        ]
                    };
                    Ok(RepairPlan { repair: offer, file_preview: None, summary, no_op })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_DB_RECONCILE => {
                self.db_reconcile_preview(offer, self.require_project(project)?, arg).await
            }
            REPAIR_DB_RECREATE => {
                self.db_recreate_preview(offer, self.require_project(project)?, arg).await
            }
            REPAIR_HOSTS_ENTRY => {
                let domain = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "add-hosts-entry needs the domain".into(),
                    })?
                    .to_ascii_lowercase();
                // Validated again here: the arg travels through the UI and
                // ends up interpolated into a root shell.
                crate::proxy::validate_local_domain(&domain)?;
                tokio::task::spawn_blocking(move || {
                    let hosts_path = crate::proxy::hosts_file_path();
                    let before = std::fs::read_to_string(hosts_path).unwrap_or_default();
                    let no_op = crate::proxy::hosts_resolves(&before, &domain);
                    let line = format!("127.0.0.1\t{domain}\t# mast local domain");
                    let after = if no_op {
                        before.clone()
                    } else {
                        let sep = if before.ends_with('\n') || before.is_empty() { "" } else { "\n" };
                        format!("{before}{sep}{line}\n")
                    };
                    let summary = if no_op {
                        vec![format!("{hosts_path} already resolves {domain} — nothing to do")]
                    } else {
                        vec![
                            format!("append \"{line}\" to {hosts_path}"),
                            crate::proxy::elevation_note().into(),
                            format!(
                                "prefer to do it yourself? add that line to {hosts_path} in \
                                 any editor — the HTTPS dialog has a copy button and an \
                                 Open hosts file button"
                            ),
                        ]
                    };
                    Ok(RepairPlan {
                        repair: offer,
                        file_preview: Some(FileEditPreview {
                            file: hosts_path.into(),
                            after,
                            before,
                            summary: summary.clone(),
                            no_op,
                        }),
                        summary,
                        no_op,
                    })
                })
                .await
                .map_err(crate::internal_err)?
            }
            REPAIR_TRUST_PROXY_CA => {
                let trusted = self.proxy_ca_trusted().await;
                // System trust is only half the Linux story — Chromium-family
                // browsers read ~/.pki/nssdb, and the plan must say which
                // half is left rather than call a half-done job finished.
                let nss_gap = if trusted { self.proxy_nss_gap().await } else { None };
                let exported = self.inner.deps.store.proxy_dir().join("root.crt");
                let no_op = trusted
                    && !matches!(nss_gap, Some(mast_diagnostics::NssTrustGap::CaMissing));
                let summary = if trusted {
                    match nss_gap {
                        Some(mast_diagnostics::NssTrustGap::CaMissing) => vec![
                            "the system trust store already holds the CA — that half is done"
                                .into(),
                            "add it to ~/.pki/nssdb (certutil) so Chromium-family \
                             browsers (Chrome, Vivaldi, Brave, Edge) trust it too — no \
                             elevation needed for this step"
                                .into(),
                            "fully restart the browser afterwards".into(),
                        ],
                        Some(mast_diagnostics::NssTrustGap::CertutilMissing) => vec![
                            "the system trust store already holds the CA".into(),
                            "but Chromium-family browsers (Chrome, Vivaldi, Brave, \
                             Edge) read ~/.pki/nssdb, and certutil — the tool that \
                             writes it — is not installed, so there is nothing this \
                             fix can do yet"
                                .into(),
                            "use the \"Install certutil\" fix instead — it installs \
                             the NSS tools and finishes this step in one go"
                                .into(),
                        ],
                        None => vec![
                            "the proxy CA is already trusted everywhere Mast can \
                             reach — nothing to do"
                                .to_string(),
                        ],
                    }
                } else if cfg!(target_os = "macos") {
                    vec![
                        "copy the proxy's root certificate out of the mast-proxy container"
                            .into(),
                        "add it to the System keychain as a trusted root \
                         (security add-trusted-cert)"
                            .into(),
                        crate::proxy::elevation_note().into(),
                        format!(
                            "the certificate file itself lands at {} — Firefox keeps its \
                             own store, so import that file there; the HTTPS dialog can \
                             also copy the path or the PEM for any other tool",
                            exported.display()
                        ),
                    ]
                } else {
                    vec![
                        "copy the proxy's root certificate out of the mast-proxy container"
                            .into(),
                        "install it into the system trust store \
                         (update-ca-certificates or update-ca-trust)"
                            .into(),
                        crate::proxy::elevation_note().into(),
                        "add it to ~/.pki/nssdb for Chrome/Chromium when certutil is available"
                            .into(),
                        format!(
                            "the certificate file itself lands at {} — Firefox keeps its \
                             own store, so import that file there (or enable \
                             security.enterprise_roots.enabled); the HTTPS dialog can \
                             also copy the path or the PEM for any other tool",
                            exported.display()
                        ),
                    ]
                };
                Ok(RepairPlan { summary, repair: offer, file_preview: None, no_op })
            }
            REPAIR_INSTALL_CERTUTIL => {
                let installed = crate::proxy::on_path("certutil");
                let script = crate::proxy::certutil_install_script();
                let (summary, no_op) = if installed {
                    (
                        vec![
                            "certutil is already installed — apply the trust fix \
                             instead; it adds the CA to ~/.pki/nssdb"
                                .to_string(),
                        ],
                        true,
                    )
                } else {
                    match &script {
                        Some((package, script)) => (
                            vec![
                                format!("install the {package} package: {script}"),
                                crate::proxy::elevation_note().into(),
                                "then add the proxy CA to ~/.pki/nssdb (certutil, no \
                                 further elevation) — Chromium-family browsers trust \
                                 it after a full restart"
                                    .into(),
                            ],
                            false,
                        ),
                        None => (
                            vec![
                                "no supported package manager found (apt-get, dnf, \
                                 yum, pacman, zypper) — install the NSS tools \
                                 (certutil) manually, then apply the trust fix"
                                    .to_string(),
                            ],
                            true,
                        ),
                    }
                };
                Ok(RepairPlan { summary, repair: offer, file_preview: None, no_op })
            }
            REPAIR_DOCKER_GROUP => {
                let user = self.current_username()?;
                Ok(RepairPlan {
                    summary: vec![
                        format!("run: pkexec usermod -aG docker {user}"),
                        "asks for elevation via polkit".into(),
                        format!(
                            "prefer to do it yourself? run `sudo usermod -aG docker {user}` \
                             in any terminal — same effect"
                        ),
                        "log out and back in for group membership to apply".into(),
                    ],
                    repair: offer,
                    file_preview: None,
                    no_op: false,
                })
            }
            _ => unreachable!("repair_spec covered above"),
        }
    }

    /// Which manager to install with, and whether the install can be frozen.
    ///
    /// The finding carries the manager it detected, but the repo can have moved
    /// on between the report and the click, so detection is re-run and the
    /// `arg` is only honoured when it still matches something real. Re-reading
    /// is also what supplies `frozen` — the finding does not carry it.
    fn node_install_target(
        &self,
        dir: &Path,
        arg: Option<&str>,
    ) -> Result<(mast_project::PackageManager, bool), ErrorInfo> {
        let node = mast_project::inspect_node_project(dir).ok_or_else(|| {
            ErrorInfo::InvalidInput {
                message: format!("{} has no package.json — nothing to install", dir.display()),
            }
        })?;
        match arg.and_then(mast_project::PackageManager::parse) {
            // The repo changed its mind since the report was generated; the
            // lockfile on disk wins over a stale click.
            Some(requested) if requested != node.manager => Err(ErrorInfo::Conflict {
                message: format!(
                    "this project now uses {}, not {} — re-run diagnostics",
                    node.manager.as_str(),
                    requested.as_str()
                ),
            }),
            _ => Ok((node.manager, node.frozen)),
        }
    }

    fn require_project<'p>(&self, project: Option<&'p ProjectId>) -> Result<&'p ProjectId, ErrorInfo> {
        project.ok_or_else(|| ErrorInfo::InvalidInput {
            message: "this repair needs a project".into(),
        })
    }

    fn current_username(&self) -> Result<String, ErrorInfo> {
        self.inner
            .deps
            .process_env
            .get("USER")
            .filter(|u| !u.is_empty())
            .cloned()
            .ok_or_else(|| ErrorInfo::Internal { message: "cannot determine your username".into() })
    }

    /// Apply a repair (already previewed client-side). Streams output for the
    /// command-shaped ones; records an audit row whatever the outcome.
    pub(crate) async fn apply_repair(
        &self,
        handle: &std::sync::Arc<crate::OpHandle>,
        op: OperationId,
        repair: &str,
        arg: Option<&str>,
        project: Option<&ProjectId>,
    ) -> Result<(), ErrorInfo> {
        let result = self.apply_repair_inner(handle, op, repair, arg, project).await;

        let risk = mast_diagnostics::repair_spec(repair, arg)
            .map(|s| s.risk)
            .unwrap_or(RiskTier::Safe);
        let project_name = project.and_then(|p| {
            let st = self.inner.state.lock().unwrap();
            st.projects.get(&p.0).map(|e| e.summary.name.clone())
        });
        let outcome = match &result {
            Ok(()) => "applied".to_string(),
            Err(e) => format!("failed: {e:?}"),
        };
        let db_path = self.inner.deps.store.diagnostics_db_path();
        let repair_owned = repair.to_string();
        let recorded = tokio::task::spawn_blocking(move || {
            DiagnosticsDb::open(&db_path)?.record_repair(
                now_unix(),
                &repair_owned,
                project_name.as_deref(),
                risk,
                &outcome,
            )
        })
        .await;
        if let Ok(Err(e)) = recorded {
            tracing::warn!("failed to record repair audit: {e}");
        }
        result
    }

    async fn apply_repair_inner(
        &self,
        handle: &std::sync::Arc<crate::OpHandle>,
        op: OperationId,
        repair: &str,
        arg: Option<&str>,
        project: Option<&ProjectId>,
    ) -> Result<(), ErrorInfo> {
        let (uid, gid) = uid_gid();
        match repair {
            REPAIR_SET_WWWUSER => {
                let path = self.project_path(self.require_project(project)?)?.join(".env");
                let backups = self.inner.deps.store.backups_dir();
                tokio::task::spawn_blocking(move || {
                    mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                        f.set("WWWUSER", &uid.to_string())?;
                        f.set("WWWGROUP", &gid.to_string())
                    })
                })
                .await
                .map_err(crate::internal_err)?
                .map_err(crate::env_write_error)?;
                self.hint();
                Ok(())
            }
            REPAIR_COPY_ENV_EXAMPLE => {
                let path = self.project_path(self.require_project(project)?)?;
                tokio::task::spawn_blocking(move || {
                    let env_path = path.join(".env");
                    if env_path.exists() {
                        return Err(ErrorInfo::Conflict {
                            message: ".env already exists — refusing to overwrite".into(),
                        });
                    }
                    std::fs::copy(path.join(".env.example"), &env_path)
                        .map(|_| ())
                        .map_err(crate::internal_err)
                })
                .await
                .map_err(crate::internal_err)??;
                self.hint();
                Ok(())
            }
            REPAIR_CREATE_NETWORK => {
                let net = arg.ok_or_else(|| ErrorInfo::InvalidInput {
                    message: "create-network needs a network name".into(),
                })?;
                let argv: Vec<String> =
                    ["docker", "network", "create", net].map(String::from).into();
                let out = run_command(&argv, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                    .await
                    .map_err(crate::internal_err)?;
                // Idempotent by design: racing an existing network is success.
                if out.success() || out.stderr.contains("already exists") {
                    self.hint();
                    Ok(())
                } else {
                    Err(ErrorInfo::Internal { message: out.stderr.trim().to_string() })
                }
            }
            REPAIR_DISCONNECT_STALE => {
                let packed = arg.ok_or_else(|| ErrorInfo::InvalidInput {
                    message: "disconnect-stale-endpoints needs a network name".into(),
                })?;
                let mut parts = packed.split_whitespace();
                let net = parts.next().unwrap_or_default();
                let leftover = parts.next();
                let inspect: Vec<String> = [
                    "docker",
                    "network",
                    "inspect",
                    "--format",
                    "{{range $id, $e := .Containers}}{{$id}} {{$e.Name}}\n{{end}}",
                    net,
                ]
                .map(String::from)
                .into();
                let out = run_command(&inspect, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                    .await
                    .map_err(crate::internal_err)?;
                if !out.success() {
                    // Nothing to clean if the network itself is gone.
                    if out.stderr.contains("not found") {
                        self.emit_op(
                            handle,
                            op,
                            OperationEventKind::Output {
                                line: format!("network {net} no longer exists — nothing to clear"),
                                stderr: false,
                            },
                        );
                        self.hint();
                        return Ok(());
                    }
                    return Err(ErrorInfo::Internal { message: out.stderr.trim().to_string() });
                }
                let mut cleared = 0usize;
                for line in out.stdout.lines() {
                    let mut parts = line.split_whitespace();
                    let Some(id) = parts.next() else { continue };
                    let name = parts.next().unwrap_or(id);
                    // A container that still exists keeps its endpoint; only
                    // records whose container is gone are stale.
                    let probe: Vec<String> =
                        ["docker", "inspect", "--format", "{{.Id}}", id].map(String::from).into();
                    let exists = run_command(&probe, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                        .await
                        .map(|o| o.success())
                        .unwrap_or(false);
                    if exists {
                        continue;
                    }
                    let disconnect: Vec<String> =
                        ["docker", "network", "disconnect", "-f", net, name]
                            .map(String::from)
                            .into();
                    let out = run_command(&disconnect, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                        .await
                        .map_err(crate::internal_err)?;
                    let line = if out.success() {
                        cleared += 1;
                        format!("cleared stale endpoint {name}")
                    } else {
                        format!("could not clear {name}: {}", out.stderr.trim())
                    };
                    self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
                }
                // The failing command may have named a container that still
                // EXISTS — a leftover carrying this project name from another
                // directory, which compose enumerates but cannot disconnect.
                // Stopped: remove it. Running: report and leave it alone.
                if let Some(leftover) = leftover {
                    let probe: Vec<String> =
                        ["docker", "inspect", "--format", "{{.State.Running}} {{.Name}}", leftover]
                            .map(String::from)
                            .into();
                    let short = &leftover[..leftover.len().min(12)];
                    match run_command(&probe, None, &[], PROBE_TIMEOUT, PROBE_CAP).await {
                        Ok(out) if out.success() => {
                            let running = out.stdout.trim().starts_with("true");
                            let name = out.stdout.split_whitespace().nth(1).unwrap_or(short);
                            if running {
                                self.emit_op(
                                    handle,
                                    op,
                                    OperationEventKind::Output {
                                        line: format!(
                                            "leftover container {name} is RUNNING — stop it \
                                             before removing (left untouched)"
                                        ),
                                        stderr: true,
                                    },
                                );
                            } else {
                                let rm: Vec<String> =
                                    ["docker", "rm", "-f", leftover].map(String::from).into();
                                let out =
                                    run_command(&rm, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                                        .await
                                        .map_err(crate::internal_err)?;
                                let line = if out.success() {
                                    cleared += 1;
                                    format!(
                                        "removed leftover container {name} — compose kept \
                                         tripping on it"
                                    )
                                } else {
                                    format!("could not remove {name}: {}", out.stderr.trim())
                                };
                                self.emit_op(
                                    handle,
                                    op,
                                    OperationEventKind::Output { line, stderr: false },
                                );
                            }
                        }
                        // Already gone: the endpoint sweep above was the fix.
                        _ => {}
                    }
                }
                // One named leftover is rarely alone (field lesson: the user
                // found a second by hand). Sweep every STOPPED container
                // carrying the network's compose project label — exactly the
                // set a successful `down` would have removed; running ones
                // are reported and left alone.
                let label_probe: Vec<String> = [
                    "docker",
                    "network",
                    "inspect",
                    "--format",
                    "{{index .Labels \"com.docker.compose.project\"}}",
                    net,
                ]
                .map(String::from)
                .into();
                let project_label = run_command(&label_probe, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                    .await
                    .ok()
                    .filter(|o| o.success())
                    .map(|o| o.stdout.trim().to_string())
                    .filter(|l| !l.is_empty() && *l != "<no value>");
                if let Some(project_label) = project_label {
                    let ps: Vec<String> = [
                        "docker",
                        "ps",
                        "-a",
                        "--filter",
                        &format!("label=com.docker.compose.project={project_label}"),
                        "--format",
                        "{{.ID}} {{.Names}} {{.State}}",
                    ]
                    .map(String::from)
                    .into();
                    let out = run_command(&ps, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                        .await
                        .map_err(crate::internal_err)?;
                    for row in out.stdout.lines() {
                        let mut parts = row.split_whitespace();
                        let (Some(id), name, state) = (parts.next(), parts.next(), parts.next())
                        else {
                            continue;
                        };
                        let name = name.unwrap_or(id);
                        if state == Some("running") {
                            self.emit_op(
                                handle,
                                op,
                                OperationEventKind::Output {
                                    line: format!("{name} is running — left untouched"),
                                    stderr: false,
                                },
                            );
                            continue;
                        }
                        let rm: Vec<String> =
                            ["docker", "rm", "-f", id].map(String::from).into();
                        let out = run_command(&rm, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                            .await
                            .map_err(crate::internal_err)?;
                        let line = if out.success() {
                            cleared += 1;
                            format!("removed stopped container {name} (project {project_label})")
                        } else {
                            format!("could not remove {name}: {}", out.stderr.trim())
                        };
                        self.emit_op(
                            handle,
                            op,
                            OperationEventKind::Output { line, stderr: false },
                        );
                    }
                }
                if cleared == 0 {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: "no stale endpoints found".into(),
                            stderr: false,
                        },
                    );
                }
                self.hint();
                Ok(())
            }
            REPAIR_REASSIGN_PORTS => {
                let project = self.require_project(project)?;
                let (remaps, notes) = self
                    .remap_conflicting_ports(project, crate::ports::RemapMode::BoundOrClaimed)
                    .await?;
                for line in remaps
                    .iter()
                    .map(|r| format!("moved {} to {} (was {})", r.key, r.to, r.from))
                    .chain(notes)
                {
                    self.emit_op(handle, op, OperationEventKind::Output { line, stderr: false });
                }
                if remaps.is_empty() {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: "no host port needed moving".into(),
                            stderr: false,
                        },
                    );
                }
                Ok(())
            }
            REPAIR_FIX_APP_URL => {
                let path = self.project_path(self.require_project(project)?)?.join(".env");
                let backups = self.inner.deps.store.backups_dir();
                let applied =
                    tokio::task::spawn_blocking(move || -> Result<Option<String>, ErrorInfo> {
                        let mut applied = None;
                        mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                            if let Some(to) = app_url_rewrite(f) {
                                f.set("APP_URL", &to)?;
                                applied = Some(to);
                            }
                            Ok(())
                        })
                        .map_err(crate::env_write_error)?;
                        Ok(applied)
                    })
                    .await
                    .map_err(crate::internal_err)??;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: match applied {
                            Some(url) => format!("APP_URL now {url}"),
                            None => {
                                "APP_URL already agrees with APP_PORT — nothing to change".into()
                            }
                        },
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_ARTISAN_MIGRATE => {
                let project = self.require_project(project)?;
                let (invocation, service, path, redactor) = self.migrate_target(project)?;
                let tail: Vec<String> =
                    ["php", "artisan", "migrate", "--force"].map(String::from).into();
                let argv = crate::db_repair::exec_env_argv(&invocation, &service, &[], &tail);
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("$ {}", argv.join(" ")),
                        stderr: false,
                    },
                );
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    Some(&path),
                    &redactor,
                    Duration::from_secs(10 * 60),
                )
                .await?;
                self.hint();
                Ok(())
            }
            REPAIR_RECREATE_SERVICE => {
                let project = self.require_project(project)?;
                let (services, invocation, path, redactor) =
                    self.recreate_targets(project, arg)?;
                let mut tail: Vec<&str> = vec!["up", "-d", "--force-recreate", "--no-deps"];
                tail.extend(services.iter().map(String::as_str));
                let (argv, env) = crate::db_repair::scoped_compose_argv(&invocation, &tail);
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("$ {}", argv.join(" ")),
                        stderr: false,
                    },
                );
                self.run_streamed_command_env(
                    handle,
                    op,
                    &argv,
                    Some(&path),
                    &env,
                    &redactor,
                    Duration::from_secs(15 * 60),
                )
                .await?;
                self.hint();
                Ok(())
            }
            REPAIR_COMPOSER_INSTALL => {
                let project = self.require_project(project)?;
                let (path, redactor) = {
                    let st = self.inner.state.lock().unwrap();
                    let entry = st.projects.get(&project.0).ok_or(ErrorInfo::NotFound {
                        what: format!("project {}", project.0),
                    })?;
                    (entry.record.path.clone(), entry.redactor.clone())
                };
                let argv = composer_install_argv(&path, uid, gid);
                self.run_streamed_command(handle, op, &argv, Some(&path), &redactor,
                    // First run pulls the composer image and the whole
                    // dependency tree — generous.
                    Duration::from_secs(30 * 60))
                    .await?;
                self.hint();
                Ok(())
            }
            REPAIR_NODE_INSTALL => {
                let project = self.require_project(project)?;
                let (invocation, dir, redactor) = self.process_context(project)?;
                let (manager, frozen) = self.node_install_target(&dir, arg)?;
                let argv = node_install_argv(&invocation, &dir, manager, frozen);
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output { line: format!("$ {}", argv.join(" ")), stderr: false },
                );
                // A cold pnpm/npm store on a large frontend is slow, and the
                // first run may pull the app image too.
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    Some(&dir),
                    &redactor,
                    Duration::from_secs(30 * 60),
                )
                .await?;
                self.hint();
                Ok(())
            }
            REPAIR_SAIL_INSTALL => {
                let project = self.require_project(project)?;
                let (path, redactor) = {
                    let st = self.inner.state.lock().unwrap();
                    let entry = st.projects.get(&project.0).ok_or(ErrorInfo::NotFound {
                        what: format!("project {}", project.0),
                    })?;
                    (entry.record.path.clone(), entry.redactor.clone())
                };
                let timeout = Duration::from_secs(30 * 60);
                let require = sail_require_argv(&path, uid, gid);
                self.run_streamed_command(handle, op, &require, Some(&path), &redactor, timeout)
                    .await?;
                let install = sail_install_argv(&path, uid, gid);
                self.run_streamed_command(handle, op, &install, Some(&path), &redactor, timeout)
                    .await?;
                self.hint();
                Ok(())
            }
            REPAIR_CHOWN_STORAGE => {
                let path = self.project_path(self.require_project(project)?)?;
                let argv = chown_storage_argv(&path, uid, gid).ok_or_else(|| {
                    ErrorInfo::InvalidInput {
                        message: "neither storage/ nor bootstrap/cache exists".into(),
                    }
                })?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output { line: format!("$ {}", argv.join(" ")), stderr: false },
                );
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    Some(&path),
                    &crate::Redactor::default(),
                    Duration::from_secs(5 * 60),
                )
                .await?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("storage/ and bootstrap/cache now belong to uid {uid}"),
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_SET_PROJECT_NAME => {
                let path = self.project_path(self.require_project(project)?)?.join(".env");
                let name = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "set-project-name needs the name to pin".into(),
                    })?
                    .to_string();
                let backups = self.inner.deps.store.backups_dir();
                let display = name.clone();
                tokio::task::spawn_blocking(move || {
                    mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                        f.set("COMPOSE_PROJECT_NAME", &name)
                    })
                })
                .await
                .map_err(crate::internal_err)?
                .map_err(crate::env_write_error)?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!(
                            "COMPOSE_PROJECT_NAME={display} pinned — takes effect on the \
                             next start (old containers/volumes keep the old name)"
                        ),
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_MIGRATE_MAILPIT => {
                let project = self.require_project(project)?;
                let service = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "migrate-mailpit needs the MailHog service name".into(),
                    })?
                    .to_string();
                let (invocation, file) = self.catalog_context(project)?;
                let source = tokio::task::spawn_blocking({
                    let file = file.clone();
                    move || std::fs::read_to_string(file)
                })
                .await
                .map_err(crate::internal_err)?
                .map_err(crate::internal_err)?;
                let plan = mast_compose::catalog::plan_mailpit_migration(&source, &service)
                    .map_err(|message| ErrorInfo::InvalidInput { message })?;
                self.write_compose(&invocation, &file, &plan.edits, plan.summary.clone()).await?;
                for line in &plan.summary {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output { line: line.clone(), stderr: false },
                    );
                }
                // Same .env updates a catalog mailpit add applies.
                let mailpit = mast_compose::catalog::catalog_def("mailpit")
                    .expect("mailpit is a catalog entry");
                let env_path = invocation.project_dir.join(".env");
                if env_path.is_file() {
                    let backups = self.inner.deps.store.backups_dir();
                    let sets: Vec<(String, String)> = mailpit
                        .env_sets
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect();
                    tokio::task::spawn_blocking(move || {
                        mast_laravel::edit_env_file(&env_path, Some(&backups), |f| {
                            for (k, v) in &sets {
                                f.set(k, v)?;
                            }
                            Ok(())
                        })
                    })
                    .await
                    .map_err(crate::internal_err)?
                    .map_err(crate::env_write_error)?;
                }
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: "MailHog replaced with Mailpit — Start recreates the stack with \
                               the new mailbox (dashboard on port 8025)"
                            .into(),
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_ADD_HOST_GATEWAY => {
                let project = self.require_project(project)?;
                let service = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "add-host-gateway needs the app service name".into(),
                    })?
                    .to_string();
                let (invocation, file) = self.catalog_context(project)?;
                let source = tokio::task::spawn_blocking({
                    let file = file.clone();
                    move || std::fs::read_to_string(file)
                })
                .await
                .map_err(crate::internal_err)?
                .map_err(crate::internal_err)?;
                let (edits, summary) = mast_compose::sail::plan_add_host_gateway(&source, &service)
                    .map_err(|message| ErrorInfo::InvalidInput { message })?;
                self.write_compose(&invocation, &file, &edits, summary).await?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!(
                            "{service}: host.docker.internal now maps to the host — recreate \
                             the container (Start) to apply it"
                        ),
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_NORMALIZE_ENV_EOL => {
                let path = self.project_path(self.require_project(project)?)?.join(".env");
                let backups = self.inner.deps.store.backups_dir();
                let normalized = tokio::task::spawn_blocking(move || {
                    mast_laravel::env_write::normalize_env_line_endings(&path, Some(&backups))
                })
                .await
                .map_err(crate::internal_err)?
                .map_err(crate::env_write_error)?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: if normalized.is_some() {
                            ".env converted to Unix line endings (backup kept)".into()
                        } else {
                            ".env already had Unix line endings".into()
                        },
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_REMOVE_HOT => {
                let path = self.project_path(self.require_project(project)?)?;
                let removed = tokio::task::spawn_blocking(move || -> Result<bool, ErrorInfo> {
                    let hot_path = path.join("public/hot");
                    // Re-probe at click time: a dev server that has started
                    // meanwhile OWNS this file — deleting it would desync a
                    // healthy dev setup.
                    if let Ok(contents) = std::fs::read_to_string(&hot_path)
                        && let Some(hot) = mast_laravel::vite::parse_hot_file(&contents)
                        && dev_server_listening(&hot) == Some(true)
                    {
                        return Err(ErrorInfo::Conflict {
                            message: format!(
                                "a Vite dev server is now listening at {} — its hot file is \
                                 not stale any more",
                                hot.url
                            ),
                        });
                    }
                    match std::fs::remove_file(&hot_path) {
                        Ok(()) => Ok(true),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                        Err(e) => Err(crate::internal_err(e)),
                    }
                })
                .await
                .map_err(crate::internal_err)??;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: if removed {
                            "removed public/hot — built assets are served again".into()
                        } else {
                            "public/hot was already gone".into()
                        },
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_CONFIG_CLEAR => {
                let path = self.project_path(self.require_project(project)?)?;
                let removed = tokio::task::spawn_blocking(move || {
                    let cache = path.join("bootstrap/cache/config.php");
                    match std::fs::remove_file(&cache) {
                        Ok(()) => Ok(true),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                        Err(e) => Err(crate::internal_err(e)),
                    }
                })
                .await
                .map_err(crate::internal_err)??;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: if removed {
                            "removed bootstrap/cache/config.php — .env is live again".into()
                        } else {
                            "no cached configuration — already clear".into()
                        },
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_DB_RECONCILE => {
                self.db_reconcile_apply(handle, op, self.require_project(project)?, arg).await
            }
            REPAIR_DB_RECREATE => {
                self.db_recreate_apply(handle, op, self.require_project(project)?, arg).await
            }
            REPAIR_GENERATE_APP_KEY => {
                let path = self.project_path(self.require_project(project)?)?.join(".env");
                let backups = self.inner.deps.store.backups_dir();
                tokio::task::spawn_blocking(move || -> Result<(), ErrorInfo> {
                    let src = std::fs::read_to_string(&path).map_err(crate::internal_err)?;
                    // Never rotate a live key from a repair — decrypting
                    // existing data depends on it. Only fill an empty slot.
                    if mast_laravel::EnvFile::parse(&src)
                        .get("APP_KEY")
                        .is_some_and(|e| !e.value.trim().is_empty())
                    {
                        return Err(ErrorInfo::Conflict {
                            message: "APP_KEY is already set — refusing to overwrite it"
                                .into(),
                        });
                    }
                    let key = mast_laravel::generate_app_key().map_err(crate::internal_err)?;
                    mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                        f.set("APP_KEY", &key)
                    })
                    .map(|_backup| ())
                    .map_err(crate::env_write_error)
                })
                .await
                .map_err(crate::internal_err)??;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: "generated a fresh APP_KEY (value not shown)".into(),
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_STORAGE_LINK => {
                let project_id = self.require_project(project)?;
                let path = self.project_path(project_id)?;
                let check_path = path.clone();
                tokio::task::spawn_blocking(move || -> Result<(), ErrorInfo> {
                    if !check_path.join("storage/app/public").is_dir() {
                        return Err(ErrorInfo::InvalidInput {
                            message: "storage/app/public does not exist — nothing to link"
                                .into(),
                        });
                    }
                    if std::fs::symlink_metadata(check_path.join("public/storage")).is_ok() {
                        return Err(ErrorInfo::Conflict {
                            message: "public/storage already exists — refusing to replace it"
                                .into(),
                        });
                    }
                    Ok(())
                })
                .await
                .map_err(crate::internal_err)??;
                #[cfg(unix)]
                {
                    let link = path.join("public/storage");
                    tokio::task::spawn_blocking(move || {
                        std::os::unix::fs::symlink("../storage/app/public", &link)
                            .map_err(crate::internal_err)
                    })
                    .await
                    .map_err(crate::internal_err)??;
                }
                // Windows cannot create a symlink without Developer Mode or
                // elevation — but the app container is Linux and shares the
                // bind mount, so make the (relative) link from inside it.
                #[cfg(not(unix))]
                {
                    let invocation = {
                        let st = self.inner.state.lock().unwrap();
                        let entry = st.projects.get(&project_id.0).ok_or(ErrorInfo::NotFound {
                            what: format!("project {}", project_id.0),
                        })?;
                        entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
                            message: "the project's compose invocation is not resolved".into(),
                        })?
                    };
                    let service = crate::project_ops::app_service_of(&path);
                    let tail: Vec<String> = ["ln", "-sfn", "../storage/app/public", "public/storage"]
                        .map(String::from)
                        .to_vec();
                    let argv =
                        crate::project_ops::compose_exec_argv(&invocation, &service, &tail);
                    let out = run_command(&argv, Some(&path), &[], PROBE_TIMEOUT, PROBE_CAP)
                        .await
                        .map_err(crate::internal_err)?;
                    if !out.success() {
                        return Err(ErrorInfo::InvalidInput {
                            message: format!(
                                "could not create the link inside the app container — on \
                                 Windows the project must be running for this repair \
                                 ({})",
                                out.stderr.trim().lines().last().unwrap_or("exec failed")
                            ),
                        });
                    }
                }
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: "created public/storage → ../storage/app/public".into(),
                        stderr: false,
                    },
                );
                self.hint();
                Ok(())
            }
            REPAIR_HOSTS_ENTRY => {
                let domain = arg
                    .ok_or_else(|| ErrorInfo::InvalidInput {
                        message: "add-hosts-entry needs the domain".into(),
                    })?
                    .to_ascii_lowercase();
                crate::proxy::validate_local_domain(&domain)?;
                let hosts_path = crate::proxy::hosts_file_path();
                let hosts = tokio::fs::read_to_string(hosts_path).await.unwrap_or_default();
                if crate::proxy::hosts_resolves(&hosts, &domain) {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: format!("{hosts_path} already resolves {domain}"),
                            stderr: false,
                        },
                    );
                    return Ok(());
                }
                // The domain passed the strict character validation above,
                // so it is safe as a printf argument word.
                let mut script = format!(
                    "printf '127.0.0.1\\t%s\\t# mast local domain\\n' {domain} >> {hosts_path}"
                );
                if cfg!(target_os = "macos") {
                    // macOS caches negative lookups; without a flush the
                    // fresh entry can take minutes to be seen.
                    script.push_str(" && dscacheutil -flushcache");
                }
                let argv = crate::proxy::privileged_shell_argv(&script);
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    None,
                    &crate::Redactor::default(),
                    Duration::from_secs(5 * 60),
                )
                .await?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!("{domain} now resolves to 127.0.0.1 on this machine"),
                        stderr: false,
                    },
                );
                Ok(())
            }
            REPAIR_TRUST_PROXY_CA => {
                let dir = self.inner.deps.store.proxy_dir();
                tokio::fs::create_dir_all(&dir).await.map_err(crate::internal_err)?;
                let crt = dir.join("root.crt");
                let crt_str = crt.to_string_lossy().into_owned();
                if crt_str.contains('\'') {
                    return Err(ErrorInfo::Internal {
                        message: "data directory path contains a quote — cannot build a \
                                  safe elevated command"
                            .into(),
                    });
                }
                let cp: Vec<String> =
                    ["docker", "cp", crate::proxy::CA_IN_CONTAINER, crt_str.as_str()]
                        .map(String::from)
                        .into();
                let copied = run_command(&cp, None, &[], PROBE_TIMEOUT, PROBE_CAP)
                    .await
                    .map_err(crate::internal_err)?;
                if !copied.success() {
                    return Err(ErrorInfo::InvalidInput {
                        message: "the proxy has not generated its certificate authority \
                                  yet — enable a local domain first, then retry"
                            .into(),
                    });
                }
                // The elevated half only when the system store does not
                // already hold the CA — re-running to finish the NSS half
                // must not raise a pointless elevation prompt.
                if self.proxy_ca_trusted().await {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: "system trust store already holds the CA — skipping \
                                   the elevated step"
                                .into(),
                            stderr: false,
                        },
                    );
                } else {
                    let script = if cfg!(target_os = "macos") {
                        format!(
                            "security add-trusted-cert -d -r trustRoot \
                             -k /Library/Keychains/System.keychain '{crt_str}'"
                        )
                    } else {
                        let (dest, refresh) = if std::path::Path::new(
                            "/usr/local/share/ca-certificates",
                        )
                        .is_dir()
                        {
                            (
                                "/usr/local/share/ca-certificates/mast-proxy.crt",
                                "update-ca-certificates",
                            )
                        } else if std::path::Path::new("/etc/pki/ca-trust/source/anchors")
                            .is_dir()
                        {
                            ("/etc/pki/ca-trust/source/anchors/mast-proxy.crt", "update-ca-trust")
                        } else {
                            return Err(ErrorInfo::Internal {
                                message: format!(
                                    "no known system trust store on this machine — the \
                                     certificate was exported to {crt_str}; install it \
                                     manually (the HTTPS dialog can copy the path or PEM)"
                                ),
                            });
                        };
                        format!("install -m 644 '{crt_str}' {dest} && {refresh}")
                    };
                    let argv = crate::proxy::privileged_shell_argv(&script);
                    self.run_streamed_command(
                        handle,
                        op,
                        &argv,
                        None,
                        &crate::Redactor::default(),
                        Duration::from_secs(5 * 60),
                    )
                    .await?;
                }
                self.nss_add_proxy_ca(handle, op, &crt_str).await;
                let done = if cfg!(target_os = "macos") {
                    "added to the System keychain — restart the browser; Firefox needs \
                     the certificate imported manually"
                } else {
                    "system trust store updated — restart the browser; Firefox needs \
                     the certificate imported manually or security.enterprise_roots.enabled"
                };
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output { line: done.into(), stderr: false },
                );
                self.hint();
                Ok(())
            }
            REPAIR_INSTALL_CERTUTIL => {
                if !crate::proxy::on_path("certutil") {
                    let (_, script) =
                        crate::proxy::certutil_install_script().ok_or_else(|| {
                            ErrorInfo::InvalidInput {
                                message: "no supported package manager found — install \
                                          the NSS tools (certutil) manually, then apply \
                                          the trust fix"
                                    .into(),
                            }
                        })?;
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: format!("$ {script}"),
                            stderr: false,
                        },
                    );
                    let argv = crate::proxy::privileged_shell_argv(&script);
                    self.run_streamed_command(
                        handle,
                        op,
                        &argv,
                        None,
                        &crate::Redactor::default(),
                        Duration::from_secs(10 * 60),
                    )
                    .await?;
                    if !crate::proxy::on_path("certutil") {
                        return Err(ErrorInfo::Internal {
                            message: "the install finished but certutil is still not on \
                                      PATH — install the NSS tools manually, then apply \
                                      the trust fix"
                                .into(),
                        });
                    }
                } else {
                    self.emit_op(
                        handle,
                        op,
                        OperationEventKind::Output {
                            line: "certutil is already installed".into(),
                            stderr: false,
                        },
                    );
                }
                // Finish the reason it was installed: the CA into nssdb.
                let dir = self.inner.deps.store.proxy_dir();
                tokio::fs::create_dir_all(&dir).await.map_err(crate::internal_err)?;
                let crt = dir.join("root.crt");
                let crt_str = crt.to_string_lossy().into_owned();
                let cp: Vec<String> =
                    ["docker", "cp", crate::proxy::CA_IN_CONTAINER, crt_str.as_str()]
                        .map(String::from)
                        .into();
                let _ = run_command(&cp, None, &[], PROBE_TIMEOUT, PROBE_CAP).await;
                if !crt.is_file() {
                    return Err(ErrorInfo::InvalidInput {
                        message: "the proxy has not generated its certificate authority \
                                  yet — enable a local domain first, then retry"
                            .into(),
                    });
                }
                self.nss_add_proxy_ca(handle, op, &crt_str).await;
                self.hint();
                Ok(())
            }
            REPAIR_DOCKER_GROUP => {
                let user = self.current_username()?;
                let argv: Vec<String> =
                    ["pkexec", "usermod", "-aG", "docker", user.as_str()].map(String::from).into();
                self.run_streamed_command(
                    handle,
                    op,
                    &argv,
                    None,
                    &crate::Redactor::default(),
                    Duration::from_secs(5 * 60),
                )
                .await?;
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: format!(
                            "{user} added to the docker group — log out and back in, then \
                             re-run diagnostics."
                        ),
                        stderr: false,
                    },
                );
                Ok(())
            }
            other => {
                Err(ErrorInfo::InvalidInput { message: format!("unknown repair {other}") })
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project dir resolvable by `mast_compose`, with the Node shape the
    /// test needs. `sail` present makes the invocation a Sail runner.
    fn node_project(files: &[(&str, &str)], sail: bool) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("compose.yaml"), "services: {}\n").unwrap();
        for (name, body) in files {
            std::fs::write(tmp.path().join(name), body).unwrap();
        }
        if sail {
            std::fs::create_dir_all(tmp.path().join("vendor/bin")).unwrap();
            std::fs::write(tmp.path().join("vendor/bin/sail"), "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    tmp.path().join("vendor/bin/sail"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
        }
        tmp
    }

    fn invocation(dir: &Path) -> mast_compose::ComposeInvocation {
        mast_compose::resolve_invocation(dir, &std::collections::HashMap::new()).unwrap()
    }

    #[test]
    fn node_install_on_sail_is_the_line_the_developer_would_type() {
        let tmp = node_project(&[("package.json", "{}"), ("pnpm-lock.yaml", "")], true);
        let node = mast_project::inspect_node_project(tmp.path()).unwrap();
        let argv = node_install_argv(&invocation(tmp.path()), tmp.path(), node.manager, node.frozen);
        assert!(argv[0].ends_with("vendor/bin/sail"), "{argv:?}");
        assert_eq!(&argv[1..], ["pnpm", "install", "--frozen-lockfile"]);
    }

    #[test]
    fn node_install_without_sail_execs_in_the_app_service() {
        let tmp = node_project(&[("package.json", "{}"), (".env", "APP_SERVICE=api\n")], false);
        let node = mast_project::inspect_node_project(tmp.path()).unwrap();
        let argv = node_install_argv(&invocation(tmp.path()), tmp.path(), node.manager, node.frozen);
        let exec_at = argv.iter().position(|a| a == "exec").unwrap();
        // No lockfile committed, so npm may resolve freely.
        assert_eq!(&argv[exec_at..], ["exec", "-T", "api", "npm", "install"]);
    }

    #[test]
    fn a_committed_lockfile_freezes_the_install() {
        let tmp = node_project(&[("package.json", "{}"), ("package-lock.json", "{}")], false);
        let node = mast_project::inspect_node_project(tmp.path()).unwrap();
        let argv = node_install_argv(&invocation(tmp.path()), tmp.path(), node.manager, node.frozen);
        assert_eq!(argv.last().unwrap(), "ci");
    }

    #[test]
    fn composer_argv_matches_laravel_docs() {
        let argv = composer_install_argv(Path::new("/home/me/app"), 1000, 1000);
        assert_eq!(
            argv,
            [
                "docker",
                "run",
                "--rm",
                "-u",
                "1000:1000",
                "-e",
                "COMPOSER_HOME=/tmp",
                "-v",
                "/home/me/app:/var/www/html",
                "-w",
                "/var/www/html",
                "composer:latest",
                "install",
                "--ignore-platform-reqs",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn sail_install_argvs_share_the_composer_image_contract() {
        let dir = Path::new("/home/me/app");
        let require = sail_require_argv(dir, 1000, 1000);
        // The image's own entrypoint is composer, so the verb comes first.
        assert_eq!(
            &require[require.len() - 4..],
            ["require", "laravel/sail", "--dev", "--ignore-platform-reqs"]
        );
        assert!(require.contains(&COMPOSER_IMAGE.to_string()));
        assert!(!require.contains(&"--entrypoint".to_string()));

        let install = sail_install_argv(dir, 1000, 1000);
        assert_eq!(
            &install[install.len() - 4..],
            ["artisan", "sail:install", "--with=mysql,redis,mailpit", "--no-interaction"]
        );
        assert!(install.contains(&COMPOSER_IMAGE.to_string()));
        // Artisan needs php, which means overriding the composer entrypoint.
        let entrypoint = install.iter().position(|a| a == "--entrypoint").expect("entrypoint");
        assert_eq!(install[entrypoint + 1], "php");
        assert!(entrypoint < install.iter().position(|a| a == COMPOSER_IMAGE).unwrap());
    }
}
