//! Comparing a project's compose services against the stubs its own vendored
//! Sail would generate today.
//!
//! **This is a two-way compare, not a three-way one, and the wording
//! everywhere reflects that.** Nothing records the stub a project was
//! originally generated from — the compose file is copied once at
//! `sail:install` and is the user's from then on — so a difference cannot be
//! attributed to a side. What CAN be said, truthfully, is "this differs from
//! what your installed Sail ships", and that is enough to catch the case
//! that matters: `composer update laravel/sail` pulls a fixed stub, and the
//! compose file that was generated before it never changes.
//!
//! Sequences are compared as multisets. `ports`, `volumes` and `networks`
//! carry no meaning in their order, and reporting a reordering as drift
//! would bury the real findings.

use std::collections::BTreeMap;

use saphyr::{LoadableYamlNode, Scalar, Yaml};

use crate::network::mapping_get;

/// Which side has something the other does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    /// The stub has it; this project does not. The shape an upstream stub
    /// fix takes — laravel/sail#883's second MongoDB volume is exactly this.
    MissingHere,
    /// This project has it; the stub does not. Usually a deliberate local
    /// addition, so never reported as a problem.
    ExtraHere,
    /// Both have it, with different values.
    Differs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubDelta {
    /// Dotted path within the service block, e.g. `volumes` or
    /// `healthcheck.retries`.
    pub path: String,
    pub kind: DeltaKind,
    /// Rendered value on the project's side.
    pub here: Option<String>,
    /// Rendered value in the vendored stub.
    pub stub: Option<String>,
}

/// A stub file is one top-level key (the service name) over its block.
fn stub_block(stub_source: &str) -> Option<Yaml<'static>> {
    let docs = Yaml::load_from_str(stub_source).ok()?;
    let root = docs.into_iter().next()?;
    let Yaml::Mapping(map) = root else { return None };
    map.into_iter().next().map(|(_, block)| block)
}

/// One node as a comparable, displayable string.
fn render(node: &Yaml) -> String {
    match node {
        Yaml::Value(Scalar::String(s)) => s.to_string(),
        Yaml::Value(Scalar::Integer(i)) => i.to_string(),
        Yaml::Value(Scalar::FloatingPoint(f)) => f.to_string(),
        Yaml::Value(Scalar::Boolean(b)) => b.to_string(),
        Yaml::Value(Scalar::Null) => "null".to_string(),
        Yaml::Sequence(items) => {
            let mut rendered: Vec<String> = items.iter().map(render).collect();
            rendered.sort();
            format!("[{}]", rendered.join(", "))
        }
        Yaml::Mapping(map) => {
            let entries: BTreeMap<String, String> =
                map.iter().map(|(k, v)| (render(k), render(v))).collect();
            let joined: Vec<String> =
                entries.into_iter().map(|(k, v)| format!("{k}: {v}")).collect();
            format!("{{{}}}", joined.join(", "))
        }
        _ => String::new(),
    }
}

/// Sequence membership, order-insensitive.
fn multiset(node: &Yaml) -> Option<Vec<String>> {
    let Yaml::Sequence(items) = node else { return None };
    let mut rendered: Vec<String> = items.iter().map(render).collect();
    rendered.sort();
    Some(rendered)
}

