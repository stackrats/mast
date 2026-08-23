//! The check set. Each check is pure over `DiagCtx`; a check that finds
//! nothing wrong emits no findings (the report's "checks run" count is what
//! tells the user everything passed).

use crate::{
    repair_spec, Check, DiagCtx, Finding, ProjectFacts, RepairSpec, RiskTier, Severity,
    REPAIR_CHOWN_STORAGE, REPAIR_COMPOSER_INSTALL, REPAIR_CONFIG_CLEAR, REPAIR_COPY_ENV_EXAMPLE,
    REPAIR_CREATE_NETWORK, REPAIR_DB_RECONCILE, REPAIR_DB_RECREATE, REPAIR_DOCKER_GROUP,
    REPAIR_GENERATE_APP_KEY, REPAIR_NODE_INSTALL, REPAIR_REASSIGN_PORTS, REPAIR_SAIL_INSTALL,
    REPAIR_SET_WWWUSER, REPAIR_STORAGE_LINK,
};
use mast_laravel::db::{ProbeFailure, VersionVerdict};

fn finding(
    check: &'static str,
    severity: Severity,
    title: impl Into<String>,
    detail: impl Into<String>,
) -> Finding {
    Finding { check, severity, title: title.into(), detail: detail.into(), project: None, repair: None }
}

fn for_project(mut f: Finding, p: &ProjectFacts) -> Finding {
    f.project = Some(p.id.clone());
    f
}

// ---------- system checks ----------

struct DockerRunning;
impl Check for DockerRunning {
    fn id(&self) -> &'static str {
        "docker-running"
    }
    fn applies(&self, _ctx: &DiagCtx) -> bool {
        true
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        if ctx.system.docker_connected {
            return Vec::new();
        }
        let endpoint = ctx.system.endpoint.as_deref().unwrap_or("(unknown endpoint)");
        let detail = match &ctx.system.docker_error {
            Some(err) => format!("Could not reach the daemon at {endpoint}: {err}"),
            None => format!("Could not reach the daemon at {endpoint}."),
        };
        vec![finding(self.id(), Severity::Error, "Docker is not reachable", detail)]
    }
}

struct ComposeV2;
impl Check for ComposeV2 {
    fn id(&self) -> &'static str {
        "compose-v2"
    }
    fn applies(&self, _ctx: &DiagCtx) -> bool {
        true
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        match ctx.system.compose_version.as_deref() {
            None => vec![finding(
                self.id(),
                Severity::Error,
                "Docker Compose v2 not found",
                "`docker compose version` failed — Mast requires the compose v2 plugin \
                 (the `docker-compose-plugin` package on Debian/Ubuntu).",
            )],
            Some(v) if v.trim_start_matches('v').starts_with("1.") => vec![finding(
                self.id(),
                Severity::Error,
                "Compose v1 detected",
                format!(
                    "Version {v} is the legacy python compose; Mast drives `docker compose` (v2)."
                ),
            )],
            Some(_) => Vec::new(),
        }
    }
}

struct SocketAccess;
impl Check for SocketAccess {
    fn id(&self) -> &'static str {
        "docker-socket"
    }
    /// Only diagnostic when the daemon is NOT reachable — if we are connected,
    /// socket access is proven.
    fn applies(&self, ctx: &DiagCtx) -> bool {
        !ctx.system.docker_connected && ctx.system.socket.is_some()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let Some(socket) = &ctx.system.socket else { return Vec::new() };
        if !socket.exists {
            return vec![finding(
                self.id(),
                Severity::Error,
                "Docker socket missing",
                format!("{} does not exist — the docker daemon is probably not running.", socket.path),
            )];
        }
        if socket.writable {
            return Vec::new();
        }
        let mut f = finding(
            self.id(),
            Severity::Error,
            "No permission on the docker socket",
            format!(
                "{} exists but is not writable by your user — typically your user is not in \
                 the `docker` group.",
                socket.path
            ),
        );
        f.repair = Some(RepairSpec {
            id: REPAIR_DOCKER_GROUP,
            title: "Add your user to the docker group".into(),
            risk: RiskTier::HighRisk,
            description: "Runs `pkexec usermod -aG docker <you>` (asks for elevation). \
                          Docker-group membership is equivalent to root on this machine. \
                          You must log out and back in for it to take effect."
                .into(),
            arg: None,
        });
        vec![f]
    }
}

struct RootlessInfo;
impl Check for RootlessInfo {
    fn id(&self) -> &'static str {
        "rootless-docker"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.rootless == Some(true)
    }
    fn run(&self, _ctx: &DiagCtx) -> Vec<Finding> {
        vec![finding(
            self.id(),
            Severity::Info,
            "Rootless Docker detected",
            "Containers already run as your user, so WWWUSER/WWWGROUP mapping is usually \
             redundant (harmless to keep).",
        )]
    }
}

struct SnapDocker;
impl Check for SnapDocker {
    fn id(&self) -> &'static str {
        "snap-docker"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.snap_docker
    }
    fn run(&self, _ctx: &DiagCtx) -> Vec<Finding> {
        vec![finding(
            self.id(),
            Severity::Warning,
            "Snap-packaged Docker detected",
            "Snap confinement relocates the socket and restricts bind mounts to your home \
             directory; projects outside $HOME will fail to mount. The distro package \
             (docker.io / docker-ce) avoids both problems.",
        )]
    }
}

struct DiskSpace;
impl Check for DiskSpace {
    fn id(&self) -> &'static str {
        "disk-space"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.disk_free_bytes.is_some()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        const GIB: u64 = 1024 * 1024 * 1024;
        let free = ctx.system.disk_free_bytes.unwrap_or(u64::MAX);
        let gb = free as f64 / GIB as f64;
        if free < GIB {
            vec![finding(
                self.id(),
                Severity::Error,
                "Docker data disk is almost full",
                format!("{gb:.1} GiB free — pulls and builds will start failing. `docker system prune` reclaims space."),
            )]
        } else if free < 5 * GIB {
            vec![finding(
                self.id(),
                Severity::Warning,
                "Docker data disk is low on space",
                format!("{gb:.1} GiB free on the docker data root."),
            )]
        } else {
            Vec::new()
        }
    }
}

struct SelinuxInfo;
impl Check for SelinuxInfo {
    fn id(&self) -> &'static str {
        "selinux"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.selinux_enforcing
    }
    fn run(&self, _ctx: &DiagCtx) -> Vec<Finding> {
        vec![finding(
            self.id(),
            Severity::Info,
            "SELinux is enforcing",
            "Bind mounts may need the `:z`/`:Z` volume flag if you see permission errors \
             inside containers.",
        )]
    }
}

