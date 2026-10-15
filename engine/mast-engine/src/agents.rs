//! What coding agents are doing around a project (ADR-0012): the git
//! worktrees they work in, the agent sessions themselves, and the processes
//! on this machine those sessions start and leave behind.
//!
//! ADR-0011 made what runs inside the app container visible. Agents work
//! outside it as much as in it: each task gets its own worktree, and from
//! there a `sail up` brings up a second stack, an `npm run dev` a Vite on the
//! host, a browser tool a headless Chrome — started in the background, and
//! still running after the session that started them has finished.
//!
//! Worktrees come from git's admin directory, so they are found wherever an
//! agent puts them. Host processes come from one `/proc` walk (Linux only):
//! agent sessions by what program they are, and process trees by what they
//! run and what is above them — a live agent session, or init and the
//! user's service manager, which adopt whatever a finished session leaves.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use mast_contract::{AgentSession, HostRun, HostRunOwner, Worktree, WorktreeContainer};
use mast_docker::ContainerObservation;

use crate::inventory::{program_words, round_memory, short_command};

// ---------- worktrees ----------

/// One entry of `<repo>/.git/worktrees`, as git records it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorktreeEntry {
    pub name: String,
    pub path: PathBuf,
    /// The branch HEAD names; None when it is detached.
    pub branch: Option<String>,
    /// The commit a detached HEAD names, abbreviated.
    pub detached_at: Option<String>,
    pub locked: bool,
    pub created_at_unix: Option<u64>,
}