fn walk(here: &Yaml, stub: &Yaml, prefix: &str, out: &mut Vec<StubDelta>) {
    let path = |key: &str| {
        if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match (here, stub) {
        (Yaml::Mapping(here_map), Yaml::Mapping(stub_map)) => {
            for (k, stub_value) in stub_map.iter() {
                let Some(key) = (match k {
                    Yaml::Value(Scalar::String(s)) => Some(s.to_string()),
                    _ => None,
                }) else {
                    continue;
                };
                match mapping_get(here, &key) {
                    Some(here_value) => walk(here_value, stub_value, &path(&key), out),
                    None => out.push(StubDelta {
                        path: path(&key),
                        kind: DeltaKind::MissingHere,
                        here: None,
                        stub: Some(render(stub_value)),
                    }),
                }
            }
            for (k, here_value) in here_map.iter() {
                let Some(key) = (match k {
                    Yaml::Value(Scalar::String(s)) => Some(s.to_string()),
                    _ => None,
                }) else {
                    continue;
                };
                if mapping_get(stub, &key).is_none() {
                    out.push(StubDelta {
                        path: path(&key),
                        kind: DeltaKind::ExtraHere,
                        here: Some(render(here_value)),
                        stub: None,
                    });
                }
            }
        }
        // Lists: what matters is which entries exist, not their order. Each
        // side's exclusive members are reported individually so a finding can
        // name the one missing volume rather than dumping both lists.
        (here_seq @ Yaml::Sequence(_), stub_seq @ Yaml::Sequence(_)) => {
            let (mine, theirs) = (multiset(here_seq).unwrap(), multiset(stub_seq).unwrap());
            for entry in theirs.iter().filter(|e| !mine.contains(e)) {
                out.push(StubDelta {
                    path: prefix.to_string(),
                    kind: DeltaKind::MissingHere,
                    here: None,
                    stub: Some(entry.clone()),
                });
            }
            for entry in mine.iter().filter(|e| !theirs.contains(e)) {
                out.push(StubDelta {
                    path: prefix.to_string(),
                    kind: DeltaKind::ExtraHere,
                    here: Some(entry.clone()),
                    stub: None,
                });
            }
        }
        _ => {
            let (mine, theirs) = (render(here), render(stub));
            if mine != theirs {
                out.push(StubDelta {
                    path: prefix.to_string(),
                    kind: DeltaKind::Differs,
                    here: Some(mine),
                    stub: Some(theirs),
                });
            }
        }
    }
}

/// Compare one compose service against the stub of the same name.
///
/// `Ok(vec![])` means the service matches what this Sail would generate.
pub fn compare_service_to_stub(
    compose_source: &str,
    service: &str,
    stub_source: &str,
) -> Result<Vec<StubDelta>, String> {
    let docs = Yaml::load_from_str(compose_source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let here = mapping_get(services, service)
        .ok_or_else(|| format!("service \"{service}\" is not in this file"))?;
    let stub = stub_block(stub_source).ok_or("the stub file has no service block")?;
    let mut out = Vec::new();
    walk(here, &stub, "", &mut out);
    Ok(out)
}

/// The stub name for a compose service, when the vendored Sail ships one.
///
/// Matched on the service KEY, which is how Sail names them when it writes
/// the file. A renamed service is not matched — guessing from the image
/// would pair a hand-rolled `cache` service against Sail's redis stub and
/// report every intentional difference as drift.
pub fn stub_path_for(project_dir: &std::path::Path, service: &str) -> Option<std::path::PathBuf> {
    let path = project_dir
        .join("vendor/laravel/sail/stubs")
        .join(format!("{service}.stub"));
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REDIS_STUB: &str = "redis:\n    image: 'redis:alpine'\n    ports:\n        - '${FORWARD_REDIS_PORT:-6379}:6379'\n    volumes:\n        - 'sail-redis:/data'\n    networks:\n        - sail\n    healthcheck:\n        test: [\"CMD\", \"redis-cli\", \"ping\"]\n        retries: 3\n        timeout: 5s\n";

    fn compose(block: &str) -> String {
        format!("services:\n  redis:\n{block}")
    }

    #[test]
    fn an_untouched_service_reports_nothing() {
        // The stub's own body, re-indented under services: — the case that
        // must be silent, or every project would be noise.
        let body = REDIS_STUB
            .lines()
            .skip(1)
            .map(|l| format!("  {l}\n"))
            .collect::<String>();
        let deltas = compare_service_to_stub(&compose(&body), "redis", REDIS_STUB).unwrap();
        assert!(deltas.is_empty(), "{deltas:#?}");
    }

    #[test]
    fn a_volume_the_stub_gained_is_reported_as_missing_here() {
        // The laravel/sail#883 shape: upstream added a list entry, and a
        // compose file generated earlier never got it.
        let stub = "mongodb:\n    image: 'mongo:8'\n    volumes:\n        - 'sail-mongodb:/data/db'\n        - 'sail-mongodb-config:/data/configdb'\n";
        let mine = "services:\n  mongodb:\n    image: 'mongo:8'\n    volumes:\n      - 'sail-mongodb:/data/db'\n";
        let deltas = compare_service_to_stub(mine, "mongodb", stub).unwrap();
        assert_eq!(deltas.len(), 1, "{deltas:#?}");
        assert_eq!(deltas[0].kind, DeltaKind::MissingHere);
        assert_eq!(deltas[0].path, "volumes");
        assert_eq!(deltas[0].stub.as_deref(), Some("sail-mongodb-config:/data/configdb"));
    }

    #[test]
    fn a_changed_env_key_is_reported_on_both_sides() {
        // laravel/sail#874: the key was renamed, so it reads as one missing
        // and one extra rather than a value change.
        let stub = "rabbitmq:\n    environment:\n        RABBITMQ_DEFAULT_USER: '${RABBITMQ_USER}'\n";
        let mine = "services:\n  rabbitmq:\n    environment:\n      RABBITMQ_USER: '${RABBITMQ_USER}'\n";
        let deltas = compare_service_to_stub(mine, "rabbitmq", stub).unwrap();
        assert_eq!(deltas.len(), 2, "{deltas:#?}");
        assert!(deltas.iter().any(|d| d.kind == DeltaKind::MissingHere
            && d.path == "environment.RABBITMQ_DEFAULT_USER"));
        assert!(
            deltas
                .iter()
                .any(|d| d.kind == DeltaKind::ExtraHere && d.path == "environment.RABBITMQ_USER")
        );
    }

    #[test]
    fn reordering_a_list_is_not_drift() {
        let stub = "redis:\n    networks:\n        - sail\n        - shared\n";
        let mine = "services:\n  redis:\n    networks:\n      - shared\n      - sail\n";
        assert!(compare_service_to_stub(mine, "redis", stub).unwrap().is_empty());
    }

    #[test]
    fn a_retagged_image_is_a_difference_not_a_gap() {
        let stub = "redis:\n    image: 'redis:alpine'\n";
        let mine = "services:\n  redis:\n    image: 'redis:7.2'\n";
        let deltas = compare_service_to_stub(mine, "redis", stub).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].kind, DeltaKind::Differs);
        assert_eq!(deltas[0].here.as_deref(), Some("redis:7.2"));
        assert_eq!(deltas[0].stub.as_deref(), Some("redis:alpine"));
    }

    #[test]
    fn local_additions_are_reported_but_kept_distinct() {
        let stub = "redis:\n    image: 'redis:alpine'\n";
        let mine =
            "services:\n  redis:\n    image: 'redis:alpine'\n    restart: unless-stopped\n";
        let deltas = compare_service_to_stub(mine, "redis", stub).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].kind, DeltaKind::ExtraHere);
        assert_eq!(deltas[0].path, "restart");
    }

    #[test]
    fn nested_maps_report_the_leaf_that_moved() {
        let stub = "redis:\n    healthcheck:\n        retries: 3\n        timeout: 5s\n";
        let mine = "services:\n  redis:\n    healthcheck:\n      retries: 10\n      timeout: 5s\n";
        let deltas = compare_service_to_stub(mine, "redis", stub).unwrap();
        assert_eq!(deltas.len(), 1, "{deltas:#?}");
        assert_eq!(deltas[0].path, "healthcheck.retries");
        assert_eq!(deltas[0].here.as_deref(), Some("10"));
    }

    #[test]
    fn a_service_or_stub_that_is_not_there_is_an_error_not_a_panic() {
        assert!(compare_service_to_stub("services:\n  redis: {}\n", "mysql", REDIS_STUB).is_err());
        assert!(compare_service_to_stub("{}", "redis", REDIS_STUB).is_err());
        assert!(compare_service_to_stub(&compose("    image: x\n"), "redis", "").is_err());
    }
}

