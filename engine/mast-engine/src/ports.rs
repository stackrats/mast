//! Host-port conflict avoidance: when something else already holds a port a
//! project is about to publish, move the project's port instead of letting
//! `up` fail with `bind: address already in use`.
//!
//! Split the usual way — [`plan_remap`] is pure over gathered facts (so the
//! whole decision is unit-testable), while probing the host and writing
//! `.env` are effects the engine performs around it. The write goes through
//! the same transactional env writer every other `.env` mutation uses, which
//! means a backup and the external-edit guard come for free.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use mast_contract::{ContainerState, ErrorInfo, ProjectId};
use mast_laravel::{is_host_port_key, next_free_port};

/// One port move: `key` in `.env` goes from `from` to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortRemap {
    pub key: String,
    pub from: u16,
    pub to: u16,
}

/// What the planner needs to know about the project and its neighbours.
#[derive(Default)]
pub(crate) struct RemapFacts {
    /// Every host port this project publishes, labelled with the `.env` key
    /// that moves it (or a service name when no key does).
    pub host_ports: Vec<(String, u16)>,
    /// Ports this project's own containers are already holding — a running
    /// stack is not in conflict with itself.
    pub self_held: BTreeSet<u16>,
    /// Ports other known projects declare. Free right now (they are stopped),
    /// but stepping onto one only moves the collision to their next start.
    pub reserved: BTreeSet<u16>,
}

/// What counts as a conflict worth moving away from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemapMode {
    /// Something holds the port right now. This is the start-time rule: a
    /// neighbour that merely *declares* the same port is not in the way
    /// while it is stopped, and moving ports out from under a user who never
    /// hits the collision would be meddling.
    Bound,
    /// Bound, or claimed by another project. This is the diagnostics rule:
    /// the report already told the user these two can never run together,
    /// and the repair is them asking for it to be settled now.
    BoundOrClaimed,
}

/// Decide which ports to move. Returns the moves plus notes about conflicts
/// that were found but cannot be fixed this way — a caller reports both.
pub(crate) fn plan_remap(
    facts: &RemapFacts,
    mode: RemapMode,
    is_bound: impl Fn(u16) -> bool,
) -> (Vec<PortRemap>, Vec<String>) {
    let mut remaps: Vec<PortRemap> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    // Nothing may land on a port this project already publishes, on one a
    // neighbour claims, or on one an earlier move in this same pass took.
    let mut taken: BTreeSet<u16> = facts.reserved.clone();
    taken.extend(facts.host_ports.iter().map(|(_, port)| *port));

    let conflicts = |port: u16| {
        is_bound(port)
            || (mode == RemapMode::BoundOrClaimed && facts.reserved.contains(&port))
    };
    for (label, port) in &facts.host_ports {
        if facts.self_held.contains(port) || !conflicts(*port) {
            continue;
        }
        if !is_host_port_key(label) {
            notes.push(format!(
                "port {port} is taken and nothing in .env moves it — it is published directly by \
                 the {label} service"
            ));
            continue;
        }
        match next_free_port(*port, |p| !taken.contains(&p) && !is_bound(p)) {
            Some(to) => {
                taken.insert(to);
                remaps.push(PortRemap { key: label.clone(), from: *port, to });
            }
            None => notes.push(format!("port {port} is taken and no free port was found near it")),
        }
    }
    (remaps, notes)
}

/// Is something listening here right now?
///
/// Both the wildcard and the loopback address are tried: a container
/// published as `127.0.0.1:8080:80` binds only the latter, and on BSD-derived
/// stacks (macOS) a wildcard bind can succeed alongside it.
pub(crate) fn port_is_bound(port: u16) -> bool {
    for addr in [Ipv4Addr::UNSPECIFIED, Ipv4Addr::LOCALHOST] {
        match TcpListener::bind(SocketAddr::from((addr, port))) {
            Ok(listener) => drop(listener),
            // Anything else (a permission error on a privileged port, say)
            // is not evidence of a conflict, and guessing would move a port
            // that was fine.
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return true,
            Err(_) => {}
        }
    }
    false
}

