//! Safe subprocess execution (plan §4): argv arrays only (no shell strings),
//! explicit environment overlay, timeouts, bounded captured output, and — for
//! lifecycle operations — process-group spawn with streamed output and
//! cancellation (SIGTERM to the group, SIGKILL after a grace period).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::observer::{
    CommandFinish, CommandStart, TAIL_LINES, clip, finish_all, start_all, tail_of,
};

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("failed to spawn {argv0}: {source}")]
    Spawn {
        argv0: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{argv0} timed out after {seconds}s")]
    Timeout { argv0: String, seconds: u64 },
    #[error("i/o error running {argv0}: {source}")]
    Io {
        argv0: String,
        #[source]
        source: std::io::Error,
    },
}

/// The argv0 actually handed to the OS. `Command::new("docker")` resolves
/// through `PATH` — which a packaged GUI app does not inherit from the user's
/// shell. A Finder/Dock launch carries launchd's bare
/// `/usr/bin:/bin:/usr/sbin:/sbin` (no `/usr/local/bin`, where Docker Desktop
/// links its CLI), and a Windows session started before Docker Desktop's
/// install misses its `PATH` edit. So a bare `docker` that `PATH` cannot see
/// is re-pointed at the first well-known install location that exists.
/// Probed on every spawn (a few stats, dwarfed by the spawn itself): install
/// Docker while Mast runs and the connect retry loop finds it, no restart.
fn effective_argv0(argv0: &str) -> String {
    if argv0 != "docker" || found_in_path(argv0) {
        return argv0.to_string();
    }
    docker_fallbacks()
        .into_iter()
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.to_string())
}

fn found_in_path(binary: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        if dir.as_os_str().is_empty() {
            return false;
        }
        let candidate = dir.join(binary);
        if cfg!(windows) { candidate.with_extension("exe").is_file() } else { candidate.is_file() }
    })
}

/// Everywhere a docker CLI lands that launchd's `PATH` cannot see: Docker
/// Desktop's `/usr/local/bin` symlink (and the bundle's own copy, which
/// survives a broken symlink), Homebrew/MacPorts for colima and friends, and
/// the per-user bins OrbStack and Rancher Desktop create.
#[cfg(target_os = "macos")]
fn docker_fallbacks() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/opt/local/bin",
        "/Applications/Docker.app/Contents/Resources/bin",
    ]
    .iter()
    .map(|dir| Path::new(dir).join("docker"))
    .collect();
    if let Some(home) = std::env::var_os("HOME") {
        let home = Path::new(&home);
        candidates.push(home.join(".orbstack/bin/docker"));
        candidates.push(home.join(".rd/bin/docker"));
    }
    candidates
}

/// Docker Desktop's machine-wide CLI directory — reachable even when this
/// login session predates the install's `PATH` edit.
#[cfg(windows)]
fn docker_fallbacks() -> Vec<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    vec![program_files.join(r"Docker\Docker\resources\bin\docker.exe")]
}

/// A Linux desktop session inherits a sane `PATH`; these cover the package
/// locations anyway so the AppImage behaves no worse than the terminal.
#[cfg(all(unix, not(target_os = "macos")))]
fn docker_fallbacks() -> Vec<PathBuf> {
    ["/usr/bin", "/usr/local/bin", "/snap/bin"]
        .iter()
        .map(|dir| Path::new(dir).join("docker"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Run `argv` with the inherited environment plus `env_overlay`, capturing
/// stdout/stderr up to `max_bytes` each. Never interprets a shell string.
pub async fn run_command(
    argv: &[String],
    cwd: Option<&Path>,
    env_overlay: &[(String, String)],
    timeout: Duration,
    max_bytes: usize,
) -> Result<CommandOutput, CommandError> {
    let watchers =
        start_all(&CommandStart { argv, cwd, env_overlay, streaming: false, detached: false });
    let result = run_command_inner(argv, cwd, env_overlay, timeout, max_bytes).await;
    match &result {
        Ok(out) => {
            finish_all(watchers, CommandFinish::Exited(out.status), &tail_of(&combined(out)))
        }
        Err(e) => finish_all(watchers, CommandFinish::Failed(e.to_string()), &[]),
    }
    result
}

/// stdout then stderr, blank streams skipped — what the user would have seen
/// in a terminal, near enough for a history tail.
fn combined(out: &CommandOutput) -> String {
    let mut text = String::new();
    for part in [out.stdout.as_str(), out.stderr.as_str()] {
        if part.trim().is_empty() {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(part);
    }
    text
}

async fn run_command_inner(
    argv: &[String],
    cwd: Option<&Path>,
    env_overlay: &[(String, String)],
    timeout: Duration,
    max_bytes: usize,
) -> Result<CommandOutput, CommandError> {
    let argv0 = effective_argv0(argv.first().map(String::as_str).unwrap_or_default());
    let mut cmd = tokio::process::Command::new(&argv0);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // A GUI-subsystem parent on Windows gets a fresh console window for
    // every console-subsystem child — one flash per docker/git call.
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in env_overlay {
        cmd.env(k, v);
    }

    let child = cmd.spawn().map_err(|source| CommandError::Spawn { argv0: argv0.clone(), source })?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| CommandError::Timeout { argv0: argv0.clone(), seconds: timeout.as_secs() })?
        .map_err(|source| CommandError::Io { argv0: argv0.clone(), source })?;

    let truncated = output.stdout.len() > max_bytes || output.stderr.len() > max_bytes;
    let clip = |bytes: &[u8]| String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).into_owned();
    Ok(CommandOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: clip(&output.stdout),
        stderr: clip(&output.stderr),
        truncated,
    })
}

/// Launch an external application (terminal, editor, file manager) detached:
/// own process group, no captured output, outlives Mast. Argv-only.
/// `console` keeps the Windows console window — wanted exactly once, for
/// launching a terminal; every other launch (explorer, `code.cmd` through
/// cmd.exe) would otherwise flash one.
pub fn spawn_detached(argv: &[String], cwd: Option<&Path>, console: bool) -> Result<(), CommandError> {
    let watchers = start_all(&CommandStart {
        argv,
        cwd,
        env_overlay: &[],
        streaming: false,
        detached: true,
    });
    let result = spawn_detached_inner(argv, cwd, console);
    match &result {
        Ok(()) => finish_all(watchers, CommandFinish::Detached, &[]),
        Err(e) => finish_all(watchers, CommandFinish::Failed(e.to_string()), &[]),
    }
    result
}

fn spawn_detached_inner(argv: &[String], cwd: Option<&Path>, console: bool) -> Result<(), CommandError> {
    let argv0 = argv.first().cloned().unwrap_or_default();
    let mut cmd = std::process::Command::new(&argv0);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let _ = console;
    }
    #[cfg(windows)]
    if !console {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.spawn().map(|_| ()).map_err(|source| CommandError::Spawn { argv0, source })
}

// ---------- streaming lifecycle commands ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Exited(i32),
    Cancelled,
}