struct ContextSanity;
impl Check for ContextSanity {
    fn id(&self) -> &'static str {
        "context-sanity"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.docker_host_env
            && ctx.system.context_name.as_deref().is_some_and(|n| n != "default")
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        vec![finding(
            self.id(),
            Severity::Info,
            "DOCKER_HOST overrides your docker context",
            format!(
                "DOCKER_HOST is set, so the named context \"{}\" is ignored (CLI precedence). \
                 Unset DOCKER_HOST if you meant to use the context.",
                ctx.system.context_name.as_deref().unwrap_or_default()
            ),
        )]
    }
}

// ---------- per-project checks ----------

struct EnvMissing;
impl Check for EnvMissing {
    fn id(&self) -> &'static str {
        "env-missing"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        !ctx.projects.is_empty()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| !p.env_present)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Error,
                    format!("{} has no .env file", p.name),
                    "Sail and Laravel read configuration from `.env`; without it the project \
                     cannot start correctly.",
                );
                if p.env_example_present {
                    f.repair = Some(RepairSpec {
                        id: REPAIR_COPY_ENV_EXAMPLE,
                        title: "Create .env from .env.example".into(),
                        risk: RiskTier::Safe,
                        description: "Copies `.env.example` to `.env` (refuses if `.env` \
                                      appears in the meantime). Re-run diagnostics afterwards \
                                      — a fresh copy usually needs an APP_KEY generated."
                            .into(),
                        arg: None,
                    });
                } else {
                    f.detail.push_str(" No `.env.example` exists to copy from.");
                }
                for_project(f, p)
            })
            .collect()
    }
}

struct VendorMissing;
impl Check for VendorMissing {
    fn id(&self) -> &'static str {
        "vendor-missing"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.sail_flavored)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.sail_flavored && !p.has_vendor)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Error,
                    format!("{} has no vendor/ directory", p.name),
                    "Composer dependencies (including Sail itself) are not installed — a fresh \
                     clone. Mast can install them inside the official Sail PHP container, no \
                     local PHP or composer needed.",
                );
                f.repair = Some(RepairSpec {
                    id: REPAIR_COMPOSER_INSTALL,
                    title: "Run composer install in a container".into(),
                    risk: RiskTier::Caution,
                    description: "Runs `composer install --ignore-platform-reqs` in the \
                                  official composer image with your UID/GID, mounting only \
                                  this project directory."
                        .into(),
                    arg: None,
                });
                for_project(f, p)
            })
            .collect()
    }
}

/// `node_modules` absent on a project that declares a frontend build. Laravel's
/// `artisan dev` shells out to the package manager, so this surfaces as a
/// registry fetch failing rather than as a missing install — worth naming
/// before the developer goes looking at their network.
struct NodeModulesMissing;
impl Check for NodeModulesMissing {
    fn id(&self) -> &'static str {
        "node-modules-missing"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.package_manager.is_some())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.package_manager.is_some() && !p.has_node_modules)
            .map(|p| {
                let pm = p.package_manager.as_deref().unwrap_or("npm");
                let source = if p.node_lockfile {
                    format!("This repo's lockfile selects {pm}")
                } else {
                    format!("No lockfile is committed, so {pm} is used")
                };
                let mut f = finding(
                    self.id(),
                    Severity::Error,
                    format!("{} has no node_modules/ directory", p.name),
                    format!(
                        "Frontend dependencies are not installed, so Vite — and anything \
                         that runs it, `artisan dev` included — cannot start. {source}."
                    ),
                );
                f.repair = Some(RepairSpec {
                    id: REPAIR_NODE_INSTALL,
                    title: format!("Run {pm} install in the app container"),
                    risk: RiskTier::Caution,
                    description: format!(
                        "Runs `{pm} install` inside this project's app container, which \
                         already carries npm, pnpm, yarn and bun. The app container must \
                         be running."
                    ),
                    arg: Some(pm.to_string()),
                });
                for_project(f, p)
            })
            .collect()
    }
}

/// More than one manager's lockfile committed. Whichever Mast picks, half the
/// team's installs would rewrite the other's lockfile, so this is reported
/// rather than repaired.
struct AmbiguousLockfiles;
impl Check for AmbiguousLockfiles {
    fn id(&self) -> &'static str {
        "lockfile-ambiguous"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| !p.conflicting_lockfiles.is_empty())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| !p.conflicting_lockfiles.is_empty())
            .map(|p| {
                let pm = p.package_manager.as_deref().unwrap_or("npm");
                let f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{} commits lockfiles for more than one package manager", p.name),
                    format!(
                        "Found {}. Mast will use {pm}, but installs will keep rewriting \
                         whichever lockfile the last manager did not own. Delete the stale \
                         ones, or pin the intended manager with a \"packageManager\" field \
                         in package.json.",
                        p.conflicting_lockfiles.join(", ")
                    ),
                );
                for_project(f, p)
            })
            .collect()
    }
}

struct SailMissing;
impl Check for SailMissing {
    fn id(&self) -> &'static str {
        "sail-missing"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        !ctx.projects.is_empty()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.is_laravel && p.has_vendor && !p.sail_flavored && !p.has_compose)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Error,
                    format!("{} has no Sail / docker-compose setup", p.name),
                    "This is a Laravel project, but there is nothing for Mast (or docker \
                     compose) to run.",
                );
                f.repair = Some(RepairSpec {
                    id: REPAIR_SAIL_INSTALL,
                    title: "Install Laravel Sail in a container".into(),
                    risk: RiskTier::Caution,
                    description: "Runs `composer require laravel/sail --dev` then `php artisan \
                                  sail:install` inside the official composer image — no local \
                                  PHP needed."
                        .into(),
                    arg: None,
                });
                for_project(f, p)
            })
            .collect()
    }
}

/// APP_KEY absent or empty on a Laravel project. Laravel refuses every request
/// with "No application encryption key has been specified" — the standard
/// state right after cloning + copying `.env.example`, and the missing half of
/// the clone-bootstrap story (laravel/sail's docs stop at `composer install`).
struct AppKeyMissing;
impl Check for AppKeyMissing {
    fn id(&self) -> &'static str {
        "app-key-missing"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.is_laravel && p.env_present)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.is_laravel && p.env_present && p.app_key_empty)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Error,
                    format!("{} has no APP_KEY", p.name),
                    "APP_KEY in `.env` is empty, so Laravel will refuse every request with \
                     \"No application encryption key has been specified\".",
                );
                f.repair = repair_spec(REPAIR_GENERATE_APP_KEY, None);
                for_project(f, p)
            })
            .collect()
    }
}

