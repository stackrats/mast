//! The check set. Each check is pure over `DiagCtx`; a check that finds
//! nothing wrong emits no findings (the report's "checks run" count is what
//! tells the user everything passed).

use crate::{
    repair_spec, Check, DiagCtx, Finding, ProjectFacts, RepairSpec, RiskTier, Severity,
    REPAIR_COMPOSER_INSTALL, REPAIR_COPY_ENV_EXAMPLE, REPAIR_CREATE_NETWORK, REPAIR_DOCKER_GROUP,
    REPAIR_GENERATE_APP_KEY, REPAIR_NODE_INSTALL, REPAIR_REASSIGN_PORTS, REPAIR_SAIL_INSTALL,
    REPAIR_SET_WWWUSER, REPAIR_STORAGE_LINK,
};

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
