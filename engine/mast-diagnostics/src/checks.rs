//! The check set. Each check is pure over `DiagCtx`; a check that finds
//! nothing wrong emits no findings (the report's "checks run" count is what
//! tells the user everything passed).

use crate::{
    repair_spec, Check, DiagCtx, Finding, ProjectFacts, RepairSpec, RiskTier, Severity,
    REPAIR_ADD_HOST_GATEWAY, REPAIR_CHOWN_STORAGE, REPAIR_COMPOSER_INSTALL, REPAIR_CONFIG_CLEAR,
    REPAIR_COPY_ENV_EXAMPLE, REPAIR_CREATE_NETWORK, REPAIR_DB_RECONCILE, REPAIR_DB_RECREATE,
    REPAIR_DOCKER_GROUP, REPAIR_GENERATE_APP_KEY, REPAIR_MIGRATE_MAILPIT, REPAIR_NODE_INSTALL,
    REPAIR_NORMALIZE_ENV_EOL, REPAIR_REASSIGN_PORTS, REPAIR_REMOVE_HOT, REPAIR_SAIL_INSTALL,
    REPAIR_SET_PROJECT_NAME, REPAIR_SET_WWWUSER, REPAIR_STORAGE_LINK,
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

/// Same semantics as `mast_compose::catalog::image_matches`, duplicated
/// here because this crate deliberately carries no compose dependency (the
/// check set stays pure over gathered facts): strip the tag (not a registry
/// port), then match the stem exactly or as a path suffix.
fn image_stem_matches(image: &str, stem: &str) -> bool {
    let name = match image.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => name,
        _ => image,
    };
    name == stem || name.ends_with(&format!("/{stem}"))
}

/// Two projects sharing an identity: the same built image tag (every Sail
/// app builds `sail-X.Y/app`, so builds overwrite each other — the theme
/// where users defect to DDEV) or the same compose project name (containers
/// and volumes collide outright).
struct IdentityCollision;
impl Check for IdentityCollision {
    fn id(&self) -> &'static str {
        "identity-collision"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.len() > 1
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut findings = Vec::new();
        // Shared compose project names: catastrophic, containers merge.
        let mut by_name: std::collections::BTreeMap<&str, Vec<&ProjectFacts>> = Default::default();
        for p in &ctx.projects {
            if let Some(name) = p.compose_name.as_deref() {
                by_name.entry(name).or_default().push(p);
            }
        }
        for (name, projects) in by_name {
            if projects.len() > 1 {
                let list =
                    projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
                findings.push(finding(
                    self.id(),
                    Severity::Error,
                    format!("Compose project name \"{name}\" is shared"),
                    format!(
                        "{list} all resolve to the same compose project, so their \
                         containers, networks and volumes collide — starting one adopts \
                         or destroys the other's. Pin COMPOSE_PROJECT_NAME in each \
                         project's .env."
                    ),
                ));
            }
        }
        // Shared Sail image tags: whichever builds last wins.
        let mut by_image: std::collections::BTreeMap<&str, Vec<&ProjectFacts>> = Default::default();
        for p in &ctx.projects {
            for (_, image) in &p.service_images {
                if image.starts_with("sail-") && image.contains("/app") {
                    by_image.entry(image.as_str()).or_default().push(p);
                }
            }
        }
        for (image, mut projects) in by_image {
            projects.dedup_by(|a, b| a.id == b.id);
            if projects.len() > 1 {
                let list =
                    projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
                findings.push(finding(
                    self.id(),
                    Severity::Warning,
                    format!("Image \"{image}\" is built by more than one project"),
                    format!(
                        "{list} all build and run \"{image}\" — every build overwrites \
                         the others' image (laravel/sail#649), which breaks projects with \
                         customized runtimes. Give each project its own tag by editing \
                         `image:` (e.g. \"myapp-8.4/app\")."
                    ),
                ));
            }
        }
        findings
    }
}

