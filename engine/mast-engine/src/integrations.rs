//! External-tool integrations (plan M4): open terminal at project, shell into
//! a container, open in editor, reveal in file manager. Everything spawns
//! detached (argv-only, own process group) so launched apps outlive Mast.
//! Living in the engine means the CLI (M8) gets these actions for free.
//!
//! Two desktops are supported. On Linux the freedesktop tools do the work
//! (`xdg-open` dispatches URLs and paths on scheme/mimetype); on macOS the
//! equivalent is `open`, which is also how a `.app` bundle is launched. Every
//! difference between them lives in this module — callers stay platform-blind.

use std::path::{Path, PathBuf};

use mast_docker::spawn_detached;

/// Probe order when no terminal is configured — modern emulators first,
/// classic fallbacks last (plan names Ghostty/WezTerm/Kitty/gnome-terminal).
#[cfg(not(target_os = "macos"))]
const TERMINAL_CANDIDATES: [&str; 8] =
    ["ghostty", "wezterm", "kitty", "alacritty", "foot", "konsole", "gnome-terminal", "xterm"];

/// macOS ships no terminal on `PATH`, so the probe covers the emulators that
/// install a CLI shim (Homebrew, or the app's own "install CLI" action) and
/// ends at [`MAC_TERMINAL`], which is always present.
#[cfg(target_os = "macos")]
const TERMINAL_CANDIDATES: [&str; 5] =
    ["ghostty", "wezterm", "kitty", "alacritty", MAC_TERMINAL];

/// The stock macOS terminal, addressed as an app bundle rather than a binary.
#[cfg(target_os = "macos")]
pub const MAC_TERMINAL: &str = "Terminal.app";

#[cfg(not(target_os = "macos"))]
const EDITOR_CANDIDATES: [&str; 4] = ["code", "codium", "zed", "subl"];

/// `mate` is TextMate's shim; the rest match the Linux list.
#[cfg(target_os = "macos")]
const EDITOR_CANDIDATES: [&str; 5] = ["code", "codium", "zed", "subl", "mate"];

/// The desktop's generic opener: a URL, a file or a directory, dispatched to
/// whatever handles it.
#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

/// Directories searched on top of `PATH`.
///
/// A macOS app launched from Finder, the Dock or Spotlight inherits the
/// bare `/usr/bin:/bin:/usr/sbin:/sbin` — not the shell's `PATH` — so
/// Homebrew and the CLI shims editors install are invisible without this.
#[cfg(target_os = "macos")]
const EXTRA_BIN_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];
#[cfg(not(target_os = "macos"))]
const EXTRA_BIN_DIRS: [&str; 0] = [];

pub fn find_in_path(binary: &str) -> Option<PathBuf> {
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        return path.is_file().then_some(path);
    }
    let from_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|dir| dir.join(binary)).find(|p| p.is_file())
    });
    from_path.or_else(|| {
        EXTRA_BIN_DIRS.iter().map(|dir| Path::new(dir).join(binary)).find(|p| p.is_file())
    })
}

/// App bundles are launched, not executed: they never live on `PATH`, and
/// `open -a` resolves them by name through Launch Services.
fn is_app_bundle(name: &str) -> bool {
    name.ends_with(".app")
}

fn pick(configured: Option<&str>, candidates: &[&str]) -> Option<String> {
    if let Some(configured) = configured
        && !configured.trim().is_empty()
    {
        return Some(configured.trim().to_string());
    }
    // Return the path the probe proved, not the bare name: the spawn resolves
    // argv0 through PATH alone, which — pared down to launchd's four dirs in
    // a Finder-launched app — cannot see EXTRA_BIN_DIRS the probe just did.
    candidates.iter().find_map(|c| {
        if is_app_bundle(c) {
            return Some(c.to_string());
        }
        find_in_path(c).map(|p| p.to_string_lossy().into_owned())
    })
}