/// One line of live subprocess output; `stderr` distinguishes the stream.
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub line: String,
    pub stderr: bool,
}

/// No console window for captured-output children (docker, git, compose):
/// a GUI-subsystem parent otherwise flashes one per spawn.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(unix)]
const SIG_TERM: i32 = libc::SIGTERM;
#[cfg(unix)]
const SIG_KILL: i32 = libc::SIGKILL;
// Non-unix: cancellation relies on kill_on_drop (Windows adapter TODO —
// Job Objects for group kill); the numbers are only passed to a no-op.
#[cfg(not(unix))]
const SIG_TERM: i32 = 15;
#[cfg(not(unix))]
const SIG_KILL: i32 = 9;

#[cfg(unix)]
fn kill_group(pid: i32, signal: i32) {
    // Negative pid targets the whole process group (compose spawns children).
    unsafe {
        libc::kill(-pid, signal);
    }
}

/// Run `argv` in its own process group, streaming stdout/stderr lines into
/// `lines`. Cancellation SIGTERMs the group, escalating to SIGKILL after
/// `grace`. Lines longer than 8 KiB are truncated.
pub async fn run_streaming(
    argv: &[String],
    cwd: Option<&Path>,
    env_overlay: &[(String, String)],
    lines: mpsc::Sender<OutputLine>,
    cancel: CancellationToken,
    timeout: Duration,
    grace: Duration,
) -> Result<CommandOutcome, CommandError> {
    let watchers =
        start_all(&CommandStart { argv, cwd, env_overlay, streaming: true, detached: false });
    // Only collected when someone is listening; a long-running dev server
    // must not accumulate its whole output here.
    let tail: Option<Tail> =
        (!watchers.is_empty()).then(|| Arc::new(Mutex::new(VecDeque::new())));
    let result =
        run_streaming_inner(argv, cwd, env_overlay, lines, cancel, timeout, grace, tail.clone())
            .await;
    if !watchers.is_empty() {
        let tail: Vec<String> =
            tail.map(|t| t.lock().unwrap().iter().cloned().collect()).unwrap_or_default();
        let finish = match &result {
            Ok(CommandOutcome::Exited(code)) => CommandFinish::Exited(*code),
            Ok(CommandOutcome::Cancelled) => CommandFinish::Cancelled,
            Err(e) => CommandFinish::Failed(e.to_string()),
        };
        finish_all(watchers, finish, &tail);
    }
    result
}

/// Bounded ring of recent output lines, shared by both reader tasks.
type Tail = Arc<Mutex<VecDeque<String>>>;

