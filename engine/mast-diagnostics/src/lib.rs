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

pub use checks::{all_checks, run_all};
pub use history::{DiagnosticsDb, DiagnosticsError, RepairAudit, RunSummary};

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
pub const REPAIR_GENERATE_APP_KEY: &str = "generate-app-key";
pub const REPAIR_STORAGE_LINK: &str = "storage-link";

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
    pub socket: Option<SocketFacts>,
    pub rootless: Option<bool>,
    pub snap_docker: bool,
    /// Free bytes on the docker data root (local unix endpoints only).
    pub disk_free_bytes: Option<u64>,
    pub selinux_enforcing: bool,
    pub uid: u32,
    pub gid: u32,
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
        REPAIR_DOCKER_GROUP => Some(RepairSpec {
            id: REPAIR_DOCKER_GROUP,
            title: "Add your user to the docker group".into(),
            risk: RiskTier::HighRisk,
            description: "Runs `pkexec usermod -aG docker <you>`. Docker-group membership \
                          is equivalent to root; you must log out and back in afterwards."
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
