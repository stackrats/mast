//! Serializable wire DTOs shared by the engine and every client.
//!
//! Rules (see the architecture plan):
//! - Everything here is serde + specta derivable and round-trips through JSON.
//! - The engine may consume these types at its boundary; its internal
//!   aggregates are free to diverge.
//! - **v1 is FROZEN (M8).** From here on: additive changes only — new
//!   fields must carry `#[serde(default)]`, new enum variants may only be
//!   appended, and nothing existing is renamed or removed. A breaking change
//!   requires bumping `PROTOCOL_VERSION`, and clients refuse a mismatch.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Frozen at v1 (M8). Bump ONLY on a breaking wire change; clients require
/// an exact match and refuse otherwise (`ErrorInfo::ProtocolMismatch`).
pub const PROTOCOL_VERSION: u32 = 1;

/// The build this binary was compiled from — the workspace version, which
/// every crate in the tree shares, so it names the contract itself and not
/// just whichever binary happens to be asking.
///
/// [`PROTOCOL_VERSION`] answers "is the framing the same" and is frozen at 1;
/// under the additive rules above it deliberately does NOT move when a field
/// is added to a DTO. That leaves a gap wide enough to fall through: a 0.4
/// CLI and a 0.6 desktop both announce protocol 1, agree to talk, and then
/// die inside `serde_json::from_value` complaining about a missing field —
/// which tells the user nothing about the actual problem, that the app and
/// the CLI came from different install channels and drifted. Comparing this
/// on the socket closes it.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The compatibility unit for the daemon socket: everything up to, but not
/// including, the patch component. `"0.4.1"` → `"0.4"`.
///
/// Patch releases are bug fixes that never move a wire shape, while a minor
/// bump is where DTOs are allowed to grow — which, while this is still 0.x,
/// is exactly where semver puts the breaking boundary.
pub fn wire_compat_key(version: &str) -> &str {
    match version.match_indices('.').nth(1) {
        Some((dot, _)) => &version[..dot],
        None => version,
    }
}

/// Whether two builds may share one socket. An empty version means the peer
/// predates the versioned handshake and therefore cannot be vouched for —
/// unknown is not the same as compatible.
pub fn wire_compatible(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && wire_compat_key(a) == wire_compat_key(b)
}

/// How a peer that announced no version at all gets named in an error. Both
/// ends of the socket report it, so it lives here rather than being spelled
/// out twice and drifting.
pub fn describe_peer_version(version: &str) -> &str {
    if version.is_empty() { "unknown (older than 0.5.0)" } else { version }
}

// ---------- identifiers ----------

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
pub struct ProjectId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
pub struct OperationId(pub u64);

// ---------- state ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ProjectStatus {
    Stopped,
    Starting,
    Running,
    Degraded,
    Failed,
}

/// Container runtime state as reported by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ContainerState {
    Created,
    Running,
    Restarting,
    Paused,
    Exited,
    Dead,
    Removing,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ServiceHealth {
    Unknown,
    Starting,
    Healthy,
    Unhealthy,
}

/// One compose service of a project: declared by the resolved model and/or
/// observed as a container. `container_id == None` means declared-but-absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ServiceState {
    pub name: String,
    pub container_id: Option<String>,
    pub state: Option<ContainerState>,
    pub health: ServiceHealth,
    /// Host-reachable address of this service's web dashboard (Mailpit,
    /// Meilisearch, MinIO console, …) when the image is recognized and the
    /// container port is published — powers "Open UI" on the service chip.
    #[serde(default)]
    pub ui_url: Option<String>,
    /// Host-side port of a recognized database service — what a GUI client
    /// on the host connects to (with the credentials from `.env`).
    #[serde(default)]
    pub db_port: Option<u16>,
    /// Observed as a container but no longer declared by the compose file —
    /// a leftover from an earlier config (the post-git-pull trap). Compose
    /// verbs cannot address it ("no such service"), so its lifecycle goes
    /// straight to the docker CLI, and a whole-project Rebuild reaps it.
    #[serde(default)]
    pub orphaned: bool,
    /// Named volumes this service mounts (compose source names, bind mounts
    /// excluded) — non-empty is what makes the data-snapshot verbs worth
    /// offering on its chip.
    #[serde(default)]
    pub data_volumes: Vec<String>,
}

/// One point-in-time copy of a service's named volumes
/// ([`Action::SnapshotServiceData`]). Docker itself is the store: each copy
/// is a labeled volume, so snapshots survive an app-data wipe and need no
/// database of their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VolumeSnapshot {
    /// Groups the per-volume copies of one snapshot action.
    pub group: String,
    pub project: ProjectId,
    pub service: String,
    pub at_unix_ms: u64,
    /// Compose source names of the volumes captured (e.g. `sail-mysql`).
    pub volumes: Vec<String>,
}

/// A Laravel app process (Reverb, Horizon, queue worker, scheduler): a
/// long-running artisan command inside the app container. Detected from
/// composer.json/.env; `running` comes from an in-container cmdline scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProcessState {
    pub id: String,
    pub title: String,
    pub running: bool,
    /// How many copies are running in the app container, whoever started
    /// them. More than one of a daemon is almost always a leftover: two
    /// Horizon masters both work the queue, two schedulers run every task
    /// twice.
    #[serde(default)]
    pub instances: u32,
}

/// Who started a process tree running inside a project's app container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContainerRunOwner {
    /// A command or process this Mast session is running.
    Mast { command: String },
    /// A command an earlier Mast session started and no longer tracks — the
    /// app quit or restarted while it ran, which never stops what a
    /// `docker exec` started.
    EarlierMast { command: String },
    /// Started in the container by something other than Mast: a terminal
    /// (`sail artisan …`), an agent, another tool.
    Outside,
    /// Whatever started it has exited, and the container's init adopted it.
    Orphaned,
}

/// One process tree running inside a project's app container beyond the
/// container's own startup: each `docker exec` session — what Mast's
/// commands, a terminal's `sail artisan …` and agents all create — and
/// anything left behind when its starter exited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ContainerRun {
    /// The tree's root, as the container numbers it. With `start_ticks`, the
    /// identity [`Action::StopContainerRun`] targets.
    pub pid: u32,
    /// The root's start time in clock ticks since boot, so a stop can never
    /// reach a different process that has since reused the PID.
    pub start_ticks: u64,
    pub owner: ContainerRunOwner,
    /// The command name for Mast's runs; otherwise the root's command line,
    /// shortened.
    pub title: String,
    pub command_line: String,
    pub started_at_unix: u64,
    /// Resident memory of the whole tree, rounded.
    pub memory_bytes: u64,
    pub process_count: u32,
    /// What it runs, in display terms ("Horizon", "Vite", …), one entry per
    /// kind present.
    pub kinds: Vec<String>,
}

/// A git worktree of a project's repository other than the project's own
/// checkout — what coding agents create to work on several things at once
/// (ADR-0012). Read from git's admin directory, so it is listed wherever it
/// lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    /// The checkout's directory. With the project, what the worktree actions
    /// target.
    pub path: String,
    /// The name git keeps it under (`.git/worktrees/<name>`).
    pub name: String,
    /// The branch checked out; None when HEAD is detached.
    pub branch: Option<String>,
    /// The commit checked out, abbreviated.
    pub head: Option<String>,
    /// Whose convention the path follows — "Claude Code" for a checkout
    /// under `.claude/worktrees/`. A location, not proof of who made it.
    pub tool: Option<String>,
    pub created_at_unix: Option<u64>,
    /// The checkout directory is gone; git lists it until it is pruned.
    pub missing: bool,
    /// Pinned with `git worktree lock`; git refuses to remove or prune it.
    pub locked: bool,
    /// Files with uncommitted changes, untracked ones included — what
    /// removing it would lose. None when it could not be read.
    pub changed_files: Option<u32>,
    /// Commits checked out there that the project's own checkout does not
    /// have. The branch keeps them after a remove, unless HEAD is detached.
    pub unmerged_commits: Option<u32>,
    /// Containers started from it: its own compose stack.
    pub containers: Vec<WorktreeContainer>,
}

/// A container whose compose project was started from a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeContainer {
    pub id: String,
    pub name: String,
    pub service: String,
    pub compose_project: String,
    pub running: bool,
    /// Host ports it publishes — usually the same ones the project's own
    /// stack wants, since the worktree's `.env` is a copy.
    pub ports: Vec<u16>,
}

/// A coding agent's CLI running in a project's checkout or one of its
/// worktrees, found by what program it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    /// Display name: "Claude Code", "Codex", …
    pub agent: String,
    pub pid: u32,
    pub start_ticks: u64,
    pub cwd: String,
    /// The worktree it works in (its path); None for the project's own
    /// checkout.
    pub worktree: Option<String>,
    pub started_at_unix: u64,
}

/// Who a [`HostRun`] belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostRunOwner {
    /// A coding agent session that is still running started it.
    Agent { agent: String, pid: u32 },
    /// Whatever started it has exited, and init or the user's service
    /// manager adopted it. It runs until someone stops it.
    Orphaned,
}