/// The worktrees git records for a repository whose `.git` is a directory.
/// `gitdir` names the checkout's `.git` file, so its parent is the checkout;
/// `HEAD` the branch; `locked` exists while it is locked. `commondir` is
/// written once, when the worktree is added, so its modification time is
/// when it was made.
pub(crate) fn read_worktree_entries(repo: &Path) -> Vec<WorktreeEntry> {
    let Ok(dir) = std::fs::read_dir(repo.join(".git").join("worktrees")) else {
        return Vec::new();
    };
    let mut out: Vec<WorktreeEntry> = dir
        .flatten()
        .filter_map(|entry| {
            let admin = entry.path();
            let gitdir = std::fs::read_to_string(admin.join("gitdir")).ok()?;
            let path = checkout_of(&admin, gitdir.trim())?;
            let head = std::fs::read_to_string(admin.join("HEAD")).unwrap_or_default();
            let (branch, detached_at) = parse_head(&head);
            let created_at_unix = std::fs::metadata(admin.join("commondir"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            Some(WorktreeEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path,
                branch,
                detached_at,
                locked: admin.join("locked").exists(),
                created_at_unix,
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The checkout a `gitdir` file points into. Git writes it absolute unless
/// `worktree.useRelativePaths` is set, and then relative to the admin entry.
fn checkout_of(admin: &Path, gitdir: &str) -> Option<PathBuf> {
    if gitdir.is_empty() {
        return None;
    }
    let file = Path::new(gitdir);
    let file = if file.is_absolute() { file.to_path_buf() } else { lexical(&admin.join(file)) };
    file.parent().map(Path::to_path_buf)
}

/// `a/b/../c` → `a/c`, without touching the filesystem: the checkout may be
/// gone, and that is exactly the case worth reporting.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `ref: refs/heads/x` → the branch; a bare object id → detached there.
fn parse_head(head: &str) -> (Option<String>, Option<String>) {
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (Some(branch.to_string()), None);
    }
    if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        return (None, Some(head[..7].to_string()));
    }
    (None, None)
}

/// The tool whose convention a worktree's location follows. A location is
/// evidence of where a tool puts its worktrees, not of who made this one,
/// and the UI words it that way.
pub(crate) fn tool_of(path: &Path) -> Option<&'static str> {
    const CONVENTIONS: &[(&str, &str)] = &[
        ("/.claude/worktrees/", "Claude Code"),
        ("/.codex/worktrees/", "Codex"),
        ("/.cursor/worktrees/", "Cursor"),
        ("/conductor/workspaces/", "Conductor"),
    ];
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some((_, tool)) = CONVENTIONS.iter().find(|(marker, _)| text.contains(*marker)) {
        return Some(*tool);
    }
    // Claude Code's per-session scratch space: `$TMPDIR/claude-<uid>/…`.
    let scratch = path
        .strip_prefix(std::env::temp_dir())
        .ok()
        .and_then(|rest| rest.components().next())
        .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with("claude-"));
    scratch.then_some("Claude Code")
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct WorktreeStatus {
    pub head: Option<String>,
    pub changed_files: Option<u32>,
    pub unmerged_commits: Option<u32>,
}

/// `git status --porcelain=v2 --branch`: the commit checked out
/// (`# branch.oid`) and one line per changed or untracked path.
fn parse_status_v2(text: &str) -> (Option<String>, u32) {
    let mut oid = None;
    let mut changed = 0;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("# branch.oid ") {
            let value = value.trim();
            if value.len() >= 7 && value.chars().all(|c| c.is_ascii_hexdigit()) {
                oid = Some(value.to_string());
            }
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            changed += 1;
        }
    }
    (oid, changed)
}

/// Run git for a reading, never contending with the agent working there:
/// `GIT_OPTIONAL_LOCKS=0` stops `status` refreshing the index, which takes
/// `index.lock` and makes a concurrent `git commit` fail.
pub(crate) fn git_read(dir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What removing a worktree would lose: its uncommitted files, and the
/// commits on it the project's own checkout does not have.
pub(crate) fn worktree_status(repo: &Path, checkout: &Path) -> WorktreeStatus {
    let Some(status) = git_read(checkout, &["status", "--porcelain=v2", "--branch"]) else {
        return WorktreeStatus::default();
    };
    let (oid, changed) = parse_status_v2(&status);
    let unmerged = oid.as_deref().and_then(|oid| {
        git_read(repo, &["rev-list", "--count", &format!("HEAD..{oid}")])
            .and_then(|count| count.trim().parse().ok())
    });
    WorktreeStatus {
        head: oid.map(|oid| oid[..7].to_string()),
        changed_files: Some(changed),
        unmerged_commits: unmerged,
    }
}

/// A project's worktrees with their state, containers still to be attached.
/// Blocking: two `git` runs per worktree that exists.
pub(crate) fn read_worktrees(repo: &Path) -> Vec<Worktree> {
    read_worktree_entries(repo)
        .into_iter()
        .map(|entry| {
            let missing = !entry.path.is_dir();
            let status = if missing {
                WorktreeStatus::default()
            } else {
                worktree_status(repo, &entry.path)
            };
            Worktree {
                path: entry.path.to_string_lossy().into_owned(),
                tool: tool_of(&entry.path).map(String::from),
                name: entry.name,
                branch: entry.branch,
                head: status.head.or(entry.detached_at),
                created_at_unix: entry.created_at_unix,
                missing,
                locked: entry.locked,
                changed_files: status.changed_files,
                unmerged_commits: status.unmerged_commits,
                containers: Vec::new(),
            }
        })
        .collect()
}

/// Whether a container was started from inside `dir`: its compose working
/// directory or one of its config files lies there.
pub(crate) fn started_from(observation: &ContainerObservation, dir: &Path) -> bool {
    observation.working_dir.as_deref().is_some_and(|w| Path::new(w).starts_with(dir))
        || observation.config_files.iter().any(|f| Path::new(f).starts_with(dir))
}

/// The containers started from one worktree.
pub(crate) fn worktree_containers(
    observations: &[ContainerObservation],
    dir: &Path,
) -> Vec<WorktreeContainer> {
    let mut out: Vec<WorktreeContainer> = observations
        .iter()
        .filter(|o| started_from(o, dir))
        .map(|o| WorktreeContainer {
            id: o.id.clone(),
            name: o.name.clone(),
            service: o.service.clone(),
            compose_project: o.project.clone(),
            running: o.state == "running",
            ports: o.published_ports.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// An advisory for the project when a worktree's containers carry the
/// project's own compose name: compose takes them for one project, so a
/// `sail up` in either recreates the other's containers from its checkout.
pub(crate) fn shared_name_advisories(
    compose_project_name: Option<&str>,
    worktrees: &[Worktree],
) -> Vec<mast_contract::Advisory> {
    let Some(name) = compose_project_name else { return Vec::new() };
    worktrees
        .iter()
        .filter(|w| w.containers.iter().any(|c| c.compose_project == name))
        .map(|w| mast_contract::Advisory {
            message: format!(
                "Containers started from the worktree {} use this project's compose name ({name}), \
                 so compose treats the two as one project: a sail up in either recreates the \
                 other's containers from its own checkout, and pages serve whichever started \
                 last. Give the worktree its own COMPOSE_PROJECT_NAME in its .env.",
                w.name
            ),
            actions: Vec::new(),
        })
        .collect()
}

// ---------- host processes ----------

// Only Linux fills these in outside tests (see `scan_host`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HostProc {
    pub pid: u32,
    pub ppid: u32,
    /// Start time in clock ticks since boot.
    pub start: u64,
    pub rss_pages: u64,
    pub cmdline: String,
    /// None for another user's process, whose working directory is not
    /// readable.
    pub cwd: Option<PathBuf>,
    /// The working directory has been deleted since (`… (deleted)`).
    pub cwd_deleted: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HostScan {
    pub own_pid: u32,
    pub clk_tck: u64,
    pub uptime_secs: f64,
    pub page_size: u64,
    pub procs: Vec<HostProc>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// `/proc/<pid>/stat` after the command name, which may hold spaces and
/// parentheses: (state, ppid, start ticks, rss pages).
fn parse_stat(stat: &str) -> Option<(char, u32, u64, u64)> {
    let rest = &stat[stat.rfind(") ")? + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some((
        fields.first()?.chars().next()?,
        fields.get(1)?.parse().ok()?,
        fields.get(19)?.parse().ok()?,
        fields.get(21)?.parse().ok()?,
    ))
}

/// Every process this user can see, with its working directory where it is
/// readable. Kernel threads (no command line) and zombies are left out.
#[cfg(target_os = "linux")]
pub(crate) fn scan_host() -> Option<HostScan> {
    let uptime_secs: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // SAFETY: sysconf only reads a configuration value.
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let mut procs = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let dir = entry.path();
        let Some((state, ppid, start, rss_pages)) =
            std::fs::read_to_string(dir.join("stat")).ok().as_deref().and_then(parse_stat)
        else {
            continue;
        };
        if matches!(state, 'Z' | 'X') {
            continue;
        }
        let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ").trim().to_string();
        if cmdline.is_empty() {
            continue;
        }
        let (cwd, cwd_deleted) = match std::fs::read_link(dir.join("cwd")) {
            Ok(path) => match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
                Some(gone) => (Some(PathBuf::from(gone)), true),
                None => (Some(path), false),
            },
            Err(_) => (None, false),
        };
        procs.push(HostProc { pid, ppid, start, rss_pages, cmdline, cwd, cwd_deleted });
    }
    Some(HostScan {
        own_pid: std::process::id(),
        clk_tck: if clk_tck > 0 { clk_tck as u64 } else { 100 },
        uptime_secs,
        page_size: if page_size > 0 { page_size as u64 } else { 4096 },
        procs,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn scan_host() -> Option<HostScan> {
    None
}

fn basename(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

/// Interpreters an agent CLI or a package manager may run under.
fn is_interpreter(program: &str) -> bool {
    matches!(program, "node" | "bun" | "deno") || program.starts_with("python")
}

/// The script an interpreter runs: its first argument that is not a flag.
fn script_of<'a>(words: &[&'a str]) -> Option<&'a str> {
    words.iter().skip(1).find(|w| !w.starts_with('-')).copied()
}

/// Which coding agent a process is, read from the program it runs — never
/// from a name anywhere in its arguments.
pub(crate) fn agent_of(cmdline: &str) -> Option<&'static str> {
    const NATIVE: &[(&str, &str)] = &[
        ("claude", "Claude Code"),
        ("codex", "Codex"),
        ("gemini", "Gemini CLI"),
        ("aider", "Aider"),
        ("opencode", "opencode"),
        ("cursor-agent", "Cursor Agent"),
        ("amp", "Amp"),
        ("goose", "Goose"),
        ("qwen", "Qwen Code"),
        ("crush", "Crush"),
        ("droid", "Factory Droid"),
        ("copilot", "Copilot CLI"),
        ("auggie", "Auggie"),
        ("kiro-cli", "Kiro"),
    ];
    const PACKAGES: &[(&str, &str)] = &[
        ("@anthropic-ai/claude-code", "Claude Code"),
        ("@openai/codex", "Codex"),
        ("@google/gemini-cli", "Gemini CLI"),
        ("opencode-ai", "opencode"),
        ("@sourcegraph/amp", "Amp"),
        ("@qwen-code/qwen-code", "Qwen Code"),
        ("@github/copilot", "Copilot CLI"),
        ("@augmentcode/auggie", "Auggie"),
        ("@charmland/crush", "Crush"),
    ];
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    let named = |program: &str| {
        // Codex's npm package launches a per-platform binary,
        // `codex-x86_64-unknown-linux-musl`.
        let program = if program.starts_with("codex-") { "codex" } else { program };
        NATIVE.iter().find(|(name, _)| *name == program).map(|(_, agent)| *agent)
    };
    let program = basename(words.first()?);
    if is_interpreter(program) {
        let script = script_of(&words)?;
        let normalized = script.replace('\\', "/");
        return PACKAGES
            .iter()
            .find(|(package, _)| normalized.contains(&format!("/{package}/")))
            .map(|(_, agent)| *agent)
            .or_else(|| named(basename(script)));
    }
    named(program)
}

/// A package manager's command line → the script it runs (`npm run dev`,
/// `pnpm dev`, `composer run dev`), when the program is a package manager.
fn package_script<'a>(words: &[&'a str]) -> Option<&'a str> {
    let program = basename(words.first()?);
    let managers = ["npm", "pnpm", "yarn", "bun", "composer"];
    let args: &[&str] = if managers.contains(&program) {
        &words[1..]
    } else if (is_interpreter(program) || program.starts_with("php"))
        && words.get(1).is_some_and(|script| {
            let name = basename(script);
            managers.iter().any(|m| name == *m || name.starts_with(&format!("{m}.")))
                || name == "npm-cli.js"
                || name == "composer.phar"
        })
    {
        &words[2..]
    } else {
        return None;
    };
    let mut rest = args.iter().filter(|w| !w.starts_with('-'));
    match rest.next()? {
        &"run" | &"run-script" => rest.next().copied(),
        script => Some(*script),
    }
}

const DEV_SCRIPTS: &[&str] = &["dev", "start", "serve", "watch", "preview"];

/// What a host process is, in the terms the Agents view uses — the
/// container kinds (Horizon, Vite, …) plus what only happens on the host:
/// a `sail`/`docker compose` client, a dev script, PHP's built-in server, a
/// browser being driven by a tool. Empty for everything else, which is how
/// an editor, a terminal or a file manager opened on the project stays out
/// of the list.
pub(crate) fn host_kinds_of(cmdline: &str) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    if let Some(kind) = crate::inventory::kind_of(cmdline) {
        kinds.push(kind);
    }
    let words = program_words(cmdline);
    let Some(first) = words.first() else { return kinds };
    let program = basename(first);
    if program == "docker" || program == "docker-compose" {
        let verbs: Vec<&str> =
            words[1..].iter().filter(|w| !w.starts_with('-')).copied().collect();
        let compose = program == "docker-compose" || verbs.contains(&"compose");
        let verb = verbs.iter().find(|v| matches!(**v, "up" | "exec" | "run"));
        match (compose, verb) {
            (true, Some(&"up")) => kinds.push("Compose up"),
            (_, Some(&"exec" | &"run")) => kinds.push("Container exec"),
            _ => {}
        }
    }
    if package_script(&words).is_some_and(|script| DEV_SCRIPTS.contains(&script)) {
        kinds.push("Dev script");
    }
    if program.starts_with("php") && words.contains(&"-S") {
        kinds.push("PHP server");
    }
    let browser = matches!(
        program,
        "chrome"
            | "chromium"
            | "chromium-browser"
            | "google-chrome"
            | "google-chrome-stable"
            | "chrome-headless-shell"
            | "headless_shell"
            | "msedge"
            | "Google Chrome"
    );
    // The browser's main process only: its renderers and helpers carry
    // `--type=` and belong to the same tree anyway.
    if browser
        && !words.iter().any(|w| w.starts_with("--type="))
        && words.iter().any(|w| {
            w.starts_with("--headless")
                || w.starts_with("--remote-debugging-port")
                || *w == "--remote-debugging-pipe"
        })
    {
        kinds.push("Headless browser");
    }
    kinds
}

/// Plumbing a job runs through on its way to the process that matters:
/// shells, package managers, process runners. A job's root is the topmost
/// of these above it — climbing stops at anything else, so an editor or a
/// terminal is never taken for part of the job.
fn is_wrapper(cmdline: &str) -> bool {
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    let Some(first) = words.first() else { return false };
    let program = basename(first);
    const PROGRAMS: &[&str] = &[
        "sh", "bash", "dash", "zsh", "fish", "ash", "env", "nohup", "setsid", "timeout",
        "stdbuf", "npm", "npx", "pnpm", "pnpx", "yarn", "bun", "bunx", "composer", "make",
        "just", "vp",
    ];
    if PROGRAMS.contains(&program) || package_script(&words).is_some() {
        return true;
    }
    if !(is_interpreter(program) || program.starts_with("php")) {
        return false;
    }
    let Some(script) = script_of(&words) else { return false };
    let script = script.replace('\\', "/");
    // A runner under an interpreter: `npx concurrently`, `@laravel/multiplex`.
    if ["/concurrently/", "/.bin/concurrently", "/multiplex", "/npx-cli.js"]
        .iter()
        .any(|marker| script.contains(marker))
    {
        return true;
    }
    // A package's CLI asked for a dev server — `vp dev`, `nuxt dev`, `astro
    // dev` — which runs the server as its child.
    script.contains("node_modules/")
        && words
            .iter()
            .skip_while(|w| !w.replace('\\', "/").ends_with(&*script))
            .skip(1)
            .find(|w| !w.starts_with('-'))
            .is_some_and(|verb| DEV_SCRIPTS.contains(verb))
}

/// The user's service manager, which adopts whatever a finished session's
/// children leave behind (as init does where there is none).
fn is_service_manager(cmdline: &str) -> bool {
    let words: Vec<&str> = cmdline.split_whitespace().collect();
    match words.first().map(|w| basename(w)) {
        Some("systemd") => words.contains(&"--user"),
        Some("launchd") => true,
        _ => false,
    }
}

/// Where something works: a project's checkout, or one of its worktrees.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    pub project: String,
    pub dir: PathBuf,
    /// The worktree's path; None for the project's own checkout.
    pub worktree: Option<PathBuf>,
}

/// The place a directory is in: the deepest one containing it, so a nested
/// worktree wins over the checkout it lives inside.
fn place_of<'a>(places: &'a [Place], dir: &Path) -> Option<&'a Place> {
    places
        .iter()
        .filter(|place| dir.starts_with(&place.dir))
        .max_by_key(|place| place.dir.components().count())
}

