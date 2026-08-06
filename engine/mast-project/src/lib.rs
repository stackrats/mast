//! Project discovery + personal metadata store.
//!
//! Metadata ownership policy (plan §5): everything here lives under the OS
//! app-data dir, keyed by canonical project path. Import writes NOTHING into
//! the developer's repo. Project-local `.mast/` is a later, explicit opt-in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod node;

pub use node::{inspect_node_project, NodeProject, PackageManager};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("corrupt metadata file {path}: {source}")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
}

const COMPOSE_FILE_NAMES: [&str; 4] =
    ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];

pub fn has_compose_file(dir: &Path) -> bool {
    COMPOSE_FILE_NAMES.iter().any(|n| dir.join(n).is_file())
}

pub fn is_sail_project(dir: &Path) -> bool {
    dir.join("vendor/bin/sail").is_file()
}

/// Does this look like a Sail project even if `vendor/` is absent (fresh
/// clone)? Checked via composer.json and compose-file references — cheap
/// string scans, no parsing.
pub fn is_sail_flavored(dir: &Path) -> bool {
    if is_sail_project(dir) {
        return true;
    }
    if let Ok(composer) = std::fs::read_to_string(dir.join("composer.json"))
        && composer.contains("laravel/sail")
    {
        return true;
    }
    COMPOSE_FILE_NAMES.iter().any(|name| {
        std::fs::read_to_string(dir.join(name))
            .map(|content| content.contains("vendor/laravel/sail"))
            .unwrap_or(false)
    })
}

/// Bootstrap-state warnings (M4): surfaced instead of silently degrading —
/// e.g. running a vendor-less Sail clone through bare compose breaks
/// WWWUSER/WWWGROUP parity (ADR-0001). One-click repairs land in M7.
pub fn project_warnings(dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    if is_sail_flavored(dir) && !dir.join("vendor/bin/sail").is_file() {
        warnings.push(
            "Sail project without vendor/ — dependencies are not installed, so lifecycle \
             runs through bare docker compose (WWWUSER/WWWGROUP will be empty). Run a \
             containerized `composer install` (laravelsail/phpXX-composer) to bootstrap."
                .to_string(),
        );
    }
    if !dir.join(".env").is_file() && dir.join(".env.example").is_file() {
        warnings.push(
            ".env is missing (but .env.example exists) — compose interpolation and sail \
             defaults will not apply until it is created."
                .to_string(),
        );
    }
    warnings
}