/// One process tree on this machine working in a project's checkout or one
/// of its worktrees, of a kind worth knowing about — a dev server, an
/// artisan daemon, a `sail`/`docker compose` client, a headless browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HostRun {
    /// The tree's root. With `start_ticks`, the identity
    /// [`Action::StopHostRun`] targets.
    pub pid: u32,
    pub start_ticks: u64,
    pub owner: HostRunOwner,
    pub title: String,
    pub command_line: String,
    pub cwd: String,
    /// The worktree it works in (its path); None for the project's own
    /// checkout.
    pub worktree: Option<String>,
    /// Its working directory has been deleted since it started — a worktree
    /// removed out from under it.
    pub cwd_deleted: bool,
    pub started_at_unix: u64,
    /// Resident memory of the whole tree, rounded.
    pub memory_bytes: u64,
    pub process_count: u32,
    pub kinds: Vec<String>,
}

/// Something wrong with a project that a click can fix, shown on its card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Advisory {
    pub message: String,
    pub actions: Vec<AdvisoryAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AdvisoryAction {
    /// Stop one process tree in the app container ([`ContainerRun`]).
    StopContainerRun {
        label: String,
        pid: u32,
        #[serde(rename = "startTicks")]
        start_ticks: u64,
    },
    /// Restart one of the project's commands.
    RestartCommand { label: String, name: String },
    /// Apply a named repair ([`Action::ApplyRepair`]).
    ApplyRepair { label: String, repair: String, arg: Option<String> },
}

/// The app's Sail PHP runtime: what `build.context` currently pins and which
/// vendored runtimes it could switch to (drives the PHP version picker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PhpVersionInfo {
    /// The Sail-built compose service (usually the app).
    pub service: String,
    /// PHP series the build context pins ("8.4").
    pub current: String,
    /// Series available under `vendor/laravel/sail/runtimes/`.
    pub available: Vec<String>,
    /// Effective Node major: the compose `build.args.NODE_VERSION` override
    /// when present, else the runtime Dockerfile's `ARG NODE_VERSION`
    /// default. None when neither is readable.
    #[serde(default)]
    pub node: Option<String>,
    /// Node majors the picker offers (nodesource release lines the runtime
    /// Dockerfile can install). Empty when the build shape cannot take an
    /// override — the chip stays read-only then.
    #[serde(default)]
    pub node_available: Vec<String>,
}

/// A user-defined per-project command (M7.5): argv-only (whitespace-split,
/// no shell), `sail` prefix resolves to `vendor/bin/sail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCommand {
    pub name: String,
    pub command: String,
    /// Run automatically once the project reaches Running.
    pub auto_start: bool,
    /// Working directory: relative to the project (`../frontend`) or
    /// absolute. None = the project directory. Lets a Sail project drive a
    /// sibling repo's dev server; `sail …` commands refuse it, since the
    /// wrapper only works from the project root.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Auto-start only once the command of this name has finished starting.
    /// A dev server never exits, so waiting for it to *finish* would wait
    /// forever — what a dependent actually needs is for it to be up.
    #[serde(default)]
    pub after: Option<String>,
    /// How to tell THIS command has finished starting — its own readiness,
    /// declared where a compose healthcheck would be, on the thing being
    /// waited for rather than on each waiter. The first of its output lines
    /// containing this text marks it up. `None` falls back to watching for
    /// the output to settle, which needs no configuration but can only ever
    /// be a guess: a server that prints its banner and goes quiet really is
    /// up, and one that keeps chattering is indistinguishable from one that
    /// is still working.
    #[serde(default)]
    pub ready_when: Option<String>,
    /// Relaunch after ANY exit that was not asked for — a dev server dying
    /// is something to recover from, not report. Rapid exits stop the loop
    /// (a command that cannot stay up needs a person), and the operation
    /// then fails with the last exit in hand.
    #[serde(default)]
    pub auto_restart: bool,
    /// Glob patterns, relative to the command's working directory, whose
    /// file changes restart the running command — a queue worker never sees
    /// new code until relaunched. Empty = never restart on file changes.
    #[serde(default)]
    pub restart_when_changed: Vec<String>,
    /// Defined by the project's committed `mast.yml` rather than saved in
    /// app data. Shown read-only (edit the file), skipped when persisting
    /// [`Action::SetProjectCommands`], and shadowed by a saved command of
    /// the same name — a local override beats the shared default.
    #[serde(default)]
    pub from_manifest: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    /// Canonical project root path, rendered as a string for the wire.
    pub path: String,
    pub status: ProjectStatus,
    /// Resolved compose project name (container↔project association key).
    pub compose_project_name: Option<String>,
    pub is_sail: bool,
    pub services: Vec<ServiceState>,
    /// Set when invocation/model resolution fails (e.g. compose file deleted);
    /// the project stays listed and degrades gracefully.
    pub resolution_error: Option<String>,
    /// User-defined commands (M7.5), persisted per project.
    #[serde(default)]
    pub commands: Vec<ProjectCommand>,
    /// Laravel app processes (Reverb/Horizon/…) relevant to this project.
    #[serde(default)]
    pub processes: Vec<ProcessState>,
    /// Current git branch ("detached" when headless); None outside a repo.
    #[serde(default)]
    pub git_branch: Option<String>,
    /// Working tree has uncommitted changes; None outside a repo.
    #[serde(default)]
    pub git_dirty: Option<bool>,
    /// Where the app answers in a browser, from `.env` (`APP_URL` plus
    /// `APP_PORT`); None when `.env` gives no http(s) address.
    #[serde(default)]
    pub app_url: Option<String>,
    /// The Sail PHP runtime and its alternatives; None when no service
    /// builds from a Sail runtime shape.
    #[serde(default)]
    pub php: Option<PhpVersionInfo>,
    /// Public URL of the live share tunnel ([`Action::ShareProject`]);
    /// None while not sharing.
    #[serde(default)]
    pub share_url: Option<String>,
    /// The live tunnel's local dashboard (SAIL_SHARE_DASHBOARD, possibly
    /// auto-moved off a busy port); None while not sharing.
    #[serde(default)]
    pub share_dashboard_url: Option<String>,
    /// Stable local HTTPS address ([`Action::SetLocalDomain`]) served by the
    /// shared `mast-proxy` Caddy container; None when not claimed.
    #[serde(default)]
    pub local_domain: Option<String>,
    /// Non-fatal conditions worth surfacing (M4): unbootstrapped Sail clone,
    /// missing .env, both compose-file families present, …
    pub warnings: Vec<String>,
    /// Process trees running in the app container beyond its own startup —
    /// Mast's commands and anything else, with who started each.
    #[serde(default)]
    pub container_runs: Vec<ContainerRun>,
    /// Problems a click can fix: duplicate daemons, pages not using the
    /// project's Vite.
    #[serde(default)]
    pub advisories: Vec<Advisory>,
    /// Git worktrees of the project's repository besides its own checkout
    /// (ADR-0012).
    #[serde(default)]
    pub worktrees: Vec<Worktree>,
    /// Coding agents running in the checkout or its worktrees.
    #[serde(default)]
    pub agent_sessions: Vec<AgentSession>,
    /// Process trees on this machine working in the checkout or its
    /// worktrees: what agent sessions are running, and what they left.
    #[serde(default)]
    pub host_runs: Vec<HostRun>,
    /// The user's dragged position in the project list
    /// ([`Action::ReorderProjects`]). Lists sort by (rank, name), so the
    /// pre-ordering world — every rank 0 — stays exactly alphabetical.
    #[serde(default)]
    pub rank: u32,
    /// Local HTTPS domains claimed for the project's host commands
    /// ([`Action::SetCommandDomain`]), in claim order. A claim whose command
    /// no longer exists stays listed until it is released, so the domain is
    /// not silently handed to someone else.
    #[serde(default)]
    pub command_domains: Vec<CommandDomain>,
    /// Where the project's running host commands said they serve, read from
    /// their output ("Local: http://localhost:5173/"). An entry lasts as long
    /// as the run that announced it.
    #[serde(default)]
    pub command_ports: Vec<CommandPort>,
}

/// A local HTTPS domain served for one of a project's host commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CommandDomain {
    pub command: String,
    pub domain: String,
    /// The host port the proxy forwards to. `None` until the command has
    /// announced one or the user pinned one; the proxy answers with an
    /// explanation meanwhile.
    pub port: Option<u16>,
    /// The user set `port`. Otherwise it follows whatever the command
    /// announces, so a Vite that moves to 5174 because 5173 was busy stays
    /// reachable at the same address.
    pub pinned: bool,
}

/// The port a running host command announced in its output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CommandPort {
    pub command: String,
    pub port: u16,
}

/// A candidate found under a watched directory, not yet imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredProject {
    pub path: String,
    pub name: String,
    pub is_sail: bool,
}