/// What a project's agents are doing, per project id.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Activity {
    pub sessions: HashMap<String, Vec<AgentSession>>,
    pub runs: HashMap<String, Vec<HostRun>>,
}

/// What is above a job's root.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Anchor {
    /// A live agent session (its topmost process).
    Agent(u32),
    /// Init or the user's service manager: its starter has exited.
    Orphaned,
}

struct Tree<'a> {
    procs: HashMap<u32, &'a HostProc>,
    children: HashMap<u32, Vec<u32>>,
    agents: HashMap<u32, &'static str>,
    own_pid: u32,
}

impl<'a> Tree<'a> {
    fn new(scan: &'a HostScan) -> Self {
        let procs: HashMap<u32, &HostProc> = scan.procs.iter().map(|p| (p.pid, p)).collect();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in &scan.procs {
            children.entry(p.ppid).or_default().push(p.pid);
        }
        let agents = scan
            .procs
            .iter()
            .filter_map(|p| agent_of(&p.cmdline).map(|agent| (p.pid, agent)))
            .collect();
        Self { procs, children, agents, own_pid: scan.own_pid }
    }

    /// A process's ancestors, nearest first, as far as this user can see.
    fn ancestors(&self, pid: u32) -> impl Iterator<Item = u32> + '_ {
        let mut current = pid;
        std::iter::from_fn(move || {
            let parent = self.procs.get(&current)?.ppid;
            (parent > 0 && parent != current).then(|| {
                current = parent;
                parent
            })
        })
        .take(256)
    }

    /// An agent process's session: the topmost of the agent processes it
    /// runs under, so a wrapper and the binary it launches are one session.
    fn session_of(&self, pid: u32) -> u32 {
        let mut top = pid;
        for ancestor in self.ancestors(pid) {
            if self.agents.contains_key(&ancestor) {
                top = ancestor;
            } else {
                break;
            }
        }
        top
    }

    fn descends_from_mast(&self, pid: u32) -> bool {
        pid == self.own_pid || self.ancestors(pid).any(|a| a == self.own_pid)
    }

    /// The root of the job a process belongs to and what is above it — or
    /// None when it is someone else's: Mast's own commands, or a job a live
    /// terminal, editor or multiplexer is running.
    fn job_of(&self, pid: u32) -> Option<(u32, Anchor)> {
        let mut root = pid;
        loop {
            let parent = self.procs.get(&root)?.ppid;
            if parent == self.own_pid {
                return None;
            }
            if parent == 1 {
                return Some((root, Anchor::Orphaned));
            }
            if self.agents.contains_key(&parent) {
                return Some((root, Anchor::Agent(self.session_of(parent))));
            }
            let above = self.procs.get(&parent)?;
            if is_service_manager(&above.cmdline) {
                return Some((root, Anchor::Orphaned));
            }
            if is_wrapper(&above.cmdline) || !host_kinds_of(&above.cmdline).is_empty() {
                root = parent;
                continue;
            }
            // Something that is neither plumbing nor the end of the line —
            // an MCP server, a terminal, an editor. Its job is an agent's if
            // an agent is above it (a browser tool's Chrome), and nobody's to
            // stop otherwise.
            for ancestor in self.ancestors(parent) {
                if ancestor == self.own_pid {
                    return None;
                }
                if self.agents.contains_key(&ancestor) {
                    return Some((root, Anchor::Agent(self.session_of(ancestor))));
                }
            }
            return None;
        }
    }

    /// The root and everything beneath it, shallowest first.
    fn members(&self, root: u32) -> Vec<&'a HostProc> {
        let mut out = Vec::new();
        let mut queue = VecDeque::from([root]);
        let mut seen = HashSet::new();
        while let Some(pid) = queue.pop_front() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(p) = self.procs.get(&pid) {
                out.push(*p);
            }
            if let Some(children) = self.children.get(&pid) {
                queue.extend(children.iter().copied());
            }
        }
        out
    }
}

