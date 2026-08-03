//! Workspace orchestration primitives (plan §7): hand-rolled layered
//! topological sort over the workspace `dependsOn` graph. Cycles are a
//! diagnostic, not a panic.

use std::collections::{BTreeMap, BTreeSet};

/// Layered topological order: members of one layer have all their
/// dependencies satisfied by earlier layers. Deterministic (BTree ordering).
pub fn topo_layers(
    members: &[(String, Vec<String>)],
) -> Result<Vec<Vec<String>>, String> {
    let ids: BTreeSet<&str> = members.iter().map(|(id, _)| id.as_str()).collect();
    let mut remaining: BTreeMap<&str, BTreeSet<&str>> = members
        .iter()
        .map(|(id, deps)| {
            // Dependencies outside the workspace are ignored (they may be
            // standalone projects the user starts separately).
            let deps: BTreeSet<&str> = deps
                .iter()
                .map(|d| d.as_str())
                .filter(|d| ids.contains(d) && *d != id.as_str())
                .collect();
            (id.as_str(), deps)
        })
        .collect();

    let mut layers: Vec<Vec<String>> = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = remaining.keys().copied().collect();
            return Err(format!(
                "dependency cycle involving: {}",
                stuck.join(", ")
            ));
        }
        for id in &ready {
            remaining.remove(id);
        }
        for deps in remaining.values_mut() {
            for id in &ready {
                deps.remove(id);
            }
        }
        layers.push(ready.into_iter().map(String::from).collect());
    }
    Ok(layers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(id, deps)| {
                (id.to_string(), deps.iter().map(|d| d.to_string()).collect())
            })
            .collect()
    }

    #[test]
    fn layers_respect_dependencies() {
        let layers = topo_layers(&m(&[
            ("api", &["db", "cache"]),
            ("db", &[]),
            ("cache", &[]),
            ("worker", &["api"]),
        ]))
        .unwrap();
        assert_eq!(layers, vec![
            vec!["cache".to_string(), "db".to_string()],
            vec!["api".to_string()],
            vec!["worker".to_string()],
        ]);
    }

    #[test]
    fn cycle_is_a_diagnostic() {
        let err = topo_layers(&m(&[("a", &["b"]), ("b", &["a"]), ("c", &[])])).unwrap_err();
        assert!(err.contains("cycle"));
        assert!(err.contains('a') && err.contains('b'));
    }

    #[test]
    fn external_and_self_deps_are_ignored() {
        let layers =
            topo_layers(&m(&[("a", &["not-a-member", "a"]), ("b", &["a"])])).unwrap();
        assert_eq!(layers, vec![vec!["a".to_string()], vec!["b".to_string()]]);
    }
}