/// Why docker could not be reached, in the terms a person can act on.
///
/// Four separate repairs hide behind one raw connection error: install it,
/// start it, be allowed to talk to it, or fix the network between you and it.
/// The error string names none of them — "Error in the hyper legacy client:
/// client error (Connect)" is true and useless — so the classification is made
/// once here rather than guessed at by every client that renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DockerUnavailable {
    /// No docker CLI on PATH. The usual state of a machine that has never had
    /// Docker, and the first thing a newcomer hits.
    NotInstalled,
    /// Docker is installed but its daemon is not answering — not started, or
    /// stopped since.
    NotRunning,
    /// The endpoint is there and refused this user. On Linux that is almost
    /// always group membership.
    PermissionDenied,
    /// Everything else: a remote endpoint that will not answer, TLS trouble, a
    /// context pointing somewhere that no longer exists.
    Unreachable,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DockerStatus {
    pub available: bool,
    pub context_name: Option<String>,
    pub endpoint: Option<String>,
    /// The underlying error, verbatim. Kept for diagnostics — clients show
    /// `reason` and keep this behind a disclosure.
    pub error: Option<String>,
    /// What kind of failure `error` describes. `None` while docker is
    /// available.
    pub reason: Option<DockerUnavailable>,
}

/// Consistent point-in-time view of engine state, tagged with the sequence
/// number of the last patch folded into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EngineSnapshot {
    pub protocol_version: u32,
    pub seq: u64,
    /// True when another Mast instance owns mutation on this machine; this
    /// engine observes but refuses mutating actions (plan §1).
    pub read_only: bool,
    pub docker: DockerStatus,
    pub integrations: IntegrationSettings,
    pub watched_directories: Vec<String>,
    pub discovered: Vec<DiscoveredProject>,
    pub projects: Vec<ProjectSummary>,
    pub workspaces: Vec<WorkspaceSummary>,
}

/// User preferences: which external tools to launch (M4 integrations, `None`
/// = auto-detect) and how Mast should behave on the user's behalf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationSettings {
    /// Terminal emulator binary (e.g. "ghostty", "kitty", "gnome-terminal");
    /// on macOS also an app bundle ("Terminal.app").
    pub terminal: Option<String>,
    /// Editor binary (e.g. "code", "zed").
    pub editor: Option<String>,
    /// Browser binary (e.g. "vivaldi", "firefox"); on macOS also an app
    /// bundle ("Vivaldi.app"). `None` = the desktop's default browser.
    /// Absent in settings written before this existed.
    #[serde(default)]
    pub browser: Option<String>,
    /// Move a published host port into `.env` when something else already
    /// holds it, instead of letting `up` fail on the bind. Defaults on.
    #[serde(default = "yes")]
    pub auto_port_remap: bool,
}

fn yes() -> bool {
    true
}

impl Default for IntegrationSettings {
    fn default() -> Self {
        Self { terminal: None, editor: None, browser: None, auto_port_remap: true }
    }
}

// ---------- workspaces (M6) ----------

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
pub struct WorkspaceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMember {
    pub project: ProjectId,
    /// Projects that must be Ready before this one starts (workspace-level
    /// `dependsOn`; compose-level `depends_on` stays inside each project).
    pub depends_on: Vec<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    pub members: Vec<WorkspaceMember>,
    /// Derived from member project statuses.
    pub status: ProjectStatus,
    /// Set when the dependency graph is unusable (e.g. a cycle).
    pub graph_error: Option<String>,
    /// Cross-project findings (M6): e.g. two members publishing the same
    /// host port.
    pub warnings: Vec<String>,
}

// ---------- workspace snapshots (M6): metadata only, never auto-applied ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFileHash {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMemberState {
    pub project: ProjectId,
    pub project_name: String,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
    /// Compose files + .env at capture time.
    pub file_hashes: Vec<SnapshotFileHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub id: String,
    pub workspace: WorkspaceId,
    pub name: String,
    pub taken_unix: u64,
    pub members: Vec<SnapshotMemberState>,
}

/// Current state vs a snapshot, as a human report (restore is a report,
/// never an automatic apply).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotReport {
    pub snapshot: WorkspaceSnapshot,
    /// (project name, change lines); empty lines list = unchanged member.
    pub deltas: Vec<SnapshotDelta>,
    pub clean: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDelta {
    pub project_name: String,
    pub changes: Vec<String>,
}

/// A previewable config edit: whole-file before/after plus a human summary.
/// (Compose files are small; clients render their own diff.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FileEditPreview {
    pub file: String,
    pub before: String,
    pub after: String,
    pub summary: Vec<String>,
    /// Nothing to do — already in the desired state.
    pub no_op: bool,
}

// ---------- env editing (M5) ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EnvEntryView {
    pub key: String,
    /// Decoded value (interpolation left raw). The UI masks when `secret`.
    pub value: String,
    /// Key matches the secret patterns (same set the redactor uses).
    pub secret: bool,
    pub in_example: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum FindingSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EnvFinding {
    pub severity: FindingSeverity,
    /// The key the finding is about, when key-specific.
    pub key: Option<String>,
    pub message: String,
}

/// On-demand env editor payload (never part of snapshots/patches — values
/// may be secrets and stay off the patch store).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EnvReport {
    pub env_exists: bool,
    pub example_exists: bool,
    pub entries: Vec<EnvEntryView>,
    /// Keys present in .env.example but missing from .env.
    pub missing_from_env: Vec<String>,
    pub findings: Vec<EnvFinding>,
}

/// One grouped entry from `storage/logs/laravel.log`: the Monolog header
/// split into fields, with the stack trace (when one follows) kept attached
/// instead of scattered across raw lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LaravelLogEntry {
    pub timestamp: String,
    pub environment: String,
    /// Monolog level, uppercase (`ERROR`, `WARNING`, …).
    pub level: String,
    pub message: String,
    /// Continuation lines — stack trace, previous exceptions — when any.
    pub detail: Option<String>,
}

/// The tail of the application log, parsed. On demand only, like
/// [`EnvReport`]: log bodies routinely carry user data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LaravelLogReport {
    /// False when `storage/logs/laravel.log` does not exist (fresh app, or
    /// LOG_CHANNEL points elsewhere).
    pub exists: bool,
    /// Newest first.
    pub entries: Vec<LaravelLogEntry>,
    /// True when the file outgrew the read window and older entries were
    /// left behind.
    pub truncated: bool,
}

/// One effective `php.ini` setting read from the running app container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PhpIniValue {
    pub key: String,
    /// As `ini_get` reports it; empty when the runtime has no value.
    pub value: String,
}

/// What the PHP runtime actually is right now: loaded extensions and the
/// common limits, read live from the app container — plus where the
/// vendored runtime keeps the files that change them (paths relative to
/// the project, present only when the files exist).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PhpRuntimeReport {
    /// `php -m`, sorted, section headers dropped.
    pub extensions: Vec<String>,
    /// The classic limits (`memory_limit`, upload sizes, …), in a stable
    /// display order.
    pub ini: Vec<PhpIniValue>,
    /// The vendored runtime's `php.ini` (e.g. `docker/8.4/php.ini`).
    pub ini_file: Option<String>,
    /// The vendored runtime's `Dockerfile` — where extensions are added.
    pub dockerfile: Option<String>,
    /// Extensions this project pins through `build.args.PHP_EXTENSIONS`,
    /// i.e. the ones Mast installed rather than the runtime's base set.
    #[serde(default)]
    pub managed: Vec<String>,
    /// Whether [`Action::SetPhpExtensions`] can work here at all.
    #[serde(default)]
    pub can_manage: bool,
    /// Why not, when it cannot — a runtime published before Sail grew the
    /// build arg, or a build shape that carries no args.
    #[serde(default)]
    pub manage_blocked: Option<String>,
}

/// The local HTTPS proxy's root certificate, exported for manual trust —
/// Firefox's import dialog wants the file, `curl --cacert` and
/// `NODE_EXTRA_CA_CERTS` want the path, and pasting into another tool wants
/// the PEM text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProxyCa {
    /// Where the exported `root.crt` lives on this machine.
    pub path: String,
    /// The certificate itself, PEM-encoded.
    pub pem: String,
}

// ---------- service catalog (M7) ----------

/// One installable companion service (Redis, Mailpit, …). `installed` means
/// the project already runs this software (matched by service key OR image,
/// however the user named the service); `removable` means the service key is
/// ours, so three-way removal can be offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub title: String,
    pub description: String,
    pub installed: bool,
    pub removable: bool,
    /// The compose service key that provides this software (ours or the
    /// user's own name) — the target for generic removal when `removable`
    /// is false.
    pub installed_service: Option<String>,
    /// When not installed but another service already fills the same role
    /// (e.g. rustfs covers object storage), the image that covers it —
    /// adding both usually conflicts on ports/env.
    pub role_covered_by: Option<String>,
    /// The image this service currently runs, when installed — the value a
    /// retag rewrites.
    pub installed_image: Option<String>,
    /// Tags offered for `installed_image`'s repo, newest first, and always
    /// including the tag it currently runs. Read from the registry and cached;
    /// empty when the repo publishes no version-shaped tags, lives on a
    /// registry Mast cannot query, or offers nothing but what is already
    /// running — in each case there is no choice to present.
    pub versions: Vec<String>,
}

/// A user-described service for the compose file: the minimal shape people
/// add by hand — image, host ports, one data volume, a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CustomServiceSpec {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub ports: Vec<String>,
    /// Container path persisted into a named volume `{name}-data`.
    #[serde(default)]
    pub volume: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
}

