//! `mast` — the CLI client (M8). Same `MastClient` surface as the desktop:
//! prefers the daemon socket (shared engine — full mutation rights while the
//! desktop runs, since the desktop serves it), else falls back to an
//! embedded engine (read-only if another instance owns the flock).

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser, Subcommand};
use futures::StreamExt;
use mast_client::MastClient;
use mast_client_local::LocalClient;
use mast_contract::{
    Action, DiagSeverity, EngineSnapshot, HistoryDetail, HistoryEntry, HistoryOrigin,
    HistoryOutcome, OperationEventKind, ProjectId, ProjectStatus,
};
use mast_engine::{Engine, EngineConfig, EngineDeps, RealConnector, RealLifecycleRunner};
use mast_project::MetadataStore;

/// Shown above `--help` and on a bare `mast`. Block glyphs, so it needs a
/// monospace terminal — which is the only place it is ever printed.
const BANNER: &str = concat!(
    " ██╗██╗        ███╗   ███╗  █████╗  ███████╗ ████████╗\n",
    " ██║█████╗     ████╗ ████║ ██╔══██╗ ██╔════╝ ╚══██╔══╝\n",
    " ██║████████╗  ██╔████╔██║ ███████║ ███████╗    ██║\n",
    " ██║█████╔═╝   ██║╚██╔╝██║ ██╔══██║ ╚════██║    ██║\n",
    " ██║██╔═╝      ██║ ╚═╝ ██║ ██║  ██║ ███████║    ██║\n",
    " ╚═╝╚═╝        ╚═╝     ╚═╝ ╚═╝  ╚═╝ ╚══════╝    ╚═╝\n",
);