/// Group a host scan into each project's agent sessions and the jobs on
/// this machine working in its checkout or worktrees.
pub(crate) fn classify(scan: &HostScan, places: &[Place], now_unix: u64) -> Activity {
    let tree = Tree::new(scan);
    let boot_unix = now_unix as f64 - scan.uptime_secs;
    let started_at = |start: u64| (boot_unix + start as f64 / scan.clk_tck as f64).max(0.0) as u64;
    let mut activity = Activity::default();

    for (&pid, agent) in &tree.agents {
        if tree.session_of(pid) != pid || tree.descends_from_mast(pid) {
            continue;
        }
        let p = tree.procs[&pid];
        let Some(place) = p.cwd.as_deref().and_then(|cwd| place_of(places, cwd)) else {
            continue;
        };
        activity.sessions.entry(place.project.clone()).or_default().push(AgentSession {
            agent: agent.to_string(),
            pid,
            start_ticks: p.start,
            cwd: p.cwd.as_deref().unwrap_or(Path::new("")).to_string_lossy().into_owned(),
            worktree: place.worktree.as_ref().map(|w| w.to_string_lossy().into_owned()),
            started_at_unix: started_at(p.start),
        });
    }

    // Each job once, however many of its processes are worth listing; the
    // shallowest of those names it and says where it works.
    let mut jobs: HashMap<u32, Anchor> = HashMap::new();
    for p in &scan.procs {
        if tree.agents.contains_key(&p.pid) || host_kinds_of(&p.cmdline).is_empty() {
            continue;
        }
        if p.cwd.as_deref().and_then(|cwd| place_of(places, cwd)).is_none() {
            continue;
        }
        if let Some((root, anchor)) = tree.job_of(p.pid) {
            jobs.insert(root, anchor);
        }
    }
    for (root, anchor) in jobs {
        let members = tree.members(root);
        let mut kinds: BTreeSet<&'static str> = BTreeSet::new();
        let mut named: Option<&HostProc> = None;
        for member in &members {
            let member_kinds = host_kinds_of(&member.cmdline);
            if !member_kinds.is_empty()
                && named.is_none()
                && member.cwd.as_deref().and_then(|cwd| place_of(places, cwd)).is_some()
            {
                named = Some(*member);
            }
            kinds.extend(member_kinds);
        }
        let Some(named) = named else { continue };
        let Some(cwd) = named.cwd.as_deref() else { continue };
        let Some(place) = place_of(places, cwd) else { continue };
        let root_proc = tree.procs[&root];
        let owner = match anchor {
            Anchor::Agent(session) => HostRunOwner::Agent {
                agent: tree.agents.get(&session).copied().unwrap_or("agent").to_string(),
                pid: session,
            },
            Anchor::Orphaned => HostRunOwner::Orphaned,
        };
        let rss: u64 = members.iter().map(|m| m.rss_pages).sum();
        activity.runs.entry(place.project.clone()).or_default().push(HostRun {
            pid: root,
            start_ticks: root_proc.start,
            owner,
            title: host_title(&named.cmdline),
            command_line: named.cmdline.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
            worktree: place.worktree.as_ref().map(|w| w.to_string_lossy().into_owned()),
            cwd_deleted: named.cwd_deleted,
            started_at_unix: started_at(root_proc.start),
            memory_bytes: round_memory(rss * scan.page_size),
            process_count: members.len() as u32,
            kinds: kinds.into_iter().map(String::from).collect(),
        });
    }
    for sessions in activity.sessions.values_mut() {
        sessions.sort_by_key(|s| (s.started_at_unix, s.pid));
    }
    for runs in activity.runs.values_mut() {
        runs.sort_by_key(|r| (r.started_at_unix, r.pid));
    }
    activity
}

