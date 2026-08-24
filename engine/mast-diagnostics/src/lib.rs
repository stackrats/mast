//! Check/Repair framework (plan §8): every check is a pure function over
//! pre-gathered facts, so the whole set is unit-testable without docker or a
//! filesystem. The engine gathers `DiagCtx`, runs the checks, applies repairs
//! through its existing transactional machinery, and records history here
//! (rusqlite, bundled).
//!
//! Risk tiers are deliberate: anything touching docker-group membership or
//! elevation is HighRisk — docker daemon access ≈ host root, so "no host
//! root" is not a safety distinction.

mod checks;
mod history;
pub mod signatures;

pub use checks::{all_checks, run_all};
pub use history::{DiagnosticsDb, DiagnosticsError, RepairAudit, RunSummary};
pub use signatures::{ErrorSignature, classify_line};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    /// Reversible, project-local (env edit, file copy).
    Safe,
    /// Runs containers or touches docker resources, but nothing shared.
    Caution,
    /// Elevation, group membership, ownership changes — explicit consent UI.
    HighRisk,
}

pub const REPAIR_SET_WWWUSER: &str = "set-wwwuser";
pub const REPAIR_COPY_ENV_EXAMPLE: &str = "copy-env-example";
pub const REPAIR_COMPOSER_INSTALL: &str = "composer-install";
pub const REPAIR_CREATE_NETWORK: &str = "create-network";
pub const REPAIR_DOCKER_GROUP: &str = "docker-group";
pub const REPAIR_SAIL_INSTALL: &str = "sail-install";
pub const REPAIR_NODE_INSTALL: &str = "node-install";
pub const REPAIR_REASSIGN_PORTS: &str = "reassign-ports";
pub const REPAIR_RECREATE_SERVICE: &str = "recreate-service";
pub const REPAIR_FIX_APP_URL: &str = "fix-app-url";
pub const REPAIR_ARTISAN_MIGRATE: &str = "artisan-migrate";
pub const REPAIR_GENERATE_APP_KEY: &str = "generate-app-key";
pub const REPAIR_STORAGE_LINK: &str = "storage-link";
pub const REPAIR_DB_RECONCILE: &str = "db-reconcile";
pub const REPAIR_DB_RECREATE: &str = "db-recreate-volume";
pub const REPAIR_CONFIG_CLEAR: &str = "config-clear";
pub const REPAIR_CHOWN_STORAGE: &str = "chown-storage";
pub const REPAIR_REMOVE_HOT: &str = "remove-hot-file";
pub const REPAIR_NORMALIZE_ENV_EOL: &str = "normalize-env-eol";
pub const REPAIR_ADD_HOST_GATEWAY: &str = "add-host-gateway";
pub const REPAIR_MIGRATE_MAILPIT: &str = "migrate-mailpit";
pub const REPAIR_SET_PROJECT_NAME: &str = "set-project-name";
pub const REPAIR_HOSTS_ENTRY: &str = "add-hosts-entry";
pub const REPAIR_DISCONNECT_STALE: &str = "disconnect-stale-endpoints";
pub const REPAIR_TRUST_PROXY_CA: &str = "trust-proxy-ca";
pub const REPAIR_INSTALL_CERTUTIL: &str = "install-certutil";

/// A repair a finding offers. `arg` carries the repair's target when the id
/// alone is ambiguous (e.g. which network to create).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairSpec {
    pub id: &'static str,
    pub title: String,
    pub risk: RiskTier,
    pub description: String,
    pub arg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// Project id when the finding is project-scoped.
    pub project: Option<String>,
    pub repair: Option<RepairSpec>,
}

/// Unix-socket facts, gathered only when the endpoint is `unix://` and local.
#[derive(Debug, Clone, Default)]
pub struct SocketFacts {
    pub path: String,
    pub exists: bool,
    pub writable: bool,
}

