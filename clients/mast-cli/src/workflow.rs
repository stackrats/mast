//! The verbs a bar widget, a keybinding or a person at a terminal drives
//! day-to-day work with: a project's saved commands, whole workspaces, and
//! clearing up what agents left running (ADR-0013, ADR-0012).

use std::time::{Duration, Instant};

use futures::StreamExt;
use mast_client::MastClient;
use mast_contract::{Action, OperationEventKind, OperationId};

use crate::{agents, resolve_project, settled_snapshot};

/// Print an operation's output until it ends. `done` is what success says.
async fn follow(client: &dyn MastClient, op: OperationId, label: &str, done: &str) -> i32 {
    let mut events = match client.operation_events(op).await {
        Ok(events) => events,
        Err(e) => {
            eprintln!("{label}: cannot follow the operation: {e}");
            return 1;
        }
    };
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => println!("{line}"),
            OperationEventKind::Completed => {
                println!("{label}: {done}");
                return 0;
            }
            OperationEventKind::Cancelled => {
                println!("{label}: cancelled");
                return 1;
            }
            OperationEventKind::Failed { error } => {
                eprintln!("{label}: {error}");
                return 1;
            }
            _ => {}
        }
    }
    0
}

async fn dispatch(client: &dyn MastClient, action: Action, label: &str, read_only: bool) -> Option<OperationId> {
    match client.dispatch(action).await {
        Ok(op) => Some(op),
        Err(e) => {
            eprintln!("{label}: {e}");
            if read_only {
                eprintln!("(the desktop app owns mutation — control it from there for now)");
            }
            None
        }
    }
}

/// `mast start|stop|restart <project> --command <name>`.
///
/// A command runs inside the engine, so it outlives this CLI only when the
/// engine does: the desktop app's or the daemon's, reached over the socket.
/// Started, it is followed for `wait` seconds — long enough to see a
/// one-shot finish or a server come up — and then left running.
pub(crate) async fn command(
    client: &dyn MastClient,
    shared_engine: bool,
    wanted: &str,
    name: &str,
    verb: &str,
    wait: u64,
) -> i32 {
    let snap = settled_snapshot(client).await;
    let (id, _) = match resolve_project(&snap, wanted) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let Some(project) = snap.projects.iter().find(|p| p.id == id) else { return 2 };
    if !project.commands.iter().any(|c| c.name == name) {
        let known: Vec<&str> = project.commands.iter().map(|c| c.name.as_str()).collect();
        eprintln!(
            "{}: no saved command named \"{name}\" ({})",
            project.name,
            if known.is_empty() { "it has none".to_string() } else { format!("known: {}", known.join(", ")) }
        );
        return 2;
    }
    if verb != "stop" && !shared_engine {
        eprintln!(
            "{}: a command runs inside the Mast engine, and this CLI's own engine ends when it \
             does — start the Mast app (or mast-daemon) and run this again",
            project.name
        );
        return 2;
    }
    let label = format!("{}: {name}", project.name);
    let id = project.id.clone();
    if verb == "stop" || verb == "restart" {
        if !project.running_commands.iter().any(|c| c == name) {
            if verb == "stop" {
                println!("{label} is not running");
                return 0;
            }
        } else {
            let stop = Action::StopProjectCommand { id: id.clone(), name: name.to_string() };
            let Some(op) = dispatch(client, stop, &label, snap.read_only).await else { return 1 };
            let code = follow(client, op, &label, "stopped").await;
            if code != 0 || verb == "stop" {
                return code;
            }
        }
    }
    let run = Action::RunProjectCommand { id, name: name.to_string() };
    let Some(op) = dispatch(client, run, &label, snap.read_only).await else { return 1 };
    let mut events = match client.operation_events(op).await {
        Ok(events) => events,
        Err(e) => {
            eprintln!("{label}: cannot follow the operation: {e}");
            return 1;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.next()).await {
            Ok(Some(event)) => match event.kind {
                OperationEventKind::Output { line, .. } => println!("{line}"),
                OperationEventKind::Completed => {
                    println!("{label} completed");
                    return 0;
                }
                OperationEventKind::Cancelled => {
                    println!("{label} was cancelled");
                    return 1;
                }
                OperationEventKind::Failed { error } => {
                    eprintln!("{label} failed: {error}");
                    return 1;
                }
                _ => {}
            },
            Ok(None) => return 0,
            // A dev server never exits; still going is the success case.
            Err(_) => {
                println!("{label} is running — Mast keeps it going; stop it with --command {name}");
                return 0;
            }
        }
    }
}