/// Wrap `s` so a POSIX shell reads it as one literal word. Words made only of
/// characters no shell touches are left bare — the line is shown to the user
/// in their terminal, so `docker exec -it abc123 …` beats a wall of quotes.
fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Wrap `s` as an AppleScript string literal.
fn applescript_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// AppleScript can drive the stock Terminal, but every other bundle speaks its
/// own dialect (iTerm2 wants `create window with default profile`), so those
/// only get `open -a`, which cannot carry a command. Callers that need one
/// check this first.
fn app_takes_a_command(app: &str) -> bool {
    app == "Terminal.app"
}

/// macOS `.app` bundles cannot be handed a working directory and a command
/// the way an emulator binary can, so Terminal is driven through AppleScript:
/// `do script` opens a window and runs one line of shell in it.
///
/// This is the one place Mast composes a shell string instead of an argv, and
/// it stays safe by quoting every interpolated word — the argv the caller
/// passed survives as literal words, exactly as it would after `execvp`.
fn terminal_app_argv(app: &str, cwd: &Path, command: Option<&[String]>) -> Vec<String> {
    if !app_takes_a_command(app) {
        // Directory only; `open -a` has nowhere to put a command.
        return vec![
            OPENER.into(),
            "-a".into(),
            app.to_string(),
            cwd.to_string_lossy().into_owned(),
        ];
    }
    let mut line = format!("cd {}", shell_quote(&cwd.to_string_lossy()));
    match command {
        Some(cmd) => {
            line.push_str(" && ");
            line.push_str(
                &cmd.iter().map(|word| shell_quote(word)).collect::<Vec<_>>().join(" "),
            );
        }
        None => line.push_str(" && clear"),
    }
    let app_name = app.trim_end_matches(".app");
    vec![
        "osascript".into(),
        "-e".into(),
        format!("tell application {}", applescript_quote(app_name)),
        "-e".into(),
        format!("do script {}", applescript_quote(&line)),
        "-e".into(),
        "activate".into(),
        "-e".into(),
        "end tell".into(),
    ]
}

/// Build the argv that opens `terminal` at `cwd`, optionally running
/// `command` inside it. Flag conventions differ per emulator; unknown
/// terminals get the widely-understood `-e` and rely on the spawn cwd.
///
/// A `terminal` naming an app bundle (`Terminal.app`, macOS) is launched
/// through AppleScript instead — see [`terminal_app_argv`].
pub fn terminal_argv(terminal: &str, cwd: &Path, command: Option<&[String]>) -> Vec<String> {
    let base = Path::new(terminal)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| terminal.to_string());
    if is_app_bundle(&base) {
        return terminal_app_argv(&base, cwd, command);
    }
    let dir = cwd.to_string_lossy().into_owned();
    let mut argv = vec![terminal.to_string()];
    match base.as_str() {
        "gnome-terminal" => {
            argv.push(format!("--working-directory={dir}"));
            if let Some(cmd) = command {
                argv.push("--".into());
                argv.extend(cmd.iter().cloned());
            }
        }
        "kitty" => {
            argv.push("--directory".into());
            argv.push(dir);
            if let Some(cmd) = command {
                argv.extend(cmd.iter().cloned());
            }
        }
        "wezterm" => {
            argv.insert(1, "start".into());
            argv.push("--cwd".into());
            argv.push(dir);
            if let Some(cmd) = command {
                argv.push("--".into());
                argv.extend(cmd.iter().cloned());
            }
        }
        "ghostty" => {
            argv.push(format!("--working-directory={dir}"));
            if let Some(cmd) = command {
                argv.push("-e".into());
                argv.extend(cmd.iter().cloned());
            }
        }
        "alacritty" => {
            argv.push("--working-directory".into());
            argv.push(dir);
            if let Some(cmd) = command {
                argv.push("-e".into());
                argv.extend(cmd.iter().cloned());
            }
        }
        "foot" => {
            argv.push(format!("--working-directory={dir}"));
            if let Some(cmd) = command {
                argv.extend(cmd.iter().cloned());
            }
        }
        "konsole" => {
            argv.push("--workdir".into());
            argv.push(dir);
            if let Some(cmd) = command {
                argv.push("-e".into());
                argv.extend(cmd.iter().cloned());
            }
        }
        // xterm and anything unknown: `-e` is the de-facto convention; the
        // working directory comes from the spawn cwd.
        _ => {
            if let Some(cmd) = command {
                argv.push("-e".into());
                argv.extend(cmd.iter().cloned());
            }
        }
    }
    argv
}

