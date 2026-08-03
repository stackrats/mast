//! Node package-manager detection.
//!
//! Which manager a project uses is a property of the repo, not a user
//! preference: the lockfile is committed and the team already chose. Running
//! the wrong one writes a competing lockfile into the developer's repo, which
//! the metadata policy in this crate's header exists to prevent — so Mast
//! detects rather than asks.
//!
//! Sail's runtime images from PHP 8.2 onward ship npm, pnpm, yarn (via
//! corepack) and bun, and `vendor/bin/sail` proxies all four, so acting on the
//! detected manager needs nothing extra installed.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Yarn => "yarn",
            PackageManager::Bun => "bun",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "npm" => Some(PackageManager::Npm),
            "pnpm" => Some(PackageManager::Pnpm),
            "yarn" => Some(PackageManager::Yarn),
            "bun" => Some(PackageManager::Bun),
            _ => None,
        }
    }

    /// The install argv tail, run through `sail <…>` or `compose exec`.
    ///
    /// A committed lockfile means the resolution is already decided, so the
    /// frozen form is used — it fails loudly instead of silently rewriting the
    /// lockfile under the developer. Yarn is the exception: the immutable flag
    /// is spelled differently by v1 and berry, and a project can pin either
    /// through `packageManager`, so plain `install` is the portable choice.
    pub fn install_argv(self, frozen: bool) -> Vec<String> {
        let args: &[&str] = match (self, frozen) {
            (PackageManager::Npm, true) => &["npm", "ci"],
            (PackageManager::Npm, false) => &["npm", "install"],
            (PackageManager::Pnpm, true) => &["pnpm", "install", "--frozen-lockfile"],
            (PackageManager::Pnpm, false) => &["pnpm", "install"],
            (PackageManager::Yarn, _) => &["yarn", "install"],
            (PackageManager::Bun, true) => &["bun", "install", "--frozen-lockfile"],
            (PackageManager::Bun, false) => &["bun", "install"],
        };
        args.iter().map(|s| s.to_string()).collect()
    }
}