// ---------- diagnostics (M7) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DiagSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RepairRisk {
    /// Reversible, project-local (env edit, file copy).
    Safe,
    /// Runs containers or touches docker resources.
    Caution,
    /// Elevation or group membership — explicit consent UI required.
    HighRisk,
}

/// A repair a finding offers. `arg` disambiguates the target when the id
/// alone is not enough (e.g. which network to create).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RepairOffer {
    pub id: String,
    pub title: String,
    pub risk: RepairRisk,
    pub description: String,
    pub arg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticFinding {
    pub check: String,
    pub severity: DiagSeverity,
    pub title: String,
    pub detail: String,
    pub project: Option<ProjectId>,
    pub project_name: Option<String>,
    pub repair: Option<RepairOffer>,
}

/// Result of one diagnostics pass. No findings + `checks_run > 0` means
/// everything passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReport {
    pub taken_unix: i64,
    pub checks_run: u32,
    pub findings: Vec<DiagnosticFinding>,
}

/// What a repair will do, shown before consent. Env-file repairs carry a
/// full before/after preview; command repairs describe their argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RepairPlan {
    pub repair: RepairOffer,
    pub summary: Vec<String>,
    pub file_preview: Option<FileEditPreview>,
    /// Already in the desired state.
    pub no_op: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRunSummary {
    pub id: i64,
    pub taken_unix: i64,
    pub checks_run: u32,
    pub errors: u32,
    pub warnings: u32,
    pub infos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RepairAuditEntry {
    pub applied_unix: i64,
    pub repair: String,
    pub project_name: Option<String>,
    pub risk: String,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsHistory {
    pub runs: Vec<DiagnosticRunSummary>,
    pub repairs: Vec<RepairAuditEntry>,
}

/// One line of a container log stream (delivered over a dedicated channel,
/// never through the patch store — plan §3 transport split).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub service: String,
    pub message: String,
    pub stderr: bool,
}

// ---------- patches ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum PatchEvent {
    ProjectAdded { project: ProjectSummary },
    /// Full-project replacement when observation/resolution changed anything
    /// beyond bare status. Still a minimal patch at project granularity.
    ProjectUpdated { project: ProjectSummary },
    ProjectStatusChanged { id: ProjectId, status: ProjectStatus },
    ProjectRemoved { id: ProjectId },
    DiscoveryChanged { discovered: Vec<DiscoveredProject> },
    WatchedDirectoriesChanged { directories: Vec<String> },
    DockerStatusChanged { status: DockerStatus },
    IntegrationsChanged { integrations: IntegrationSettings },
    WorkspacesChanged { workspaces: Vec<WorkspaceSummary> },
}

// ---------- effect history (M9) ----------
//
// History does NOT travel on the patch stream. Background upkeep alone
// produces a command every second or so, which would evict real state patches
// from the replay window and force clients to resync. Like container logs
// (plan §3) it gets its own channel: `Engine::history_recent` for the backlog,
// `Engine::subscribe_history` for the live feed. Clients upsert by `id`.

/// Why an effect happened: something the user asked for, or Mast's own upkeep
/// (reconciliation, readiness probes, invocation resolution). Clients default
/// to showing only `User` — background traffic is constant and would bury it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum HistoryOrigin {
    User,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum HistoryOutcome {
    /// Still going. Streamed commands can sit here for a long time.
    Running,
    Exited { status: i32 },
    Cancelled,
    /// Never produced a status: spawn failure, timeout, or a rejected write.
    Failed { error: String },
    /// Launched detached (terminal, editor, browser) — outcome unknowable.
    Detached,
    /// A file write that went through.
    Applied,
}

impl HistoryOutcome {
    /// Did this go wrong? Drives the failure filter and the status dot.
    pub fn is_failure(&self) -> bool {
        match self {
            HistoryOutcome::Failed { .. } => true,
            HistoryOutcome::Exited { status } => *status != 0,
            _ => false,
        }
    }
}

/// One env-overlay entry as recorded. Secret-looking keys carry a mask, never
/// the value — history is copyable, and a copy button must not hand out
/// credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEnvVar {
    pub key: String,
    pub value: String,
    pub masked: bool,
}

/// What Mast actually did. Not every effect is a subprocess — config writes
/// change the developer's machine too, and a history that omitted them would
/// misrepresent what Mast had done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum HistoryDetail {
    Command {
        argv: Vec<String>,
        cwd: Option<String>,
        env: Vec<HistoryEnvVar>,
        /// Streamed rather than captured — output went to the logs panel live.
        streaming: bool,
    },
    FileWrite {
        path: String,
        /// Human change lines, as shown in the edit preview.
        summary: Vec<String>,
    },
}

/// One recorded effect. Redacted at construction: argv, env, and output have
/// already had known `.env` secrets removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// Monotonic per engine; identifies the entry an update replaces.
    pub id: u64,
    pub at_unix_ms: u64,
    /// What the user asked for ("Start acme-app"), not the argv.
    pub label: String,
    pub project: Option<ProjectId>,
    /// The operation this effect belongs to, when it has one — lets a failed
    /// operation link straight to the command behind it.
    pub operation: Option<OperationId>,
    pub origin: HistoryOrigin,
    pub detail: HistoryDetail,
    pub outcome: HistoryOutcome,
    pub duration_ms: Option<u64>,
    /// Last lines of output, redacted; empty while still running.
    pub output: Vec<String>,
}

// ---------- log captures (M10) ----------
//
// Live log streams are bound to one container id and end when that container
// goes away, taking the output that explains the failure with them. A capture
// is the post-mortem: the tail of a container's output, read at the moment it
// went down and written to disk so it survives both the recreate and the app.
//
// Like history (ADR-0004 §3) captures get their own channel rather than the
// patch stream — they are events with a body, not state, and a client that
// resynchronizes must not silently lose one.

/// Why a capture was taken. The reason is the whole diagnostic frame: the same
/// forty lines mean something different before a user-requested restart than
/// they do after a container fell over on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum CaptureReason {
    /// Mast is about to stop/restart/rebuild this container. Taken *before*
    /// the command, because a recreate removes the container and its log.
    Teardown { verb: String },
    /// Observed to have exited without Mast asking. `status` is `None` when
    /// the daemon did not report a code.
    Exited { status: Option<i32> },
    /// Observed to have gone unhealthy.
    Unhealthy,
    /// A workspace start gave up waiting for this service to become ready.
    ReadyTimeout,
    /// The user asked for it from the service menu.
    Manual,
}

/// One captured line. Carries Docker's own timestamp rather than an arrival
/// order, because a capture is read after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LogCaptureLine {
    /// Docker's RFC3339 stamp, verbatim; `None` if the line had none.
    pub at: Option<String>,
    pub message: String,
    pub stderr: bool,
}

/// A container's last words. Persisted, therefore redacted at write time —
/// unlike a live log stream, which is transient and is not (see `redact.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LogCapture {
    /// Row id, ascending. Clients upsert by it.
    pub id: u64,
    pub at_unix_ms: u64,
    pub project: ProjectId,
    /// Denormalized so a capture stays readable after the project is removed.
    pub project_name: String,
    pub service: String,
    pub container_id: String,
    pub reason: CaptureReason,
    /// How far back the read reached, in seconds.
    pub window_secs: u32,
    pub lines: Vec<LogCaptureLine>,
    /// The window held more lines than the cap; the oldest were dropped.
    pub truncated: bool,
}

// ---------- resource usage (M11) ----------
//
// What each container costs, sampled live. Like logs, history and captures
// this gets its own channel rather than the patch stream: a sample every
// couple of seconds would evict real state patches from the replay window
// (ADR-0004 §3), and unlike state, a sample is worthless a minute later.
//
// Nothing here is persisted and nothing is redacted — these are numbers, and
// they carry none of the developer's own content.

/// One service's share of the machine, over one sampling interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ServiceUsage {
    pub project: ProjectId,
    pub service: String,
    /// Cores consumed over the interval: `1.0` is one saturated core. Cores
    /// rather than a percentage because Docker's percentage is of *all* cores
    /// — 800% is reachable on an 8-core box, so a 0–100 reading misleads.
    pub cpu_cores: f64,
    /// Working set: page cache excluded, as `docker stats` reports it. Raw
    /// cgroup usage counts reclaimable cache and overstates badly.
    pub memory_bytes: u64,
    pub memory_limit_bytes: u64,
    /// Whether `memory_limit_bytes` is a real cgroup limit rather than just
    /// host RAM. This is the difference between "using a third of this
    /// machine" and "a third of the way to being OOM-killed".
    pub memory_limited: bool,
}

/// One tick: every running service Mast knows about, measured together so the
/// numbers can be summed and compared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UsageSample {
    pub at_unix_ms: u64,
    /// Cores the host has, so a client can say "2.3 of 8" rather than "2.3".
    pub host_cores: u32,
    pub host_memory_bytes: u64,
    pub services: Vec<ServiceUsage>,
}

/// One minimal typed state change. `seq` values are contiguous per engine;
/// a gap observed by a client means it must resynchronize via snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EnginePatch {
    pub seq: u64,
    pub event: PatchEvent,
}