/// A compose project name derived from a directory whose basename carries
/// characters compose versions keep tripping over (dots, spaces — the
/// "Invalid name; only [a-zA-Z0-9_-]" wave, laravel/sail#481/#786).
struct ProjectNameShape;
impl Check for ProjectNameShape {
    fn id(&self) -> &'static str {
        "project-name"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.name_from_dir)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| {
                p.name_from_dir
                    && !p.dir_basename.is_empty()
                    && !p
                        .dir_basename
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
            .map(|p| {
                let suggested = normalize_project_name(&p.dir_basename);
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: the directory name decides the compose project name", p.name),
                    format!(
                        "\"{}\" contains characters some Docker/compose releases reject \
                         (\"Invalid name; only [a-zA-Z0-9_-] are allowed\") or normalize \
                         differently across versions, silently changing which containers \
                         belong to this project. Pin it explicitly.",
                        p.dir_basename
                    ),
                );
                f.repair = repair_spec(REPAIR_SET_PROJECT_NAME, Some(&suggested));
                for_project(f, p)
            })
            .collect()
    }
}

/// Duplicate of `mast_compose::normalize_project_name` (this crate carries
/// no compose dependency): lowercase, strip anything outside [a-z0-9_-],
/// trim leading separators.
fn normalize_project_name(raw: &str) -> String {
    let cleaned: String = raw
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')
        .collect();
    let trimmed = cleaned.trim_start_matches(['-', '_']).to_string();
    if trimmed.is_empty() { "app".to_string() } else { trimmed }
}

/// PHP_CLI_SERVER_WORKERS is silently ignored under Sail since Laravel
/// 11.45 (framework #56922): `serve` drops it when LARAVEL_SAIL is set, the
/// dev server runs single-worker, and any request the app makes to itself
/// deadlocks with no error anywhere.
struct CliServerWorkers;
impl Check for CliServerWorkers {
    fn id(&self) -> &'static str {
        "cli-server-workers"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.sail_flavored && p.cli_server_workers)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.sail_flavored && p.cli_server_workers)
            .map(|p| {
                for_project(
                    finding(
                        self.id(),
                        Severity::Warning,
                        format!("{}: PHP_CLI_SERVER_WORKERS does nothing under Sail", p.name),
                        "Since Laravel 11.45 the serve path ignores this variable when \
                         LARAVEL_SAIL is set (framework #56922), so the dev server runs a \
                         single worker — an HTTP request the app makes to itself deadlocks \
                         silently. Expect one worker, or serve through Octane.",
                    ),
                    p,
                )
            })
            .collect()
    }
}

/// Redis-backed app whose container has no phpredis extension — every redis
/// call throws "Please make sure the PHP Redis extension is installed"
/// (laravel/sail#302). Laravel defaults REDIS_CLIENT to phpredis, so unset
/// counts as phpredis.
struct RedisExtension;
impl Check for RedisExtension {
    fn id(&self) -> &'static str {
        "redis-extension"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.php_modules.is_some())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| {
                let Some(modules) = &p.php_modules else { return false };
                let wants_phpredis =
                    p.redis_client.as_deref().is_none_or(|client| client == "phpredis");
                let has_redis_service = p.service_images.iter().any(|(_, image)| {
                    image_stem_matches(image, "redis") || image_stem_matches(image, "valkey/valkey")
                });
                wants_phpredis && has_redis_service && !modules.iter().any(|m| m == "redis")
            })
            .map(|p| {
                for_project(
                    finding(
                        self.id(),
                        Severity::Error,
                        format!("{}: the app container has no phpredis extension", p.name),
                        "Redis calls will throw \"Please make sure the PHP Redis extension \
                         is installed\". Rebuild the app image without cache; if it \
                         persists, set REDIS_CLIENT=predis and `composer require \
                         predis/predis` instead.",
                    ),
                    p,
                )
            })
            .collect()
    }
}

/// Stub rot: services still running what Sail shipped years ago and has
/// since replaced. The compose file is generated once and never migrated,
/// so projects quietly diverge from every current doc and fix.
struct StubDrift;
impl Check for StubDrift {
    fn id(&self) -> &'static str {
        "stub-drift"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| !p.service_images.is_empty())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut findings = Vec::new();
        for p in &ctx.projects {
            for (service, image) in &p.service_images {
                if image_stem_matches(image, "mailhog/mailhog") || image_stem_matches(image, "mailhog")
                {
                    let mut f = finding(
                        self.id(),
                        Severity::Warning,
                        format!("{}: \"{service}\" still runs MailHog", p.name),
                        "Sail replaced MailHog with Mailpit in 2023 and upstream MailHog is \
                         abandoned — current docs, env keys and fixes all assume Mailpit.",
                    );
                    f.repair = repair_spec(REPAIR_MIGRATE_MAILPIT, Some(service));
                    findings.push(for_project(f, p));
                } else if image_stem_matches(image, "mysql/mysql-server") {
                    findings.push(for_project(
                        finding(
                            self.id(),
                            Severity::Warning,
                            format!(
                                "{}: \"{service}\" runs the abandoned mysql/mysql-server image",
                                p.name
                            ),
                            format!(
                                "Oracle stopped publishing mysql/mysql-server; current Sail \
                                 uses mysql:8.4. Retag \"{service}\" to mysql:8.4 from the \
                                 Services card — the volume guard will walk the data \
                                 upgrade ({image} today)."
                            ),
                        ),
                        p,
                    ));
                }
            }
        }
        findings
    }
}

