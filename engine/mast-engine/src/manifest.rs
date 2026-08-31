//! The committed project manifest — `mast.yml` at the project root.
//!
//! Saved commands (M7.5) live in app data, which means a project's command
//! setup dies with the machine: a teammate clones the repo, imports the
//! project, and starts from nothing. The manifest is the same commands in a
//! file the repo carries, so importing the project IS the setup. App data
//! stays the personal layer — a saved command whose name a manifest also
//! uses shadows the manifest entry (a local override beats the shared
//! default), and [`mast_contract::Action::SetProjectCommands`] never writes
//! manifest entries back.
//!
//! Parsing is forgiving about what it can run and loud about what it cannot:
//! a malformed file, an unknown key, a wrong type each become a project
//! warning, never a refusal to load the project — the manifest is someone
//! else's commit, and their typo must not take your project down.

use std::path::{Path, PathBuf};

use mast_contract::ProjectCommand;
use saphyr::{LoadableYamlNode, Scalar, Yaml};

/// Both spellings are read; [`Self::MANIFEST_NAMES[0]`] is the one Mast
/// writes and names in messages.
pub(crate) const MANIFEST_NAMES: [&str; 2] = ["mast.yml", "mast.yaml"];

/// A manifest is a handful of command lines; anything bigger is not one.
const MANIFEST_CAP: u64 = 256 * 1024;

/// What a project's manifest contributes: commands to show beside the saved
/// ones, and everything worth saying about the file itself.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub commands: Vec<ProjectCommand>,
    pub warnings: Vec<String>,
}

/// The manifest file present in `dir`, if any.
pub(crate) fn manifest_path(dir: &Path) -> Option<PathBuf> {
    MANIFEST_NAMES.iter().map(|name| dir.join(name)).find(|p| p.is_file())
}

/// Read and parse the project's manifest. Absent file = empty manifest; a
/// present-but-broken one comes back with warnings and whatever commands
/// survived.
pub(crate) fn read(dir: &Path) -> Manifest {
    let Some(path) = manifest_path(dir) else { return Manifest::default() };
    let mut manifest = Manifest::default();
    if MANIFEST_NAMES.iter().all(|name| dir.join(name).is_file()) {
        manifest.warnings.push(
            "Both mast.yml and mast.yaml exist — mast.yml is the one being read.".to_string(),
        );
    }
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MANIFEST_CAP {
        manifest.warnings.push(format!(
            "{} is over 256 KiB — not read (a manifest is a handful of commands)",
            file_name(&path)
        ));
        return manifest;
    }
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(e) => {
            manifest.warnings.push(format!("{} could not be read: {e}", file_name(&path)));
            return manifest;
        }
    };
    let mut parsed = parse(&source);
    // Warnings name the file the project actually has, not the canonical one.
    for warning in &mut parsed.warnings {
        if let Some(rest) = warning.strip_prefix("mast.yml") {
            *warning = format!("{}{rest}", file_name(&path));
        }
    }
    manifest.commands = parsed.commands;
    manifest.warnings.append(&mut parsed.warnings);
    manifest
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "mast.yml".into())
}