/// Item delivered on a patch subscription. `ResyncRequired` is terminal for
/// the subscription: the client must fetch a fresh snapshot and resubscribe.
/// A lagged subscriber is told to resync — never silently dropped patches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
// Patches dominate the stream; ResyncRequired is once-per-resync. Boxing the
// patch would complicate every construction site for no realistic win.
#[allow(clippy::large_enum_variant)]
pub enum SubscriptionItem {
    Patch { patch: EnginePatch },
    ResyncRequired,
}

// ---------- actions & operations ----------

/// Client-initiated mutations. Every dispatch yields an [`OperationId`] whose
/// lifecycle is streamed as [`OperationEvent`]s — no fire-and-forget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Action {
    /// Exercises the operation machinery (progress/cancel) without touching
    /// state. Kept for protocol tests and demos.
    StartFakeOperation { project: ProjectId },
    ImportProject { path: String },
    RemoveProject { id: ProjectId },
    AddWatchedDirectory { path: String },
    RemoveWatchedDirectory { path: String },
    /// `up -d` via the project's runner (sail or compose), terminal parity.
    StartProject { id: ProjectId },
    /// Per-service lifecycle: `up -d <service>` / `stop <service>` /
    /// `restart <service>` (compose and sail both accept trailing services).
    StartService { id: ProjectId, service: String },
    StopService { id: ProjectId, service: String },
    RestartService { id: ProjectId, service: String },
    /// `stop` via the project's runner.
    StopProject { id: ProjectId },
    /// `restart` via the project's runner.
    RestartProject { id: ProjectId },
    /// Open the configured (or auto-detected) terminal at the project root.
    OpenTerminal { id: ProjectId },
    /// Open a terminal running an interactive shell inside the service's
    /// container.
    ShellIntoContainer { id: ProjectId, service: String },
    /// Open the project in the configured (or auto-detected) editor.
    OpenInEditor { id: ProjectId },
    /// Reveal the project directory in the system file manager.
    RevealInFileManager { id: ProjectId },
    /// Open the project's `.env` address (`ProjectSummary::app_url`) in the
    /// default browser.
    OpenInBrowser { id: ProjectId },
    /// Open an http(s) URL Mast itself surfaced (share URL, tunnel
    /// dashboard) in the default browser. Non-http schemes are refused.
    OpenUrl { url: String },
    /// Persist external-tool preferences.
    SetIntegrations { integrations: IntegrationSettings },
    /// Set a LITERAL value in the project's .env (lossless model; creates
    /// the file if absent).
    SetEnvVar { id: ProjectId, key: String, value: String },
    /// Remove a key from the project's .env.
    RemoveEnvVar { id: ProjectId, key: String },
    /// Create or replace a workspace definition (matched by id when present).
    SaveWorkspace { id: Option<WorkspaceId>, name: String, members: Vec<WorkspaceMember> },
    RemoveWorkspace { id: WorkspaceId },
    /// Start all members in dependency order, waiting for readiness between
    /// layers; a failing member blocks its dependents.
    StartWorkspace { id: WorkspaceId },
    /// Stop all members in reverse dependency order.
    StopWorkspace { id: WorkspaceId },
    /// Attach a member project's services to the workspace's shared
    /// `mast-{slug}` network via the compose write transaction (preview
    /// first with `network_attach_preview`).
    AttachNetwork { workspace: WorkspaceId, project: ProjectId },
    /// Capture git refs + config hashes for every member (metadata only).
    TakeSnapshot { workspace: WorkspaceId, name: String },
    RemoveSnapshot { id: String },
    /// Apply a previewed diagnostic repair. `repair`/`arg` come from a
    /// [`RepairOffer`]; high-risk repairs require explicit consent in the UI.
    ApplyRepair { repair: String, arg: Option<String>, project: Option<ProjectId> },
    /// Add a catalog service to the project's compose file (previewed,
    /// transactional) plus its documented `.env` updates.
    AddCatalogService { id: ProjectId, service: String },
    /// Three-way removal: refused if the service block was customized after
    /// it was added.
    RemoveCatalogService { id: ProjectId, service: String },
    /// Remove ANY service by its compose key, as-is (previewed; no three-way
    /// baseline — for services Mast did not add).
    RemoveService { id: ProjectId, service: String },
    /// Add a user-described service (previewed transactional compose edit).
    AddCustomService { id: ProjectId, spec: CustomServiceSpec },
    /// Retag a service's image — the whole of "change the MySQL version"
    /// (previewed transactional compose edit). The container keeps running the
    /// old image until a [`Action::RebuildService`].
    SetServiceImage { id: ProjectId, service: String, image: String },
    /// Pull the service's image and recreate just that container, so a retag
    /// (or any edited service block) takes effect.
    RebuildService { id: ProjectId, service: String },
    /// Rebuild the whole project: rebuild service images, pull newer ones and
    /// recreate every container, dropping orphans — the recovery for a compose
    /// config that changed underneath the project (a git pull), which
    /// `restart` (reuses containers) and `up` (reuses images) both miss.
    RebuildProject { id: ProjectId },
    /// Remove one orphaned service's leftover container (`docker rm -f`) —
    /// the targeted disposal for a single [`ServiceState::orphaned`] chip,
    /// where a whole-project Rebuild would be overkill.
    RemoveOrphanContainer { id: ProjectId, service: String },
    /// Switch a Sail-built service to another vendored PHP runtime, as ONE
    /// operation: rewrites `build.context` and the `sail-X.Y/app` image tag
    /// together, rebuilds without cache, recreates the container when the
    /// project is running, and verifies `php -v` inside it — the exact
    /// sequence users half-do by hand (laravel/sail#442).
    SetPhpVersion { id: ProjectId, service: String, series: String },
    /// Pin the app's Node major (`build.args.NODE_VERSION`, Sail's
    /// documented override of the runtime Dockerfile's default) and run the
    /// same verified switch as PHP: rebuild without cache, recreate when
    /// running, confirm `node -v` inside the container.
    SetNodeVersion { id: ProjectId, service: String, major: String },
    /// Install PHP extensions into the app's runtime image via Sail's
    /// `build.args.PHP_EXTENSIONS` (laravel/sail#879), then the same
    /// verified rebuild as the PHP and Node switches. The list REPLACES
    /// whatever was pinned before; an empty list clears it. Extensions the
    /// runtime already carries in its base set are unaffected either way.
    SetPhpExtensions { id: ProjectId, service: String, extensions: Vec<String> },
    /// Publish the running app through Sail's expose tunnel (`sail share`,
    /// same image, flags and `.env` knobs). Long-running: the operation
    /// stays live while the tunnel is up, cancelling it stops the share.
    /// The public URL lands on [`ProjectSummary::share_url`] once the
    /// tunnel reports it.
    ShareProject { id: ProjectId },
    /// Claim (or clear, with `domain: None`) a stable `https://…test`
    /// address for this project. Mast keeps one shared Caddy container
    /// (`mast-proxy`, ports 80/443) whose internal CA signs the
    /// certificate; the two host-side steps it cannot do silently — the
    /// `/etc/hosts` line and trusting that CA — surface as high-risk Fix
    /// buttons on the operation.
    SetLocalDomain { id: ProjectId, domain: Option<String> },
    /// Open the platform's hosts file in the desktop's default editor — the
    /// manual fallback when the elevated add-hosts-entry repair is not
    /// possible or not wanted. Opens read-only for most users; the dialog
    /// shows the exact line to add.
    OpenHostsFile,
    /// Truncate `storage/logs/laravel.log` — a fresh slate for the App log
    /// viewer. The file is kept (empty), so running processes keep their
    /// open handle and Laravel appends as before.
    ClearLaravelLog { id: ProjectId },
    /// Open a directory in the file manager. Restricted to paths Mast
    /// already knows — a watched directory or an imported project's root —
    /// so the surface stays as narrow as the buttons that use it.
    RevealPath { path: String },
    /// Open one file inside a project in the configured editor — the
    /// vendored runtime's `php.ini` or `Dockerfile` from the PHP runtime
    /// dialog, or a file named by a log entry. `file` is project-relative
    /// and may not escape the project. `line` jumps there when the editor
    /// has a syntax for it (absent otherwise — older clients never send it).
    OpenProjectFile {
        id: ProjectId,
        file: String,
        #[serde(default)]
        line: Option<u32>,
    },
    /// New-project wizard (M7): the documented Sail install — composer
    /// create-project, `composer require laravel/sail --dev`, then `php artisan
    /// sail:install --php=…` — each run in the official composer image inside
    /// `parent`, then import the result. `php` is the series ("85"); `services`
    /// empty = Sail's defaults.
    CreateProject { parent: String, name: String, php: String, services: Vec<String> },
    /// Start a Laravel app process (`php artisan …` in the app container) as
    /// a streamed cancellable operation.
    StartProcess { id: ProjectId, process: String },
    /// Stop a Laravel app process (SIGTERM by cmdline match in-container —
    /// also catches processes started from a terminal).
    StopProcess { id: ProjectId, process: String },
    /// Stop one process tree in the app container ([`ContainerRun`]) —
    /// SIGTERM to the root and every descendant, provided the root still has
    /// the start time it was listed with.
    StopContainerRun {
        id: ProjectId,
        pid: u32,
        #[serde(rename = "startTicks")]
        start_ticks: u64,
    },
    /// Replace the project's user-defined command list (persisted).
    SetProjectCommands { id: ProjectId, commands: Vec<ProjectCommand> },
    /// Run one user-defined command as a streamed cancellable operation.
    RunProjectCommand { id: ProjectId, name: String },
    /// Trigger an immediate discovery + observation reconcile.
    RefreshNow,
    /// Capture a service's recent output now, without stopping anything.
    CaptureServiceLogs { id: ProjectId, service: String },
    /// Delete every stored log capture.
    ClearLogCaptures,
    /// Move the project's saved commands into a committed `mast.yml` at the
    /// project root, so the setup travels with the repo instead of living on
    /// one machine. Refuses when the file already exists — Mast writes the
    /// first draft, people maintain it.
    ExportProjectManifest { id: ProjectId },
    /// Open a terminal running `php artisan tinker` inside the app
    /// container — the REPL one click from the project, needing nothing on
    /// the host.
    OpenTinker { id: ProjectId },
    /// Hand a database connection URL to the desktop, which dispatches on
    /// scheme to whatever client registered it. Only database schemes are
    /// accepted; http(s) has [`Action::OpenUrl`].
    OpenDbUrl { url: String },
    /// Copy a service's named volumes into labeled snapshot volumes, cold:
    /// the container is stopped for the copy and restarted after if it was
    /// running. The insurance to buy BEFORE a risky migration — or before
    /// [`Action::RestoreServiceData`] overwrites what is there now.
    SnapshotServiceData { id: ProjectId, service: String },
    /// Overwrite the service's current volume data with a snapshot's, cold
    /// (stop, wipe, copy back, restart). DESTRUCTIVE of the current data —
    /// clients must confirm, and should offer a fresh snapshot first.
    RestoreServiceData { id: ProjectId, group: String },
    /// Delete one snapshot's volumes permanently.
    RemoveServiceDataSnapshot { group: String },
    /// Clone a git repository into `parent/name`, bootstrap what a fresh
    /// clone always lacks (containerized `composer install`, `.env` from
    /// `.env.example`, an app key — each only when missing), and import the
    /// result: the team-onboarding mirror of [`Action::CreateProject`].
    /// Refuses http(s) URLs with embedded credentials — effect history
    /// records every argv verbatim, and a token must never land there.
    CloneProject { url: String, parent: String, name: String },
    /// Persist a dragged ordering of the project list: rank = index in
    /// `ids`. Projects the list does not name keep their old rank — a stale
    /// client cannot shuffle what it never saw.
    ReorderProjects { ids: Vec<ProjectId> },
    /// Persist a dragged ordering of the workspace list; workspaces not
    /// named keep their relative order after the named ones.
    ReorderWorkspaces { ids: Vec<WorkspaceId> },
    /// Stop one process tree on this machine ([`HostRun`]): SIGTERM to the
    /// root and every descendant, SIGKILL to what is left after ten
    /// seconds — provided the root still has the start time it was listed
    /// with.
    StopHostRun {
        id: ProjectId,
        pid: u32,
        #[serde(rename = "startTicks")]
        start_ticks: u64,
    },
    /// Stop the containers started from one of the project's worktrees,
    /// keeping them.
    StopWorktreeContainers { id: ProjectId, path: String },
    /// Remove the containers started from one of the project's worktrees.
    /// Named volumes are kept.
    RemoveWorktreeContainers { id: ProjectId, path: String },
    /// `git worktree remove` one of the project's worktrees, after stopping
    /// every process working in it and removing its containers. The branch
    /// is kept. Refused while it has uncommitted changes unless `force` —
    /// which DESTROYS them: clients must confirm.
    RemoveWorktree { id: ProjectId, path: String, force: bool },
    /// `git worktree prune`: forget the worktrees whose directory is gone.
    PruneWorktrees { id: ProjectId },
    /// Claim (or clear, with `domain: None`) a stable `https://…test`
    /// address for one of the project's host commands — a dev server Mast
    /// runs on this machine, such as a separate frontend's `vp dev` — on
    /// the same proxy as [`Action::SetLocalDomain`]. The proxy forwards to
    /// `port` when given (pinned), otherwise to the port the command
    /// announces in its own output, followed as it moves. Commands that run
    /// in the app container (`sail …`) are refused: their address is the
    /// app's own domain.
    SetCommandDomain {
        id: ProjectId,
        command: String,
        domain: Option<String>,
        #[serde(default)]
        port: Option<u16>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum OperationEventKind {
    Started,
    Progress { percent: u8, message: String },
    /// One line of live subprocess output (compose/sail shell-outs).
    Output { line: String, stderr: bool },
    /// A one-click repair that addresses a failure signature spotted in this
    /// operation's output — emitted just before [`Self::Failed`], so a
    /// failure can carry its own Fix button. Preview the repair before
    /// applying; the preview says exactly what will change.
    FixAvailable { repair: RepairOffer, project: ProjectId },
    Completed,
    Failed { error: String },
    Cancelled,
}

impl OperationEventKind {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OperationEventKind::Completed
                | OperationEventKind::Failed { .. }
                | OperationEventKind::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OperationEvent {
    pub operation: OperationId,
    pub kind: OperationEventKind,
}

// ---------- errors ----------

/// Serializable error surface crossing the client boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ErrorInfo {
    NotFound { what: String },
    InvalidInput { message: String },
    /// A conflicting operation is already running (per-project op lock).
    Conflict { message: String },
    /// This engine instance does not own mutation (another instance does).
    ReadOnly { owner_pid: Option<u32> },
    ProtocolMismatch { expected: u32, actual: u32 },
    Internal { message: String },
    /// The two ends of the daemon socket came from different builds of Mast
    /// (see [`wire_compatible`]). Appended, per the additive rule above;
    /// clients too old to know this variant degrade to `Internal` carrying
    /// the raw JSON, which is the best available for a binary that shipped
    /// before the check existed.
    VersionMismatch { client: String, server: String },
}

impl core::fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ErrorInfo::NotFound { what } => write!(f, "not found: {what}"),
            ErrorInfo::InvalidInput { message } => write!(f, "invalid input: {message}"),
            ErrorInfo::Conflict { message } => write!(f, "conflict: {message}"),
            ErrorInfo::ReadOnly { owner_pid } => match owner_pid {
                Some(pid) => {
                    write!(f, "read-only: another Mast instance (pid {pid}) owns mutation")
                }
                None => write!(f, "read-only: another Mast instance owns mutation"),
            },
            ErrorInfo::ProtocolMismatch { expected, actual } => {
                write!(f, "protocol mismatch: client {expected}, engine {actual}")
            }
            ErrorInfo::Internal { message } => write!(f, "internal error: {message}"),
            ErrorInfo::VersionMismatch { client, server } => write!(
                f,
                "version mismatch: this build is Mast {client}, \
                 the Mast already running is {server}"
            ),
        }
    }
}