/// `storage/app/public` exists but `public/storage` does not point at it, so
/// everything stored on the `public` disk 404s in the browser. Repaired with a
/// relative symlink rather than `artisan storage:link`: artisan's default
/// absolute target only resolves on whichever side of the bind mount created
/// it, and creating the link needs no running container anyway.
struct StorageLinkMissing;
impl Check for StorageLinkMissing {
    fn id(&self) -> &'static str {
        "storage-link"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.is_laravel)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.is_laravel && p.storage_link_missing)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{} has no public/storage link", p.name),
                    "`storage/app/public` exists but is not linked into `public/`, so files \
                     on the public disk will 404 in the browser.",
                );
                f.repair = repair_spec(REPAIR_STORAGE_LINK, None);
                for_project(f, p)
            })
            .collect()
    }
}

/// The init-once-volume trap, Sail's most-reported "not a bug": the database
/// image creates user/password/database only when its data volume is first
/// initialized, so `.env` edits after the first start silently never apply
/// and the app fails with "Access denied". The engine probed the live
/// container with the `.env` credentials; this check turns the outcome into
/// an explanation and the right repair for what admin access remains.
struct DbCredentials;
impl Check for DbCredentials {
    fn id(&self) -> &'static str {
        "db-credentials"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.db.is_some())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter_map(|p| p.db.as_ref().and_then(|db| db.failure.map(|f| (p, db, f))))
            .map(|(p, db, failure)| {
                let engine = db.kind.as_str();
                let title = match failure {
                    ProbeFailure::AccessDenied => format!(
                        "{}: .env database credentials don't work against \"{}\"",
                        p.name, db.service
                    ),
                    ProbeFailure::UnknownDatabase => format!(
                        "{}: database \"{}\" does not exist in \"{}\"",
                        p.name, db.database, db.service
                    ),
                };
                let trap = format!(
                    "The {engine} image only creates user, password and database when its \
                     data volume is first initialized — editing .env afterwards changes \
                     nothing inside the volume. Logging in as \"{}\" with the current .env \
                     values failed.",
                    db.username
                );
                let (detail, repair_id) = if db.admin_access {
                    (
                        format!(
                            "{trap} An administrative login still works, so Mast can bring \
                             the live server in line with .env without touching data."
                        ),
                        REPAIR_DB_RECONCILE,
                    )
                } else {
                    (
                        format!(
                            "{trap} No administrative login works with the current values \
                             either (the volume was initialized under a different \
                             password), so a live repair is impossible — the volume must \
                             be recreated, which destroys its data."
                        ),
                        REPAIR_DB_RECREATE,
                    )
                };
                let mut f = finding(self.id(), Severity::Error, title, detail);
                f.repair = repair_spec(repair_id, Some(&db.service));
                for_project(f, p)
            })
            .collect()
    }
}

/// A database image tag that disagrees with what its data volume holds —
/// caught while the project is stopped, which is the whole point: the
/// alternative is a crash-looping container after the next start.
struct DbVolumeVersion;
impl Check for DbVolumeVersion {
    fn id(&self) -> &'static str {
        "db-volume-version"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| !p.db_versions.is_empty())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut findings = Vec::new();
        for p in &ctx.projects {
            for issue in &p.db_versions {
                let f = match &issue.verdict {
                    VersionVerdict::WillNotStart { reason } => {
                        let mut f = finding(
                            self.id(),
                            Severity::Error,
                            format!(
                                "{}: \"{}\" will not start on its existing data",
                                p.name, issue.service
                            ),
                            format!(
                                "{reason}. If the data matters, revert the image to a \
                                 {}-compatible tag first, export a dump, then retag. If it \
                                 does not, recreate the volume.",
                                issue.volume_version
                            ),
                        );
                        f.repair = repair_spec(REPAIR_DB_RECREATE, Some(&issue.service));
                        f
                    }
                    VersionVerdict::InPlaceUpgrade { note } => finding(
                        self.id(),
                        Severity::Warning,
                        format!(
                            "{}: \"{}\" will upgrade its data on next start",
                            p.name, issue.service
                        ),
                        format!("{note}. (Volume holds {}.)", issue.volume_version),
                    ),
                };
                findings.push(for_project(f, p));
            }
        }
        findings
    }
}

/// `bootstrap/cache/config.php` in a dev project: every `.env` edit —
/// including the ones Mast itself writes — is silently ignored until the
/// cache is cleared. The classic "changed .env, nothing happened" multiplier.
struct ConfigCache;
impl Check for ConfigCache {
    fn id(&self) -> &'static str {
        "config-cache"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.is_laravel)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.is_laravel && p.config_cached)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: configuration is cached — .env edits are not live", p.name),
                    "bootstrap/cache/config.php exists, so Laravel reads the cached values \
                     and ignores .env entirely (config:cache is meant for production).",
                );
                f.repair = repair_spec(REPAIR_CONFIG_CLEAR, None);
                for_project(f, p)
            })
            .collect()
    }
}

/// Containers running a configuration the files no longer describe. The
/// classic ".env edits do nothing" trap (Sail closed it as an edge case):
/// compose injects env at container CREATION, so `restart` keeps the old
/// values forever. The compose config-hash label makes this exact — only
/// services whose resolved config actually changed are named.
struct ConfigDrift;
impl Check for ConfigDrift {
    fn id(&self) -> &'static str {
        "config-drift"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| !p.drifted_services.is_empty())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| !p.drifted_services.is_empty())
            .map(|p| {
                for_project(
                    finding(
                        self.id(),
                        Severity::Warning,
                        format!(
                            "{}: running containers are behind the configuration",
                            p.name
                        ),
                        format!(
                            "{} started under an older configuration — compose only \
                             applies .env and file changes when a container is created, \
                             so a plain restart changes nothing. Press Start (compose \
                             `up -d`): it recreates exactly the drifted services.",
                            p.drifted_services.join(", ")
                        ),
                    ),
                    p,
                )
            })
            .collect()
    }
}

