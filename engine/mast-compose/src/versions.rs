//! Selectable image versions for a compose service.
//!
//! Changing "the version of mysql" is really rewriting one scalar — the tag on
//! a service's `image:` — so the offer is keyed by the repo the service
//! already uses, not by which catalog entry it resembles. A project scaffolded
//! by Sail runs `mysql:8.4` while [`crate::catalog`] installs
//! `mysql/mysql-server:8.0`; those are different repos with different tag
//! namespaces, and offering one's tags for the other would produce an image
//! that cannot be pulled.
//!
//! The real answer comes from the registry (`mast-registry`, cached by the
//! engine). The table below is the offline fallback only: what to show before
//! the first successful fetch, or on a machine with no network. It is
//! therefore allowed to go stale — a hand-written list always does, which is
//! why it is no longer the source of truth — but it must stay *pullable*, so
//! every tag here was real when written.

/// Tags offered for one image repo, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageVersions {
    /// Repo as it appears in the compose `image:` value, sans tag.
    pub repo: &'static str,
    pub tags: &'static [&'static str],
}

pub const IMAGE_VERSIONS: &[ImageVersions] = &[
    ImageVersions { repo: "mysql", tags: &["9.7", "8.4", "8.0"] },
    ImageVersions { repo: "mysql/mysql-server", tags: &["8.0", "5.7"] },
    ImageVersions { repo: "mariadb", tags: &["12", "11", "10.11"] },
    ImageVersions { repo: "postgres", tags: &["18", "17", "16", "15"] },
    ImageVersions { repo: "redis", tags: &["8", "7", "alpine"] },
    ImageVersions { repo: "typesense/typesense", tags: &["30.0", "29.0", "27.1"] },
];

/// Split a compose image value into `(repo, tag)`.
///
/// A colon can also introduce a registry port (`localhost:5000/redis`), so a
/// candidate tag containing `/` is part of the repo instead.
pub fn split_image(image: &str) -> (&str, Option<&str>) {
    match image.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => (repo, Some(tag)),
        _ => (image, None),
    }
}

/// Strip any registry host so `ghcr.io/library/redis` matches `redis`.
fn repo_key(repo: &str) -> &str {
    for entry in IMAGE_VERSIONS {
        if repo == entry.repo || repo.ends_with(&format!("/{}", entry.repo)) {
            return entry.repo;
        }
    }
    repo
}

/// Fallback tags for `image`, or empty when the repo is not one we pin.
/// Callers should prefer cached registry data and fall back to this.
pub fn versions_for(image: &str) -> &'static [&'static str] {
    let (repo, _) = split_image(image);
    let key = repo_key(repo);
    IMAGE_VERSIONS
        .iter()
        .find(|entry| entry.repo == key)
        .map(|entry| entry.tags)
        .unwrap_or(&[])
}

/// `image` with its tag replaced by `tag`, keeping registry and repo intact.
pub fn with_tag(image: &str, tag: &str) -> String {
    let (repo, _) = split_image(image);
    format!("{repo}:{tag}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_repo_and_tag_without_eating_a_registry_port() {
        assert_eq!(split_image("redis:alpine"), ("redis", Some("alpine")));
        assert_eq!(split_image("mysql/mysql-server:8.0"), ("mysql/mysql-server", Some("8.0")));
        assert_eq!(split_image("redis"), ("redis", None));
        // The colon here is a port, not a tag.
        assert_eq!(split_image("localhost:5000/redis"), ("localhost:5000/redis", None));
        assert_eq!(
            split_image("localhost:5000/redis:7"),
            ("localhost:5000/redis", Some("7"))
        );
    }

    #[test]
    fn versions_follow_the_repo_the_service_actually_uses() {
        // Sail's mysql and the catalog's mysql are different repos.
        assert_eq!(versions_for("mysql:8.4"), ["9.7", "8.4", "8.0"]);
        assert_eq!(versions_for("mysql/mysql-server:8.0"), ["8.0", "5.7"]);
        // Registry-qualified still resolves.
        assert_eq!(versions_for("docker.io/mariadb:11"), ["12", "11", "10.11"]);
        // Unpinned repos offer nothing rather than a wrong list.
        assert!(versions_for("axllent/mailpit:latest").is_empty());
        assert!(versions_for("some/unknown:1").is_empty());
    }

    #[test]
    fn with_tag_preserves_registry_and_repo() {
        assert_eq!(with_tag("mysql:8.0", "8.4"), "mysql:8.4");
        assert_eq!(with_tag("mysql/mysql-server:8.0", "5.7"), "mysql/mysql-server:5.7");
        assert_eq!(with_tag("localhost:5000/redis:7", "8"), "localhost:5000/redis:8");
        assert_eq!(with_tag("redis", "8"), "redis:8");
    }

    /// The tag a catalog entry installs must be one the fallback can offer
    /// back, otherwise an offline dropdown opens on a value absent from its
    /// own list. (Online this is also guaranteed independently, by passing the
    /// installed tag through as a must-include.)
    #[test]
    fn catalog_default_tags_are_offered() {
        for def in crate::catalog::CATALOG {
            let Some(image) = crate::catalog::def_image(def) else {
                continue;
            };
            let offered = versions_for(image);
            if offered.is_empty() {
                continue; // not a pinned repo — no dropdown at all
            }
            let (_, tag) = split_image(image);
            let tag = tag.expect("a pinned repo must carry a tag");
            assert!(
                offered.contains(&tag),
                "{} installs {image} but {tag} is not offered ({offered:?})",
                def.id
            );
        }
    }
}