/// `mast workspace start|stop <name>`: every member, in dependency order
/// (stopping in reverse), as the app's Start all does.
pub(crate) async fn workspace(client: &dyn MastClient, verb: &str, wanted: &str) -> i32 {
    let snap = settled_snapshot(client).await;
    let matches: Vec<_> = snap
        .workspaces
        .iter()
        .filter(|w| w.name == wanted || w.id.0 == wanted || w.name.eq_ignore_ascii_case(wanted))
        .collect();
    let workspace = match matches.as_slice() {
        [one] => *one,
        [] => {
            let known: Vec<&str> = snap.workspaces.iter().map(|w| w.name.as_str()).collect();
            eprintln!(
                "no workspace named \"{wanted}\" ({})",
                if known.is_empty() { "none exist yet".to_string() } else { format!("known: {}", known.join(", ")) }
            );
            return 2;
        }
        _ => {
            eprintln!("\"{wanted}\" is ambiguous");
            return 2;
        }
    };
    if let Some(error) = &workspace.graph_error {
        eprintln!("{}: {error}", workspace.name);
        return 1;
    }
    let action = if verb == "stop" {
        Action::StopWorkspace { id: workspace.id.clone() }
    } else {
        Action::StartWorkspace { id: workspace.id.clone() }
    };
    let Some(op) = dispatch(client, action, &workspace.name, snap.read_only).await else { return 1 };
    follow(client, op, &workspace.name, &format!("{verb} completed")).await
}

/// `mast agents <project> --stop <pid>` / `--stop-container <pid>`: stop one
/// tree the report lists, by the identity the report gave it.
pub(crate) async fn stop_run(client: &dyn MastClient, wanted: &str, pid: u32, container: bool) -> i32 {
    let snap = settled_snapshot(client).await;
    let (id, name) = match resolve_project(&snap, wanted) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let Some(project) = snap.projects.iter().find(|p| p.id == id) else { return 2 };
    let found = if container {
        project
            .container_runs
            .iter()
            .find(|r| r.pid == pid)
            .map(|r| (r.title.clone(), Action::StopContainerRun { id: id.clone(), pid, start_ticks: r.start_ticks }))
    } else {
        project
            .host_runs
            .iter()
            .find(|r| r.pid == pid)
            .map(|r| (r.title.clone(), Action::StopHostRun { id: id.clone(), pid, start_ticks: r.start_ticks }))
    };
    let Some((title, action)) = found else {
        eprintln!(
            "{name}: no {} process {pid} is listed — `mast agents {name}` shows what is",
            if container { "app-container" } else { "host" }
        );
        return 2;
    };
    let label = format!("{name}: {title}");
    let Some(op) = dispatch(client, action, &label, snap.read_only).await else { return 1 };
    follow(client, op, &label, "stopped").await
}

/// `mast agents [project] --clean-up [--dry-run]`: what the app's Clean up
/// ticks by itself — what nothing live is using — and nothing more.
pub(crate) async fn clean_up(client: &dyn MastClient, wanted: Option<&str>, dry_run: bool) -> i32 {
    let snap = settled_snapshot(client).await;
    let projects: Vec<_> = match wanted {
        Some(wanted) => match resolve_project(&snap, wanted) {
            Ok((id, _)) => snap.projects.iter().filter(|p| p.id == id).cloned().collect(),
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        },
        None => snap.projects.clone(),
    };
    let plan = agents::cleanup_plan(&projects);
    if plan.is_empty() {
        println!("nothing left running that nothing is using");
        return 0;
    }
    if dry_run {
        for step in &plan {
            println!("would {}", step.label);
        }
        return 0;
    }
    let mut failed = 0;
    for step in plan {
        let Some(op) = dispatch(client, step.action, &step.label, snap.read_only).await else {
            failed += 1;
            continue;
        };
        if follow(client, op, &step.label, "done").await != 0 {
            failed += 1;
        }
    }
    if failed > 0 {
        eprintln!("{failed} step(s) failed");
        1
    } else {
        0
    }
}
