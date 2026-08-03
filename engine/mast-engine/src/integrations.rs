//! External-tool integrations (plan M4): open terminal at project, shell into
//! a container, open in editor, reveal in file manager. Everything spawns
//! detached (argv-only, own process group) so launched apps outlive Mast.
//! Living in the engine means the CLI (M8) gets these actions for free.

use std::path::{Path, PathBuf};

use mast_docker::spawn_detached;

/// Probe order when no terminal is configured — modern emulators first,
/// classic fallbacks last (plan names Ghostty/WezTerm/Kitty/gnome-terminal).
const TERMINAL_CANDIDATES: [&str; 8] =
    ["ghostty", "wezterm", "kitty", "alacritty", "foot", "konsole", "gnome-terminal", "xterm"];

const EDITOR_CANDIDATES: [&str; 4] = ["code", "codium", "zed", "subl"];

pub fn find_in_path(binary: &str) -> Option<PathBuf> {
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        return path.is_file().then_some(path);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|dir| dir.join(binary)).find(|p| p.is_file())
    })
}

fn pick(configured: Option<&str>, candidates: &[&str]) -> Option<String> {
    if let Some(configured) = configured
        && !configured.trim().is_empty()
    {
        return Some(configured.trim().to_string());
    }
    candidates.iter().find(|c| find_in_path(c).is_some()).map(|c| c.to_string())
}

/// Build the argv that opens `terminal` at `cwd`, optionally running
/// `command` inside it. Flag conventions differ per emulator; unknown
/// terminals get the widely-understood `-e` and rely on the spawn cwd.
pub fn terminal_argv(terminal: &str, cwd: &Path, command: Option<&[String]>) -> Vec<String> {
    let base = Path::new(terminal)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| terminal.to_string());
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
    let argv = terminal_argv(&terminal, cwd, command);
    spawn_detached(&argv, Some(cwd)).map_err(|e| e.to_string())
}

pub fn open_editor(configured: Option<&str>, path: &Path) -> Result<(), String> {
    let argv = match pick(configured, &EDITOR_CANDIDATES) {
        Some(editor) => vec![editor, path.to_string_lossy().into_owned()],
        // Last resort: let the desktop decide.
        None => vec!["xdg-open".into(), path.to_string_lossy().into_owned()],
    };
    spawn_detached(&argv, None).map_err(|e| e.to_string())
}

pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    spawn_detached(&["xdg-open".into(), path.to_string_lossy().into_owned()], None)
        .map_err(|e| e.to_string())
}

/// Hand a URL to the desktop's default browser. The scheme is re-checked
/// here (it was already checked when the URL was derived from `.env`) —
/// `xdg-open` dispatches on scheme, so that check is the whole safety story.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    let scheme_ok = ["http://", "https://"]
        .iter()
        .any(|s| url.len() > s.len() && url[..s.len()].eq_ignore_ascii_case(s));
    if !scheme_ok {
        return Err(format!("not an http(s) address: {url}"));
    }
    spawn_detached(&["xdg-open".into(), url.to_string()], None).map_err(|e| e.to_string())
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