/// Files under storage/ or bootstrap/cache owned by someone else — root
/// from a sudo'd installer, 1337 from a container that ran without WWWUSER
/// mapping. The cure for Sail's most-commented issue ever (#81), whose
/// top-voted answer is a manual chown; wwwuser-parity prevents NEW damage,
/// this repairs the existing kind.
struct StorageOwnership;
impl Check for StorageOwnership {
    fn id(&self) -> &'static str {
        "storage-ownership"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.foreign_owned.is_some())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter_map(|p| p.foreign_owned.as_ref().map(|f| (p, f)))
            .map(|(p, (count, example, uid))| {
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: {} storage file(s) owned by another user", p.name, count),
                    format!(
                        "e.g. {example} is owned by uid {uid}. Laravel will fail with \
                         \"laravel.log could not be opened: Permission denied\", and your \
                         editor cannot save these files. Typical causes: an installer run \
                         with sudo, or a container that started without WWWUSER mapping."
                    ),
                );
                f.repair = repair_spec(REPAIR_CHOWN_STORAGE, None);
                for_project(f, p)
            })
            .collect()
    }
}

/// PHP runtime coherence for Sail-shaped builds. Two traps from the tracker:
/// a Sail update removes the runtime a committed compose file still builds
/// from (PHP 7.4/8.0 removals), and a half-done version switch — context
/// changed, image tag not, or vice versa — that builds one PHP and labels it
/// as another (laravel/sail#442's afternoon-eating shape).
struct PhpRuntime;
impl Check for PhpRuntime {
    fn id(&self) -> &'static str {
        "php-runtime"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| !p.sail_builds.is_empty())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut findings = Vec::new();
        for p in &ctx.projects {
            for b in &p.sail_builds {
                if !b.context_exists {
                    let available = if p.available_runtimes.is_empty() {
                        "run composer install first".to_string()
                    } else {
                        format!("available: {}", p.available_runtimes.join(", "))
                    };
                    findings.push(for_project(
                        finding(
                            self.id(),
                            Severity::Error,
                            format!(
                                "{}: \"{}\" builds from a runtime that does not exist",
                                p.name, b.service
                            ),
                            format!(
                                "build.context is {} but that directory is missing — a Sail \
                                 update has likely dropped PHP {} ({available}). Point the \
                                 context (and the image tag) at an available series, then \
                                 rebuild without cache.",
                                b.context, b.context_series
                            ),
                        ),
                        p,
                    ));
                } else if let Some(image_series) = &b.image_series
                    && *image_series != b.context_series
                {
                    findings.push(for_project(
                        finding(
                            self.id(),
                            Severity::Warning,
                            format!(
                                "{}: \"{}\" builds PHP {} but tags it sail-{}/app",
                                p.name, b.service, b.context_series, image_series
                            ),
                            "A PHP version switch changes BOTH build.context and the image \
                             tag; half-done, the container runs a different PHP than \
                             everything believes. Align them, then rebuild without cache.",
                        ),
                        p,
                    ));
                }
            }
        }
        findings
    }
}

fn version_series(text: &str) -> Vec<u32> {
    text.trim()
        .trim_start_matches('v')
        .split('.')
        .map_while(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok()
        })
        .collect()
}

/// Known-bad Docker/Compose version combinations that break Sail's default
/// file shapes — each one a real regression wave from the issue tracker.
struct ComposeQuirks;
impl Check for ComposeQuirks {
    fn id(&self) -> &'static str {
        "compose-quirks"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.compose_version.is_some() || ctx.system.docker_server_version.is_some()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut findings = Vec::new();
        let dotted: Vec<String> = ctx
            .projects
            .iter()
            .flat_map(|p| p.dotted_services.iter().map(move |s| format!("{} ({s})", p.name)))
            .collect();

        if let Some(compose) = ctx.system.compose_version.as_deref() {
            let v = version_series(compose);
            // compose 2.24.0/2.24.1 rejected dotted service names outright
            // (docker/compose#11336) — Sail's default is `laravel.test`.
            if (v == [2, 24] || v == [2, 24, 0] || v == [2, 24, 1]) && !dotted.is_empty() {
                findings.push(finding(
                    self.id(),
                    Severity::Error,
                    format!("Compose {compose} rejects dotted service names"),
                    format!(
                        "This compose release refuses names like Sail's `laravel.test` \
                         (\"expected a map\" errors). Affected: {}. Upgrade to compose \
                         2.24.2 or newer.",
                        dotted.join(", ")
                    ),
                ));
            }
            // Buildx Bake (default in Docker Desktop 4.40 / compose ≥ 2.34)
            // mangled dotted names until 2.37.1.
            if v >= vec![2, 34] && v < vec![2, 37, 1] && !dotted.is_empty() {
                findings.push(finding(
                    self.id(),
                    Severity::Warning,
                    format!("Compose {compose} + Bake can reject dotted service names"),
                    format!(
                        "If builds fail with `invalid name; only [a-zA-Z0-9_-]+ allowed`, \
                         set COMPOSE_BAKE=false or upgrade compose to 2.37.1+. Affected: {}.",
                        dotted.join(", ")
                    ),
                ));
            }
        }
        if let Some(server) = ctx.system.docker_server_version.as_deref() {
            let v = version_series(server);
            if !v.is_empty() && v < vec![20, 10] {
                findings.push(finding(
                    self.id(),
                    Severity::Warning,
                    format!("Docker Engine {server} predates host-gateway"),
                    "`extra_hosts: host.docker.internal:host-gateway` (which Sail's Xdebug \
                     and share recipes rely on) needs Engine 20.10+.",
                ));
            }
        }
        findings
    }
}

struct WwwUserParity;
impl Check for WwwUserParity {
    fn id(&self) -> &'static str {
        "wwwuser-parity"
    }
    /// Rootless docker makes the mapping redundant, so skip the nag there.
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.rootless != Some(true)
            && ctx.projects.iter().any(|p| p.sail_flavored && p.env_present)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let (uid, gid) = (ctx.system.uid, ctx.system.gid);
        ctx.projects
            .iter()
            .filter(|p| p.sail_flavored && p.env_present)
            .filter(|p| !crate::wwwuser_repair_edits(p, uid, gid).is_empty())
            .map(|p| {
                let current = match (&p.wwwuser, &p.wwwgroup) {
                    (None, None) => "WWWUSER/WWWGROUP are not set".to_string(),
                    (u, g) => format!(
                        "WWWUSER={} WWWGROUP={}",
                        u.as_deref().unwrap_or("(unset)"),
                        g.as_deref().unwrap_or("(unset)")
                    ),
                };
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: container user does not match yours", p.name),
                    format!(
                        "{current}, but you are uid {uid} / gid {gid}. Files created inside \
                         the container will be owned by the wrong user."
                    ),
                );
                f.repair = Some(RepairSpec {
                    id: REPAIR_SET_WWWUSER,
                    title: format!("Set WWWUSER={uid} and WWWGROUP={gid} in .env"),
                    risk: RiskTier::Safe,
                    description: "Edits `.env` through the transactional writer (previewed, \
                                  backed up, byte-exact outside the edit)."
                        .into(),
                    arg: None,
                });
                for_project(f, p)
            })
            .collect()
    }
}