/// Why Chromium-family browsers (Chrome, Vivaldi, Brave, Edge) still warn
/// about the local HTTPS proxy on Linux even though the *system* store
/// trusts its certificate authority: they read the NSS user store
/// (`~/.pki/nssdb`), which is a separate step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NssTrustGap {
    /// `certutil` (libnss3-tools / nss-tools) is not installed, so the
    /// trust repair could only fill the system store.
    CertutilMissing,
    /// `certutil` exists but the CA is not in `~/.pki/nssdb` yet —
    /// re-applying the trust repair adds it.
    CaMissing,
}

#[derive(Debug, Clone)]
pub struct SystemFacts {
    pub docker_connected: bool,
    pub docker_error: Option<String>,
    pub endpoint: Option<String>,
    pub context_name: Option<String>,
    /// True when `DOCKER_HOST` is set in the real environment (it silently
    /// beats any named context — ADR-0002).
    pub docker_host_env: bool,
    /// `docker compose version --short`; `None` = command unavailable.
    pub compose_version: Option<String>,
    /// Daemon version from `docker info` (`{{.ServerVersion}}`).
    pub docker_server_version: Option<String>,
    /// Mast itself runs on Linux (some findings only make sense there).
    pub linux: bool,
    pub socket: Option<SocketFacts>,
    pub rootless: Option<bool>,
    pub snap_docker: bool,
    /// Free bytes on the docker data root (local unix endpoints only).
    pub disk_free_bytes: Option<u64>,
    pub selinux_enforcing: bool,
    pub uid: u32,
    pub gid: u32,
    /// The system store trusts the HTTPS proxy's CA but Chromium-family
    /// browsers still would not (Linux; probed only once the trust repair
    /// has filled the system store). `None` = no gap or not applicable.
    pub proxy_nss_gap: Option<NssTrustGap>,
}

/// Outcome of probing the project's database service with the credentials
/// `.env` declares (engine-gathered; only for running projects whose DB_HOST
/// names a compose service).
#[derive(Debug, Clone)]
pub struct DbProbeFacts {
    /// Compose service the probe ran in.
    pub service: String,
    pub kind: mast_laravel::db::DbKind,
    pub database: String,
    pub username: String,
    /// `None` = the `.env` credentials work.
    pub failure: Option<mast_laravel::db::ProbeFailure>,
    /// An administrative login still works on the initialized volume
    /// (probed only after a failure) — the gate between a live reconcile
    /// and a destructive volume recreate.
    pub admin_access: bool,
    /// The database holds Laravel's `migrations` table (probed only when
    /// the credentials work). `Some(false)` is the fresh-bootstrap trap:
    /// every request touching the database 500s on a missing table until
    /// the first `artisan migrate` runs. `None` = not determinable.
    pub migrations_table: Option<bool>,
}

/// A database service whose pinned image version disagrees with what its
/// data volume was written by (engine-gathered from the volume's marker
/// file; only mismatches are recorded).
#[derive(Debug, Clone)]
pub struct DbVersionIssue {
    pub service: String,
    pub image: String,
    /// The version the volume's data was written by, e.g. "10.6.14".
    pub volume_version: String,
    pub verdict: mast_laravel::db::VersionVerdict,
}