/// Interactive shell inside a container: prefer bash, fall back to sh. The
/// inner `sh -lc` string is a fixed constant interpreted INSIDE the
/// container — host-side this stays a pure argv array (plan §4).
pub fn container_shell_command(container_id: &str) -> Vec<String> {
    [
        "docker",
        "exec",
        "-it",
        container_id,
        "sh",
        "-lc",
        "command -v bash >/dev/null 2>&1 && exec bash -l || exec sh -l",
    ]
    .map(String::from)
    .to_vec()
}

pub fn open_terminal(
    configured: Option<&str>,
    cwd: &Path,
    command: Option<&[String]>,
) -> Result<(), String> {
    let terminal = pick(configured, &TERMINAL_CANDIDATES)
        .ok_or("no terminal emulator found — set one in Settings")?;
    if command.is_some() && is_app_bundle(&terminal) && !app_takes_a_command(&terminal) {
        return Err(format!(
            "{terminal} can only be opened at a folder — set a terminal with a command-line \
             launcher (ghostty, wezterm, kitty, alacritty) in Settings to run a command in it"
        ));
    }
    let argv = terminal_argv(&terminal, cwd, command);
    spawn_detached(&argv, Some(cwd)).map_err(|e| e.to_string())
}

pub fn open_editor(configured: Option<&str>, path: &Path) -> Result<(), String> {
    let argv = match pick(configured, &EDITOR_CANDIDATES) {
        Some(editor) if is_app_bundle(&editor) => {
            vec![OPENER.into(), "-a".into(), editor, path.to_string_lossy().into_owned()]
        }
        Some(editor) => vec![editor, path.to_string_lossy().into_owned()],
        // Last resort: let the desktop decide.
        None => vec![OPENER.into(), path.to_string_lossy().into_owned()],
    };
    spawn_detached(&argv, None).map_err(|e| e.to_string())
}

/// The target is a project directory, and both openers show a directory in
/// the file manager (Finder, Nautilus, …) rather than opening its contents.
/// Open a file with the desktop's default application (xdg-open/open) —
/// the manual fallback for files Mast must not edit itself (/etc/hosts).
pub fn open_path(path: &Path) -> Result<(), String> {
    spawn_detached(&[OPENER.into(), path.to_string_lossy().into_owned()], None)
        .map_err(|e| e.to_string())
}

pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    spawn_detached(&[OPENER.into(), path.to_string_lossy().into_owned()], None)
        .map_err(|e| e.to_string())
}

