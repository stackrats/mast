//! ComposeInvocation resolution + read-only resolved model, exactly per
//! ADR-0001's empirical findings:
//!
//! - The project-dir `.env` is parsed FIRST because it can set behavior
//!   variables (`COMPOSE_FILE`, `COMPOSE_PROJECT_NAME`, `COMPOSE_PROFILES`,
//!   `COMPOSE_PATH_SEPARATOR`); the real environment wins per-key.
//! - Cross-family overrides apply (compose.yaml + docker-compose.override.yml
//!   DO merge); both-base-families is surfaced as a warning flag.
//! - Name precedence: env > .env > top-level `name:` > normalized basename.
//! - Sail projects resolve their model through
//!   `SAIL_SKIP_CHECKS=1 vendor/bin/sail config` (parity by construction);
//!   Mast never re-implements sail's env computation.

pub mod catalog;
pub mod network;
pub mod sail;
pub mod transaction;
pub mod versions;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use saphyr::{LoadableYamlNode, Scalar, Yaml};

pub use network::{NetworkAttachPlan, plan_network_attach, workspace_network_name};
pub use transaction::{ComposeEditError, EditReceipt, apply_compose_edit};

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("no compose file found in {0}")]
    NoComposeFile(PathBuf),
    #[error("compose file listed in COMPOSE_FILE is missing: {0}")]
    MissingFile(PathBuf),
    #[error("docker compose config failed: {0}")]
    ConfigFailed(String),
    #[error("could not parse resolved model: {0}")]
    BadModel(String),
    #[error(transparent)]
    Command(#[from] mast_docker::CommandError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    DockerCompose,
    Sail { script: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSource {
    ComposeFileEnv,
    ComposeFileDotEnv,
    DefaultDiscovery,
    OverrideDiscovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationFile {
    pub path: PathBuf,
    pub source: FileSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSource {
    EnvVar,
    DotEnv,
    NameKey,
    DirBasename,
}

/// Everything needed to invoke compose exactly as the developer's terminal
/// would (plan §4). Every field carries provenance for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeInvocation {
    pub project_dir: PathBuf,
    pub runner: Runner,
    pub files: Vec<InvocationFile>,
    pub project_name: String,
    pub name_source: NameSource,
    pub profiles: Vec<String>,
    /// Both `compose.*` and `docker-compose.*` base files exist — compose
    /// warns on stderr and picks `compose.*`; users often miss it.
    pub both_base_families: bool,
}

impl ComposeInvocation {
    pub fn is_sail(&self) -> bool {
        matches!(self.runner, Runner::Sail { .. })
    }
}

/// Minimal `.env` reader for compose *behavior* variables (also used by the
/// engine's secret redactor). Full lossless env modelling
/// (quoting/escapes/multiline) is M5's `env-corpus`-driven model; behavior
/// vars are plain `KEY=VALUE` in practice.
pub fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return vars;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_start_matches("export ").trim();
        let mut value = value.trim();
        if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
            || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
        {
            value = &value[1..value.len() - 1];
        }
        vars.insert(key.to_string(), value.to_string());
    }
    vars
}

/// Service keys declared in one compose file's `services:` mapping (used for
/// catalog installed-state; does not merge multi-file invocations).
pub fn declared_service_keys(source: &str) -> Vec<String> {
    let Ok(docs) = Yaml::load_from_str(source) else { return Vec::new() };
    let Some(root) = docs.first() else { return Vec::new() };
    let Some(Yaml::Mapping(services)) = network::mapping_get(root, "services") else {
        return Vec::new();
    };
    services
        .iter()
        .filter_map(|(k, _)| match k {
            Yaml::Value(Scalar::String(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect()
}

/// Default project name normalization observed in ADR-0001: lowercase,
/// invalid characters stripped (not replaced), must start alphanumeric.
pub fn normalize_project_name(raw: &str) -> String {
    let cleaned: String = raw
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')
        .collect();
    let trimmed = cleaned.trim_start_matches(['-', '_']).to_string();
    if trimmed.is_empty() { "default".to_string() } else { trimmed }
}

fn peek_name_key(file: &Path) -> Option<String> {
    let content = std::fs::read_to_string(file).ok()?;
    let docs = Yaml::load_from_str(&content).ok()?;
    let doc = docs.first()?;
    if let Yaml::Mapping(map) = doc {
        for (k, v) in map.iter() {
            if let (Yaml::Value(Scalar::String(key)), Yaml::Value(Scalar::String(value))) = (k, v)
                && key.as_ref() == "name"
            {
                return Some(value.to_string());
            }
        }
    }
    None
}

const BASE_CANDIDATES: [&str; 4] =
    ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
const OVERRIDE_CANDIDATES: [&str; 4] = [
    "compose.override.yaml",
    "compose.override.yml",
    "docker-compose.override.yaml",
    "docker-compose.override.yml",
];

/// Resolve the invocation for `project_dir`. `process_env` is the real
/// environment (wins over `.env` per-key, ADR-0001 finding 3) — injected so
/// tests are hermetic.
pub fn resolve_invocation(
    project_dir: &Path,
    process_env: &HashMap<String, String>,
) -> Result<ComposeInvocation, ComposeError> {
    let dotenv = parse_env_file(&project_dir.join(".env"));
    let get = |key: &str| -> Option<(String, bool)> {
        // (value, from_real_env)
        if let Some(v) = process_env.get(key)
            && !v.is_empty()
        {
            return Some((v.clone(), true));
        }
        dotenv.get(key).filter(|v| !v.is_empty()).map(|v| (v.clone(), false))
    };

    let separator = get("COMPOSE_PATH_SEPARATOR").map(|(v, _)| v).unwrap_or_else(|| ":".into());

    let mut both_base_families = false;
    let files: Vec<InvocationFile> = if let Some((compose_file, from_env)) = get("COMPOSE_FILE") {
        let source =
            if from_env { FileSource::ComposeFileEnv } else { FileSource::ComposeFileDotEnv };
        let mut out = Vec::new();
        for part in compose_file.split(separator.as_str()).filter(|p| !p.is_empty()) {
            let path = project_dir.join(part);
            if !path.is_file() {
                return Err(ComposeError::MissingFile(path));
            }
            out.push(InvocationFile { path, source });
        }
        if out.is_empty() {
            return Err(ComposeError::NoComposeFile(project_dir.to_path_buf()));
        }
        out
    } else {
        let existing_bases: Vec<PathBuf> = BASE_CANDIDATES
            .iter()
            .map(|n| project_dir.join(n))
            .filter(|p| p.is_file())
            .collect();
        let Some(base) = existing_bases.first().cloned() else {
            return Err(ComposeError::NoComposeFile(project_dir.to_path_buf()));
        };
        let has_compose_family = existing_bases
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("compose.")));
        let has_docker_family = existing_bases.iter().any(|p| {
            p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("docker-compose."))
        });
        both_base_families = has_compose_family && has_docker_family;

        let mut out = vec![InvocationFile { path: base, source: FileSource::DefaultDiscovery }];
        // ADR-0001: override files merge across families.
        for name in OVERRIDE_CANDIDATES {
            let path = project_dir.join(name);
            if path.is_file() {
                out.push(InvocationFile { path, source: FileSource::OverrideDiscovery });
            }
        }
        out
    };

    let (project_name, name_source) = if let Some((name, from_env)) = get("COMPOSE_PROJECT_NAME") {
        (name, if from_env { NameSource::EnvVar } else { NameSource::DotEnv })
    } else if let Some(name) = files.first().and_then(|f| peek_name_key(&f.path)) {
        (name, NameSource::NameKey)
    } else {
        let basename = project_dir.file_name().map(|n| n.to_string_lossy().into_owned());
        (normalize_project_name(&basename.unwrap_or_default()), NameSource::DirBasename)
    };

    let profiles = get("COMPOSE_PROFILES")
        .map(|(v, _)| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    let sail_script = project_dir.join("vendor/bin/sail");
    let runner = if is_executable(&sail_script) {
        Runner::Sail { script: sail_script }
    } else {
        Runner::DockerCompose
    };

    Ok(ComposeInvocation {
        project_dir: project_dir.to_path_buf(),
        runner,
        files,
        project_name,
        name_source,
        profiles,
        both_base_families,
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

// ---------- resolved model ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedService {
    pub name: String,
    pub image: Option<String>,
    /// Other DNS names this service answers to on the compose network:
    /// `container_name` plus every per-network alias. Hostname cross-checks
    /// must accept these, not just the service key.
    pub aliases: Vec<String>,
    /// Host ports this service publishes, post-interpolation. Sail's own
    /// shape (`'${VITE_PORT:-5173}:${VITE_PORT:-5173}'`) leaves nothing in
    /// `.env` to read, so collision checks that scan `.env` alone miss it —
    /// this is the authoritative list.
    pub published_ports: Vec<u16>,
    /// Named volumes this service mounts (the compose-file source names,
    /// e.g. `sail-mysql`) — bind mounts excluded. What a data-destroying
    /// repair must name before touching anything.
    pub volumes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub name: String,
    pub services: Vec<ResolvedService>,
    /// Daemon-side names of networks the model declares `external: true`
    /// (compose refuses to start until these exist).
    pub external_networks: Vec<String>,
}

/// Host port(s) claimed by one `ports` entry of the resolved model.
///
/// `published` arrives as a string from current compose versions and as a
/// number from older ones. An absent or empty value means "pick any free
/// port", which claims nothing predictable; a range (`"8000-8005"`) claims
/// every port in it.
fn parse_published_port(value: Option<&serde_json::Value>) -> Vec<u16> {
    // Keeps a malformed range from expanding into tens of thousands of ports.
    const MAX_RANGE: usize = 128;
    let raw = match value {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => return Vec::new(),
    };
    if let Some((low, high)) = raw.split_once('-') {
        let (Ok(low), Ok(high)) = (low.trim().parse::<u16>(), high.trim().parse::<u16>()) else {
            return Vec::new();
        };
        if low > high {
            return Vec::new();
        }
        return (low..=high).take(MAX_RANGE).collect();
    }
    raw.parse::<u16>().map(|port| vec![port]).unwrap_or_default()
}

/// Which `.env` key governs which published host port, read from the compose
/// source as `port default → key`.
///
/// The resolved model has already lost this: `'${APP_PORT:-80}:80'` becomes
/// the number 80, with nothing left to say that writing `APP_PORT` is how you
/// move it. Sail's own files are full of that shape and Laravel's
/// `.env.example` ships none of those keys, so without this a stock project
/// publishes 80, 5173 and 3306 with no key in `.env` to point at.
///
/// This reads the file as text rather than YAML on purpose: it is a label
/// lookup, not an edit (edits go through `mast-yaml-edit`'s parser per
/// ADR-0003), and only list entries under a `ports:` key can carry a
/// published port — an interpolation anywhere else (an `environment:` value,
/// say) must not be mistaken for one.
pub fn published_port_env_keys(compose_text: &str) -> std::collections::BTreeMap<u16, String> {
    let mut keys = std::collections::BTreeMap::new();
    for line in compose_text.lines() {
        let Some(entry) = line.trim().strip_prefix('-') else { continue };
        let entry = entry.trim().trim_matches(['\'', '"']);
        // `[HOST_IP:]HOST:CONTAINER` — the host side comes first, so the
        // first interpolation in the entry is the published port.
        let Some(interp) = entry.split_once("${") else { continue };
        let Some((body, _)) = interp.1.split_once('}') else { continue };
        // Only the `:-default` form names a port we can recognise later; a
        // bare `${APP_PORT}` publishes whatever `.env` already says, which
        // the `.env` scan sees on its own.
        let Some((key, default)) = body.split_once(":-") else { continue };
        let (Ok(port), true) = (default.trim().parse::<u16>(), !key.is_empty()) else { continue };
        keys.entry(port).or_insert_with(|| key.trim().to_string());
    }
    keys
}

/// Run the runner's `config --format json` (read-only; works with the daemon
/// down — ADR-0001 finding 6) and parse the resolved model.
pub async fn resolve_model(invocation: &ComposeInvocation) -> Result<ResolvedModel, ComposeError> {
    let mut env_overlay: Vec<(String, String)> = Vec::new();
    let argv: Vec<String> = match &invocation.runner {
        Runner::Sail { script } => {
            // ADR-0001: read-only verbs go through sail WITH the checks
            // skipped — no docker-info gate, and critically no auto-`down`
            // side effect on exited projects.
            env_overlay.push(("SAIL_SKIP_CHECKS".into(), "1".into()));
            vec![script.to_string_lossy().into_owned(), "config".into(), "--format".into(), "json".into()]
        }
        Runner::DockerCompose => {
            let mut argv = vec!["docker".to_string(), "compose".to_string()];
            for file in &invocation.files {
                argv.push("-f".into());
                argv.push(file.path.to_string_lossy().into_owned());
            }
            for profile in &invocation.profiles {
                argv.push("--profile".into());
                argv.push(profile.clone());
            }
            argv.extend(["config".into(), "--format".into(), "json".into()]);
            argv
        }
    };

    let out = mast_docker::run_command(
        &argv,
        Some(&invocation.project_dir),
        &env_overlay,
        Duration::from_secs(20),
        4 * 1024 * 1024,
    )
    .await?;
    if !out.success() {
        return Err(ComposeError::ConfigFailed(out.stderr.trim().to_string()));
    }

    let value: serde_json::Value =
        serde_json::from_str(&out.stdout).map_err(|e| ComposeError::BadModel(e.to_string()))?;
    let name = value
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(&invocation.project_name)
        .to_string();
    let mut services: Vec<ResolvedService> = value
        .get("services")
        .and_then(|s| s.as_object())
        .map(|map| {
            map.iter()
                .map(|(service, def)| {
                    let mut aliases: Vec<String> = def
                        .get("container_name")
                        .and_then(|n| n.as_str())
                        .map(String::from)
                        .into_iter()
                        .collect();
                    if let Some(networks) = def.get("networks").and_then(|n| n.as_object()) {
                        for net in networks.values() {
                            if let Some(list) = net.get("aliases").and_then(|a| a.as_array()) {
                                aliases.extend(
                                    list.iter().filter_map(|a| a.as_str()).map(String::from),
                                );
                            }
                        }
                    }
                    aliases.sort();
                    aliases.dedup();
                    let mut published_ports: Vec<u16> = def
                        .get("ports")
                        .and_then(|p| p.as_array())
                        .map(|list| {
                            list.iter()
                                .flat_map(|p| parse_published_port(p.get("published")))
                                .collect()
                        })
                        .unwrap_or_default();
                    published_ports.sort_unstable();
                    published_ports.dedup();
                    // `config` expands volumes to long form, so named mounts
                    // are exactly the entries with type == "volume".
                    let mut volumes: Vec<String> = def
                        .get("volumes")
                        .and_then(|v| v.as_array())
                        .map(|list| {
                            list.iter()
                                .filter(|m| {
                                    m.get("type").and_then(|t| t.as_str()) == Some("volume")
                                })
                                .filter_map(|m| m.get("source").and_then(|s| s.as_str()))
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    volumes.sort();
                    volumes.dedup();
                    ResolvedService {
                        name: service.clone(),
                        image: def.get("image").and_then(|i| i.as_str()).map(String::from),
                        aliases,
                        published_ports,
                        volumes,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    let mut external_networks: Vec<String> = value
        .get("networks")
        .and_then(|n| n.as_object())
        .map(|map| {
            map.iter()
                .filter(|(_, def)| {
                    def.get("external").and_then(|e| e.as_bool()).unwrap_or(false)
                })
                // `config` resolves the daemon-side name; fall back to the key.
                .map(|(key, def)| {
                    def.get("name").and_then(|n| n.as_str()).unwrap_or(key).to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    external_networks.sort();
    Ok(ResolvedModel { name, services, external_networks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    const MINIMAL: &str = "services:\n  app:\n    image: alpine\n";

    #[test]
    fn default_discovery_prefers_compose_yaml_and_flags_both_families() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "compose.yaml", MINIMAL);
        write(tmp.path(), "docker-compose.yml", MINIMAL);
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert!(inv.files[0].path.ends_with("compose.yaml"));
        assert!(inv.both_base_families);
    }

    #[test]
    fn cross_family_override_is_included() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "compose.yaml", MINIMAL);
        write(tmp.path(), "docker-compose.override.yml", MINIMAL);
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        let names: Vec<_> = inv
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["compose.yaml", "docker-compose.override.yml"]);
        assert_eq!(inv.files[1].source, FileSource::OverrideDiscovery);
    }

    #[test]
    fn compose_file_from_dotenv_redirects_file_selection() {
        // The ADR-0001 chicken-and-egg: .env decides which files apply.
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "compose.yaml", MINIMAL);
        write(tmp.path(), "alt.yml", MINIMAL);
        write(tmp.path(), ".env", "COMPOSE_FILE=alt.yml\n");
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        assert_eq!(inv.files.len(), 1);
        assert!(inv.files[0].path.ends_with("alt.yml"));
        assert_eq!(inv.files[0].source, FileSource::ComposeFileDotEnv);
    }

    #[test]
    fn real_env_beats_dotenv() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "compose.yaml", MINIMAL);
        write(tmp.path(), "alt.yml", MINIMAL);
        write(tmp.path(), ".env", "COMPOSE_FILE=alt.yml\nCOMPOSE_PROJECT_NAME=fromdotenv\n");
        let inv =
            resolve_invocation(tmp.path(), &env(&[("COMPOSE_FILE", "compose.yaml")])).unwrap();
        assert!(inv.files[0].path.ends_with("compose.yaml"));
        assert_eq!(inv.files[0].source, FileSource::ComposeFileEnv);
        // Name still comes from .env — real env only had COMPOSE_FILE.
        assert_eq!(inv.project_name, "fromdotenv");
        assert_eq!(inv.name_source, NameSource::DotEnv);
    }

    #[test]
    fn missing_compose_file_entry_errors() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), ".env", "COMPOSE_FILE=nope.yml\n");
        assert!(matches!(
            resolve_invocation(tmp.path(), &env(&[])),
            Err(ComposeError::MissingFile(_))
        ));
    }

    #[test]
    fn name_precedence_name_key_then_basename() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "compose.yaml", "name: fromfile\nservices:\n  app:\n    image: alpine\n");
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        assert_eq!(inv.project_name, "fromfile");
        assert_eq!(inv.name_source, NameSource::NameKey);

        write(tmp.path(), "compose.yaml", MINIMAL);
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        assert_eq!(inv.name_source, NameSource::DirBasename);
        assert_eq!(inv.project_name, normalize_project_name(
            &tmp.path().file_name().unwrap().to_string_lossy()
        ));
    }

    #[test]
    fn normalization_matches_adr_observations() {
        // ADR-0001: `My_Weird.Dir-NAME` → `my_weirddir-name` (dot stripped).
        assert_eq!(normalize_project_name("My_Weird.Dir-NAME"), "my_weirddir-name");
        assert_eq!(normalize_project_name("--x"), "x");
        assert_eq!(normalize_project_name("...."), "default");
    }

    #[test]
    fn sail_runner_detected_when_script_executable() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "docker-compose.yml", MINIMAL);
        let bin = tmp.path().join("vendor/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("sail"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("sail"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        assert!(inv.is_sail());
    }

    #[test]
    fn profiles_parsed_from_env() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "compose.yaml", MINIMAL);
        write(tmp.path(), ".env", "COMPOSE_PROFILES=debug, extra\n");
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        assert_eq!(inv.profiles, vec!["debug", "extra"]);
    }

    #[tokio::test]
    async fn resolved_model_via_real_docker_cli() {
        // Offline-capable (ADR-0001 finding 6) but needs the docker CLI.
        if mast_docker::run_command(
            &["docker".into(), "compose".into(), "version".into()],
            None,
            &[],
            Duration::from_secs(5),
            4096,
        )
        .await
        .map(|o| !o.success())
        .unwrap_or(true)
        {
            eprintln!("skipping: docker compose CLI not available");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "compose.yaml",
            "services:\n  app:\n    image: alpine:latest\n    volumes:\n      - ./src:/app\n  db:\n    image: mysql:8\n    volumes:\n      - 'sail-mysql:/var/lib/mysql'\nvolumes:\n  sail-mysql:\n    driver: local\n",
        );
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        let model = resolve_model(&inv).await.unwrap();
        assert_eq!(model.name, inv.project_name);
        let names: Vec<_> = model.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["app", "db"]);
        assert_eq!(model.services[0].image.as_deref(), Some("alpine:latest"));
        // Named mounts surface as sources; bind mounts stay out.
        assert!(model.services[0].volumes.is_empty(), "{:?}", model.services[0].volumes);
        assert_eq!(model.services[1].volumes, vec!["sail-mysql"]);
    }

    #[test]
    fn published_ports_parse_across_compose_shapes() {
        use serde_json::json;
        let parse = |v: serde_json::Value| parse_published_port(Some(&v));
        assert_eq!(parse(json!("8081")), vec![8081]);
        assert_eq!(parse(json!(8081)), vec![8081]);
        assert_eq!(parse(json!("8000-8002")), vec![8000, 8001, 8002]);
        // Empty published = "pick any free port": claims nothing predictable.
        assert_eq!(parse(json!("")), Vec::<u16>::new());
        assert_eq!(parse(json!("8005-8000")), Vec::<u16>::new());
        assert_eq!(parse(json!("nonsense")), Vec::<u16>::new());
        assert_eq!(parse_published_port(None), Vec::<u16>::new());
        // A malformed range stays bounded rather than expanding to 64k.
        assert_eq!(parse(json!("1-65535")).len(), 128);
    }

    #[tokio::test]
    async fn published_ports_see_compose_defaults_with_no_env_file() {
        // The Sail shape that defeated the .env-only port scan: nothing sets
        // VITE_PORT, so only the resolved model knows 5173 is claimed.
        if mast_docker::run_command(
            &["docker".into(), "compose".into(), "version".into()],
            None,
            &[],
            Duration::from_secs(5),
            4096,
        )
        .await
        .map(|o| !o.success())
        .unwrap_or(true)
        {
            eprintln!("skipping: docker compose CLI not available");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "compose.yaml",
            "services:\n  laravel.test:\n    image: alpine:latest\n    ports:\n      \
             - '${APP_PORT:-80}:80'\n      - '${VITE_PORT:-5173}:${VITE_PORT:-5173}'\n",
        );
        let inv = resolve_invocation(tmp.path(), &env(&[])).unwrap();
        let model = resolve_model(&inv).await.unwrap();
        assert_eq!(model.services[0].published_ports, vec![80, 5173]);
    }

    fn corpus(name: &str) -> String {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/compose-corpus").join(name);
        std::fs::read_to_string(path).expect("corpus file")
    }

    #[test]
    fn port_env_keys_come_from_sails_own_shapes() {
        let keys = published_port_env_keys(&corpus("sail-stock.yaml"));
        assert_eq!(keys.get(&80).map(String::as_str), Some("APP_PORT"));
        assert_eq!(keys.get(&5173).map(String::as_str), Some("VITE_PORT"));
        assert_eq!(keys.get(&3306).map(String::as_str), Some("FORWARD_DB_PORT"));
        assert_eq!(keys.get(&6379).map(String::as_str), Some("FORWARD_REDIS_PORT"));
    }

    #[test]
    fn interpolations_outside_a_ports_list_are_not_ports() {
        // `PLAIN_INTERP: ${APP_PORT:-80}` under `environment:` — same text,
        // publishes nothing.
        assert!(published_port_env_keys(&corpus("quoting-escapes.yaml")).is_empty());
        assert!(published_port_env_keys("services:\n  a:\n    x: ${APP_PORT:-80}\n").is_empty());
    }

    #[test]
    fn published_port_is_the_host_side_even_behind_an_ip() {
        let keys = published_port_env_keys(
            "    ports:\n      - '127.0.0.1:${FORWARD_DB_PORT:-3306}:3306'\n      \
             - 8080:80\n      - ${APP_PORT}:80\n",
        );
        assert_eq!(keys.get(&3306).map(String::as_str), Some("FORWARD_DB_PORT"));
        // A literal, and a key with no default: neither names a port default.
        assert_eq!(keys.len(), 1);
    }
}
