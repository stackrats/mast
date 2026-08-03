//! Shared-workspace-network attachment (plan §7): joining a project to the
//! `mast-{slug}` network is an explicit, previewable compose edit — planned
//! here as mast-yaml-edit operations, applied through the write transaction.
//!
//! Semantics per service: a service with no `networks:` key is implicitly on
//! `default`, so attaching must add BOTH `default` and the shared network
//! (otherwise the edit would silently disconnect it). Services with an
//! explicit list get the network appended; map-form gets a new key. The
//! shared network itself is declared `external: true` — Mast creates it at
//! workspace start, compose never owns it.

use mast_yaml_edit::{Edit, key};
use saphyr::{LoadableYamlNode, Scalar, Yaml};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAttachPlan {
    pub edits: Vec<Edit>,
    pub attached_services: Vec<String>,
    pub already_attached: Vec<String>,
    /// (service, reason) pairs we refused to touch (e.g. alias-valued
    /// networks) — surfaced in the preview, never silently skipped.
    pub skipped: Vec<(String, String)>,
}

fn as_str<'a>(y: &'a Yaml<'_>) -> Option<&'a str> {
    match y {
        Yaml::Value(Scalar::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

pub(crate) fn mapping_get<'a, 'i>(y: &'a Yaml<'i>, wanted: &str) -> Option<&'a Yaml<'i>> {
    if let Yaml::Mapping(map) = y {
        for (k, v) in map.iter() {
            if as_str(k) == Some(wanted) {
                return Some(v);
            }
        }
    }
    None
}

/// Plan the edits that attach every service in `source` to `network`.
pub fn plan_network_attach(source: &str, network: &str) -> Result<NetworkAttachPlan, String> {
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let Yaml::Mapping(service_map) = services else {
        return Err("services is not a mapping".into());
    };

    let mut plan = NetworkAttachPlan {
        edits: Vec::new(),
        attached_services: Vec::new(),
        already_attached: Vec::new(),
        skipped: Vec::new(),
    };

    for (service_key, service_def) in service_map.iter() {
        let Some(name) = as_str(service_key) else {
            continue;
        };
        match mapping_get(service_def, "networks") {
            None => {
                plan.edits.push(Edit::InsertMapKey {
                    path: vec![key("services"), key(name)],
                    key: "networks".into(),
                    value: format!("[default, {network}]"),
                });
                plan.attached_services.push(name.to_string());
            }
            Some(Yaml::Sequence(items)) => {
                if items.iter().any(|i| as_str(i) == Some(network)) {
                    plan.already_attached.push(name.to_string());
                } else {
                    plan.edits.push(Edit::InsertSeqItem {
                        path: vec![key("services"), key(name), key("networks")],
                        value: network.to_string(),
                    });
                    plan.attached_services.push(name.to_string());
                }
            }
            Some(Yaml::Mapping(map)) => {
                if map.iter().any(|(k, _)| as_str(k) == Some(network)) {
                    plan.already_attached.push(name.to_string());
                } else {
                    plan.edits.push(Edit::InsertMapKey {
                        path: vec![key("services"), key(name), key("networks")],
                        key: network.into(),
                        value: "{}".into(),
                    });
                    plan.attached_services.push(name.to_string());
                }
            }
            Some(_) => {
                plan.skipped.push((
                    name.to_string(),
                    "networks uses an unsupported form (anchor/alias?) — edit manually".into(),
                ));
            }
        }
    }

    // Declare the external network at top level (once).
    if !plan.attached_services.is_empty() {
        match mapping_get(root, "networks") {
            None => plan.edits.push(Edit::InsertMapKey {
                path: vec![],
                key: "networks".into(),
                value: format!("{{ {network}: {{ external: true, name: {network} }} }}"),
            }),
            Some(map) if mapping_get(map, network).is_none() => {
                plan.edits.push(Edit::InsertMapKey {
                    path: vec![key("networks")],
                    key: network.into(),
                    value: format!("{{ external: true, name: {network} }}"),
                });
            }
            Some(_) => {}
        }
    }

    Ok(plan)
}

/// `mast-{slug}` network name for a workspace.
pub fn workspace_network_name(workspace_name: &str) -> String {
    format!("mast-{}", crate::normalize_project_name(workspace_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attaches_all_service_shapes() {
        let source = "services:\n  bare:\n    image: a\n  listed:\n    image: b\n    networks:\n      - sail\n  mapped:\n    image: c\n    networks:\n      internal: {}\n  done:\n    image: d\n    networks:\n      - mast-suite\nnetworks:\n  sail:\n    driver: bridge\n";
        let plan = plan_network_attach(source, "mast-suite").unwrap();
        assert_eq!(plan.attached_services, vec!["bare", "listed", "mapped"]);
        assert_eq!(plan.already_attached, vec!["done"]);
        assert!(plan.skipped.is_empty());

        let after = mast_yaml_edit::apply_all(source, &plan.edits).unwrap();
        // Bare service keeps default connectivity AND joins the network.
        assert!(after.contains("networks: [default, mast-suite]"));
        // Listed service gets an appended item; original items survive.
        assert!(after.contains("- sail\n      - mast-suite"));
        // Map-form gains a key.
        assert!(after.contains("mast-suite: {}"));
        // Top-level external declaration appended to the existing mapping.
        assert!(after.contains("mast-suite: { external: true, name: mast-suite }"));
        // Idempotence: replanning the result finds nothing to do.
        let replan = plan_network_attach(&after, "mast-suite").unwrap();
        assert!(replan.edits.is_empty(), "{replan:?}");
        assert_eq!(replan.already_attached.len(), 4);
    }

    #[test]
    fn creates_top_level_networks_when_absent() {
        let source = "services:\n  app:\n    image: x\n";
        let plan = plan_network_attach(source, "mast-w").unwrap();
        let after = mast_yaml_edit::apply_all(source, &plan.edits).unwrap();
        assert!(after.contains("networks: { mast-w: { external: true, name: mast-w } }"));
    }

    #[test]
    fn slug_and_errors() {
        assert_eq!(workspace_network_name("My Stuff!"), "mast-mystuff");
        assert!(plan_network_attach("volumes: {}\n", "n").unwrap_err().contains("no services"));
    }
}