/// Hand a URL to the desktop's default browser. The scheme is re-checked
/// here (it was already checked when the URL was derived from `.env`) — the
/// opener dispatches on scheme, so that check is the whole safety story.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    let scheme_ok = ["http://", "https://"]
        .iter()
        .any(|s| url.len() > s.len() && url[..s.len()].eq_ignore_ascii_case(s));
    if !scheme_ok {
        return Err(format!("not an http(s) address: {url}"));
    }
    spawn_detached(&[OPENER.into(), url.to_string()], None).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_argv_conventions() {
        let cwd = Path::new("/proj");
        let cmd = vec!["docker".to_string(), "exec".to_string()];

        assert_eq!(
            terminal_argv("gnome-terminal", cwd, Some(&cmd)),
            vec!["gnome-terminal", "--working-directory=/proj", "--", "docker", "exec"]
        );
        assert_eq!(
            terminal_argv("kitty", cwd, None),
            vec!["kitty", "--directory", "/proj"]
        );
        assert_eq!(
            terminal_argv("wezterm", cwd, Some(&cmd)),
            vec!["wezterm", "start", "--cwd", "/proj", "--", "docker", "exec"]
        );
        assert_eq!(
            terminal_argv("/usr/bin/ghostty", cwd, Some(&cmd)),
            vec!["/usr/bin/ghostty", "--working-directory=/proj", "-e", "docker", "exec"]
        );
        // Unknown terminals get the -e convention and no workdir flag.
        assert_eq!(
            terminal_argv("st", cwd, Some(&cmd)),
            vec!["st", "-e", "docker", "exec"]
        );
        assert_eq!(terminal_argv("st", cwd, None), vec!["st"]);
    }

    /// The macOS branch is pure argv building, so it is tested everywhere —
    /// a Linux box would otherwise never exercise it.
    #[test]
    fn terminal_app_is_driven_through_applescript() {
        let argv = terminal_argv("Terminal.app", Path::new("/proj"), None);
        assert_eq!(argv[0], "osascript");
        assert!(argv.contains(&"tell application \"Terminal\"".to_string()), "{argv:?}");
        assert!(argv.contains(&"do script \"cd /proj && clear\"".to_string()), "{argv:?}");

        let cmd = container_shell_command("abc123");
        let argv = terminal_argv("Terminal.app", Path::new("/proj"), Some(&cmd));
        let script = argv.iter().find(|a| a.starts_with("do script")).expect("do script");
        // Every word survives as one literal word: the inner `sh -lc` string
        // carries &&, || and > and must not be re-split by the outer shell.
        let expected = format!(
            "cd /proj && docker exec -it abc123 sh -lc {}",
            shell_quote("command -v bash >/dev/null 2>&1 && exec bash -l || exec sh -l")
        );
        assert!(script.contains(&expected), "{script}");
    }

    #[test]
    fn quoting_survives_hostile_words() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("docker"), "docker");
        // A directory with a quote in it stays one word inside the script.
        let argv = terminal_argv("Terminal.app", Path::new(r#"/a"b"#), None);
        let script = argv.iter().find(|a| a.starts_with("do script")).unwrap();
        assert_eq!(script, r#"do script "cd '/a\"b' && clear""#);
    }

    #[test]
    fn app_bundles_without_a_scripting_dialect_only_open_the_folder() {
        assert_eq!(
            terminal_argv("iTerm.app", Path::new("/proj"), None),
            [OPENER, "-a", "iTerm.app", "/proj"].map(String::from)
        );
        // …and refuse the container shell rather than dropping the command.
        let cmd = container_shell_command("abc123");
        let err = open_terminal(Some("iTerm.app"), Path::new("/proj"), Some(&cmd)).unwrap_err();
        assert!(err.contains("command-line launcher"), "{err}");
    }

    #[test]
    fn container_shell_prefers_bash_with_sh_fallback() {
        let argv = container_shell_command("abc123");
        assert_eq!(argv[..4], ["docker", "exec", "-it", "abc123"]);
        assert!(argv.last().unwrap().contains("exec bash -l || exec sh -l"));
    }

    #[test]
    fn browser_refuses_non_http_schemes() {
        assert!(open_in_browser("file:///etc/passwd").is_err());
        assert!(open_in_browser("javascript:alert(1)").is_err());
        assert!(open_in_browser("http://").is_err());
    }

    #[test]
    fn configured_binary_wins_over_probing() {
        assert_eq!(pick(Some("myterm"), &TERMINAL_CANDIDATES), Some("myterm".into()));
        assert_eq!(pick(Some("  "), &[]), None);
    }
}