#[allow(clippy::too_many_arguments)]
async fn run_streaming_inner(
    argv: &[String],
    cwd: Option<&Path>,
    env_overlay: &[(String, String)],
    lines: mpsc::Sender<OutputLine>,
    cancel: CancellationToken,
    timeout: Duration,
    grace: Duration,
    tail: Option<Tail>,
) -> Result<CommandOutcome, CommandError> {
    const MAX_LINE: usize = 8 * 1024;
    let argv0 = effective_argv0(argv.first().map(String::as_str).unwrap_or_default());
    let mut cmd = tokio::process::Command::new(&argv0);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in env_overlay {
        cmd.env(k, v);
    }

    let mut child =
        cmd.spawn().map_err(|source| CommandError::Spawn { argv0: argv0.clone(), source })?;
    // Only the signalling path below wants it; elsewhere kill_on_drop is all
    // there is (see SIG_TERM).
    #[cfg(unix)]
    let pid = child.id().unwrap_or(0) as i32;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    for (reader, is_stderr) in [(Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Send + Unpin>, false)]
        .into_iter()
        .chain([(Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Send + Unpin>, true)])
    {
        let lines = lines.clone();
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader).lines();
            while let Ok(Some(mut line)) = reader.next_line().await {
                line.truncate(MAX_LINE);
                if let Some(tail) = &tail {
                    let mut tail = tail.lock().unwrap();
                    tail.push_back(clip(&line));
                    while tail.len() > TAIL_LINES {
                        tail.pop_front();
                    }
                }
                if lines.send(OutputLine { line, stderr: is_stderr }).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(lines);

    let mut wait = tokio::spawn(async move { child.wait().await });
    let escalate = |first: i32| async move {
        #[cfg(unix)]
        kill_group(pid, first);
        #[cfg(not(unix))]
        let _ = first;
    };

    tokio::select! {
        status = &mut wait => {
            let status = status
                .map_err(|e| CommandError::Io { argv0: argv0.clone(), source: std::io::Error::other(e) })?
                .map_err(|source| CommandError::Io { argv0: argv0.clone(), source })?;
            Ok(CommandOutcome::Exited(status.code().unwrap_or(-1)))
        }
        _ = cancel.cancelled() => {
            escalate(SIG_TERM).await;
            if tokio::time::timeout(grace, &mut wait).await.is_err() {
                escalate(SIG_KILL).await;
                let _ = wait.await;
            }
            Ok(CommandOutcome::Cancelled)
        }
        _ = tokio::time::sleep(timeout) => {
            escalate(SIG_KILL).await;
            let _ = wait.await;
            Err(CommandError::Timeout { argv0, seconds: timeout.as_secs() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn streaming_captures_both_streams_in_order() {
        let (tx, mut rx) = mpsc::channel(64);
        let argv: Vec<String> =
            ["bash", "-c", "echo one; echo two >&2; echo three"].map(String::from).to_vec();
        let outcome = run_streaming(
            &argv,
            None,
            &[],
            tx,
            CancellationToken::new(),
            Duration::from_secs(10),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(outcome, CommandOutcome::Exited(0));
        let mut stdout_lines = Vec::new();
        let mut stderr_lines = Vec::new();
        while let Some(line) = rx.recv().await {
            if line.stderr {
                stderr_lines.push(line.line);
            } else {
                stdout_lines.push(line.line);
            }
        }
        assert_eq!(stdout_lines, vec!["one", "three"]);
        assert_eq!(stderr_lines, vec!["two"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_kills_the_whole_process_group() {
        let (tx, mut rx) = mpsc::channel(64);
        // Child spawns a grandchild; killing only the direct child would leak
        // the sleeper. `pgleak` marks the grandchild so we can probe for it.
        let argv: Vec<String> =
            ["bash", "-c", "echo ready; sleep 300 & MAST_PGLEAK=1 sleep 300; wait"]
                .map(String::from)
                .to_vec();
        let cancel = CancellationToken::new();
        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                run_streaming(
                    &argv,
                    None,
                    &[],
                    tx,
                    cancel,
                    Duration::from_secs(60),
                    Duration::from_secs(2),
                )
                .await
            })
        };
        // Wait until it's actually running.
        let first = rx.recv().await.unwrap();
        assert_eq!(first.line, "ready");
        let started = std::time::Instant::now();
        cancel.cancel();
        let outcome = handle.await.unwrap().unwrap();
        assert_eq!(outcome, CommandOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5), "cancel was not prompt");
    }

    #[test]
    fn only_a_bare_docker_argv0_is_rewritten() {
        // Other programs and explicit paths are the caller's decision.
        assert_eq!(effective_argv0("bash"), "bash");
        assert_eq!(effective_argv0("/no/such/docker"), "/no/such/docker");
    }

    /// Environment-dependent by nature: on a box with docker on PATH the name
    /// stays bare; anywhere else the fallback must only ever substitute a
    /// path that exists (or give the name back for the honest spawn error).
    #[test]
    fn docker_resolves_to_bare_name_or_existing_path() {
        let resolved = effective_argv0("docker");
        assert!(resolved == "docker" || Path::new(&resolved).is_file(), "{resolved}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonzero_exit_is_reported() {
        let (tx, _rx) = mpsc::channel(8);
        let argv: Vec<String> = ["bash", "-c", "exit 7"].map(String::from).to_vec();
        let outcome = run_streaming(
            &argv,
            None,
            &[],
            tx,
            CancellationToken::new(),
            Duration::from_secs(10),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(outcome, CommandOutcome::Exited(7));
    }
}