struct EnvValidation;
impl Check for EnvValidation {
    fn id(&self) -> &'static str {
        "env-validation"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.env_present)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.env_error_count > 0)
            .map(|p| {
                for_project(
                    finding(
                        self.id(),
                        Severity::Warning,
                        format!(
                            "{}: {} problem{} in .env",
                            p.name,
                            p.env_error_count,
                            if p.env_error_count == 1 { "" } else { "s" }
                        ),
                        "Open the project's Env panel for the full list with per-key details.",
                    ),
                    p,
                )
            })
            .collect()
    }
}

struct PortConflicts;
impl Check for PortConflicts {
    fn id(&self) -> &'static str {
        "port-conflicts"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.len() > 1
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut by_port: std::collections::BTreeMap<u16, Vec<(&ProjectFacts, &str)>> =
            Default::default();
        for p in &ctx.projects {
            for (key, port) in &p.host_ports {
                by_port.entry(*port).or_default().push((p, key.as_str()));
            }
        }
        by_port
            .into_iter()
            .filter(|(_, users)| users.len() > 1)
            .map(|(port, users)| {
                let list = users
                    .iter()
                    .map(|(p, key)| format!("{} ({key})", p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("Host port {port} is claimed by multiple projects"),
                    format!("{list} — they cannot run at the same time until one changes."),
                );
                // Offer to move one of them. A stopped project is the polite
                // choice: moving a port under a running stack would only take
                // effect on its next start, and would misdescribe what is
                // published right now.
                if let Some((mover, _)) = users
                    .iter()
                    .find(|(p, key)| !p.running && mast_laravel::is_host_port_key(key))
                {
                    f.project = Some(mover.id.clone());
                    f.detail = format!(
                        "{list} — they cannot run at the same time until one changes. \
                         {} is stopped, so its ports are the ones that can be moved.",
                        mover.name
                    );
                    f.repair = repair_spec(REPAIR_REASSIGN_PORTS, None);
                }
                f
            })
            .collect()
    }
}

struct ExternalNetworks;
impl Check for ExternalNetworks {
    fn id(&self) -> &'static str {
        "external-networks"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.docker_networks.is_some()
            && ctx.projects.iter().any(|p| !p.external_networks.is_empty())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let Some(existing) = &ctx.docker_networks else { return Vec::new() };
        let mut findings = Vec::new();
        for p in &ctx.projects {
            for net in &p.external_networks {
                if existing.iter().any(|n| n == net) {
                    continue;
                }
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: external network \"{net}\" does not exist", p.name),
                    "The compose file marks this network `external`, so compose will refuse \
                     to start until it exists.",
                );
                f.repair = Some(RepairSpec {
                    id: REPAIR_CREATE_NETWORK,
                    title: format!("Create docker network \"{net}\""),
                    risk: RiskTier::Safe,
                    description: format!("Runs `docker network create {net}` (idempotent)."),
                    arg: Some(net.clone()),
                });
                findings.push(for_project(f, p));
            }
        }
        findings
    }
}

struct ResolutionErrors;
impl Check for ResolutionErrors {
    fn id(&self) -> &'static str {
        "compose-resolution"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        !ctx.projects.is_empty()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter_map(|p| {
                p.resolution_error.as_ref().map(|err| {
                    for_project(
                        finding(
                            self.id(),
                            Severity::Error,
                            format!("{}: compose configuration is invalid", p.name),
                            err.clone(),
                        ),
                        p,
                    )
                })
            })
            .collect()
    }
}

struct WorkspaceIntegrity;
impl Check for WorkspaceIntegrity {
    fn id(&self) -> &'static str {
        "workspace-integrity"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        !ctx.workspace_issues.is_empty()
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.workspace_issues
            .iter()
            .map(|(name, err)| {
                finding(
                    self.id(),
                    Severity::Error,
                    format!("Workspace \"{name}\" cannot start"),
                    format!("{err} — open the workspace's Edit dialog to fix it."),
                )
            })
            .collect()
    }
}

pub fn all_checks() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(DockerRunning),
        Box::new(ComposeV2),
        Box::new(SocketAccess),
        Box::new(RootlessInfo),
        Box::new(SnapDocker),
        Box::new(DiskSpace),
        Box::new(SelinuxInfo),
        Box::new(ContextSanity),
        Box::new(EnvMissing),
        Box::new(VendorMissing),
        Box::new(NodeModulesMissing),
        Box::new(AmbiguousLockfiles),
        Box::new(SailMissing),
        Box::new(AppKeyMissing),
        Box::new(StorageLinkMissing),
        Box::new(DbCredentials),
        Box::new(DbVolumeVersion),
        Box::new(ConfigCache),
        Box::new(ComposeQuirks),
        Box::new(PhpRuntime),
        Box::new(StorageOwnership),
        Box::new(ConfigDrift),
        Box::new(WwwUserParity),
        Box::new(EnvValidation),
        Box::new(PortConflicts),
        Box::new(ExternalNetworks),
        Box::new(ResolutionErrors),
        Box::new(WorkspaceIntegrity),
    ]
}

