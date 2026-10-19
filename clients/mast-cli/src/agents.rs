//! `mast agents` and the `mast_agents` MCP tool: what coding agents are
//! doing around each project (ADR-0012) — their sessions, what runs on this
//! machine and in the app container that Mast did not start, and the
//! worktrees they work in. One text rendering for both, so a person and an
//! agent read the same answer.

use mast_contract::{ContainerRunOwner, HostRunOwner, ProjectSummary};

/// Two significant figures were already applied by the engine; this only
/// picks the unit.
pub(crate) fn bytes(n: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1_000_000_000), ("MB", 1_000_000), ("KB", 1_000)];
    for (unit, size) in UNITS {
        if n >= size {
            let value = n as f64 / size as f64;
            return if value >= 10.0 {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
    }
    format!("{n} B")
}

/// `5m`, `3h`, `2d`: how long something has been running.
pub(crate) fn up(started_at_unix: u64, now_unix: u64) -> String {
    let secs = now_unix.saturating_sub(started_at_unix);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A worktree or the checkout, as a place: "worktree fix-login" or "the
/// checkout".
fn place(project: &ProjectSummary, worktree: Option<&str>) -> String {
    match worktree {
        None => "the checkout".to_string(),
        Some(path) => project
            .worktrees
            .iter()
            .find(|w| w.path == path)
            .map(|w| format!("worktree {}", w.name))
            .unwrap_or_else(|| format!("worktree {path}")),
    }
}

/// Whether a project has anything worth reporting.
pub(crate) fn has_activity(project: &ProjectSummary) -> bool {
    !project.agent_sessions.is_empty()
        || !project.host_runs.is_empty()
        || !project.worktrees.is_empty()
        || project.container_runs.iter().any(|r| !matches!(r.owner, ContainerRunOwner::Mast { .. }))
}

/// One project's block.
pub(crate) fn project_report(project: &ProjectSummary, now_unix: u64) -> Vec<String> {
    let mut lines = vec![project.name.clone()];
    if !project.agent_sessions.is_empty() {
        lines.push("  agent sessions".into());
        for session in &project.agent_sessions {
            lines.push(format!(
                "    {} (pid {}) in {}, up {}",
                session.agent,
                session.pid,
                place(project, session.worktree.as_deref()),
                up(session.started_at_unix, now_unix)
            ));
        }
    }
    if !project.host_runs.is_empty() {
        lines.push("  running on this machine".into());
        for run in &project.host_runs {
            let owner = match &run.owner {
                HostRunOwner::Agent { agent, pid } => format!("started by {agent} (pid {pid})"),
                HostRunOwner::Orphaned => "orphaned — whatever started it has exited".to_string(),
            };
            let gone = if run.cwd_deleted { "; its directory was deleted" } else { "" };
            lines.push(format!(
                "    {} [{}] in {} — {owner}{gone}; {}, {}, up {} (pid {})",
                run.title,
                run.kinds.join(", "),
                place(project, run.worktree.as_deref()),
                plural(run.process_count, "process", "processes"),
                bytes(run.memory_bytes),
                up(run.started_at_unix, now_unix),
                run.pid
            ));
        }
    }
    let foreign: Vec<_> = project
        .container_runs
        .iter()
        .filter(|r| !matches!(r.owner, ContainerRunOwner::Mast { .. }))
        .collect();
    if !foreign.is_empty() {
        lines.push("  in the app container, not started by Mast".into());
        for run in foreign {
            let owner = match &run.owner {
                ContainerRunOwner::EarlierMast { command } => {
                    format!("{command}, left by an earlier Mast session")
                }
                ContainerRunOwner::Outside => "started outside Mast".to_string(),
                _ => "orphaned".to_string(),
            };
            lines.push(format!(
                "    {} — {owner}; {}, {}, up {}",
                run.title,
                plural(run.process_count, "process", "processes"),
                bytes(run.memory_bytes),
                up(run.started_at_unix, now_unix)
            ));
        }
    }
    if !project.worktrees.is_empty() {
        lines.push("  worktrees".into());
        for worktree in &project.worktrees {
            let mut facts = Vec::new();
            match (&worktree.branch, &worktree.head) {
                (Some(branch), _) => facts.push(format!("branch {branch}")),
                (None, Some(head)) => facts.push(format!("detached at {head}")),
                _ => {}
            }
            if let Some(tool) = &worktree.tool {
                facts.push(format!("where {tool} makes them"));
            }
            if worktree.missing {
                facts.push("its directory is gone (pruning forgets it)".into());
            }
            if worktree.locked {
                facts.push("locked".into());
            }
            if let Some(changed) = worktree.changed_files.filter(|n| *n > 0) {
                facts.push(plural(changed, "uncommitted change", "uncommitted changes"));
            }
            if let Some(commits) = worktree.unmerged_commits.filter(|n| *n > 0) {
                facts.push(format!("{} the checkout does not have", plural(commits, "commit", "commits")));
            }
            if !worktree.containers.is_empty() {
                let running = worktree.containers.iter().filter(|c| c.running).count();
                let mut ports: Vec<u16> =
                    worktree.containers.iter().flat_map(|c| c.ports.iter().copied()).collect();
                ports.sort_unstable();
                ports.dedup();
                let ports = if ports.is_empty() {
                    String::new()
                } else {
                    format!(
                        ", ports {}",
                        ports.iter().map(u16::to_string).collect::<Vec<_>>().join(" ")
                    )
                };
                facts.push(format!(
                    "its own containers: {running}/{} running{ports}",
                    worktree.containers.len()
                ));
            }
            if let Some(at) = worktree.created_at_unix {
                facts.push(format!("made {} ago", up(at, now_unix)));
            }
            lines.push(format!("    {} ({})", worktree.name, worktree.path));
            if !facts.is_empty() {
                lines.push(format!("      {}", facts.join("; ")));
            }
        }
    }
    if lines.len() == 1 {
        lines.push("  no agents, worktrees or leftovers".into());
    }
    lines
}

/// The whole report: the projects with something to say, and one line for
/// the rest.
pub(crate) fn report(projects: &[ProjectSummary], now_unix: u64) -> String {
    let (busy, quiet): (Vec<&ProjectSummary>, Vec<&ProjectSummary>) =
        projects.iter().partition(|p| has_activity(p));
    let mut blocks: Vec<String> =
        busy.iter().map(|p| project_report(p, now_unix).join("\n")).collect();
    if !quiet.is_empty() {
        let names: Vec<&str> = quiet.iter().map(|p| p.name.as_str()).collect();
        blocks.push(format!("nothing from agents in {}", names.join(", ")));
    }
    if !cfg!(target_os = "linux") {
        blocks.push(
            "processes on this machine are watched on Linux only; worktrees and their \
             containers are listed everywhere"
                .into(),
        );
    }
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_contract::{AgentSession, HostRun, ProjectId, ProjectStatus, Worktree};

    fn project() -> ProjectSummary {
        ProjectSummary {
            id: ProjectId("p1".into()),
            name: "shop".into(),
            path: "/home/dev/shop".into(),
            status: ProjectStatus::Running,
            compose_project_name: Some("shop".into()),
            is_sail: true,
            services: Vec::new(),
            resolution_error: None,
            commands: Vec::new(),
            processes: Vec::new(),
            git_branch: None,
            git_dirty: None,
            app_url: None,
            php: None,
            share_url: None,
            share_dashboard_url: None,
            local_domain: None,
            command_domains: Vec::new(),
            command_ports: Vec::new(),
            warnings: Vec::new(),
            container_runs: Vec::new(),
            advisories: Vec::new(),
            worktrees: Vec::new(),
            agent_sessions: Vec::new(),
            host_runs: Vec::new(),
            rank: 0,
        }
    }

    #[test]
    fn a_project_with_agents_and_leftovers_reads_as_one_block() {
        let mut p = project();
        let wt = "/home/dev/shop/.claude/worktrees/fix-login".to_string();
        p.worktrees.push(Worktree {
            path: wt.clone(),
            name: "fix-login".into(),
            branch: Some("worktree-fix-login".into()),
            head: Some("8ba4734".into()),
            tool: Some("Claude Code".into()),
            created_at_unix: Some(1_000_000 - 7200),
            missing: false,
            locked: false,
            changed_files: Some(3),
            unmerged_commits: Some(1),
            containers: Vec::new(),
        });
        p.agent_sessions.push(AgentSession {
            agent: "Claude Code".into(),
            pid: 42,
            start_ticks: 1,
            cwd: wt.clone(),
            worktree: Some(wt.clone()),
            started_at_unix: 1_000_000 - 600,
        });
        p.host_runs.push(HostRun {
            pid: 77,
            start_ticks: 2,
            owner: HostRunOwner::Orphaned,
            title: "Vite :5299".into(),
            command_line: "node vite".into(),
            cwd: "/home/dev/shop".into(),
            worktree: None,
            cwd_deleted: false,
            started_at_unix: 1_000_000 - 3 * 3600,
            memory_bytes: 1_400_000_000,
            process_count: 2,
            kinds: vec!["Vite".into()],
        });
        let text = report(&[p, ProjectSummary { name: "blog".into(), ..project() }], 1_000_000);
        assert!(text.starts_with("shop\n  agent sessions\n    Claude Code (pid 42) in worktree fix-login, up 10m"), "{text}");
        assert!(text.contains(
            "    Vite :5299 [Vite] in the checkout — orphaned — whatever started it has exited; \
             2 processes, 1.4 GB, up 3h (pid 77)"
        ), "{text}");
        assert!(text.contains(
            "      branch worktree-fix-login; where Claude Code makes them; 3 uncommitted changes; \
             1 commit the checkout does not have; made 2h ago"
        ), "{text}");
        assert!(text.contains("nothing from agents in blog"), "{text}");
    }

    #[test]
    fn sizes_and_ages_read_at_a_glance() {
        assert_eq!(bytes(2_700_000_000), "2.7 GB");
        assert_eq!(bytes(44_000_000), "44 MB");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(up(100, 145), "45s");
        assert_eq!(up(0, 90_000), "1d");
    }
}