/// The Xdebug doctor. Every failure mode here looks identical from the
/// browser — breakpoints never hit — which is why no single piece of advice
/// in the tracker ever worked. The ladder names which rung actually broke:
/// the compose file never passes the mode in, Linux can't resolve the
/// default client_host, the image shipped without the extension, or nothing
/// is listening for the connection.
struct Xdebug;
impl Check for Xdebug {
    fn id(&self) -> &'static str {
        "xdebug"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.xdebug.is_some())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        let mut findings = Vec::new();
        for p in &ctx.projects {
            let Some(x) = &p.xdebug else { continue };
            if !x.compose_passes_mode {
                findings.push(for_project(
                    finding(
                        self.id(),
                        Severity::Error,
                        format!(
                            "{}: SAIL_XDEBUG_MODE is set but never reaches the container",
                            p.name
                        ),
                        format!(
                            "The compose file predates Sail's Xdebug wiring — nothing passes \
                             XDEBUG_MODE through. Add under services.{}.environment:\n\
                             XDEBUG_MODE: '${{SAIL_XDEBUG_MODE:-off}}'\n\
                             XDEBUG_CONFIG: '${{SAIL_XDEBUG_CONFIG:-client_host=host.docker.internal}}'",
                            x.app_service
                        ),
                    ),
                    p,
                ));
                continue; // the rungs below assume the mode arrives at all
            }
            if x.env.wants_debug()
                && ctx.system.linux
                && x.env.client_host == "host.docker.internal"
                && !x.compose_has_host_gateway
            {
                let mut f = finding(
                    self.id(),
                    Severity::Error,
                    format!(
                        "{}: Xdebug's client_host cannot resolve on Linux",
                        p.name
                    ),
                    "host.docker.internal does not exist on Linux unless the compose file \
                     maps it (`extra_hosts: host.docker.internal:host-gateway`) — Xdebug \
                     connects to nothing and breakpoints never hit. Files published before \
                     Sail added the mapping are missing it.",
                );
                f.repair = repair_spec(REPAIR_ADD_HOST_GATEWAY, Some(&x.app_service));
                findings.push(for_project(f, p));
            }
            if x.extension_loaded == Some(false) {
                findings.push(for_project(
                    finding(
                        self.id(),
                        Severity::Error,
                        format!("{}: the app container has no xdebug extension", p.name),
                        "php -m inside the container does not list xdebug. Rebuild without \
                         cache; if it still fails, the PPA has not published the extension \
                         for this PHP series yet (a recurring gap right after new PHP \
                         releases).",
                    ),
                    p,
                ));
            }
            if x.env.wants_debug() && x.ide_listening == Some(false) {
                findings.push(for_project(
                    finding(
                        self.id(),
                        Severity::Info,
                        format!(
                            "{}: no debugger is listening on port {}",
                            p.name, x.env.client_port
                        ),
                        "Xdebug connects OUT to your IDE; until something listens \
                         (PhpStorm: \"Start Listening for PHP Debug Connections\", VS Code: \
                         a running launch config), breakpoints cannot hit.",
                    ),
                    p,
                ));
            }
        }
        findings
    }
}

/// CRLF line endings in `.env`. Compose tolerates them for interpolation,
/// but the sail script `source`s the file with bash, so every value grows an
/// invisible `\r` — the "service \"laravel.test\r\" is not running" class of
/// error, usually from a Windows editor or `core.autocrlf=true` checkout.
struct EnvLineEndings;
impl Check for EnvLineEndings {
    fn id(&self) -> &'static str {
        "env-line-endings"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.env_crlf)
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter(|p| p.env_crlf)
            .map(|p| {
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: .env has Windows (CRLF) line endings", p.name),
                    "Sail sources .env with bash, so every value carries an invisible \\r — \
                     the classic symptom is compose reporting a service name with a stray \
                     `\\r` as not running. Usually a Windows editor or a \
                     core.autocrlf=true checkout.",
                );
                f.repair = repair_spec(REPAIR_NORMALIZE_ENV_EOL, None);
                for_project(f, p)
            })
            .collect()
    }
}