/// A host command line short enough for a row: a browser by its name
/// rather than its forty flags, everything else as the container list
/// shortens it.
fn host_title(cmdline: &str) -> String {
    let words = program_words(cmdline);
    let program = words.first().map(|w| basename(w)).unwrap_or("");
    let kinds = host_kinds_of(cmdline);
    // A Vite's command line is a long path into node_modules; the port is
    // what tells two apart.
    if kinds.contains(&"Vite") {
        let port = words
            .iter()
            .position(|w| *w == "--port")
            .and_then(|at| words.get(at + 1))
            .map(|port| port.to_string())
            .or_else(|| words.iter().find_map(|w| w.strip_prefix("--port=")).map(String::from));
        return match port {
            Some(port) => format!("Vite :{port}"),
            None => "Vite".to_string(),
        };
    }
    // What a container client runs inside, not the client's own flags.
    if kinds.contains(&"Container exec")
        && let Some(command) = container_command(&words)
    {
        return short_command(&command.join(" "));
    }
    if kinds.contains(&"Headless browser") {
        let headless = words.iter().any(|w| w.starts_with("--headless"));
        let name = if program.contains("chrom") || program == "headless_shell" {
            "Chrome"
        } else {
            program
        };
        return if headless { format!("headless {name}") } else { format!("{name} (automated)") };
    }
    short_command(cmdline)
}

/// The command a `docker exec`, `docker compose exec` or `run` starts in its
/// container: the words after its options and the container or service.
fn container_command<'a>(words: &[&'a str]) -> Option<Vec<&'a str>> {
    const WITH_VALUE: &[&str] = &[
        "-e", "--env", "--env-file", "-u", "--user", "-w", "--workdir", "--detach-keys",
        "--index", "-v", "--volume", "-p", "--publish", "--name", "--entrypoint", "-l",
        "--label",
    ];
    let verb = words.iter().position(|w| matches!(*w, "exec" | "run"))?;
    let mut rest = words[verb + 1..].iter();
    while let Some(word) = rest.next() {
        if !word.starts_with('-') {
            let command: Vec<&str> = rest.copied().collect();
            return (!command.is_empty()).then_some(command);
        }
        if WITH_VALUE.contains(word) {
            rest.next();
        }
    }
    None
}

/// The job roots working inside `dir` that the listing would show — what
/// removing a worktree stops first — and the agent sessions working there,
/// which it refuses to pull the directory out from under.
pub(crate) fn work_in(
    scan: &HostScan,
    project: &str,
    places: &[Place],
    dir: &Path,
    now_unix: u64,
) -> (Vec<HostRun>, Vec<AgentSession>) {
    let activity = classify(scan, places, now_unix);
    let inside = |cwd: &str| Path::new(cwd).starts_with(dir);
    let runs = activity
        .runs
        .get(project)
        .map(|runs| runs.iter().filter(|r| inside(&r.cwd)).cloned().collect())
        .unwrap_or_default();
    let sessions = activity
        .sessions
        .get(project)
        .map(|sessions| sessions.iter().filter(|s| inside(&s.cwd)).cloned().collect())
        .unwrap_or_default();
    (runs, sessions)
}