/// Run every applicable check; returns (checks run, findings).
pub fn run_all(ctx: &DiagCtx) -> (usize, Vec<Finding>) {
    let mut findings = Vec::new();
    let mut ran = 0;
    for check in all_checks() {
        if !check.applies(ctx) {
            continue;
        }
        ran += 1;
        findings.extend(check.run(ctx));
    }
    // Errors first, then warnings — the order the user should read them in.
    findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    (ran, findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SocketFacts, SystemFacts};

    fn healthy_system() -> SystemFacts {
        SystemFacts {
            docker_connected: true,
            docker_error: None,
            endpoint: Some("unix:///var/run/docker.sock".into()),
            context_name: Some("default".into()),
            docker_host_env: false,
            compose_version: Some("2.29.0".into()),
            docker_server_version: Some("29.7.2".into()),
            socket: None,
            rootless: Some(false),
            snap_docker: false,
            disk_free_bytes: Some(50 * 1024 * 1024 * 1024),
            selinux_enforcing: false,
            uid: 1000,
            gid: 1000,
        }
    }

    fn sail_project(id: &str) -> ProjectFacts {
        ProjectFacts {
            id: id.into(),
            name: id.into(),
            sail_flavored: true,
            is_laravel: true,
            has_compose: true,
            has_vendor: true,
            package_manager: Some("npm".into()),
            node_lockfile: true,
            has_node_modules: true,
            conflicting_lockfiles: Vec::new(),
            env_present: true,
            env_example_present: true,
            app_key_empty: false,
            storage_link_missing: false,
            wwwuser: Some("1000".into()),
            wwwgroup: Some("1000".into()),
            env_error_count: 0,
            host_ports: Vec::new(),
            running: false,
            external_networks: Vec::new(),
            resolution_error: None,
            db: None,
            db_versions: Vec::new(),
            config_cached: false,
            dotted_services: Vec::new(),
            sail_builds: Vec::new(),
            available_runtimes: Vec::new(),
            foreign_owned: None,
            drifted_services: Vec::new(),
        }
    }

    fn db_facts(failure: Option<ProbeFailure>, admin_access: bool) -> crate::DbProbeFacts {
        crate::DbProbeFacts {
            service: "mysql".into(),
            kind: mast_laravel::db::DbKind::Mysql,
            database: "laravel".into(),
            username: "sail".into(),
            failure,
            admin_access,
        }
    }

    fn ctx(projects: Vec<ProjectFacts>) -> DiagCtx {
        DiagCtx {
            system: healthy_system(),
            projects,
            docker_networks: Some(vec!["bridge".into()]),
            workspace_issues: Vec::new(),
        }
    }

    #[test]
    fn healthy_context_yields_no_findings() {
        let (ran, findings) = run_all(&ctx(vec![sail_project("a")]));
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
        assert!(ran >= 5);
    }

    #[test]
    fn docker_down_is_an_error_and_socket_check_engages() {
        let mut c = ctx(vec![]);
        c.system.docker_connected = false;
        c.system.docker_error = Some("connection refused".into());
        c.system.socket = Some(SocketFacts {
            path: "/var/run/docker.sock".into(),
            exists: true,
            writable: false,
        });
        let (_, findings) = run_all(&c);
        assert!(findings.iter().any(|f| f.check == "docker-running" && f.severity == Severity::Error));
        let socket = findings.iter().find(|f| f.check == "docker-socket").unwrap();
        let repair = socket.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_DOCKER_GROUP);
        assert_eq!(repair.risk, RiskTier::HighRisk);
    }

    #[test]
    fn socket_check_skipped_when_connected() {
        let mut c = ctx(vec![]);
        c.system.socket =
            Some(SocketFacts { path: "/x".into(), exists: false, writable: false });
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "docker-socket"));
    }

    #[test]
    fn compose_v1_and_missing_are_errors() {
        let mut c = ctx(vec![]);
        c.system.compose_version = Some("1.29.2".into());
        let (_, findings) = run_all(&c);
        assert!(findings.iter().any(|f| f.check == "compose-v2" && f.title.contains("v1")));

        c.system.compose_version = None;
        let (_, findings) = run_all(&c);
        assert!(findings.iter().any(|f| f.check == "compose-v2" && f.severity == Severity::Error));
    }

    #[test]
    fn wwwuser_mismatch_offers_safe_env_repair() {
        let mut p = sail_project("a");
        p.wwwuser = Some("33".into());
        let (_, findings) = run_all(&ctx(vec![p.clone()]));
        let f = findings.iter().find(|f| f.check == "wwwuser-parity").unwrap();
        assert_eq!(f.repair.as_ref().unwrap().risk, RiskTier::Safe);
        assert_eq!(f.project.as_deref(), Some("a"));
        assert_eq!(crate::wwwuser_repair_edits(&p, 1000, 1000), vec![("WWWUSER".into(), "1000".into())]);
    }

    #[test]
    fn wwwuser_nag_suppressed_under_rootless() {
        let mut p = sail_project("a");
        p.wwwuser = None;
        let mut c = ctx(vec![p]);
        c.system.rootless = Some(true);
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "wwwuser-parity"));
        // Rootless itself is surfaced as info.
        assert!(findings.iter().any(|f| f.check == "rootless-docker" && f.severity == Severity::Info));
    }

    #[test]
    fn missing_env_offers_copy_only_when_example_exists() {
        let mut a = sail_project("a");
        a.env_present = false;
        let mut b = sail_project("b");
        b.env_present = false;
        b.env_example_present = false;
        let (_, findings) = run_all(&ctx(vec![a, b]));
        let fa = findings.iter().find(|f| f.check == "env-missing" && f.project.as_deref() == Some("a")).unwrap();
        assert_eq!(fa.repair.as_ref().unwrap().id, REPAIR_COPY_ENV_EXAMPLE);
        let fb = findings.iter().find(|f| f.check == "env-missing" && f.project.as_deref() == Some("b")).unwrap();
        assert!(fb.repair.is_none());
    }

    #[test]
    fn empty_app_key_is_an_error_with_a_safe_repair() {
        let mut p = sail_project("a");
        p.app_key_empty = true;
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "app-key-missing").unwrap();
        assert_eq!(f.severity, Severity::Error);
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_GENERATE_APP_KEY);
        assert_eq!(repair.risk, RiskTier::Safe);

        // Not a Laravel project → not our key to mint.
        let mut q = sail_project("b");
        q.app_key_empty = true;
        q.is_laravel = false;
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "app-key-missing"));
    }

    #[test]
    fn db_credential_failures_route_to_the_right_repair() {
        // Admin access alive → live reconcile, Caution.
        let mut p = sail_project("a");
        p.db = Some(db_facts(Some(ProbeFailure::AccessDenied), true));
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "db-credentials").unwrap();
        assert_eq!(f.severity, Severity::Error);
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_DB_RECONCILE);
        assert_eq!(repair.risk, RiskTier::Caution);
        assert_eq!(repair.arg.as_deref(), Some("mysql"));
        assert!(f.detail.contains("volume is first initialized"), "{}", f.detail);

        // No admin access → only the destructive path remains, HighRisk.
        let mut p = sail_project("a");
        p.db = Some(db_facts(Some(ProbeFailure::UnknownDatabase), false));
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "db-credentials").unwrap();
        assert!(f.title.contains("does not exist"), "{}", f.title);
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_DB_RECREATE);
        assert_eq!(repair.risk, RiskTier::HighRisk);

        // A healthy probe emits nothing.
        let mut p = sail_project("a");
        p.db = Some(db_facts(None, false));
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(!findings.iter().any(|f| f.check == "db-credentials"));
    }

    #[test]
    fn db_volume_version_mismatches_split_by_severity() {
        let mut p = sail_project("a");
        p.db_versions = vec![
            crate::DbVersionIssue {
                service: "pgsql".into(),
                image: "postgres:17".into(),
                volume_version: "16".into(),
                verdict: VersionVerdict::WillNotStart { reason: "never in place".into() },
            },
            crate::DbVersionIssue {
                service: "mysql".into(),
                image: "mysql:8.4".into(),
                volume_version: "8.0.32".into(),
                verdict: VersionVerdict::InPlaceUpgrade { note: "irreversible".into() },
            },
        ];
        let (_, findings) = run_all(&ctx(vec![p]));
        let fatal = findings
            .iter()
            .find(|f| f.check == "db-volume-version" && f.severity == Severity::Error)
            .unwrap();
        assert!(fatal.title.contains("will not start"), "{}", fatal.title);
        assert_eq!(fatal.repair.as_ref().unwrap().id, REPAIR_DB_RECREATE);
        assert_eq!(fatal.repair.as_ref().unwrap().arg.as_deref(), Some("pgsql"));
        assert!(fatal.detail.contains("revert the image"), "{}", fatal.detail);
        let upgrade = findings
            .iter()
            .find(|f| f.check == "db-volume-version" && f.severity == Severity::Warning)
            .unwrap();
        assert!(upgrade.repair.is_none(), "an in-place upgrade is a heads-up, not a repair");
    }

    #[test]
    fn cached_config_warns_with_the_clear_repair() {
        let mut p = sail_project("a");
        p.config_cached = true;
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "config-cache").unwrap();
        assert_eq!(f.repair.as_ref().unwrap().id, REPAIR_CONFIG_CLEAR);
        assert_eq!(f.repair.as_ref().unwrap().risk, RiskTier::Safe);

        let mut q = sail_project("b");
        q.config_cached = true;
        q.is_laravel = false;
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "config-cache"));
    }

    #[test]
    fn drifted_containers_warn_naming_only_the_stale_services() {
        let mut p = sail_project("a");
        p.drifted_services = vec!["laravel.test".into(), "mysql".into()];
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "config-drift").unwrap();
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.detail.contains("laravel.test, mysql"), "{}", f.detail);
        assert!(f.detail.contains("restart changes nothing"), "{}", f.detail);
        assert!(f.repair.is_none(), "the Start button is the repair");
    }

    #[test]
    fn foreign_owned_storage_warns_with_the_containerized_chown() {
        let mut p = sail_project("a");
        p.foreign_owned = Some((17, "storage/logs/laravel.log".into(), 0));
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "storage-ownership").unwrap();
        assert!(f.title.contains("17 storage file(s)"), "{}", f.title);
        assert!(f.detail.contains("uid 0"), "{}", f.detail);
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_CHOWN_STORAGE);
        assert_eq!(repair.risk, RiskTier::Caution);
    }

    #[test]
    fn php_runtime_issues_split_missing_context_from_mismatched_tag() {
        let mut p = sail_project("a");
        p.available_runtimes = vec!["8.3".into(), "8.4".into()];
        p.sail_builds = vec![
            crate::SailBuildFacts {
                service: "laravel.test".into(),
                context: "./vendor/laravel/sail/runtimes/8.0".into(),
                context_series: "8.0".into(),
                context_exists: false,
                image_series: Some("8.0".into()),
            },
            crate::SailBuildFacts {
                service: "worker".into(),
                context: "./vendor/laravel/sail/runtimes/8.4".into(),
                context_series: "8.4".into(),
                context_exists: true,
                image_series: Some("8.2".into()),
            },
        ];
        let (_, findings) = run_all(&ctx(vec![p]));
        let missing = findings
            .iter()
            .find(|f| f.check == "php-runtime" && f.severity == Severity::Error)
            .unwrap();
        assert!(missing.detail.contains("8.3, 8.4"), "{}", missing.detail);
        let mismatch = findings
            .iter()
            .find(|f| f.check == "php-runtime" && f.severity == Severity::Warning)
            .unwrap();
        assert!(mismatch.title.contains("builds PHP 8.4 but tags it sail-8.2/app"), "{}", mismatch.title);

        // A coherent build says nothing.
        let mut q = sail_project("b");
        q.sail_builds = vec![crate::SailBuildFacts {
            service: "laravel.test".into(),
            context: "./vendor/laravel/sail/runtimes/8.4".into(),
            context_series: "8.4".into(),
            context_exists: true,
            image_series: Some("8.4".into()),
        }];
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "php-runtime"));
    }

    #[test]
    fn compose_quirks_fire_only_on_known_bad_combinations() {
        let dotted = || {
            let mut p = sail_project("a");
            p.dotted_services = vec!["laravel.test".into()];
            p
        };
        // 2.24.0 + dotted name = the docker/compose#11336 wave.
        let mut c = ctx(vec![dotted()]);
        c.system.compose_version = Some("2.24.0".into());
        let (_, findings) = run_all(&c);
        let f = findings
            .iter()
            .find(|f| f.check == "compose-quirks" && f.severity == Severity::Error)
            .unwrap();
        assert!(f.detail.contains("laravel.test"), "{}", f.detail);

        // Bake window: warning with the COMPOSE_BAKE escape hatch.
        let mut c = ctx(vec![dotted()]);
        c.system.compose_version = Some("2.35.1".into());
        let (_, findings) = run_all(&c);
        let f = findings.iter().find(|f| f.check == "compose-quirks").unwrap();
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.detail.contains("COMPOSE_BAKE=false"), "{}", f.detail);

        // Same versions without dotted names: silence.
        let mut c = ctx(vec![sail_project("a")]);
        c.system.compose_version = Some("2.24.0".into());
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "compose-quirks"));

        // Fixed versions with dotted names: silence.
        let mut c = ctx(vec![dotted()]);
        c.system.compose_version = Some("2.37.1".into());
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "compose-quirks"));

        // Ancient engine: host-gateway heads-up.
        let mut c = ctx(vec![]);
        c.system.docker_server_version = Some("19.03.15".into());
        let (_, findings) = run_all(&c);
        let f = findings.iter().find(|f| f.check == "compose-quirks").unwrap();
        assert!(f.detail.contains("host-gateway"), "{}", f.detail);
    }

    #[test]
    fn missing_storage_link_warns_with_a_safe_repair() {
        let mut p = sail_project("a");
        p.storage_link_missing = true;
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "storage-link").unwrap();
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.repair.as_ref().unwrap().id, REPAIR_STORAGE_LINK);
        assert_eq!(f.repair.as_ref().unwrap().risk, RiskTier::Safe);
    }

    #[test]
    fn laravel_without_sail_offers_containerized_sail_install() {
        let mut p = sail_project("a");
        p.sail_flavored = false;
        p.has_compose = false;
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "sail-missing").unwrap();
        assert_eq!(f.repair.as_ref().unwrap().id, REPAIR_SAIL_INSTALL);
        assert_eq!(f.repair.as_ref().unwrap().risk, RiskTier::Caution);

        // A plain compose project that is not Laravel is none of our business.
        let mut q = sail_project("b");
        q.is_laravel = false;
        q.sail_flavored = false;
        q.has_compose = false;
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "sail-missing"));
    }

    #[test]
    fn vendorless_sail_clone_offers_containerized_install() {
        let mut p = sail_project("a");
        p.has_vendor = false;
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "vendor-missing").unwrap();
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_COMPOSER_INSTALL);
        assert_eq!(repair.risk, RiskTier::Caution);
    }

    #[test]
    fn missing_node_modules_offers_the_repos_own_manager() {
        let mut p = sail_project("a");
        p.has_node_modules = false;
        p.package_manager = Some("pnpm".into());
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "node-modules-missing").unwrap();
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_NODE_INSTALL);
        assert_eq!(repair.arg.as_deref(), Some("pnpm"));
        assert_eq!(repair.risk, RiskTier::Caution);
        // The repair names the manager, so the user is not offered "npm" on a
        // pnpm repo.
        assert!(repair.title.contains("pnpm"), "{}", repair.title);
    }

    #[test]
    fn a_project_without_package_json_is_not_asked_to_install_anything() {
        let mut p = sail_project("a");
        p.package_manager = None;
        p.has_node_modules = false;
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(!findings.iter().any(|f| f.check == "node-modules-missing"));
    }

    #[test]
    fn competing_lockfiles_warn_without_offering_a_repair() {
        let mut p = sail_project("a");
        p.conflicting_lockfiles = vec!["pnpm-lock.yaml".into(), "package-lock.json".into()];
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "lockfile-ambiguous").unwrap();
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.repair.is_none());
        assert!(f.detail.contains("pnpm-lock.yaml"));
    }

    #[test]
    fn cross_project_port_conflicts_reported_once_per_port() {
        let mut a = sail_project("a");
        a.host_ports = vec![("APP_PORT".into(), 80), ("FORWARD_DB_PORT".into(), 3306)];
        let mut b = sail_project("b");
        b.host_ports = vec![("APP_PORT".into(), 80)];
        let (_, findings) = run_all(&ctx(vec![a, b]));
        let conflicts: Vec<_> = findings.iter().filter(|f| f.check == "port-conflicts").collect();
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].title.contains("80"));
    }

    #[test]
    fn the_port_conflict_repair_targets_a_stopped_claimant() {
        let mut a = sail_project("a");
        a.host_ports = vec![("APP_PORT".into(), 80)];
        a.running = true;
        let mut b = sail_project("b");
        b.host_ports = vec![("APP_PORT".into(), 80)];
        let (_, findings) = run_all(&ctx(vec![a, b]));
        let f = findings.iter().find(|f| f.check == "port-conflicts").unwrap();
        // `a` is up and publishing 80; `b` is the one that can move.
        assert_eq!(f.project.as_deref(), Some("b"));
        assert_eq!(f.repair.as_ref().map(|r| r.id), Some(REPAIR_REASSIGN_PORTS));
        assert!(f.detail.contains("b is stopped"), "{}", f.detail);
    }

    #[test]
    fn a_port_no_env_key_governs_is_reported_without_a_repair() {
        let mut a = sail_project("a");
        a.host_ports = vec![("minio".into(), 9000)];
        let mut b = sail_project("b");
        b.host_ports = vec![("minio".into(), 9000)];
        let (_, findings) = run_all(&ctx(vec![a, b]));
        let f = findings.iter().find(|f| f.check == "port-conflicts").unwrap();
        assert!(f.repair.is_none(), "nothing in .env would move it");
    }

    #[test]
    fn missing_external_network_offers_create_with_arg() {
        let mut p = sail_project("a");
        p.external_networks = vec!["mast-shared".into(), "bridge".into()];
        let (_, findings) = run_all(&ctx(vec![p]));
        let nets: Vec<_> = findings.iter().filter(|f| f.check == "external-networks").collect();
        assert_eq!(nets.len(), 1, "existing network must not be reported");
        let repair = nets[0].repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_CREATE_NETWORK);
        assert_eq!(repair.arg.as_deref(), Some("mast-shared"));
    }

    #[test]
    fn network_check_skipped_when_docker_down() {
        let mut p = sail_project("a");
        p.external_networks = vec!["mast-shared".into()];
        let mut c = ctx(vec![p]);
        c.docker_networks = None;
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "external-networks"));
    }

    #[test]
    fn findings_sorted_errors_first() {
        let mut a = sail_project("a");
        a.has_vendor = false; // error
        a.wwwuser = Some("33".into()); // warning
        let mut c = ctx(vec![a]);
        c.system.rootless = Some(true); // info (and suppresses wwwuser)
        c.system.snap_docker = true; // warning
        let (_, findings) = run_all(&c);
        let severities: Vec<_> = findings.iter().map(|f| f.severity).collect();
        let mut sorted = severities.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(severities, sorted);
    }

    #[test]
    fn workspace_issues_surface_as_errors() {
        let mut c = ctx(vec![]);
        c.workspace_issues = vec![("stack".into(), "dependency cycle: a → b → a".into())];
        let (_, findings) = run_all(&c);
        let f = findings.iter().find(|f| f.check == "workspace-integrity").unwrap();
        assert!(f.title.contains("stack"));
        assert_eq!(f.severity, Severity::Error);
    }
}