/// Docker Desktop on Linux: the daemon lives in a VM whose file sharing maps
/// uids differently than native docker-engine, which silently defeats the
/// WWWUSER scheme Sail's permissions depend on — the community's accepted
/// fix is switching to docker-ce (laravel/sail#548, #459).
struct DockerDesktopLinux;
impl Check for DockerDesktopLinux {
    fn id(&self) -> &'static str {
        "docker-desktop-linux"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.system.linux
            && ctx.system.context_name.as_deref() == Some("desktop-linux")
    }
    fn run(&self, _ctx: &DiagCtx) -> Vec<Finding> {
        vec![finding(
            self.id(),
            Severity::Warning,
            "Docker Desktop for Linux is the active context",
            "Its daemon runs in a VM whose file sharing maps ownership differently than \
             native Docker Engine, so WWWUSER/WWWGROUP parity silently stops working and \
             storage/ permission errors follow. Native docker-engine (docker-ce) with \
             `docker context use default` is the reliable setup on Linux.",
        )]
    }
}

/// A `public/hot` file pointing at a Vite dev server that is not running.
/// Left behind when the dev server is killed abruptly; while it exists,
/// Blade renders dev-server asset URLs, pages load without CSS/JS, and —
/// the confusion that wastes the afternoon — `npm run build` changes
/// nothing, because Laravel never looks at the manifest.
struct ViteHotStale;
impl Check for ViteHotStale {
    fn id(&self) -> &'static str {
        "vite-hot-stale"
    }
    fn applies(&self, ctx: &DiagCtx) -> bool {
        ctx.projects.iter().any(|p| p.vite_hot.is_some())
    }
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding> {
        ctx.projects
            .iter()
            .filter_map(|p| p.vite_hot.as_ref().map(|hot| (p, hot)))
            .filter(|(_, hot)| hot.dev_server_listening == Some(false))
            .map(|(p, hot)| {
                let mut f = finding(
                    self.id(),
                    Severity::Warning,
                    format!("{}: public/hot points at a dead Vite dev server", p.name),
                    format!(
                        "Nothing listens at {}, but while public/hot exists Laravel keeps \
                         rendering dev-server asset URLs — pages load without CSS/JS, \
                         `npm run build` changes nothing, and through a share tunnel the \
                         browser reports Private Network Access/CORS errors. The file is \
                         left behind when the dev server is killed abruptly.",
                        hot.url
                    ),
                );
                f.repair = repair_spec(REPAIR_REMOVE_HOT, None);
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
        Box::new(ViteHotStale),
        Box::new(EnvLineEndings),
        Box::new(DockerDesktopLinux),
        Box::new(Xdebug),
        Box::new(StubDrift),
        Box::new(IdentityCollision),
        Box::new(ProjectNameShape),
        Box::new(CliServerWorkers),
        Box::new(RedisExtension),
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
            linux: true,
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
            vite_hot: None,
            env_crlf: false,
            xdebug: None,
            service_images: Vec::new(),
            compose_name: Some(id.into()),
            dir_basename: id.into(),
            name_from_dir: false,
            php_modules: None,
            cli_server_workers: false,
            redis_client: None,
            app_service: "laravel.test".into(),
        }
    }

    fn xdebug_facts(mode: &str) -> crate::XdebugFacts {
        crate::XdebugFacts {
            env: mast_laravel::xdebug::xdebug_env(&[(
                "SAIL_XDEBUG_MODE".to_string(),
                mode.to_string(),
            )])
            .unwrap(),
            app_service: "laravel.test".into(),
            compose_passes_mode: true,
            compose_has_host_gateway: true,
            ide_listening: None,
            extension_loaded: None,
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
    fn identity_collisions_split_fatal_names_from_image_overwrites() {
        let mut a = sail_project("a");
        a.service_images = vec![("laravel.test".into(), "sail-8.4/app".into())];
        let mut b = sail_project("b");
        b.service_images = vec![("laravel.test".into(), "sail-8.4/app".into())];
        let (_, findings) = run_all(&ctx(vec![a, b]));
        let image = findings
            .iter()
            .find(|f| f.check == "identity-collision" && f.severity == Severity::Warning)
            .unwrap();
        assert!(image.title.contains("sail-8.4/app"), "{}", image.title);
        assert!(image.detail.contains("a, b"), "{}", image.detail);

        // Same compose name: the fatal variant.
        let mut a = sail_project("a");
        a.compose_name = Some("shop".into());
        let mut b = sail_project("b");
        b.compose_name = Some("shop".into());
        let (_, findings) = run_all(&ctx(vec![a, b]));
        let name = findings
            .iter()
            .find(|f| f.check == "identity-collision" && f.severity == Severity::Error)
            .unwrap();
        assert!(name.detail.contains("COMPOSE_PROJECT_NAME"), "{}", name.detail);

        // Distinct everything: silence.
        let (_, findings) = run_all(&ctx(vec![sail_project("a"), sail_project("b")]));
        assert!(!findings.iter().any(|f| f.check == "identity-collision"));
    }

    #[test]
    fn awkward_directory_names_offer_the_pinning_repair() {
        let mut p = sail_project("a");
        p.name_from_dir = true;
        p.dir_basename = "My.Shop App".into();
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "project-name").unwrap();
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_SET_PROJECT_NAME);
        assert_eq!(repair.arg.as_deref(), Some("myshopapp"));

        // An explicitly named project is nobody's business.
        let mut p = sail_project("a");
        p.name_from_dir = false;
        p.dir_basename = "My.Shop App".into();
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(!findings.iter().any(|f| f.check == "project-name"));
        // A clean basename is fine too.
        let mut p = sail_project("a");
        p.name_from_dir = true;
        p.dir_basename = "my-shop".into();
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(!findings.iter().any(|f| f.check == "project-name"));
    }

    #[test]
    fn cli_server_workers_and_redis_extension_checks() {
        let mut p = sail_project("a");
        p.cli_server_workers = true;
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(
            findings
                .iter()
                .any(|f| f.check == "cli-server-workers" && f.detail.contains("#56922"))
        );

        // Redis service + phpredis default + no extension in the container.
        let mut p = sail_project("a");
        p.service_images = vec![("redis".into(), "redis:alpine".into())];
        p.php_modules = Some(vec!["curl".into(), "pdo_mysql".into()]);
        let (_, findings) = run_all(&ctx(vec![p.clone()]));
        assert!(findings.iter().any(|f| f.check == "redis-extension"));

        // predis chosen, extension loaded, or no redis service: silence.
        let mut q = p.clone();
        q.redis_client = Some("predis".into());
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "redis-extension"));
        let mut q = p.clone();
        q.php_modules = Some(vec!["redis".into()]);
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "redis-extension"));
        let mut q = p.clone();
        q.service_images = vec![("mysql".into(), "mysql:8.4".into())];
        let (_, findings) = run_all(&ctx(vec![q]));
        assert!(!findings.iter().any(|f| f.check == "redis-extension"));
    }

    #[test]
    fn stub_drift_flags_mailhog_with_migration_and_dead_mysql_repo() {
        let mut p = sail_project("a");
        p.service_images = vec![
            ("mailhog".into(), "mailhog/mailhog:latest".into()),
            ("mysql".into(), "mysql/mysql-server:8.0".into()),
            ("redis".into(), "redis:alpine".into()),
        ];
        let (_, findings) = run_all(&ctx(vec![p]));
        let drift: Vec<_> = findings.iter().filter(|f| f.check == "stub-drift").collect();
        assert_eq!(drift.len(), 2, "{drift:?}");
        let mailhog = drift.iter().find(|f| f.title.contains("MailHog")).unwrap();
        let repair = mailhog.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_MIGRATE_MAILPIT);
        assert_eq!(repair.arg.as_deref(), Some("mailhog"));
        let mysql = drift.iter().find(|f| f.title.contains("mysql-server")).unwrap();
        assert!(mysql.repair.is_none(), "retag flow owns the fix");
        assert!(mysql.detail.contains("mysql:8.4"), "{}", mysql.detail);
    }

    #[test]
    fn xdebug_ladder_names_the_broken_rung() {
        // Compose never passes the mode: the fatal first rung, and the only
        // finding (the rest assume the mode arrives).
        let mut p = sail_project("a");
        let mut x = xdebug_facts("develop,debug");
        x.compose_passes_mode = false;
        x.compose_has_host_gateway = false;
        p.xdebug = Some(x);
        let (_, findings) = run_all(&ctx(vec![p]));
        let xdebug: Vec<_> = findings.iter().filter(|f| f.check == "xdebug").collect();
        assert_eq!(xdebug.len(), 1, "{xdebug:?}");
        assert!(xdebug[0].detail.contains("XDEBUG_MODE: '${SAIL_XDEBUG_MODE:-off}'"));

        // Linux without host-gateway: Error with the one-click repair.
        let mut p = sail_project("a");
        let mut x = xdebug_facts("debug");
        x.compose_has_host_gateway = false;
        p.xdebug = Some(x);
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings
            .iter()
            .find(|f| f.check == "xdebug" && f.title.contains("client_host"))
            .unwrap();
        let repair = f.repair.as_ref().unwrap();
        assert_eq!(repair.id, REPAIR_ADD_HOST_GATEWAY);
        assert_eq!(repair.arg.as_deref(), Some("laravel.test"));

        // …but not on macOS, and not when client_host was overridden.
        let mut p = sail_project("a");
        let mut x = xdebug_facts("debug");
        x.compose_has_host_gateway = false;
        p.xdebug = Some(x);
        let mut c = ctx(vec![p]);
        c.system.linux = false;
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "xdebug"));

        // Missing extension and silent IDE stack up as separate rungs.
        let mut p = sail_project("a");
        let mut x = xdebug_facts("develop,debug");
        x.extension_loaded = Some(false);
        x.ide_listening = Some(false);
        p.xdebug = Some(x);
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(findings.iter().any(|f| f.check == "xdebug" && f.title.contains("no xdebug")));
        let ide = findings
            .iter()
            .find(|f| f.check == "xdebug" && f.title.contains("no debugger is listening"))
            .unwrap();
        assert_eq!(ide.severity, Severity::Info);
        assert!(ide.title.contains("9003"));

        // Everything wired: silence.
        let mut p = sail_project("a");
        let mut x = xdebug_facts("develop,debug");
        x.ide_listening = Some(true);
        x.extension_loaded = Some(true);
        p.xdebug = Some(x);
        let (_, findings) = run_all(&ctx(vec![p]));
        assert!(!findings.iter().any(|f| f.check == "xdebug"));
    }

    #[test]
    fn crlf_env_warns_with_the_normalize_repair() {
        let mut p = sail_project("a");
        p.env_crlf = true;
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "env-line-endings").unwrap();
        assert!(f.detail.contains("sources .env with bash"), "{}", f.detail);
        assert_eq!(f.repair.as_ref().unwrap().id, REPAIR_NORMALIZE_ENV_EOL);
        assert_eq!(f.repair.as_ref().unwrap().risk, RiskTier::Safe);
    }

    #[test]
    fn docker_desktop_on_linux_warns_and_only_there() {
        let mut c = ctx(vec![]);
        c.system.context_name = Some("desktop-linux".into());
        let (_, findings) = run_all(&c);
        let f = findings.iter().find(|f| f.check == "docker-desktop-linux").unwrap();
        assert!(f.detail.contains("WWWUSER"), "{}", f.detail);

        // Same context on a non-Linux host (macOS ships it too): silence.
        let mut c = ctx(vec![]);
        c.system.context_name = Some("desktop-linux".into());
        c.system.linux = false;
        let (_, findings) = run_all(&c);
        assert!(!findings.iter().any(|f| f.check == "docker-desktop-linux"));
    }

    #[test]
    fn stale_hot_file_warns_but_a_live_dev_server_does_not() {
        let hot = |listening| crate::ViteHotFacts {
            url: "http://[::1]:5173".into(),
            dev_server_listening: listening,
        };
        let mut p = sail_project("a");
        p.vite_hot = Some(hot(Some(false)));
        let (_, findings) = run_all(&ctx(vec![p]));
        let f = findings.iter().find(|f| f.check == "vite-hot-stale").unwrap();
        assert!(f.detail.contains("npm run build` changes nothing"), "{}", f.detail);
        assert_eq!(f.repair.as_ref().unwrap().id, REPAIR_REMOVE_HOT);
        assert_eq!(f.repair.as_ref().unwrap().risk, RiskTier::Safe);

        // A running dev server owns its hot file; an unknowable host says
        // nothing either.
        for listening in [Some(true), None] {
            let mut p = sail_project("a");
            p.vite_hot = Some(hot(listening));
            let (_, findings) = run_all(&ctx(vec![p]));
            assert!(!findings.iter().any(|f| f.check == "vite-hot-stale"), "{listening:?}");
        }
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