/// Ports a project's own containers are holding: only states that keep the
/// daemon's port binding alive count. An exited container has released it.
fn held_by(state: Option<ContainerState>) -> bool {
    matches!(
        state,
        Some(ContainerState::Running | ContainerState::Restarting | ContainerState::Paused)
    )
}

impl crate::Engine {
    /// Gather the facts [`plan_remap`] needs from live state.
    ///
    /// Only ports the resolved model actually publishes are considered. A key
    /// the compose file never reads cannot collide with anything, and
    /// rewriting it would still have consequences — `APP_PORT` is where the
    /// browsable URL comes from, so moving it for nothing would point the
    /// Browser button at a port no one serves. No model means no evidence,
    /// which means hands off.
    fn remap_facts(&self, project: &ProjectId) -> Result<RemapFacts, ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let Some(model) = entry.model.as_ref() else { return Ok(RemapFacts::default()) };
        let published: BTreeSet<u16> =
            model.services.iter().flat_map(|s| s.published_ports.iter().copied()).collect();
        let holding: BTreeSet<&str> = entry
            .summary
            .services
            .iter()
            .filter(|s| held_by(s.state))
            .map(|s| s.name.as_str())
            .collect();
        let self_held = model
            .services
            .iter()
            .filter(|s| holding.contains(s.name.as_str()))
            .flat_map(|s| s.published_ports.iter().copied())
            .collect();
        let reserved = st
            .projects
            .values()
            .filter(|e| e.record.id != project.0)
            .flat_map(|e| e.host_ports.iter().map(|(_, port)| *port))
            .collect();
        let host_ports = entry
            .host_ports
            .iter()
            .filter(|(_, port)| published.contains(port))
            .cloned()
            .collect();
        Ok(RemapFacts { host_ports, self_held, reserved })
    }

    /// Plan the moves and render the `.env` they would produce, without
    /// touching the file — the diagnostics repair previews with this.
    pub(crate) async fn preview_port_remap(
        &self,
        project: &ProjectId,
        mode: RemapMode,
    ) -> Result<(Vec<PortRemap>, Vec<String>, String, String), ErrorInfo> {
        let facts = self.remap_facts(project)?;
        let path = self.project_path(project)?.join(".env");
        tokio::task::spawn_blocking(move || {
            let (remaps, notes) = plan_remap(&facts, mode, port_is_bound);
            let before = std::fs::read_to_string(&path).unwrap_or_default();
            let mut file = mast_laravel::EnvFile::parse(&before);
            for remap in &remaps {
                file.set(&remap.key, &remap.to.to_string())
                    .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
            }
            Ok((remaps, notes, before, file.to_string()))
        })
        .await
        .map_err(crate::internal_err)?
    }

    /// Move any host port that is in the way, writing the new values to
    /// `.env`. Returns the moves that were applied and any notes worth
    /// showing; an empty result means nothing was in the way.
    pub(crate) async fn remap_conflicting_ports(
        &self,
        project: &ProjectId,
        mode: RemapMode,
    ) -> Result<(Vec<PortRemap>, Vec<String>), ErrorInfo> {
        let facts = self.remap_facts(project)?;
        if facts.host_ports.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let path = self.project_path(project)?.join(".env");
        let backups = self.inner.deps.store.backups_dir();

        let (remaps, notes) = tokio::task::spawn_blocking(move || {
            let (remaps, notes) = plan_remap(&facts, mode, port_is_bound);
            if remaps.is_empty() {
                return Ok((remaps, notes));
            }
            mast_laravel::edit_env_file(&path, Some(&backups), |f| {
                for remap in &remaps {
                    f.set(&remap.key, &remap.to.to_string())?;
                }
                Ok(())
            })
            .map(|_| (remaps, notes))
        })
        .await
        .map_err(crate::internal_err)?
        .map_err(crate::env_write_error)?;

        if !remaps.is_empty() {
            // The reconcile re-reads `.env`: app URL, probe port and the
            // collision warnings all follow from it.
            self.hint();
        }
        Ok((remaps, notes))
    }

    /// Start-time hook: clear the way, then say what was done in the
    /// operation's output, where the user is already looking.
    ///
    /// Nothing here fails the start. A port that could not be moved still has
    /// its old value, so compose gets to report the bind error itself — which
    /// is exactly the behaviour Mast had before this existed.
    ///
    /// `prefix` labels the output lines, so a workspace start can attribute
    /// them to the member they belong to.
    pub(crate) async fn preflight_ports(
        &self,
        handle: &std::sync::Arc<crate::OpHandle>,
        op: crate::OperationId,
        project: &ProjectId,
        prefix: &str,
    ) {
        if !self.inner.state.lock().unwrap().integrations.auto_port_remap {
            return;
        }
        let say = |line: String, stderr: bool| {
            self.emit_op(
                handle,
                op,
                crate::OperationEventKind::Output { line: format!("{prefix}{line}"), stderr },
            );
        };
        match self.remap_conflicting_ports(project, RemapMode::Bound).await {
            Ok((remaps, notes)) => {
                for remap in &remaps {
                    say(
                        format!(
                            "port {} is already in use — moved {} to {} in .env",
                            remap.from, remap.key, remap.to
                        ),
                        false,
                    );
                }
                for note in &notes {
                    say(note.clone(), true);
                }
            }
            Err(e) => say(format!("could not check host ports: {e:?}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(host_ports: &[(&str, u16)]) -> RemapFacts {
        RemapFacts {
            host_ports: host_ports.iter().map(|(k, p)| ((*k).to_string(), *p)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_free_stack_is_left_alone() {
        let (remaps, notes) = plan_remap(&facts(&[("APP_PORT", 80)]), RemapMode::Bound, |_| false);
        assert!(remaps.is_empty() && notes.is_empty());
    }

    #[test]
    fn a_busy_port_moves_and_the_rest_stay() {
        let f = facts(&[("APP_PORT", 80), ("FORWARD_DB_PORT", 3306)]);
        let (remaps, _) = plan_remap(&f, RemapMode::Bound, |p| p == 80);
        assert_eq!(remaps, vec![PortRemap { key: "APP_PORT".into(), from: 80, to: 8080 }]);
    }

    #[test]
    fn the_projects_own_running_containers_are_not_a_conflict() {
        let mut f = facts(&[("APP_PORT", 80)]);
        f.self_held = [80].into();
        // Bound — by this very project. Restarting it must not move it.
        let (remaps, notes) = plan_remap(&f, RemapMode::Bound, |_| true);
        assert!(remaps.is_empty() && notes.is_empty());
    }

    #[test]
    fn a_new_port_dodges_neighbours_and_earlier_moves() {
        let mut f = facts(&[("APP_PORT", 80), ("VITE_PORT", 5173)]);
        // A stopped neighbour already claims 8080, and this project itself
        // publishes 8081 elsewhere.
        f.reserved = [8080].into();
        f.host_ports.push(("FORWARD_DB_PORT".into(), 8081));
        let (remaps, _) = plan_remap(&f, RemapMode::Bound, |p| p == 80 || p == 5173);
        assert_eq!(
            remaps,
            vec![
                PortRemap { key: "APP_PORT".into(), from: 80, to: 8082 },
                PortRemap { key: "VITE_PORT".into(), from: 5173, to: 5174 },
            ]
        );
    }

    #[test]
    fn only_the_repair_mode_moves_a_port_a_stopped_neighbour_claims() {
        let mut f = facts(&[("APP_PORT", 80)]);
        f.reserved = [80].into();
        // Nothing is listening: starting now would work, so a start leaves it.
        assert!(plan_remap(&f, RemapMode::Bound, |_| false).0.is_empty());
        // Asked directly, settle the declared conflict.
        let (remaps, _) = plan_remap(&f, RemapMode::BoundOrClaimed, |_| false);
        assert_eq!(remaps, vec![PortRemap { key: "APP_PORT".into(), from: 80, to: 8080 }]);
    }

    #[test]
    fn a_port_with_no_key_is_reported_not_moved() {
        let (remaps, notes) = plan_remap(&facts(&[("minio", 9000)]), RemapMode::Bound, |_| true);
        assert!(remaps.is_empty());
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("nothing in .env moves it"), "{notes:?}");
    }

    #[test]
    fn binding_detects_a_real_listener() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_is_bound(port));
        drop(listener);
        assert!(!port_is_bound(port));
    }
}