fn as_str<'a>(y: &'a Yaml<'_>) -> Option<&'a str> {
    match y {
        Yaml::Value(Scalar::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

/// Parse manifest source. Every message starts with `mast.yml` so it reads
/// as a file problem wherever it surfaces (project warnings, tests).
pub(crate) fn parse(source: &str) -> Manifest {
    let mut manifest = Manifest::default();
    let warn = |m: &mut Manifest, text: String| m.warnings.push(format!("mast.yml: {text}"));

    if source.trim().is_empty() {
        return manifest;
    }
    let docs = match Yaml::load_from_str(source) {
        Ok(docs) => docs,
        Err(e) => {
            warn(&mut manifest, format!("not valid YAML — {e}"));
            return manifest;
        }
    };
    let Some(root) = docs.first() else { return manifest };
    let Yaml::Mapping(top) = root else {
        warn(&mut manifest, "expected a mapping with a `commands:` section".to_string());
        return manifest;
    };
    for (key, _) in top.iter() {
        match as_str(key) {
            Some("commands") | None => {}
            Some(other) => {
                warn(&mut manifest, format!("unknown section \"{other}\" (only `commands:` is read)"));
            }
        }
    }
    let Some(commands) = top.iter().find(|(k, _)| as_str(k) == Some("commands")).map(|(_, v)| v)
    else {
        warn(&mut manifest, "no `commands:` section".to_string());
        return manifest;
    };
    if matches!(commands, Yaml::Value(Scalar::Null)) {
        return manifest; // `commands:` left empty is a manifest waiting to be filled
    }
    let Yaml::Mapping(commands) = commands else {
        warn(
            &mut manifest,
            "`commands:` must be a mapping of name → { command: … }".to_string(),
        );
        return manifest;
    };

    for (name, body) in commands.iter() {
        let Some(name) = as_str(name).map(str::trim).filter(|n| !n.is_empty()) else {
            warn(&mut manifest, "a command needs a non-empty name".to_string());
            continue;
        };
        if manifest.commands.iter().any(|c| c.name == name) {
            warn(&mut manifest, format!("command \"{name}\" is defined twice — keeping the first"));
            continue;
        }
        let Yaml::Mapping(body) = body else {
            warn(&mut manifest, format!("command \"{name}\" must be a mapping with `command: …`"));
            continue;
        };
        let mut command = ProjectCommand {
            name: name.to_string(),
            command: String::new(),
            auto_start: false,
            cwd: None,
            after: None,
            ready_when: None,
            auto_restart: false,
            restart_when_changed: Vec::new(),
            from_manifest: true,
        };
        let mut ok = true;
        for (key, value) in body.iter() {
            let Some(key) = as_str(key) else { continue };
            match key {
                "command" | "cwd" | "after" | "ready_when" => {
                    let Some(text) = as_str(value).map(str::trim).filter(|t| !t.is_empty()) else {
                        warn(&mut manifest, format!("command \"{name}\": `{key}` must be text"));
                        ok &= key != "command";
                        continue;
                    };
                    match key {
                        "command" => command.command = text.to_string(),
                        "cwd" => command.cwd = Some(text.to_string()),
                        "after" => command.after = Some(text.to_string()),
                        _ => command.ready_when = Some(text.to_string()),
                    }
                }
                "auto_start" | "auto_restart" => {
                    let Yaml::Value(Scalar::Boolean(flag)) = value else {
                        warn(
                            &mut manifest,
                            format!("command \"{name}\": `{key}` must be true or false"),
                        );
                        continue;
                    };
                    if key == "auto_start" {
                        command.auto_start = *flag;
                    } else {
                        command.auto_restart = *flag;
                    }
                }
                "restart_when_changed" => match value {
                    Yaml::Sequence(items) => {
                        for item in items {
                            match as_str(item).map(str::trim).filter(|t| !t.is_empty()) {
                                Some(pattern) => {
                                    command.restart_when_changed.push(pattern.to_string());
                                }
                                None => warn(
                                    &mut manifest,
                                    format!(
                                        "command \"{name}\": `restart_when_changed` entries \
                                         must be glob patterns"
                                    ),
                                ),
                            }
                        }
                    }
                    // A single pattern reads naturally without list syntax.
                    _ if as_str(value).is_some_and(|t| !t.trim().is_empty()) => {
                        command
                            .restart_when_changed
                            .push(as_str(value).unwrap().trim().to_string());
                    }
                    _ => warn(
                        &mut manifest,
                        format!(
                            "command \"{name}\": `restart_when_changed` must be a glob pattern \
                             or a list of them"
                        ),
                    ),
                },
                other => {
                    warn(&mut manifest, format!("command \"{name}\": unknown key \"{other}\""));
                }
            }
        }
        if command.command.is_empty() {
            warn(&mut manifest, format!("command \"{name}\" has no `command:` line — skipped"));
            continue;
        }
        if !ok {
            continue;
        }
        manifest.commands.push(command);
    }
    manifest
}

/// The command list a project shows: manifest commands first (they are the
/// shared base), each shadowed by a saved command of the same name.
pub(crate) fn merged(manifest: &[ProjectCommand], saved: &[ProjectCommand]) -> Vec<ProjectCommand> {
    let mut out: Vec<ProjectCommand> = manifest
        .iter()
        .filter(|m| !saved.iter().any(|s| s.name == m.name))
        .cloned()
        .collect();
    out.extend(saved.iter().cloned());
    out
}

/// A scalar as the manifest writes it: bare when unmistakably a plain
/// string, single-quoted otherwise — the reader above must get the same
/// text back, and so must every other YAML parser the team's tooling runs.
fn scalar(text: &str) -> String {
    let bare = !text.is_empty()
        && !text.starts_with(|c: char| c.is_whitespace() || "!&*-?|>%@`\"'#{[".contains(c))
        && !text.ends_with(char::is_whitespace)
        && !text.contains(['\n', ':', '#'])
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
        )
        && text.parse::<f64>().is_err();
    if bare { text.to_string() } else { format!("'{}'", text.replace('\'', "''")) }
}

/// The manifest Mast writes on export — the current saved commands, defaults
/// omitted, with enough of a header that the next reader knows what they are
/// looking at without the app in front of them.
pub(crate) fn render(commands: &[ProjectCommand]) -> String {
    let mut out = String::from(
        "# Mast project manifest — commands shared with everyone who works on this repo.\n\
         # Each command is argv-only (no shell); a leading `sail` resolves to\n\
         # vendor/bin/sail. Per command: command (required), cwd, after, ready_when,\n\
         # auto_start, auto_restart, restart_when_changed.\n\
         commands:\n",
    );
    for cmd in commands {
        out.push_str(&format!("  {}:\n", scalar(&cmd.name)));
        out.push_str(&format!("    command: {}\n", scalar(&cmd.command)));
        if let Some(cwd) = cmd.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
            out.push_str(&format!("    cwd: {}\n", scalar(cwd)));
        }
        if let Some(after) = cmd.after.as_deref().filter(|a| !a.trim().is_empty()) {
            out.push_str(&format!("    after: {}\n", scalar(after)));
        }
        if let Some(ready) = cmd.ready_when.as_deref().filter(|r| !r.trim().is_empty()) {
            out.push_str(&format!("    ready_when: {}\n", scalar(ready)));
        }
        if cmd.auto_start {
            out.push_str("    auto_start: true\n");
        }
        if cmd.auto_restart {
            out.push_str("    auto_restart: true\n");
        }
        if !cmd.restart_when_changed.is_empty() {
            out.push_str("    restart_when_changed:\n");
            for pattern in &cmd.restart_when_changed {
                out.push_str(&format!("      - {}\n", scalar(pattern)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_manifest_parses() {
        let manifest = parse(
            "commands:\n\
             \x20 vite:\n\
             \x20   command: sail npm run dev\n\
             \x20   auto_start: true\n\
             \x20   ready_when: 'ready in'\n\
             \x20 queue:\n\
             \x20   command: sail artisan queue:work\n\
             \x20   auto_start: true\n\
             \x20   auto_restart: true\n\
             \x20   after: vite\n\
             \x20   restart_when_changed:\n\
             \x20     - app/**\n\
             \x20     - config/**\n\
             \x20 frontend:\n\
             \x20   command: npm run dev\n\
             \x20   cwd: ../frontend\n",
        );
        assert_eq!(manifest.warnings, Vec::<String>::new());
        assert_eq!(manifest.commands.len(), 3);
        let queue = &manifest.commands[1];
        assert_eq!(queue.name, "queue");
        assert_eq!(queue.command, "sail artisan queue:work");
        assert!(queue.auto_start && queue.auto_restart && queue.from_manifest);
        assert_eq!(queue.after.as_deref(), Some("vite"));
        assert_eq!(queue.restart_when_changed, vec!["app/**", "config/**"]);
        assert_eq!(manifest.commands[2].cwd.as_deref(), Some("../frontend"));
    }

    #[test]
    fn a_single_restart_pattern_needs_no_list() {
        let manifest = parse(
            "commands:\n\
             \x20 queue:\n\
             \x20   command: sail artisan queue:work\n\
             \x20   restart_when_changed: app/**\n",
        );
        assert_eq!(manifest.commands[0].restart_when_changed, vec!["app/**"]);
    }

    /// The manifest is someone else's commit; their mistake becomes a
    /// warning that names it, never a project that fails to load.
    #[test]
    fn mistakes_are_named_and_survived() {
        let manifest = parse(
            "processes:\n\
             \x20 web: {}\n\
             commands:\n\
             \x20 ok:\n\
             \x20   command: npm run dev\n\
             \x20   auto_start: yes please\n\
             \x20   colour: green\n\
             \x20 broken:\n\
             \x20   cwd: ../elsewhere\n",
        );
        // The parseable command still loads, with its good fields intact.
        assert_eq!(manifest.commands.len(), 1);
        assert_eq!(manifest.commands[0].name, "ok");
        assert!(!manifest.commands[0].auto_start);
        let all = manifest.warnings.join("\n");
        assert!(all.contains("unknown section \"processes\""), "{all}");
        assert!(all.contains("`auto_start` must be true or false"), "{all}");
        assert!(all.contains("unknown key \"colour\""), "{all}");
        assert!(all.contains("\"broken\" has no `command:`"), "{all}");
    }

    #[test]
    fn garbage_and_emptiness_are_calm() {
        assert_eq!(parse(""), Manifest::default());
        assert_eq!(parse("commands:\n"), Manifest::default());
        assert!(parse("commands: [broken").warnings.iter().any(|w| w.contains("not valid YAML")));
        assert!(parse("- a\n- b").warnings.iter().any(|w| w.contains("expected a mapping")));
    }

    #[test]
    fn saved_commands_shadow_manifest_ones() {
        let manifest_cmd = |name: &str| ProjectCommand {
            name: name.into(),
            command: "from manifest".into(),
            auto_start: false,
            cwd: None,
            after: None,
            ready_when: None,
            auto_restart: false,
            restart_when_changed: Vec::new(),
            from_manifest: true,
        };
        let saved = ProjectCommand { command: "mine".into(), from_manifest: false, ..manifest_cmd("vite") };
        let merged = merged(&[manifest_cmd("vite"), manifest_cmd("queue")], &[saved]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "queue"); // unshadowed manifest entry first
        assert_eq!(merged[1].command, "mine"); // the local override won
    }

    /// What render writes, parse reads back identically — the export is only
    /// trustworthy if the round trip is.
    #[test]
    fn rendered_manifests_roundtrip() {
        let commands = vec![
            ProjectCommand {
                name: "vite".into(),
                command: "sail npm run dev".into(),
                auto_start: true,
                cwd: None,
                after: None,
                ready_when: Some("ready in".into()),
                auto_restart: false,
                restart_when_changed: Vec::new(),
                from_manifest: true,
            },
            ProjectCommand {
                name: "queue: the weird one".into(),
                command: "sail artisan queue:work --tries=3".into(),
                auto_start: false,
                cwd: Some("../frontend".into()),
                after: Some("vite".into()),
                ready_when: None,
                auto_restart: true,
                restart_when_changed: vec!["app/**".into(), "config/*.php".into()],
                from_manifest: true,
            },
        ];
        let parsed = parse(&render(&commands));
        assert_eq!(parsed.warnings, Vec::<String>::new());
        assert_eq!(parsed.commands, commands);
    }
}