/// One Sail-shaped `build:` service from the compose source, with the facts
/// the PHP-runtime consistency check needs.
#[derive(Debug, Clone)]
pub struct SailBuildFacts {
    pub service: String,
    /// The `build.context` as spelled in the file.
    pub context: String,
    /// The PHP series the context path pins ("8.4").
    pub context_series: String,
    /// The context directory exists on disk.
    pub context_exists: bool,
    /// The PHP series the `image:` tag pins (`sail-8.2/app` → "8.2").
    pub image_series: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectFacts {
    pub id: String,
    pub name: String,
    pub sail_flavored: bool,
    /// composer.json requires laravel/framework.
    pub is_laravel: bool,
    pub has_compose: bool,
    pub has_vendor: bool,
    /// The Node package manager the repo itself selects — `packageManager` in
    /// package.json, else the committed lockfile, else npm. `None` when there
    /// is no package.json and the project has no frontend build at all.
    pub package_manager: Option<String>,
    /// A lockfile for `package_manager` is committed, so installs can be
    /// frozen rather than re-resolved.
    pub node_lockfile: bool,
    pub has_node_modules: bool,
    /// Lockfiles for competing managers, when the repo holds more than one.
    pub conflicting_lockfiles: Vec<String>,
    pub env_present: bool,
    pub env_example_present: bool,
    /// `.env` exists but APP_KEY is absent or empty — Laravel 500s on every
    /// request until it is set ("No application encryption key").
    pub app_key_empty: bool,
    /// `storage/app/public` and `public/` both exist but `public/storage`
    /// does not — uploaded-file URLs 404 until `storage:link` runs.
    pub storage_link_missing: bool,
    pub wwwuser: Option<String>,
    pub wwwgroup: Option<String>,
    /// Error-severity findings from the `.env` validator.
    pub env_error_count: usize,
    /// Host ports this project publishes, labelled with the `.env` key that
    /// moves each one — or with a service name when no key does.
    pub host_ports: Vec<(String, u16)>,
    /// At least one container of this project is up. A stopped project is
    /// the one whose ports a conflict repair should move.
    pub running: bool,
    /// Networks the compose model declares as `external: true`.
    pub external_networks: Vec<String>,
    pub resolution_error: Option<String>,
    /// Database credential probe outcome; `None` when nothing was probeable
    /// (stopped project, no Sail-shaped DB config, docker down).
    pub db: Option<DbProbeFacts>,
    /// Database services whose image tag disagrees with their volume's data
    /// version (checked even when stopped — the point is warning BEFORE the
    /// crash-loop).
    pub db_versions: Vec<DbVersionIssue>,
    /// `APP_URL` pins an explicit port the project does not publish while
    /// `APP_PORT` names another — `(url_port, app_port)`. What a port remap
    /// leaves behind when `APP_URL` had the old port written in: the
    /// Browser button opens a refused connection until the URL follows.
    pub app_url_mismatch: Option<(u16, u16)>,
    /// `bootstrap/cache/config.php` exists — Laravel is not reading `.env`.
    pub config_cached: bool,
    /// Compose service names containing a dot (`laravel.test`) — the shape
    /// several compose releases have choked on.
    pub dotted_services: Vec<String>,
    /// Sail-shaped build services (context + image series) for the
    /// PHP-runtime consistency check.
    pub sail_builds: Vec<SailBuildFacts>,
    /// PHP series present under `vendor/laravel/sail/runtimes/`.
    pub available_runtimes: Vec<String>,
    /// Writable-by-Laravel paths (under storage/ and bootstrap/cache) owned
    /// by a uid other than the user's — root from a sudo'd install, 1337
    /// from an unmapped container. (count, example path, example uid);
    /// `None` when nothing foreign was found or the scan does not apply.
    pub foreign_owned: Option<(usize, String, u32)>,
    /// Running services whose containers were created from a different
    /// configuration than the files+`.env` now resolve to (compose
    /// `config-hash` label vs current `config --hash`). A restart will NOT
    /// pick the changes up; only a recreate does.
    pub drifted_services: Vec<String>,
    /// Running services whose containers are attached to no docker network
    /// and publish none of the host ports the model says they should — the
    /// wreckage of a start whose port/network setup failed halfway (say, a
    /// bind lost a race) followed by a plain `docker start`. The container
    /// reports "running" while nothing can reach it, and — unlike drift —
    /// its config-hash still matches, so `up -d` is a no-op: only a
    /// force-recreate reattaches it.
    pub detached_services: Vec<String>,
    /// Vite's `public/hot` marker, when present: the dev-server URL it pins
    /// and whether anything actually listens there (`None` = unknowable,
    /// e.g. a non-loopback host).
    pub vite_hot: Option<ViteHotFacts>,
    /// `.env` has CRLF line endings — sail sources it with bash, so every
    /// value grows an invisible `\r`.
    pub env_crlf: bool,
    /// Xdebug facts, when `.env` requests a mode other than `off`.
    pub xdebug: Option<XdebugFacts>,
    /// (service, image) for every image-based service in the resolved model
    /// — what the stub-drift check reads.
    pub service_images: Vec<(String, String)>,
    /// Resolved compose project name (None when the model is unresolved).
    pub compose_name: Option<String>,
    /// The project directory's basename, and whether the compose project
    /// name is merely derived from it (nothing pins it explicitly).
    pub dir_basename: String,
    pub name_from_dir: bool,
    /// Lowercased `php -m` output from the running app container; `None`
    /// when not probeable. Feeds the Xdebug and Redis extension checks.
    pub php_modules: Option<Vec<String>>,
    /// `.env` sets PHP_CLI_SERVER_WORKERS (silently ignored under Sail
    /// since Laravel 11.45 — framework #56922).
    pub cli_server_workers: bool,
    /// `.env` REDIS_CLIENT value, when set.
    pub redis_client: Option<String>,
    /// The app service (`APP_SERVICE`, default laravel.test) — the target
    /// for in-container probes.
    pub app_service: String,
}

/// Everything the Xdebug doctor needs, per project. Gathered only when
/// SAIL_XDEBUG_MODE requests something.
#[derive(Debug, Clone)]
pub struct XdebugFacts {
    pub env: mast_laravel::xdebug::XdebugEnv,
    /// The compose service the app runs as (APP_SERVICE, default
    /// laravel.test) — the repair's target.
    pub app_service: String,
    /// Some compose source passes XDEBUG_MODE into the container; a file
    /// published before Sail's Xdebug wiring never does.
    pub compose_passes_mode: bool,
    /// Some compose source maps host.docker.internal to host-gateway —
    /// required for the default client_host on Linux.
    pub compose_has_host_gateway: bool,
    /// Something listens on the debugger port on this machine (probed only
    /// when step-debugging is requested); `None` = not probed.
    pub ide_listening: Option<bool>,
    /// `php -m` inside the running app container reports xdebug; `None`
    /// when the container was not probeable.
    pub extension_loaded: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ViteHotFacts {
    pub url: String,
    pub dev_server_listening: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct DiagCtx {
    pub system: SystemFacts,
    pub projects: Vec<ProjectFacts>,
    /// Networks known to the daemon; `None` when docker is unreachable.
    pub docker_networks: Option<Vec<String>>,
    /// (workspace name, graph error) for broken workspaces.
    pub workspace_issues: Vec<(String, String)>,
}

pub trait Check {
    fn id(&self) -> &'static str;
    fn applies(&self, ctx: &DiagCtx) -> bool;
    fn run(&self, ctx: &DiagCtx) -> Vec<Finding>;
}

/// Generic spec for a repair id — the canonical title/risk/description used
/// when previewing or auditing outside a finding's context. `None` for
/// unknown ids.
pub fn repair_spec(id: &str, arg: Option<&str>) -> Option<RepairSpec> {
    match id {
        REPAIR_SET_WWWUSER => Some(RepairSpec {
            id: REPAIR_SET_WWWUSER,
            title: "Set WWWUSER/WWWGROUP to your uid/gid".into(),
            risk: RiskTier::Safe,
            description: "Edits `.env` through the transactional writer.".into(),
            arg: None,
        }),
        REPAIR_COPY_ENV_EXAMPLE => Some(RepairSpec {
            id: REPAIR_COPY_ENV_EXAMPLE,
            title: "Create .env from .env.example".into(),
            risk: RiskTier::Safe,
            description: "Copies `.env.example` to `.env`.".into(),
            arg: None,
        }),
        REPAIR_COMPOSER_INSTALL => Some(RepairSpec {
            id: REPAIR_COMPOSER_INSTALL,
            title: "Run composer install in a container".into(),
            risk: RiskTier::Caution,
            description: "Installs composer dependencies via the official composer image \
                          — no local PHP needed."
                .into(),
            arg: None,
        }),
        REPAIR_CREATE_NETWORK => Some(RepairSpec {
            id: REPAIR_CREATE_NETWORK,
            title: match arg {
                Some(net) => format!("Create docker network \"{net}\""),
                None => "Create docker network".into(),
            },
            risk: RiskTier::Safe,
            description: "Runs `docker network create` (idempotent).".into(),
            arg: arg.map(String::from),
        }),
        REPAIR_DISCONNECT_STALE => Some(RepairSpec {
            id: REPAIR_DISCONNECT_STALE,
            title: match arg {
                Some(net) => format!("Clear stale endpoints from network \"{net}\""),
                None => "Clear stale network endpoints".into(),
            },
            risk: RiskTier::Safe,
            description: "Force-disconnects endpoint records whose container no longer \
                          exists — the residue of force-removed containers, which blocks \
                          `docker compose down`/`up` with \"is not connected to the \
                          network\". Containers that still exist are left untouched."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_NODE_INSTALL => Some(RepairSpec {
            id: REPAIR_NODE_INSTALL,
            title: match arg {
                Some(pm) => format!("Run {pm} install in the app container"),
                None => "Install Node dependencies in the app container".into(),
            },
            risk: RiskTier::Caution,
            description: "Installs the frontend dependencies with the manager this repo \
                          already uses, inside the app container — so native modules are \
                          built against the runtime that will load them."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_SAIL_INSTALL => Some(RepairSpec {
            id: REPAIR_SAIL_INSTALL,
            title: "Install Laravel Sail in a container".into(),
            risk: RiskTier::Caution,
            description: "Runs `composer require laravel/sail --dev` then `php artisan \
                          sail:install` inside the official composer image — no local PHP \
                          needed."
                .into(),
            arg: None,
        }),
        REPAIR_REASSIGN_PORTS => Some(RepairSpec {
            id: REPAIR_REASSIGN_PORTS,
            title: "Move this project's conflicting host ports".into(),
            risk: RiskTier::Safe,
            description: "Writes free port numbers to the `.env` keys that publish the \
                          contested ports (`APP_PORT`, `VITE_PORT`, `FORWARD_*_PORT`), through \
                          the transactional writer. The compose file is untouched."
                .into(),
            arg: None,
        }),
        REPAIR_RECREATE_SERVICE => Some(RepairSpec {
            id: REPAIR_RECREATE_SERVICE,
            title: match arg {
                Some(services) => format!("Recreate {services} from the current configuration"),
                None => "Recreate the unreachable containers".into(),
            },
            risk: RiskTier::Caution,
            description: "Runs compose `up -d --force-recreate --no-deps` on exactly the \
                          named services — the one thing that replaces a running container \
                          whose config-hash still matches. Named volumes and their data are \
                          untouched."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_FIX_APP_URL => Some(RepairSpec {
            id: REPAIR_FIX_APP_URL,
            title: "Point APP_URL at the app's real port".into(),
            risk: RiskTier::Safe,
            description: "Rewrites the port pinned in `APP_URL` to the current `APP_PORT`, \
                          through the transactional env writer — nothing else about the URL \
                          changes."
                .into(),
            arg: None,
        }),
        REPAIR_ARTISAN_MIGRATE => Some(RepairSpec {
            id: REPAIR_ARTISAN_MIGRATE,
            title: "Run the database migrations".into(),
            risk: RiskTier::Caution,
            description: "Runs `php artisan migrate --force` inside the running app service \
                          — the project's own migrations, applied to the database `.env` \
                          names. Creates and alters tables; nothing is dropped or rolled \
                          back."
                .into(),
            arg: None,
        }),
        REPAIR_GENERATE_APP_KEY => Some(RepairSpec {
            id: REPAIR_GENERATE_APP_KEY,
            title: "Generate an APP_KEY".into(),
            risk: RiskTier::Safe,
            description: "Mints a fresh `base64:` key from OS entropy (the same shape \
                          `artisan key:generate` produces) and writes it to `.env` through \
                          the transactional writer. Refuses if APP_KEY is no longer empty."
                .into(),
            arg: None,
        }),
        REPAIR_STORAGE_LINK => Some(RepairSpec {
            id: REPAIR_STORAGE_LINK,
            title: "Link public/storage".into(),
            risk: RiskTier::Safe,
            description: "Creates the `public/storage → ../storage/app/public` symlink as a \
                          relative link, so it resolves both on the host and inside the \
                          container (artisan's default absolute link only works in one)."
                .into(),
            arg: None,
        }),
        REPAIR_CONFIG_CLEAR => Some(RepairSpec {
            id: REPAIR_CONFIG_CLEAR,
            title: "Clear the cached configuration".into(),
            risk: RiskTier::Safe,
            description: "Deletes `bootstrap/cache/config.php` (what `artisan config:clear` \
                          does), so Laravel reads `.env` again."
                .into(),
            arg: None,
        }),
        REPAIR_CHOWN_STORAGE => Some(RepairSpec {
            id: REPAIR_CHOWN_STORAGE,
            title: "Take back ownership of storage/".into(),
            risk: RiskTier::Caution,
            description: "Runs `chown -R <you>` over `storage/` and `bootstrap/cache` — and \
                          nothing else — inside a throwaway root container, so no sudo is \
                          needed on the host."
                .into(),
            arg: None,
        }),
        REPAIR_REMOVE_HOT => Some(RepairSpec {
            id: REPAIR_REMOVE_HOT,
            title: "Remove the stale public/hot file".into(),
            risk: RiskTier::Safe,
            description: "Deletes `public/hot` so Blade serves built assets again. Refuses \
                          if a Vite dev server has meanwhile started listening (a running \
                          dev server owns that file)."
                .into(),
            arg: None,
        }),
        REPAIR_NORMALIZE_ENV_EOL => Some(RepairSpec {
            id: REPAIR_NORMALIZE_ENV_EOL,
            title: "Convert .env to Unix line endings".into(),
            risk: RiskTier::Safe,
            description: "Rewrites CRLF line endings to LF (backup kept, values untouched). \
                          Sail sources `.env` with bash, so CRLF appends an invisible \\r to \
                          every value."
                .into(),
            arg: None,
        }),
        REPAIR_ADD_HOST_GATEWAY => Some(RepairSpec {
            id: REPAIR_ADD_HOST_GATEWAY,
            title: match arg {
                Some(service) => format!("Map host.docker.internal on \"{service}\""),
                None => "Map host.docker.internal to the host".into(),
            },
            risk: RiskTier::Safe,
            description: "Adds `extra_hosts: host.docker.internal:host-gateway` to the app \
                          service through the compose write transaction — what Sail's \
                          current stub ships and files published before it lack. Without \
                          it, Xdebug's default client_host resolves to nothing on Linux."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_SET_PROJECT_NAME => Some(RepairSpec {
            id: REPAIR_SET_PROJECT_NAME,
            title: match arg {
                Some(name) => format!("Pin the compose project name to \"{name}\""),
                None => "Pin the compose project name".into(),
            },
            risk: RiskTier::Caution,
            description: "Writes COMPOSE_PROJECT_NAME to `.env` through the transactional \
                          writer. Apply while the project is STOPPED: the new name is a new \
                          compose identity, so existing containers and named volumes are \
                          left behind under the old name (data is not deleted, but fresh \
                          empty volumes are created on next start)."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_MIGRATE_MAILPIT => Some(RepairSpec {
            id: REPAIR_MIGRATE_MAILPIT,
            title: match arg {
                Some(service) => format!("Replace \"{service}\" with Mailpit"),
                None => "Replace MailHog with Mailpit".into(),
            },
            risk: RiskTier::Caution,
            description: "Removes the MailHog service exactly as it stands and adds Sail's \
                          current Mailpit in the same compose transaction, updating MAIL_* \
                          in `.env`. Mail data is not migrated (both are catch-and-discard \
                          dev mailboxes)."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_DB_RECONCILE => Some(RepairSpec {
            id: REPAIR_DB_RECONCILE,
            title: "Reconcile the database with .env".into(),
            risk: RiskTier::Caution,
            description: "Logs into the database as an administrator inside the service \
                          container and creates/updates the database, user, password and \
                          grants to match `.env` — no data is touched. Verifies the app \
                          credentials afterwards."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_DB_RECREATE => Some(RepairSpec {
            id: REPAIR_DB_RECREATE,
            title: "Recreate the database volume (DESTROYS ITS DATA)".into(),
            risk: RiskTier::HighRisk,
            description: "Stops the database service, deletes its named data volume, and \
                          starts it again so the image re-initializes from the current \
                          `.env`. Every database in that volume is permanently lost — \
                          export anything you need first."
                .into(),
            arg: arg.map(String::from),
        }),
        REPAIR_DOCKER_GROUP => Some(RepairSpec {
            id: REPAIR_DOCKER_GROUP,
            title: "Add your user to the docker group".into(),
            risk: RiskTier::HighRisk,
            description: "Runs `pkexec usermod -aG docker <you>`. Docker-group membership \
                          is equivalent to root; you must log out and back in afterwards."
                .into(),
            arg: None,
        }),
        REPAIR_HOSTS_ENTRY => Some(RepairSpec {
            id: REPAIR_HOSTS_ENTRY,
            title: match arg {
                Some(domain) => format!("Point {domain} at this machine (/etc/hosts)"),
                None => "Add the /etc/hosts entry".into(),
            },
            risk: RiskTier::HighRisk,
            description: if cfg!(target_os = "macos") {
                "Appends one `127.0.0.1` line to /etc/hosts so the browser resolves \
                 the local domain. Asks for your administrator password, and the \
                 preview shows the exact line."
            } else {
                "Appends one `127.0.0.1` line to /etc/hosts so the browser resolves \
                 the local domain. Runs via polkit (pkexec) — you will be asked for \
                 elevation, and the preview shows the exact line."
            }
            .into(),
            arg: arg.map(str::to_string),
        }),
        REPAIR_INSTALL_CERTUTIL => Some(RepairSpec {
            id: REPAIR_INSTALL_CERTUTIL,
            title: "Install certutil and finish browser trust".into(),
            risk: RiskTier::HighRisk,
            description: "Installs the NSS tools package (libnss3-tools / nss-tools) with \
                          your distribution's package manager via pkexec, then adds the \
                          local HTTPS certificate authority to ~/.pki/nssdb so \
                          Chromium-family browsers trust it — the half the system trust \
                          store cannot reach."
                .into(),
            arg: None,
        }),
        REPAIR_TRUST_PROXY_CA => Some(RepairSpec {
            id: REPAIR_TRUST_PROXY_CA,
            title: "Trust the local HTTPS certificate authority".into(),
            risk: RiskTier::HighRisk,
            description: if cfg!(target_os = "macos") {
                "Copies the proxy's own root certificate out of the `mast-proxy` \
                 container and adds it to the System keychain as a trusted root \
                 (asks for your administrator password). It only ever signs \
                 certificates for your local domains and never leaves this machine."
            } else {
                "Copies the proxy's own root certificate out of the `mast-proxy` \
                 container and installs it into the system trust store (via pkexec) \
                 and, when `certutil` exists, into ~/.pki/nssdb for Chrome/Chromium. \
                 It only ever signs certificates for your local domains and never \
                 leaves this machine."
            }
            .into(),
            arg: None,
        }),
        _ => None,
    }
}

/// The env edits `set-wwwuser` will apply (pure planning; the engine routes
/// them through the transactional env writer).
pub fn wwwuser_repair_edits(facts: &ProjectFacts, uid: u32, gid: u32) -> Vec<(String, String)> {
    let mut edits = Vec::new();
    if facts.wwwuser.as_deref() != Some(uid.to_string().as_str()) {
        edits.push(("WWWUSER".to_string(), uid.to_string()));
    }
    if facts.wwwgroup.as_deref() != Some(gid.to_string().as_str()) {
        edits.push(("WWWGROUP".to_string(), gid.to_string()));
    }
    edits
}