/// Lockfile → manager, in the order a multi-lockfile repo is reported.
const LOCKFILES: [(&str, PackageManager); 6] = [
    ("pnpm-lock.yaml", PackageManager::Pnpm),
    ("yarn.lock", PackageManager::Yarn),
    ("bun.lock", PackageManager::Bun),
    ("bun.lockb", PackageManager::Bun),
    ("package-lock.json", PackageManager::Npm),
    ("npm-shrinkwrap.json", PackageManager::Npm),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProject {
    pub manager: PackageManager,
    /// A lockfile for `manager` is committed — installs can be frozen.
    pub frozen: bool,
    /// `packageManager` in package.json decided it, so the lockfiles below
    /// were not consulted and disagreement among them is not a problem.
    pub pinned: bool,
    /// Every lockfile present, when more than one is — the repo is ambiguous
    /// and worth reporting rather than silently resolving.
    pub conflicting_lockfiles: Vec<String>,
    pub has_node_modules: bool,
}

/// Inspect a directory's Node setup. `None` when there is no package.json —
/// the project has no frontend build and nothing here applies.
///
/// Resolution order matches corepack and the major frameworks: the
/// `packageManager` field wins (it is explicit and versioned), then the
/// lockfile, then npm as the default Laravel scaffolds with.
pub fn inspect_node_project(dir: &Path) -> Option<NodeProject> {
    let package_json = std::fs::read_to_string(dir.join("package.json")).ok()?;

    let present: Vec<(&str, PackageManager)> =
        LOCKFILES.iter().copied().filter(|(name, _)| dir.join(name).is_file()).collect();
    let has_node_modules = dir.join("node_modules").is_dir();

    if let Some(pinned) = package_manager_field(&package_json) {
        return Some(NodeProject {
            manager: pinned,
            frozen: present.iter().any(|(_, m)| *m == pinned),
            pinned: true,
            conflicting_lockfiles: Vec::new(),
            has_node_modules,
        });
    }

    // Distinct managers, not distinct files: bun.lock alongside bun.lockb is
    // one manager mid-format-migration, not a conflict.
    let mut managers: Vec<PackageManager> = Vec::new();
    for (_, m) in &present {
        if !managers.contains(m) {
            managers.push(*m);
        }
    }
    let conflicting_lockfiles = if managers.len() > 1 {
        present.iter().map(|(name, _)| (*name).to_string()).collect()
    } else {
        Vec::new()
    };

    Some(NodeProject {
        manager: managers.first().copied().unwrap_or(PackageManager::Npm),
        frozen: !managers.is_empty(),
        pinned: false,
        conflicting_lockfiles,
        has_node_modules,
    })
}

/// The corepack `"packageManager": "pnpm@9.1.0"` field, name only. Parsed with
/// serde rather than scanned, so a match inside an unrelated string or a
/// dependency name cannot be mistaken for the real key.
fn package_manager_field(package_json: &str) -> Option<PackageManager> {
    let value: serde_json::Value = serde_json::from_str(package_json).ok()?;
    let spec = value.get("packageManager")?.as_str()?;
    // "pnpm@9.1.0+sha512.…" — everything before the version separator.
    PackageManager::parse(spec.split('@').next().unwrap_or(spec).trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(d: &tempfile::TempDir, name: &str, body: &str) {
        std::fs::write(d.path().join(name), body).unwrap();
    }

    #[test]
    fn no_package_json_means_not_a_node_project() {
        assert!(inspect_node_project(dir().path()).is_none());
    }

    #[test]
    fn bare_package_json_defaults_to_npm_unfrozen() {
        let d = dir();
        write(&d, "package.json", "{}");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(n.manager, PackageManager::Npm);
        assert!(!n.frozen);
        assert!(!n.pinned);
        assert!(!n.has_node_modules);
    }

    #[test]
    fn lockfile_selects_the_manager_and_freezes() {
        for (file, expected) in [
            ("pnpm-lock.yaml", PackageManager::Pnpm),
            ("yarn.lock", PackageManager::Yarn),
            ("bun.lockb", PackageManager::Bun),
            ("package-lock.json", PackageManager::Npm),
        ] {
            let d = dir();
            write(&d, "package.json", "{}");
            write(&d, file, "");
            let n = inspect_node_project(d.path()).unwrap();
            assert_eq!(n.manager, expected, "{file}");
            assert!(n.frozen, "{file}");
            assert!(n.conflicting_lockfiles.is_empty(), "{file}");
        }
    }

    #[test]
    fn package_manager_field_beats_the_lockfile() {
        let d = dir();
        write(&d, "package.json", r#"{"packageManager":"pnpm@9.1.0"}"#);
        write(&d, "package-lock.json", "{}");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(n.manager, PackageManager::Pnpm);
        assert!(n.pinned);
        // The npm lockfile says nothing about whether pnpm's is committed.
        assert!(!n.frozen);
        assert!(n.conflicting_lockfiles.is_empty());
    }

    #[test]
    fn pinned_manager_with_its_own_lockfile_is_frozen() {
        let d = dir();
        write(&d, "package.json", r#"{"packageManager":"pnpm@9.1.0+sha512.abc"}"#);
        write(&d, "pnpm-lock.yaml", "");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(n.manager, PackageManager::Pnpm);
        assert!(n.frozen);
    }

    #[test]
    fn competing_lockfiles_are_reported_not_silently_resolved() {
        let d = dir();
        write(&d, "package.json", "{}");
        write(&d, "pnpm-lock.yaml", "");
        write(&d, "package-lock.json", "{}");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(
            n.conflicting_lockfiles,
            vec!["pnpm-lock.yaml".to_string(), "package-lock.json".to_string()]
        );
        // Still resolves, so the repair stays offerable — pnpm wins on order.
        assert_eq!(n.manager, PackageManager::Pnpm);
    }

    #[test]
    fn bun_lock_formats_are_one_manager_not_a_conflict() {
        let d = dir();
        write(&d, "package.json", "{}");
        write(&d, "bun.lock", "");
        write(&d, "bun.lockb", "");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(n.manager, PackageManager::Bun);
        assert!(n.conflicting_lockfiles.is_empty());
    }

    #[test]
    fn a_dependency_named_like_the_field_is_not_mistaken_for_it() {
        let d = dir();
        write(&d, "package.json", r#"{"devDependencies":{"packageManager":"yarn@4"}}"#);
        write(&d, "pnpm-lock.yaml", "");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(n.manager, PackageManager::Pnpm);
        assert!(!n.pinned);
    }

    #[test]
    fn unparseable_package_json_falls_back_to_lockfile_detection() {
        let d = dir();
        write(&d, "package.json", "{ not json");
        write(&d, "yarn.lock", "");
        let n = inspect_node_project(d.path()).unwrap();
        assert_eq!(n.manager, PackageManager::Yarn);
    }

    #[test]
    fn node_modules_is_detected() {
        let d = dir();
        write(&d, "package.json", "{}");
        std::fs::create_dir(d.path().join("node_modules")).unwrap();
        assert!(inspect_node_project(d.path()).unwrap().has_node_modules);
    }

    #[test]
    fn frozen_installs_use_each_managers_ci_form() {
        assert_eq!(PackageManager::Npm.install_argv(true), ["npm", "ci"]);
        assert_eq!(PackageManager::Npm.install_argv(false), ["npm", "install"]);
        assert_eq!(
            PackageManager::Pnpm.install_argv(true),
            ["pnpm", "install", "--frozen-lockfile"]
        );
        assert_eq!(PackageManager::Yarn.install_argv(true), ["yarn", "install"]);
        assert_eq!(PackageManager::Bun.install_argv(true), ["bun", "install", "--frozen-lockfile"]);
    }
}