/// Stable project identity: hash of the canonical path (plan §5 keying).
pub fn project_id(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let mut id = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCandidate {
    pub path: PathBuf,
    pub name: String,
    pub is_sail: bool,
}

/// Scan watched directories one level deep (plus the directory itself) for compose
/// projects. Pure fs — callers wrap in `spawn_blocking`.
pub fn scan_directories(directories: &[PathBuf]) -> Vec<DiscoveredCandidate> {
    let mut out: Vec<DiscoveredCandidate> = Vec::new();
    let mut push = |dir: PathBuf| {
        if has_compose_file(&dir) || is_sail_project(&dir) {
            let canonical = dir.canonicalize().unwrap_or(dir);
            if out.iter().any(|c| c.path == canonical) {
                return;
            }
            out.push(DiscoveredCandidate {
                name: canonical
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                is_sail: is_sail_project(&canonical),
                path: canonical,
            });
        }
    };
    for directory in directories {
        push(directory.clone());
        let Ok(entries) = std::fs::read_dir(directory) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                push(path);
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

// ---------- metadata store ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub path: PathBuf,
    pub display_name: String,
    pub is_sail: bool,
    /// User-defined commands (M7.5): name → whitespace-split argv line.
    #[serde(default)]
    pub commands: Vec<ProjectCommandRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCommandRecord {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub auto_start: bool,
}

/// `Default` is what a machine with no `settings.json` starts from, so it has
/// to agree with the serde defaults below rather than with `bool::default()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub watched_directories: Vec<PathBuf>,
    /// Preferred terminal emulator binary (None = auto-detect).
    #[serde(default)]
    pub terminal: Option<String>,
    /// Preferred editor binary (None = auto-detect).
    #[serde(default)]
    pub editor: Option<String>,
    /// Move a busy host port into `.env` on start rather than failing the
    /// bind. Absent in settings written before this existed — on by default.
    #[serde(default = "yes")]
    pub auto_port_remap: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            watched_directories: Vec::new(),
            terminal: None,
            editor: None,
            auto_port_remap: yes(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMemberRecord {
    pub project_id: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: String,
    pub name: String,
    pub members: Vec<WorkspaceMemberRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMemberRecord {
    pub project_id: String,
    pub project_name: String,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
    /// (path, sha256 hex) of compose files + .env at capture time.
    pub files: Vec<(String, String)>,
}

/// Workspace snapshot: metadata only (plan §7) — refs and hashes, never file
/// contents; restore is a report, never an apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub taken_unix: u64,
    pub members: Vec<SnapshotMemberRecord>,
}

/// One repo's offerable tags as of the last successful registry read.
///
/// Stores the *filtered* release lines rather than the raw tag list — a repo
/// can publish well over a thousand tags, and only the filtered handful is
/// ever consumed. A record is written only on success, so a registry that is
/// unreachable leaves the previous answer in place instead of blanking it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RegistryTagRecord {
    pub versions: Vec<String>,
    pub fetched_unix: u64,
}

impl RegistryTagRecord {
    /// Whether this record is older than `ttl_secs`. A record from the future
    /// (clock moved backwards) counts as stale rather than pinning the cache
    /// forever.
    pub fn is_stale(&self, now_unix: u64, ttl_secs: u64) -> bool {
        match now_unix.checked_sub(self.fetched_unix) {
            Some(age) => age >= ttl_secs,
            None => true,
        }
    }
}

/// Seconds since the Unix epoch, saturating at 0 before it.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Hex sha256 of a file's bytes; None when unreadable/absent.
pub fn file_sha256(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// One in-flight lifecycle operation, persisted so a crash mid-operation is
/// detected on the next start (plan M4). Deliberately carries NO command
/// output — nothing here can leak a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationJournalEntry {
    pub operation: u64,
    pub project_id: String,
    pub verb: String,
    pub started_unix: u64,
}

/// Flat-file JSON store under the app-data dir. Small files, synchronous fs —
/// callers wrap in `spawn_blocking` where it matters. `diagnostics.db`
/// (rusqlite) arrives in M7 for history-shaped data.
pub struct MetadataStore {
    dir: PathBuf,
}

impl MetadataStore {
    /// Default per-user location (XDG data dir on Linux).
    pub fn default_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("mast")
    }

    pub fn open(dir: PathBuf) -> Result<Self, ProjectError> {
        std::fs::create_dir_all(&dir)
            .map_err(|source| ProjectError::Io { path: dir.clone(), source })?;
        Ok(Self { dir })
    }

    fn read_json<T: Default + for<'de> Deserialize<'de>>(
        &self,
        file: &str,
    ) -> Result<T, ProjectError> {
        let path = self.dir.join(file);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content)
                .map_err(|source| ProjectError::Corrupt { path, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(source) => Err(ProjectError::Io { path, source }),
        }
    }

    fn write_json<T: Serialize>(&self, file: &str, value: &T) -> Result<(), ProjectError> {
        let path = self.dir.join(file);
        let tmp = self.dir.join(format!("{file}.tmp"));
        let content = serde_json::to_string_pretty(value).expect("serializable");
        std::fs::write(&tmp, content)
            .map_err(|source| ProjectError::Io { path: tmp.clone(), source })?;
        std::fs::rename(&tmp, &path).map_err(|source| ProjectError::Io { path, source })
    }

    pub fn load_settings(&self) -> Result<Settings, ProjectError> {
        self.read_json("settings.json")
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), ProjectError> {
        self.write_json("settings.json", settings)
    }

    pub fn load_projects(&self) -> Result<Vec<ProjectRecord>, ProjectError> {
        self.read_json("projects.json")
    }

    pub fn save_projects(&self, projects: &[ProjectRecord]) -> Result<(), ProjectError> {
        self.write_json("projects.json", &projects.to_vec())
    }

    /// Import a project directory: canonicalize, validate, record. Returns
    /// the (possibly pre-existing) record — importing twice is idempotent.
    pub fn import_project(&self, path: &Path) -> Result<ProjectRecord, ProjectError> {
        let canonical = path
            .canonicalize()
            .map_err(|source| ProjectError::Io { path: path.to_path_buf(), source })?;
        if !canonical.is_dir() {
            return Err(ProjectError::NotADirectory(canonical));
        }
        let mut projects = self.load_projects()?;
        let id = project_id(&canonical);
        if let Some(existing) = projects.iter().find(|p| p.id == id) {
            return Ok(existing.clone());
        }
        let record = ProjectRecord {
            id,
            display_name: canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            is_sail: is_sail_project(&canonical),
            path: canonical,
            commands: Vec::new(),
        };
        projects.push(record.clone());
        self.save_projects(&projects)?;
        Ok(record)
    }

    /// Directory for config-file backups written by edit transactions.
    /// Where the diagnostics history database lives (rusqlite, owned by
    /// mast-diagnostics).
    pub fn diagnostics_db_path(&self) -> PathBuf {
        self.dir.join("diagnostics.db")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.dir.join("backups")
    }

    /// Image tags last read from the registry, keyed by repo as it appears in
    /// a compose `image:` value.
    pub fn load_registry_tags(&self) -> Result<HashMap<String, RegistryTagRecord>, ProjectError> {
        self.read_json("registry-tags.json")
    }

    /// Record one repo's tags. Read-modify-write: refreshes land one repo at a
    /// time, and losing a concurrent one only costs a re-fetch next time.
    pub fn save_registry_tags(
        &self,
        repo: &str,
        record: RegistryTagRecord,
    ) -> Result<(), ProjectError> {
        let mut all = self.load_registry_tags()?;
        all.insert(repo.to_string(), record);
        self.write_json("registry-tags.json", &all)
    }

    pub fn load_snapshots(&self) -> Result<Vec<SnapshotRecord>, ProjectError> {
        self.read_json("snapshots.json")
    }

    pub fn save_snapshots(&self, snapshots: &[SnapshotRecord]) -> Result<(), ProjectError> {
        self.write_json("snapshots.json", &snapshots.to_vec())
    }

    pub fn push_snapshot(&self, snapshot: SnapshotRecord) -> Result<(), ProjectError> {
        let mut all = self.load_snapshots()?;
        all.retain(|s| s.id != snapshot.id);
        all.push(snapshot);
        self.save_snapshots(&all)
    }

    pub fn remove_snapshot(&self, id: &str) -> Result<bool, ProjectError> {
        let mut all = self.load_snapshots()?;
        let before = all.len();
        all.retain(|s| s.id != id);
        let removed = all.len() != before;
        if removed {
            self.save_snapshots(&all)?;
        }
        Ok(removed)
    }

    pub fn load_workspaces(&self) -> Result<Vec<WorkspaceRecord>, ProjectError> {
        self.read_json("workspaces.json")
    }

    pub fn save_workspaces(&self, workspaces: &[WorkspaceRecord]) -> Result<(), ProjectError> {
        self.write_json("workspaces.json", &workspaces.to_vec())
    }

    pub fn load_journal(&self) -> Result<Vec<OperationJournalEntry>, ProjectError> {
        self.read_json("operations-journal.json")
    }

    pub fn save_journal(&self, entries: &[OperationJournalEntry]) -> Result<(), ProjectError> {
        self.write_json("operations-journal.json", &entries.to_vec())
    }

    pub fn journal_push(&self, entry: OperationJournalEntry) -> Result<(), ProjectError> {
        let mut entries = self.load_journal()?;
        entries.retain(|e| e.operation != entry.operation);
        entries.push(entry);
        self.save_journal(&entries)
    }

    pub fn journal_remove(&self, operation: u64) -> Result<(), ProjectError> {
        let mut entries = self.load_journal()?;
        let before = entries.len();
        entries.retain(|e| e.operation != operation);
        if entries.len() != before {
            self.save_journal(&entries)?;
        }
        Ok(())
    }

    pub fn remove_project(&self, id: &str) -> Result<bool, ProjectError> {
        let mut projects = self.load_projects()?;
        let before = projects.len();
        projects.retain(|p| p.id != id);
        let removed = projects.len() != before;
        if removed {
            self.save_projects(&projects)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_tags_round_trip_per_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MetadataStore::open(tmp.path().to_path_buf()).unwrap();
        assert!(store.load_registry_tags().unwrap().is_empty());

        let mariadb =
            RegistryTagRecord { versions: vec!["12".into(), "11.4".into()], fetched_unix: 100 };
        store.save_registry_tags("mariadb", mariadb.clone()).unwrap();
        // A second repo must not displace the first.
        store
            .save_registry_tags(
                "redis",
                RegistryTagRecord { versions: vec!["8".into()], fetched_unix: 100 },
            )
            .unwrap();

        let all = store.load_registry_tags().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["mariadb"], mariadb);
    }

    #[test]
    fn staleness_is_ttl_based_and_survives_a_backwards_clock() {
        let record = RegistryTagRecord { versions: vec![], fetched_unix: 1_000 };
        assert!(!record.is_stale(1_500, 86_400));
        assert!(record.is_stale(1_000 + 86_400, 86_400));
        // Clock moved behind the record: refetch rather than trust it forever.
        assert!(record.is_stale(500, 86_400));
    }

    #[test]
    fn ids_are_stable_and_path_keyed() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir(&a).unwrap();
        assert_eq!(project_id(&a), project_id(&a));
        assert_eq!(project_id(&a).len(), 16);
    }

    #[test]
    fn scan_finds_compose_and_sail_projects_one_level_deep() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        std::fs::write(plain.join("docker-compose.yml"), "services: {}\n").unwrap();
        let sail = tmp.path().join("sailapp");
        std::fs::create_dir_all(sail.join("vendor/bin")).unwrap();
        std::fs::write(sail.join("vendor/bin/sail"), "#!/bin/sh\n").unwrap();
        std::fs::write(sail.join("docker-compose.yml"), "services: {}\n").unwrap();
        std::fs::create_dir(tmp.path().join("not-a-project")).unwrap();

        let found = scan_directories(&[tmp.path().to_path_buf()]);
        let names: Vec<_> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["plain", "sailapp"]);
        assert!(!found[0].is_sail);
        assert!(found[1].is_sail);
    }

    #[test]
    fn store_roundtrip_and_idempotent_import() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir(&project).unwrap();

        let record = store.import_project(&project).unwrap();
        let again = store.import_project(&project).unwrap();
        assert_eq!(record, again);
        assert_eq!(store.load_projects().unwrap().len(), 1);

        let settings = Settings {
            watched_directories: vec![tmp.path().to_path_buf()],
            terminal: Some("kitty".into()),
            editor: None,
            auto_port_remap: false,
        };
        store.save_settings(&settings).unwrap();
        assert_eq!(store.load_settings().unwrap(), settings);

        assert!(store.remove_project(&record.id).unwrap());
        assert!(!store.remove_project(&record.id).unwrap());
        assert!(store.load_projects().unwrap().is_empty());
    }

    #[test]
    fn bootstrap_warnings_for_vendorless_sail_clone_and_missing_env() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Fresh clone shape: sail in composer.json, compose file committed,
        // but no vendor/ and no .env.
        std::fs::write(
            dir.join("composer.json"),
            r#"{"require": {"php": "^8.2", "laravel/sail": "^1.26"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("docker-compose.yml"), "services: {}\n").unwrap();
        std::fs::write(dir.join(".env.example"), "APP_NAME=x\n").unwrap();

        assert!(is_sail_flavored(dir));
        assert!(!is_sail_project(dir));
        let warnings = project_warnings(dir);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("composer install"));
        assert!(warnings[1].contains(".env is missing"));

        // Bootstrapped: vendor/bin/sail + .env present → no warnings.
        std::fs::create_dir_all(dir.join("vendor/bin")).unwrap();
        std::fs::write(dir.join("vendor/bin/sail"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join(".env"), "APP_NAME=x\n").unwrap();
        assert!(project_warnings(dir).is_empty());
    }

    #[test]
    fn operation_journal_push_remove_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
        assert!(store.load_journal().unwrap().is_empty());
        let entry = OperationJournalEntry {
            operation: 7,
            project_id: "abc".into(),
            verb: "start".into(),
            started_unix: 1_700_000_000,
        };
        store.journal_push(entry.clone()).unwrap();
        assert_eq!(store.load_journal().unwrap(), vec![entry]);
        store.journal_remove(7).unwrap();
        assert!(store.load_journal().unwrap().is_empty());
        store.journal_remove(7).unwrap(); // idempotent
    }

    #[test]
    fn compose_reference_alone_marks_sail_flavored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("docker-compose.yml"),
            "services:\n  laravel.test:\n    build:\n      context: ./vendor/laravel/sail/runtimes/8.4\n",
        )
        .unwrap();
        assert!(is_sail_flavored(tmp.path()));
    }

    #[test]
    fn metadata_never_touches_the_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MetadataStore::open(tmp.path().join("meta")).unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir(&project).unwrap();
        store.import_project(&project).unwrap();
        assert_eq!(std::fs::read_dir(&project).unwrap().count(), 0, "import wrote into the repo");
    }
}