/// Stop a process tree on this machine: TERM to the root and every
/// descendant, including any started while it stops, up to `grace` for all
/// of it to exit, then KILL what is left. The root must still have `start`
/// — a mismatch is a different process that reused the number, and stops
/// nothing. Mast itself and its ancestors are never signalled. Blocking.
/// Returns how many processes were signalled.
#[cfg(unix)]
pub(crate) fn stop_host_tree(
    pid: u32,
    start: u64,
    grace: std::time::Duration,
) -> Result<usize, String> {
    use std::time::{Duration, Instant};
    let scan = scan_host().ok_or("cannot read the process table")?;
    if !scan.procs.iter().any(|p| p.pid == pid && p.start == start) {
        return Ok(0);
    }
    let protected: HashSet<u32> = {
        let tree = Tree::new(&scan);
        std::iter::once(scan.own_pid).chain(tree.ancestors(scan.own_pid)).collect()
    };
    if protected.contains(&pid) {
        return Err("refusing to stop Mast itself".into());
    }
    let mut tracked: HashMap<u32, u64> = HashMap::from([(pid, start)]);
    let mut signalled: HashSet<u32> = HashSet::new();
    let deadline = Instant::now() + grace;
    let mut killed_at: Option<Instant> = None;
    loop {
        let scan = scan_host().ok_or("cannot read the process table")?;
        let alive: HashMap<u32, &HostProc> = scan.procs.iter().map(|p| (p.pid, p)).collect();
        tracked.retain(|pid, start| alive.get(pid).is_some_and(|p| p.start == *start));
        // Everything beneath what is tracked, however deep, including
        // children forked since the last look.
        let mut grown = true;
        while grown {
            grown = false;
            for p in &scan.procs {
                if !tracked.contains_key(&p.pid)
                    && tracked.contains_key(&p.ppid)
                    && !protected.contains(&p.pid)
                {
                    tracked.insert(p.pid, p.start);
                    grown = true;
                }
            }
        }
        if tracked.is_empty() {
            return Ok(signalled.len());
        }
        let now = Instant::now();
        if let Some(at) = killed_at
            && now >= at + Duration::from_secs(2)
        {
            let mut left: Vec<u32> = tracked.keys().copied().collect();
            left.sort_unstable();
            let left: Vec<String> = left.iter().map(u32::to_string).collect();
            return Err(format!("processes survived SIGKILL: {}", left.join(" ")));
        }
        let kill = now >= deadline;
        for pid in tracked.keys() {
            if kill || !signalled.contains(pid) {
                let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
                // SAFETY: kill only sends a signal; the PID was just read
                // with the start time it was tracked by.
                unsafe { libc::kill(*pid as libc::pid_t, signal) };
                signalled.insert(*pid);
            }
        }
        if kill && killed_at.is_none() {
            killed_at = Some(now);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(not(unix))]
pub(crate) fn stop_host_tree(
    _pid: u32,
    _start: u64,
    _grace: std::time::Duration,
) -> Result<usize, String> {
    Err("stopping processes on this machine is only supported on Linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, start: u64, cwd: Option<&str>, cmdline: &str) -> HostProc {
        HostProc {
            pid,
            ppid,
            start,
            rss_pages: 1000,
            cmdline: cmdline.into(),
            cwd: cwd.map(PathBuf::from),
            cwd_deleted: false,
        }
    }

    const APP: &str = "/home/dev/code/shop";
    const WT: &str = "/home/dev/code/shop/.claude/worktrees/fix-login";

    fn places() -> Vec<Place> {
        vec![
            Place { project: "p1".into(), dir: APP.into(), worktree: None },
            Place { project: "p1".into(), dir: WT.into(), worktree: Some(WT.into()) },
        ]
    }

    /// The shape of the machine this was built against: Mast and the
    /// commands it runs, a Claude Code session with a background dev server
    /// in a worktree and a browser tool's Chrome, a Chrome a finished
    /// session left to the service manager, a terminal running a dev
    /// server, and an editor opened on the project.
    fn sample() -> HostScan {
        let home = Some("/home/dev");
        let app = Some(APP);
        let wt = Some(WT);
        HostScan {
            own_pid: 500,
            clk_tck: 100,
            uptime_secs: 10_000.0,
            page_size: 4096,
            procs: vec![
                proc(1602, 1, 100, Some("/"), "/usr/lib/systemd/systemd --user"),
                // Mast and a Sail command it runs.
                proc(500, 1602, 1_000, home, "/usr/bin/mast-desktop"),
                proc(501, 500, 1_100, app, "bash /home/dev/code/shop/vendor/bin/sail artisan dev"),
                proc(502, 501, 1_101, app, "docker compose exec -T laravel.test php artisan dev"),
                // A terminal with Claude Code in it.
                proc(700, 1602, 2_000, home, "/usr/bin/konsole"),
                proc(701, 700, 2_001, app, "/bin/bash"),
                proc(702, 701, 2_002, app, "claude"),
                // Its background dev server, in the worktree.
                proc(710, 702, 3_000, wt, "/bin/bash -c source /home/dev/.claude/snap.sh && eval 'npm run dev'"),
                proc(711, 710, 3_001, wt, "npm run dev"),
                proc(712, 711, 3_002, wt, "sh -c vite"),
                proc(713, 712, 3_003, wt, "node /home/dev/code/shop/.claude/worktrees/fix-login/node_modules/.bin/vite"),
                // Its MCP servers and the browser one launched.
                proc(720, 702, 2_010, app, "node /home/dev/.npm/_npx/x/node_modules/.bin/chrome-devtools-mcp"),
                proc(721, 720, 2_500, app, "/opt/google/chrome/chrome --headless=new --remote-debugging-port=9222"),
                proc(722, 721, 2_501, app, "/opt/google/chrome/chrome --type=renderer --headless=new"),
                proc(730, 702, 2_011, app, "/home/dev/.local/bin/mast mcp"),
                // A short tool call that is nothing worth listing.
                proc(740, 702, 4_000, app, "/bin/bash -c source /home/dev/.claude/snap.sh && eval 'git status'"),
                proc(741, 740, 4_001, app, "git status"),
                // A Chrome a finished session left to the service manager.
                proc(800, 1602, 1_500, app, "/opt/google/chrome/chrome --headless=new --remote-debugging-port=9333"),
                proc(801, 800, 1_501, app, "/opt/google/chrome/chrome --type=zygote --headless=new"),
                proc(802, 801, 1_502, app, "/opt/google/chrome/chrome --type=renderer --headless=new"),
                // Someone's own terminal running Vite.
                proc(900, 1602, 5_000, home, "/usr/bin/kitty"),
                proc(901, 900, 5_001, app, "/bin/zsh"),
                proc(902, 901, 5_002, app, "npm run dev"),
                proc(903, 902, 5_003, app, "node node_modules/.bin/vite"),
                // An editor opened on the project, reparented when its
                // launcher exited, running a language server.
                proc(950, 1602, 6_000, app, "/usr/share/code/code ."),
                proc(951, 950, 6_001, app, "/usr/share/code/code --type=utility node_modules/intelephense"),
                // A queue worker whose starter exited, in the worktree.
                proc(960, 1, 7_000, wt, "php artisan queue:work"),
            ],
        }
    }

    fn run(activity: &Activity, pid: u32) -> &HostRun {
        activity.runs["p1"].iter().find(|r| r.pid == pid).unwrap_or_else(|| {
            panic!("no run {pid}: {:#?}", activity.runs)
        })
    }

    #[test]
    fn agent_sessions_are_found_by_program_and_place() {
        let activity = classify(&sample(), &places(), 1_000_000);
        let sessions = &activity.sessions["p1"];
        assert_eq!(sessions.len(), 1, "{sessions:#?}");
        assert_eq!(sessions[0].agent, "Claude Code");
        assert_eq!(sessions[0].pid, 702);
        assert_eq!(sessions[0].worktree, None, "it works from the checkout");
        assert_eq!(sessions[0].started_at_unix, 1_000_000 - 10_000 + 20);
    }

    #[test]
    fn a_sessions_background_job_is_one_run_rooted_at_its_tool_call() {
        let activity = classify(&sample(), &places(), 1_000_000);
        let dev = run(&activity, 710);
        assert_eq!(dev.owner, HostRunOwner::Agent { agent: "Claude Code".into(), pid: 702 });
        assert_eq!(dev.title, "npm run dev", "named by its shallowest listed process");
        assert_eq!(dev.kinds, ["Dev script", "Vite"]);
        assert_eq!(dev.worktree.as_deref(), Some(WT));
        assert_eq!(dev.process_count, 4);
        assert!(activity.runs["p1"].iter().all(|r| ![711, 712, 713].contains(&r.pid)));
    }

    #[test]
    fn a_browser_under_an_mcp_server_is_the_sessions_and_only_the_browser_is_stopped() {
        let activity = classify(&sample(), &places(), 1_000_000);
        let chrome = run(&activity, 721);
        assert_eq!(chrome.owner, HostRunOwner::Agent { agent: "Claude Code".into(), pid: 702 });
        assert_eq!(chrome.title, "headless Chrome");
        assert_eq!(chrome.process_count, 2);
        assert!(activity.runs["p1"].iter().all(|r| r.pid != 720), "the MCP server is not a run");
    }

    #[test]
    fn what_a_finished_session_left_is_orphaned() {
        let activity = classify(&sample(), &places(), 1_000_000);
        let chrome = run(&activity, 800);
        assert_eq!(chrome.owner, HostRunOwner::Orphaned);
        assert_eq!(chrome.process_count, 3);
        assert_eq!(chrome.memory_bytes, 12_000_000);
        let worker = run(&activity, 960);
        assert_eq!(worker.owner, HostRunOwner::Orphaned);
        assert_eq!(worker.kinds, ["Queue worker"]);
        assert_eq!(worker.worktree.as_deref(), Some(WT));
    }

    #[test]
    fn mast_terminals_editors_and_passing_tool_calls_are_not_listed() {
        let activity = classify(&sample(), &places(), 1_000_000);
        let pids: Vec<u32> = activity.runs["p1"].iter().map(|r| r.pid).collect();
        assert_eq!(pids, [800, 721, 710, 960], "{:#?}", activity.runs["p1"]);
    }

    #[test]
    fn a_wrapper_and_the_binary_it_launches_are_one_session() {
        let mut scan = sample();
        scan.procs.push(proc(1000, 701, 8_000, Some(WT), "node /usr/lib/node_modules/@openai/codex/bin/codex.js"));
        scan.procs.push(proc(1001, 1000, 8_001, Some(WT), "/usr/lib/node_modules/@openai/codex/vendor/codex-x86_64-unknown-linux-musl"));
        let activity = classify(&scan, &places(), 1_000_000);
        let codex: Vec<&AgentSession> =
            activity.sessions["p1"].iter().filter(|s| s.agent == "Codex").collect();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].pid, 1000);
        assert_eq!(codex[0].worktree.as_deref(), Some(WT));
    }

    #[test]
    fn work_outside_every_project_is_not_listed() {
        let mut scan = sample();
        scan.procs.push(proc(1100, 1, 9_000, Some("/home/dev/other"), "php artisan horizon"));
        scan.procs.push(proc(1101, 701, 9_000, Some("/home/dev/other"), "claude"));
        let activity = classify(&scan, &places(), 1_000_000);
        assert!(activity.runs["p1"].iter().all(|r| r.pid != 1100));
        assert!(activity.sessions["p1"].iter().all(|s| s.pid != 1101));
    }

    #[test]
    fn agents_are_read_from_the_program_not_the_arguments() {
        assert_eq!(agent_of("claude"), Some("Claude Code"));
        assert_eq!(agent_of("/home/dev/.local/bin/claude --resume"), Some("Claude Code"));
        assert_eq!(
            agent_of("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
            Some("Claude Code")
        );
        assert_eq!(agent_of("node /usr/local/bin/gemini"), Some("Gemini CLI"));
        assert_eq!(agent_of("/usr/bin/python3 /home/dev/.local/bin/aider --model x"), Some("Aider"));
        assert_eq!(agent_of("codex-aarch64-apple-darwin exec"), Some("Codex"));
        assert_eq!(agent_of("grep claude"), None);
        assert_eq!(agent_of("vim notes-about-codex.md"), None);
        assert_eq!(agent_of("node /app/node_modules/.bin/vite"), None);
    }

    #[test]
    fn host_kinds() {
        assert_eq!(host_kinds_of("docker compose exec -T laravel.test php artisan horizon"), ["Container exec"]);
        assert_eq!(
            host_kinds_of("/usr/libexec/docker/cli-plugins/docker-compose compose exec laravel.test bash"),
            ["Container exec"]
        );
        assert_eq!(host_kinds_of("docker compose -f compose.yaml up"), ["Compose up"]);
        assert_eq!(host_kinds_of("docker exec -it abc sh"), ["Container exec"]);
        assert!(host_kinds_of("docker compose ps").is_empty());
        assert_eq!(host_kinds_of("pnpm dev"), ["Dev script"]);
        assert_eq!(host_kinds_of("node /usr/bin/npm run watch"), ["Dev script"]);
        assert_eq!(host_kinds_of("php /usr/local/bin/composer run dev"), ["Dev script"]);
        assert!(host_kinds_of("npm install").is_empty());
        assert!(host_kinds_of("npm run build").is_empty());
        assert_eq!(host_kinds_of("php -S 127.0.0.1:8000 -t public"), ["PHP server"]);
        assert_eq!(host_kinds_of("php artisan serve"), ["App server"]);
        assert_eq!(
            host_kinds_of("/opt/google/chrome/chrome --headless=new --user-data-dir=/tmp/x"),
            ["Headless browser"]
        );
        assert!(host_kinds_of("/opt/google/chrome/chrome --type=renderer --headless=new").is_empty());
        assert!(host_kinds_of("/opt/google/chrome/chrome https://example.test").is_empty());
        assert!(host_kinds_of("/usr/share/code/code .").is_empty());
    }

    #[test]
    fn a_package_cli_asked_for_a_dev_server_is_the_job_not_someone_elses() {
        // vite-plus's `vp dev`, orphaned with the Vite it runs beneath it.
        let mut scan = sample();
        scan.procs.push(proc(1200, 1602, 9_500, Some(APP), "node /home/dev/code/shop/node_modules/.pnpm/vite-plus@1/node_modules/vite-plus/dist/bin.js dev --port 5299"));
        scan.procs.push(proc(1201, 1200, 9_501, Some(APP), "node /home/dev/code/shop/node_modules/.pnpm/x/node_modules/@voidzero-dev/vite-plus-core/dist/vite/node/cli.js dev --port 5299 --strictPort"));
        let activity = classify(&scan, &places(), 1_000_000);
        let vite = run(&activity, 1200);
        assert_eq!(vite.owner, HostRunOwner::Orphaned);
        assert_eq!(vite.title, "Vite :5299");
        assert_eq!(vite.process_count, 2);
        assert!(!is_wrapper("node /home/dev/.npm/_npx/x/node_modules/.bin/chrome-devtools-mcp"));
        assert!(!is_wrapper("node /home/dev/code/shop/node_modules/.bin/eslint --fix"));
    }

    #[test]
    fn a_container_client_is_titled_by_what_it_runs_inside() {
        assert_eq!(
            host_title("docker exec -e DB_DATABASE=testing -w /var/www/html shop-laravel.test-1 php artisan test --parallel"),
            "artisan test"
        );
        assert_eq!(
            host_title("docker compose exec -T -u sail laravel.test php artisan horizon"),
            "artisan horizon"
        );
        assert_eq!(host_title("docker compose up"), "docker compose up");
        assert_eq!(host_title("node node_modules/vite/bin/vite.js --port=5180"), "Vite :5180");
    }

    #[test]
    fn stat_lines_with_awkward_names_parse() {
        let stat = "4242 (my (odd) name) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 3 0 \
                    123456 1000000 250 18446744073709551615";
        assert_eq!(parse_stat(stat), Some(('S', 1, 123456, 250)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn head_files_name_a_branch_or_a_commit() {
        assert_eq!(parse_head("ref: refs/heads/worktree-fix\n"), (Some("worktree-fix".into()), None));
        assert_eq!(
            parse_head("8ba4734c0ffee8ba4734c0ffee8ba4734c0ffee00\n"),
            (None, Some("8ba4734".into()))
        );
        assert_eq!(parse_head(""), (None, None));
    }

    #[test]
    fn porcelain_v2_status_counts_changes_and_names_the_commit() {
        let text = "# branch.oid 8ba4734c0ffee8ba4734c0ffee8ba4734c0ffee00\n\
                    # branch.head fix-login\n\
                    1 .M N... 100644 100644 100644 a b app/Http/Kernel.php\n\
                    ? notes.md\n";
        assert_eq!(
            parse_status_v2(text),
            (Some("8ba4734c0ffee8ba4734c0ffee8ba4734c0ffee00".into()), 2)
        );
        assert_eq!(parse_status_v2("# branch.oid (initial)\n"), (None, 0));
    }

    #[test]
    fn locations_name_the_tool_whose_convention_they_follow() {
        assert_eq!(tool_of(Path::new("/home/dev/shop/.claude/worktrees/fix")), Some("Claude Code"));
        assert_eq!(tool_of(Path::new("/home/dev/.codex/worktrees/a1b2/shop")), Some("Codex"));
        assert_eq!(tool_of(Path::new("/home/dev/.cursor/worktrees/shop/x")), Some("Cursor"));
        assert_eq!(
            tool_of(&std::env::temp_dir().join("claude-1000/shop/s/scratchpad/wt")),
            Some("Claude Code")
        );
        assert_eq!(tool_of(Path::new("/home/dev/shop-feature")), None);
    }

    #[test]
    fn worktrees_are_read_from_gits_admin_directory() {
        let repo = tempfile::tempdir().unwrap();
        let admin = repo.path().join(".git/worktrees");
        let nested = repo.path().join(".claude/worktrees/fix-login");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(admin.join("fix-login")).unwrap();
        std::fs::write(admin.join("fix-login/gitdir"), format!("{}\n", nested.join(".git").display())).unwrap();
        std::fs::write(admin.join("fix-login/HEAD"), "ref: refs/heads/worktree-fix-login\n").unwrap();
        std::fs::write(admin.join("fix-login/commondir"), "../..\n").unwrap();
        // One whose checkout is gone, recorded relative to its admin entry.
        std::fs::create_dir_all(admin.join("wt")).unwrap();
        std::fs::write(admin.join("wt/gitdir"), "../../../../gone/wt/.git\n").unwrap();
        std::fs::write(admin.join("wt/HEAD"), "8ba4734c0ffee8ba4734c0ffee8ba4734c0ffee00\n").unwrap();
        std::fs::write(admin.join("wt/locked"), "").unwrap();
        let entries = read_worktree_entries(repo.path());
        assert_eq!(entries.len(), 2, "{entries:#?}");
        let fix = entries.iter().find(|e| e.name == "fix-login").unwrap();
        assert_eq!(fix.path, nested);
        assert_eq!(fix.branch.as_deref(), Some("worktree-fix-login"));
        assert!(fix.created_at_unix.is_some());
        let gone = entries.iter().find(|e| e.name == "wt").unwrap();
        assert_eq!(gone.path, repo.path().parent().unwrap().join("gone/wt"));
        assert_eq!(gone.detached_at.as_deref(), Some("8ba4734"));
        assert!(gone.locked);
        // A repository without worktrees, or no repository at all.
        assert!(read_worktree_entries(&nested).is_empty());
    }

    fn observation(working_dir: &str, project: &str) -> ContainerObservation {
        ContainerObservation {
            id: format!("{project}-id"),
            name: format!("{project}-laravel.test-1"),
            project: project.into(),
            service: "laravel.test".into(),
            config_files: vec![format!("{working_dir}/compose.yaml")],
            working_dir: Some(working_dir.into()),
            state: "running".into(),
            health: None,
            exit_code: None,
            config_hash: None,
            networks: Vec::new(),
            published_ports: vec![80],
        }
    }

    #[test]
    fn containers_belong_to_the_worktree_they_were_started_from() {
        let observations = [observation(APP, "shop"), observation(WT, "fix-login")];
        let containers = worktree_containers(&observations, Path::new(WT));
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].compose_project, "fix-login");
        assert!(containers[0].running);
        assert!(!started_from(&observations[0], Path::new(WT)));
    }

    #[test]
    fn a_worktree_sharing_the_compose_name_is_an_advisory() {
        let worktree = |project: &str| Worktree {
            path: WT.into(),
            name: "fix-login".into(),
            branch: None,
            head: None,
            tool: None,
            created_at_unix: None,
            missing: false,
            locked: false,
            changed_files: None,
            unmerged_commits: None,
            containers: worktree_containers(&[observation(WT, project)], Path::new(WT)),
        };
        assert_eq!(shared_name_advisories(Some("shop"), &[worktree("shop")]).len(), 1);
        assert!(shared_name_advisories(Some("shop"), &[worktree("fix-login")]).is_empty());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod process_tests {
    use super::*;
    use std::time::Duration;

    fn start_of(pid: u32) -> u64 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        parse_stat(&stat).unwrap().2
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| parse_stat(&s))
            .is_some_and(|(state, ..)| !matches!(state, 'Z' | 'X'))
    }

    #[test]
    fn the_scan_sees_this_process_with_its_working_directory() {
        let scan = scan_host().expect("a /proc to read");
        let own = scan.procs.iter().find(|p| p.pid == scan.own_pid).expect("itself");
        assert_eq!(own.cwd.as_deref(), std::env::current_dir().ok().as_deref());
        assert!(scan.clk_tck > 0 && scan.uptime_secs > 0.0);
    }

    // A shell with a child that ignores TERM: the tree must still go, the
    // stubborn child by KILL once the grace runs out, and a start time that
    // no longer matches must stop nothing.
    #[test]
    fn a_tree_is_stopped_whole_and_a_stale_start_time_stops_nothing() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sh -c 'trap \"\" TERM; while :; do sleep 0.1; done' & wait"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let stubborn = loop {
            let scan = scan_host().unwrap();
            if let Some(p) = scan.procs.iter().find(|p| p.ppid == pid) {
                break p.pid;
            }
            assert!(std::time::Instant::now() < deadline, "the child did not start");
            std::thread::sleep(Duration::from_millis(50));
        };
        let start = start_of(pid);
        assert_eq!(stop_host_tree(pid, start + 1, Duration::from_millis(500)), Ok(0));
        assert!(alive(pid) && alive(stubborn));

        let signalled = stop_host_tree(pid, start, Duration::from_millis(500)).unwrap();
        assert!(signalled >= 2, "{signalled}");
        let _ = child.wait();
        assert!(!alive(stubborn), "the TERM-ignoring child was killed");
    }

    #[test]
    fn mast_never_stops_itself() {
        let own = std::process::id();
        assert!(stop_host_tree(own, start_of(own), Duration::from_millis(100)).is_err());
    }
}