impl core::error::Error for ErrorInfo {}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + core::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, value, "JSON round-trip changed value ({json})");
    }

    #[test]
    fn wire_compat_is_major_minor() {
        assert_eq!(wire_compat_key("0.4.1"), "0.4");
        assert_eq!(wire_compat_key("1.10.3"), "1.10");
        // Pre-release suffixes hang off the patch component, so they fall
        // away with it rather than splitting the key in the wrong place.
        assert_eq!(wire_compat_key("0.5.0-rc.2"), "0.5");
        // Nothing to trim.
        assert_eq!(wire_compat_key("0.5"), "0.5");
        assert_eq!(wire_compat_key(""), "");

        assert!(wire_compatible("0.4.0", "0.4.7"));
        assert!(!wire_compatible("0.4.0", "0.5.0"));
        assert!(!wire_compatible("0.4.0", "1.4.0"));
        // A peer that announces nothing is unknown, and unknown is refused —
        // that is the whole point of the check.
        assert!(!wire_compatible("", "0.4.0"));
        assert!(!wire_compatible("0.4.0", ""));

        // The constant every binary in the tree compares against must itself
        // be a version this rule can read.
        assert!(!wire_compat_key(BUILD_VERSION).is_empty());
        assert!(wire_compatible(BUILD_VERSION, BUILD_VERSION));
    }

    fn sample_service() -> ServiceState {
        ServiceState {
            name: "laravel.test".into(),
            container_id: Some("abc123".into()),
            state: Some(ContainerState::Running),
            health: ServiceHealth::Healthy,
            ui_url: None,
            db_port: None,
            orphaned: false,
            data_volumes: vec!["sail-mysql".into()],
        }
    }

    fn sample_project() -> ProjectSummary {
        ProjectSummary {
            id: ProjectId("p1".into()),
            name: "fake-sail-app".into(),
            path: "/home/dev/code/fake-sail-app".into(),
            status: ProjectStatus::Running,
            compose_project_name: Some("fake-sail-app".into()),
            is_sail: true,
            services: vec![sample_service()],
            resolution_error: None,
            warnings: vec!["Sail project without vendor/".into()],
            rank: 3,
            command_domains: vec![CommandDomain {
                command: "dev".into(),
                domain: "web.fake-sail-app.test".into(),
                port: Some(5173),
                pinned: false,
            }],
            command_ports: vec![CommandPort { command: "dev".into(), port: 5173 }],
            commands: vec![ProjectCommand {
                name: "dev".into(),
                command: "sail npm run dev".into(),
                auto_start: true,
                cwd: None,
                after: None,
                ready_when: None,
                auto_restart: false,
                restart_when_changed: Vec::new(),
                from_manifest: true,
            }],
            processes: vec![ProcessState {
                id: "horizon".into(),
                title: "Horizon".into(),
                running: true,
                instances: 2,
            }],
            container_runs: vec![ContainerRun {
                pid: 42,
                start_ticks: 16_984_000,
                owner: ContainerRunOwner::EarlierMast { command: "dev".into() },
                title: "dev".into(),
                command_line: "sh -c pnpm dlx @laravel/multiplex".into(),
                started_at_unix: 1_791_961_641,
                memory_bytes: 2_400_000_000,
                process_count: 29,
                kinds: vec!["Horizon".into(), "Vite".into()],
            }],
            advisories: vec![Advisory {
                message: "Horizon is running twice.".into(),
                actions: vec![
                    AdvisoryAction::StopContainerRun {
                        label: "Stop the leftover dev".into(),
                        pid: 42,
                        start_ticks: 16_984_000,
                    },
                    AdvisoryAction::RestartCommand { label: "Restart dev".into(), name: "dev".into() },
                    AdvisoryAction::ApplyRepair {
                        label: "Remove public/hot".into(),
                        repair: "remove-hot".into(),
                        arg: None,
                    },
                ],
            }],
            worktrees: vec![Worktree {
                path: "/home/dev/code/fake-sail-app/.claude/worktrees/fix-login".into(),
                name: "fix-login".into(),
                branch: Some("worktree-fix-login".into()),
                head: Some("8ba4734".into()),
                tool: Some("Claude Code".into()),
                created_at_unix: Some(1_791_961_000),
                missing: false,
                locked: false,
                changed_files: Some(3),
                unmerged_commits: Some(1),
                containers: vec![WorktreeContainer {
                    id: "c0ffee".into(),
                    name: "fix-login-laravel.test-1".into(),
                    service: "laravel.test".into(),
                    compose_project: "fix-login".into(),
                    running: true,
                    ports: vec![80, 5173],
                }],
            }],
            agent_sessions: vec![AgentSession {
                agent: "Claude Code".into(),
                pid: 15_479,
                start_ticks: 9_000_000,
                cwd: "/home/dev/code/fake-sail-app/.claude/worktrees/fix-login".into(),
                worktree: Some("/home/dev/code/fake-sail-app/.claude/worktrees/fix-login".into()),
                started_at_unix: 1_791_960_000,
            }],
            host_runs: vec![
                HostRun {
                    pid: 2_453_807,
                    start_ticks: 9_100_000,
                    owner: HostRunOwner::Orphaned,
                    title: "headless chrome".into(),
                    command_line: "/opt/google/chrome/chrome --headless=new".into(),
                    cwd: "/home/dev/code/fake-sail-app".into(),
                    worktree: None,
                    cwd_deleted: false,
                    started_at_unix: 1_791_960_100,
                    memory_bytes: 870_000_000,
                    process_count: 21,
                    kinds: vec!["Headless browser".into()],
                },
                HostRun {
                    pid: 2_460_000,
                    start_ticks: 9_200_000,
                    owner: HostRunOwner::Agent { agent: "Claude Code".into(), pid: 15_479 },
                    title: "vite".into(),
                    command_line: "node node_modules/.bin/vite".into(),
                    cwd: "/home/dev/code/fake-sail-app/.claude/worktrees/fix-login".into(),
                    worktree: Some(
                        "/home/dev/code/fake-sail-app/.claude/worktrees/fix-login".into(),
                    ),
                    cwd_deleted: false,
                    started_at_unix: 1_791_960_200,
                    memory_bytes: 120_000_000,
                    process_count: 3,
                    kinds: vec!["Vite".into()],
                },
            ],
            git_branch: Some("main".into()),
            git_dirty: Some(false),
            app_url: Some("http://localhost:8080".into()),
            php: Some(PhpVersionInfo {
                service: "laravel.test".into(),
                current: "8.4".into(),
                available: vec!["8.3".into(), "8.4".into()],
                node: Some("24".into()),
                node_available: vec!["18".into(), "20".into(), "22".into(), "24".into()],
            }),
            share_url: None,
            share_dashboard_url: None,
            local_domain: None,
        }
    }

    fn sample_docker() -> DockerStatus {
        DockerStatus {
            available: true,
            context_name: Some("default".into()),
            endpoint: Some("unix:///var/run/docker.sock".into()),
            error: None,
            reason: None,
        }
    }

    #[test]
    fn every_contract_type_roundtrips_through_json() {
        roundtrip(&sample_service());
        roundtrip(&sample_project());
        roundtrip(&sample_docker());
        roundtrip(&LogLine { service: "redis".into(), message: "ready".into(), stderr: false });
        roundtrip(&IntegrationSettings {
            terminal: Some("kitty".into()),
            editor: None,
            browser: Some("vivaldi".into()),
            auto_port_remap: true,
        });
        roundtrip(&SnapshotReport {
            snapshot: WorkspaceSnapshot {
                id: "snap-1".into(),
                workspace: WorkspaceId("w1".into()),
                name: "pre-upgrade".into(),
                taken_unix: 1_700_000_000,
                members: vec![SnapshotMemberState {
                    project: ProjectId("p1".into()),
                    project_name: "api".into(),
                    git_branch: Some("main".into()),
                    git_commit: Some("abc123".into()),
                    git_dirty: Some(false),
                    file_hashes: vec![SnapshotFileHash {
                        path: "/p/compose.yaml".into(),
                        sha256: "deadbeef".into(),
                    }],
                }],
            },
            deltas: vec![SnapshotDelta {
                project_name: "api".into(),
                changes: vec!["compose.yaml changed".into()],
            }],
            clean: false,
        });
        roundtrip(&FileEditPreview {
            file: "/p/compose.yaml".into(),
            before: "services: {}\n".into(),
            after: "services: {}\nnetworks: {}\n".into(),
            summary: vec!["attach app".into()],
            no_op: false,
        });
        roundtrip(&EnvReport {
            env_exists: true,
            example_exists: true,
            entries: vec![EnvEntryView {
                key: "DB_PASSWORD".into(),
                value: "hunter2".into(),
                secret: true,
                in_example: true,
            }],
            missing_from_env: vec!["MAIL_HOST".into()],
            findings: vec![EnvFinding {
                severity: FindingSeverity::Warning,
                key: Some("DB_HOST".into()),
                message: "not a service in this project".into(),
            }],
        });
        roundtrip(&EngineSnapshot {
            protocol_version: PROTOCOL_VERSION,
            seq: 42,
            read_only: false,
            docker: sample_docker(),
            integrations: IntegrationSettings::default(),
            watched_directories: vec!["/home/dev/code".into()],
            discovered: vec![DiscoveredProject {
                path: "/home/dev/code/other".into(),
                name: "other".into(),
                is_sail: false,
            }],
            projects: vec![sample_project()],
            workspaces: vec![WorkspaceSummary {
                id: WorkspaceId("w1".into()),
                name: "suite".into(),
                members: vec![WorkspaceMember {
                    project: ProjectId("p1".into()),
                    depends_on: vec![],
                }],
                status: ProjectStatus::Stopped,
                graph_error: None,
                warnings: vec!["host port 8080 used by two members".into()],
            }],
        });
        for event in [
            PatchEvent::ProjectAdded { project: sample_project() },
            PatchEvent::ProjectUpdated { project: sample_project() },
            PatchEvent::ProjectStatusChanged {
                id: ProjectId("p1".into()),
                status: ProjectStatus::Degraded,
            },
            PatchEvent::ProjectRemoved { id: ProjectId("p1".into()) },
            PatchEvent::DiscoveryChanged { discovered: vec![] },
            PatchEvent::WatchedDirectoriesChanged { directories: vec!["/x".into()] },
            PatchEvent::DockerStatusChanged { status: sample_docker() },
            PatchEvent::IntegrationsChanged {
                integrations: IntegrationSettings {
                    terminal: None,
                    editor: Some("code".into()),
                    browser: None,
                    auto_port_remap: false,
                },
            },
        ] {
            roundtrip(&EnginePatch { seq: 7, event });
        }
        roundtrip(&SubscriptionItem::ResyncRequired);
        for action in [
            Action::StartFakeOperation { project: ProjectId("p1".into()) },
            Action::ImportProject { path: "/x".into() },
            Action::RemoveProject { id: ProjectId("p1".into()) },
            Action::AddWatchedDirectory { path: "/x".into() },
            Action::RemoveWatchedDirectory { path: "/x".into() },
            Action::StartProject { id: ProjectId("p1".into()) },
            Action::StopProject { id: ProjectId("p1".into()) },
            Action::RestartProject { id: ProjectId("p1".into()) },
            Action::RebuildProject { id: ProjectId("p1".into()) },
            Action::RemoveOrphanContainer { id: ProjectId("p1".into()), service: "old".into() },
            Action::OpenTerminal { id: ProjectId("p1".into()) },
            Action::ShellIntoContainer { id: ProjectId("p1".into()), service: "app".into() },
            Action::OpenInEditor { id: ProjectId("p1".into()) },
            Action::RevealInFileManager { id: ProjectId("p1".into()) },
            Action::OpenInBrowser { id: ProjectId("p1".into()) },
            Action::SetIntegrations {
                integrations: IntegrationSettings {
                    terminal: Some("kitty".into()),
                    editor: None,
                    browser: None,
                    auto_port_remap: true,
                },
            },
            Action::SetEnvVar {
                id: ProjectId("p1".into()),
                key: "APP_PORT".into(),
                value: "8080".into(),
            },
            Action::RemoveEnvVar { id: ProjectId("p1".into()), key: "STALE".into() },
            Action::SaveWorkspace {
                id: None,
                name: "suite".into(),
                members: vec![WorkspaceMember {
                    project: ProjectId("p1".into()),
                    depends_on: vec![ProjectId("p0".into())],
                }],
            },
            Action::RemoveWorkspace { id: WorkspaceId("w1".into()) },
            Action::StartWorkspace { id: WorkspaceId("w1".into()) },
            Action::StopWorkspace { id: WorkspaceId("w1".into()) },
            Action::AttachNetwork {
                workspace: WorkspaceId("w1".into()),
                project: ProjectId("p1".into()),
            },
            Action::TakeSnapshot { workspace: WorkspaceId("w1".into()), name: "pre-upgrade".into() },
            Action::RemoveSnapshot { id: "snap-1".into() },
            Action::ApplyRepair {
                repair: "create-network".into(),
                arg: Some("mast-shared".into()),
                project: Some(ProjectId("p1".into())),
            },
            Action::AddCatalogService { id: ProjectId("p1".into()), service: "redis".into() },
            Action::RemoveCatalogService { id: ProjectId("p1".into()), service: "redis".into() },
            Action::RemoveService { id: ProjectId("p1".into()), service: "thinksolar-redis".into() },
            Action::AddCustomService {
                id: ProjectId("p1".into()),
                spec: CustomServiceSpec {
                    name: "tools".into(),
                    image: "ghcr.io/acme/tools:1".into(),
                    ports: vec!["8081:80".into()],
                    volume: Some("/data".into()),
                    command: None,
                },
            },
            Action::StartService { id: ProjectId("p1".into()), service: "redis".into() },
            Action::StopService { id: ProjectId("p1".into()), service: "redis".into() },
            Action::RestartService { id: ProjectId("p1".into()), service: "redis".into() },
            Action::CreateProject {
                parent: "/home/dev/code".into(),
                name: "new-app".into(),
                php: "84".into(),
                services: vec!["mysql".into(), "redis".into()],
            },
            Action::StartProcess { id: ProjectId("p1".into()), process: "reverb".into() },
            Action::StopProcess { id: ProjectId("p1".into()), process: "reverb".into() },
            Action::StopContainerRun { id: ProjectId("p1".into()), pid: 42, start_ticks: 16_984_000 },
            Action::SetProjectCommands {
                id: ProjectId("p1".into()),
                commands: vec![ProjectCommand {
                    name: "dev".into(),
                    command: "sail npm run dev".into(),
                    auto_start: true,
                    cwd: None,
                    after: Some("api".into()),
                    ready_when: Some("Server running on".into()),
                    auto_restart: true,
                    restart_when_changed: vec!["app/**".into(), "config/**".into()],
                    from_manifest: false,
                }],
            },
            Action::RunProjectCommand { id: ProjectId("p1".into()), name: "dev".into() },
            Action::RefreshNow,
            Action::CaptureServiceLogs { id: ProjectId("p1".into()), service: "queue".into() },
            Action::ClearLogCaptures,
            Action::ExportProjectManifest { id: ProjectId("p1".into()) },
            Action::OpenProjectFile {
                id: ProjectId("p1".into()),
                file: "app/Jobs/Ship.php".into(),
                line: Some(42),
            },
            Action::OpenTinker { id: ProjectId("p1".into()) },
            Action::OpenDbUrl { url: "mysql://sail:password@127.0.0.1:3306/laravel".into() },
            Action::SnapshotServiceData { id: ProjectId("p1".into()), service: "mysql".into() },
            Action::RestoreServiceData { id: ProjectId("p1".into()), group: "1a2b3c".into() },
            Action::RemoveServiceDataSnapshot { group: "1a2b3c".into() },
            Action::CloneProject {
                url: "git@github.com:acme/shop.git".into(),
                parent: "/home/dev/code".into(),
                name: "shop".into(),
            },
            Action::ReorderProjects {
                ids: vec![ProjectId("p2".into()), ProjectId("p1".into())],
            },
            Action::ReorderWorkspaces { ids: vec![WorkspaceId("w1".into())] },
            Action::StopHostRun { id: ProjectId("p1".into()), pid: 2_453_807, start_ticks: 9_100_000 },
            Action::StopWorktreeContainers {
                id: ProjectId("p1".into()),
                path: "/home/dev/code/app/.claude/worktrees/x".into(),
            },
            Action::RemoveWorktreeContainers {
                id: ProjectId("p1".into()),
                path: "/home/dev/code/app/.claude/worktrees/x".into(),
            },
            Action::RemoveWorktree {
                id: ProjectId("p1".into()),
                path: "/home/dev/code/app/.claude/worktrees/x".into(),
                force: false,
            },
            Action::PruneWorktrees { id: ProjectId("p1".into()) },
            Action::SetCommandDomain {
                id: ProjectId("p1".into()),
                command: "dev frontend".into(),
                domain: Some("suite.shop.test".into()),
                port: Some(5173),
            },
            Action::SetCommandDomain {
                id: ProjectId("p1".into()),
                command: "dev frontend".into(),
                domain: None,
                port: None,
            },
        ] {
            roundtrip(&action);
        }
        // An older client never sends `port`: it means "follow the command".
        let action: Action = serde_json::from_str(
            r#"{"type":"setCommandDomain","id":"p1","command":"dev","domain":"web.shop.test"}"#,
        )
        .unwrap();
        assert_eq!(
            action,
            Action::SetCommandDomain {
                id: ProjectId("p1".into()),
                command: "dev".into(),
                domain: Some("web.shop.test".into()),
                port: None,
            }
        );
        roundtrip(&VolumeSnapshot {
            group: "1a2b3c".into(),
            project: ProjectId("p1".into()),
            service: "mysql".into(),
            at_unix_ms: 1_700_000_000_000,
            volumes: vec!["sail-mysql".into()],
        });
        roundtrip(&UsageSample {
            at_unix_ms: 1_700_000_000_000,
            host_cores: 8,
            host_memory_bytes: 16 * 1024 * 1024 * 1024,
            services: vec![ServiceUsage {
                project: ProjectId("p1".into()),
                service: "mysql".into(),
                cpu_cores: 1.8125,
                memory_bytes: 240 * 1024 * 1024,
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_limited: true,
            }],
        });
        for reason in [
            CaptureReason::Teardown { verb: "restart".into() },
            CaptureReason::Exited { status: Some(137) },
            CaptureReason::Exited { status: None },
            CaptureReason::Unhealthy,
            CaptureReason::ReadyTimeout,
            CaptureReason::Manual,
        ] {
            roundtrip(&LogCapture {
                id: 12,
                at_unix_ms: 1_700_000_000_000,
                project: ProjectId("p1".into()),
                project_name: "acme".into(),
                service: "queue".into(),
                container_id: "abc123".into(),
                reason,
                window_secs: 60,
                lines: vec![
                    LogCaptureLine {
                        at: Some("2026-08-12T14:22:03.123456789Z".into()),
                        message: "Processing jobs".into(),
                        stderr: false,
                    },
                    LogCaptureLine {
                        at: None,
                        message: "connection refused".into(),
                        stderr: true,
                    },
                ],
                truncated: true,
            });
        }
        for kind in [
            OperationEventKind::Started,
            OperationEventKind::Progress { percent: 40, message: "scanning".into() },
            OperationEventKind::Output { line: "Container redis Created".into(), stderr: true },
            OperationEventKind::Completed,
            OperationEventKind::Failed { error: "boom".into() },
            OperationEventKind::Cancelled,
        ] {
            roundtrip(&OperationEvent { operation: OperationId(3), kind });
        }
        for err in [
            ErrorInfo::NotFound { what: "project p9".into() },
            ErrorInfo::InvalidInput { message: "bad path".into() },
            ErrorInfo::Conflict { message: "operation already running".into() },
            ErrorInfo::ReadOnly { owner_pid: Some(1234) },
            ErrorInfo::ProtocolMismatch { expected: 0, actual: 1 },
            ErrorInfo::Internal { message: "x".into() },
            ErrorInfo::VersionMismatch { client: "0.4.0".into(), server: "0.5.0".into() },
        ] {
            roundtrip(&err);
        }
    }
}