#[derive(Parser)]
#[command(
    name = "mast",
    version,
    about = "Control tower for local Laravel Sail development",
    before_help = BANNER
)]
struct Cli {
    /// Absent for a bare `mast`, which prints the banner and the help.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Projects, workspaces and their live container state.
    Status,
    /// `up -d` a project (or one service with --service).
    Start {
        project: String,
        #[arg(long)]
        service: Option<String>,
    },
    /// `stop` a project (or one service).
    Stop {
        project: String,
        #[arg(long)]
        service: Option<String>,
    },
    /// `restart` a project (or one service).
    Restart {
        project: String,
        #[arg(long)]
        service: Option<String>,
    },
    /// Run the full diagnostic check set.
    Diagnose,
    /// What Mast has run and written recently, newest last.
    History {
        /// Include Mast's own upkeep (resolution, probes, inspection).
        #[arg(long)]
        background: bool,
        /// How many entries to show.
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // Nothing to do without a subcommand, and connecting first would make a
    // bare `mast` wait on a daemon just to print help.
    let Some(command) = cli.command else {
        let _ = Cli::command().print_help();
        println!();
        std::process::exit(0);
    };
    // Prefer a live daemon (shared engine — full mutation rights even while
    // the desktop runs); fall back to an embedded engine.
    let client: Arc<dyn MastClient> =
        match mast_client_ipc::IpcClient::connect(&mast_daemon::default_socket_path()).await {
            Ok(ipc) => Arc::new(ipc),
            Err(_) => embedded_engine(),
        };

    let code = match command {
        Command::Status => status(client.as_ref()).await,
        Command::Start { project, service } => {
            lifecycle(client.as_ref(), &project, service, "start").await
        }
        Command::Stop { project, service } => {
            lifecycle(client.as_ref(), &project, service, "stop").await
        }
        Command::Restart { project, service } => {
            lifecycle(client.as_ref(), &project, service, "restart").await
        }
        Command::Diagnose => diagnose(client.as_ref()).await,
        Command::History { background, limit } => {
            history(client.as_ref(), background, limit).await
        }
    };
    std::process::exit(code);
}

fn embedded_engine() -> Arc<dyn MastClient> {
    let engine = Engine::new(
        EngineConfig::default(),
        EngineDeps {
            connector: Arc::new(RealConnector),
            store: match MetadataStore::open(MetadataStore::default_dir()) {
                Ok(store) => store,
                Err(e) => {
                    eprintln!("cannot open mast metadata: {e}");
                    std::process::exit(2);
                }
            },
            process_env: std::env::vars().collect(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: mast_engine::acquire_ownership(None),
        },
    );
    engine.start();
    Arc::new(LocalClient::new(engine))
}

/// Wait for the effect loops to produce a meaningful snapshot: docker status
/// resolved and a settle window with no new patches.
async fn settled_snapshot(client: &dyn MastClient) -> EngineSnapshot {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last_seq = 0;
    let mut stable_since = Instant::now();
    loop {
        let snap = match client.snapshot().await {
            Ok(snap) => snap,
            Err(e) => {
                eprintln!("cannot read engine state: {e}");
                std::process::exit(2);
            }
        };
        let docker_known = snap.docker.available || snap.docker.error.is_some();
        if snap.seq != last_seq {
            last_seq = snap.seq;
            stable_since = Instant::now();
        }
        if (docker_known && stable_since.elapsed() > Duration::from_millis(400))
            || Instant::now() > deadline
        {
            break snap;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn glyph(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Running => "▶",
        ProjectStatus::Starting => "…",
        ProjectStatus::Degraded | ProjectStatus::Failed => "⚠",
        _ => "○",
    }
}

async fn status(client: &dyn MastClient) -> i32 {
    let snap = settled_snapshot(client).await;
    match (&snap.docker.available, &snap.docker.error) {
        (true, _) => println!(
            "docker: connected ({})",
            snap.docker.context_name.as_deref().unwrap_or("default")
        ),
        (false, Some(e)) => println!("docker: unavailable — {e}"),
        (false, None) => println!("docker: connecting…"),
    }
    if snap.read_only {
        println!("read-only: another Mast instance owns mutation (the desktop app?)");
    }
    if snap.projects.is_empty() {
        println!("no projects imported — open the desktop app to add some");
        return 0;
    }

    let member_of: std::collections::HashMap<&str, &str> = snap
        .workspaces
        .iter()
        .flat_map(|w| w.members.iter().map(|m| (m.project.0.as_str(), w.name.as_str())))
        .collect();
    println!();
    for project in &snap.projects {
        let running = project
            .services
            .iter()
            .filter(|s| s.state == Some(mast_contract::ContainerState::Running))
            .count();
        let branch = match (&project.git_branch, project.git_dirty) {
            (Some(branch), Some(true)) => format!("  [{branch}*]"),
            (Some(branch), _) => format!("  [{branch}]"),
            _ => String::new(),
        };
        let workspace = member_of
            .get(project.id.0.as_str())
            .map(|w| format!("  ({w})"))
            .unwrap_or_default();
        println!(
            "{} {:<28} {:?}  {}/{} services{}{}",
            glyph(project.status),
            project.name,
            project.status,
            running,
            project.services.len(),
            branch,
            workspace,
        );
    }
    0
}

/// Quote one argument so pasting the printed line into a shell reproduces the
/// command exactly — the whole point of showing it.
fn shell_quote(arg: &str) -> String {
    let bare = !arg.is_empty()
        && arg.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if bare {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

async fn history(client: &dyn MastClient, background: bool, limit: usize) -> i32 {
    let entries = match client.history_recent().await {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("cannot read history: {e}");
            return 2;
        }
    };
    let shown: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|entry| background || entry.origin == HistoryOrigin::User)
        .rev()
        .take(limit)
        .collect();
    if shown.is_empty() {
        // History lives in the engine that ran the commands. Without a daemon
        // this CLI just started its own engine, which has done nothing yet.
        println!("no history yet — history comes from the running engine, so start Mast (or the");
        println!("daemon) and it will fill as commands run");
        return 0;
    }
    for entry in shown.iter().rev() {
        let outcome = match &entry.outcome {
            HistoryOutcome::Running => "running".to_string(),
            HistoryOutcome::Exited { status: 0 } => "ok".to_string(),
            HistoryOutcome::Exited { status } => format!("exit {status}"),
            HistoryOutcome::Cancelled => "cancelled".to_string(),
            HistoryOutcome::Failed { error } => format!("failed: {error}"),
            HistoryOutcome::Detached => "launched".to_string(),
            HistoryOutcome::Applied => "applied".to_string(),
        };
        let took = entry
            .duration_ms
            .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
            .unwrap_or_default();
        println!("{}  {outcome}{took}", entry.label);
        match &entry.detail {
            HistoryDetail::Command { argv, cwd, .. } => {
                if let Some(cwd) = cwd {
                    println!("  in {cwd}");
                }
                println!(
                    "  $ {}",
                    argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
                );
            }
            HistoryDetail::FileWrite { path, summary } => {
                println!("  wrote {path}");
                for line in summary {
                    println!("    {line}");
                }
            }
        }
    }
    0
}

fn resolve_project(snap: &EngineSnapshot, wanted: &str) -> Result<(ProjectId, String), String> {
    let matches: Vec<_> = snap
        .projects
        .iter()
        .filter(|p| p.name == wanted || p.path.ends_with(wanted))
        .collect();
    match matches.as_slice() {
        [one] => Ok((one.id.clone(), one.name.clone())),
        [] => Err(format!(
            "no project named \"{wanted}\" (known: {})",
            snap.projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
        _ => Err(format!("\"{wanted}\" is ambiguous")),
    }
}

async fn lifecycle(
    client: &dyn MastClient,
    wanted: &str,
    service: Option<String>,
    verb: &str,
) -> i32 {
    let snap = settled_snapshot(client).await;
    let (id, name) = match resolve_project(&snap, wanted) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let action = match (verb, service) {
        ("start", None) => Action::StartProject { id },
        ("stop", None) => Action::StopProject { id },
        ("restart", None) => Action::RestartProject { id },
        ("start", Some(service)) => Action::StartService { id, service },
        ("stop", Some(service)) => Action::StopService { id, service },
        ("restart", Some(service)) => Action::RestartService { id, service },
        _ => unreachable!(),
    };
    let op = match client.dispatch(action).await {
        Ok(op) => op,
        Err(e) => {
            eprintln!("{name}: {e}");
            if snap.read_only {
                eprintln!("(the desktop app owns mutation — control it from there for now)");
            }
            return 1;
        }
    };
    let mut events = match client.operation_events(op).await {
        Ok(events) => events,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => println!("{line}"),
            OperationEventKind::Completed => {
                println!("{name}: {verb} completed");
                return 0;
            }
            OperationEventKind::Cancelled => {
                println!("{name}: cancelled");
                return 1;
            }
            OperationEventKind::Failed { error } => {
                eprintln!("{name}: {error}");
                return 1;
            }
            _ => {}
        }
    }
    0
}

async fn diagnose(client: &dyn MastClient) -> i32 {
    let _ = settled_snapshot(client).await;
    let report = match client.run_diagnostics().await {
        Ok(report) => report,
        Err(e) => {
            eprintln!("diagnostics failed: {e}");
            return 2;
        }
    };
    if report.findings.is_empty() {
        println!("{} checks — everything looks healthy", report.checks_run);
        return 0;
    }
    let mut errors = 0;
    for finding in &report.findings {
        let tag = match finding.severity {
            DiagSeverity::Error => {
                errors += 1;
                "ERROR"
            }
            DiagSeverity::Warning => "WARN ",
            DiagSeverity::Info => "info ",
        };
        println!("{tag} {}", finding.title);
        println!("      {}", finding.detail);
        if let Some(repair) = &finding.repair {
            println!("      repair available in the desktop app: {}", repair.title);
        }
    }
    if errors > 0 { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_contract::{ProjectStatus, ProjectSummary};

    fn snapshot_with(projects: Vec<ProjectSummary>) -> EngineSnapshot {
        EngineSnapshot {
            protocol_version: mast_contract::PROTOCOL_VERSION,
            seq: 0,
            read_only: false,
            docker: Default::default(),
            integrations: mast_contract::IntegrationSettings { terminal: None, editor: None },
            watched_directories: Vec::new(),
            discovered: Vec::new(),
            projects,
            workspaces: Vec::new(),
        }
    }

    fn project(name: &str, path: &str) -> ProjectSummary {
        ProjectSummary {
            id: ProjectId(format!("id-{name}")),
            name: name.into(),
            path: path.into(),
            status: ProjectStatus::Stopped,
            compose_project_name: None,
            is_sail: true,
            services: Vec::new(),
            resolution_error: None,
            warnings: Vec::new(),
            commands: Vec::new(),
            processes: Vec::new(),
            git_branch: None,
            git_dirty: None,
            app_url: None,
        }
    }

    #[test]
    fn resolves_by_exact_name_or_path_suffix() {
        let snap = snapshot_with(vec![
            project("api", "/home/dev/code/api"),
            project("web", "/home/dev/code/web"),
        ]);
        assert_eq!(resolve_project(&snap, "api").unwrap().1, "api");
        assert_eq!(resolve_project(&snap, "code/web").unwrap().1, "web");
    }

    #[test]
    fn unknown_names_list_the_candidates() {
        let snap = snapshot_with(vec![project("api", "/x/api")]);
        let err = resolve_project(&snap, "nope").unwrap_err();
        assert!(err.contains("no project named"), "{err}");
        assert!(err.contains("api"), "{err}");
    }

    #[test]
    fn ambiguous_matches_are_refused() {
        // Same directory basename under two parents: path-suffix matches both.
        let snap = snapshot_with(vec![project("a", "/x/app"), project("b", "/y/app")]);
        let err = resolve_project(&snap, "app").unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
    }
}
