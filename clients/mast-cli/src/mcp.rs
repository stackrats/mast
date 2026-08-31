//! `mast mcp` — an MCP server over stdio, so a coding agent working in a
//! Sail project can ask Mast what the environment actually is (and drive it)
//! instead of shelling out to `docker compose` blind.
//!
//! The wire is newline-delimited JSON-RPC 2.0, one object per line — the
//! same framing the daemon socket already speaks — so it is hand-rolled on
//! serde_json rather than pulled in as a dependency: the surface MCP needs
//! here is a page of routing, not a protocol stack. Stdout carries protocol
//! messages and nothing else; anything diagnostic goes to stderr.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use mast_client::MastClient;
use mast_contract::{
    Action, CaptureReason, ContainerState, DiagSeverity, EngineSnapshot, HistoryDetail,
    HistoryEntry, HistoryOrigin, HistoryOutcome, OperationEventKind, ProjectStatus,
    ProjectSummary, ServiceHealth, ServiceState,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{resolve_project, settled_snapshot, shell_quote};

/// What we answer when the client does not name a protocol revision.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// Streamed command output is capped at its tail — for compose and dev
/// servers the end is where the outcome lives, and an agent's context is
/// too expensive to spend on npm install progress bars.
const OUTPUT_CAP: usize = 100;

/// Stack traces are capped at their head instead: the exception and the top
/// frames matter, the framework floor below them does not.
const TRACE_CAP: usize = 40;

/// How many recent errors get their stack trace inline; older ones just say
/// how much detail exists.
const TRACES_SHOWN: usize = 3;

pub(crate) async fn serve(client: &dyn MastClient) -> i32 {
    // Stdin is read on a plain thread: tokio's own stdin wants a feature the
    // workspace does not enable, and a blocking reader feeding a channel is
    // all a line-at-a-time protocol asks for.
    let (tx, mut rx) = mpsc::channel::<String>(16);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    let mut stdout = std::io::stdout();
    while let Some(line) = rx.recv().await {
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle_line(client, &line).await else { continue };
        if writeln!(stdout, "{response}").and_then(|()| stdout.flush()).is_err() {
            // The agent hung up mid-reply; there is nobody left to serve.
            return 0;
        }
    }
    // Stdin EOF is how an MCP session ends on purpose.
    0
}

/// One inbound line to at most one outbound line. `None` means "say
/// nothing": notifications never get a reply, whatever their method.
async fn handle_line(client: &dyn MastClient, line: &str) -> Option<String> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(e) => return Some(error_response(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let id = message.get("id").cloned().filter(|id| !id.is_null())?;
    let outcome = match method.as_str() {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_specs()})),
        "tools/call" => tools_call(client, &params).await,
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        Err((code, message)) => error_response(id, code, &message),
    })
}

/// Echo whatever protocol revision the client asked for. Everything this
/// server offers — tools, nothing else — exists in every revision to date,
/// so agreeing beats making the client negotiate down.
fn initialize_result(params: &Value) -> Value {
    let requested =
        params.get("protocolVersion").and_then(Value::as_str).unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": requested,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "mast", "version": mast_contract::BUILD_VERSION},
    })
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

async fn tools_call(client: &dyn MastClient, params: &Value) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((-32602, "tools/call needs a string \"name\"".to_string()));
    };
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let outcome = match name {
        "mast_status" => status_tool(client, &args).await,
        "mast_start" => lifecycle_tool(client, &args, "start").await,
        "mast_stop" => lifecycle_tool(client, &args, "stop").await,
        "mast_restart" => lifecycle_tool(client, &args, "restart").await,
        "mast_rebuild" => lifecycle_tool(client, &args, "rebuild").await,
        "mast_logs" => logs_tool(client, &args).await,
        "mast_laravel_log" => laravel_log_tool(client, &args).await,
        "mast_captures" => captures_tool(client, &args).await,
        "mast_diagnose" => diagnose_tool(client, &args).await,
        "mast_run_command" => run_command_tool(client, &args).await,
        "mast_wait" => wait_tool(client, &args).await,
        "mast_history" => history_tool(client, &args).await,
        "mast_snapshots" => snapshots_tool(client, &args).await,
        "mast_snapshot_data" => snapshot_take_tool(client, &args).await,
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    Ok(tool_result(outcome))
}

/// Every tool failure is an `isError` result rather than a protocol error:
/// the agent is expected to read the sentence and try something else, and
/// protocol errors are for a broken client, not a stopped container.
fn tool_result(outcome: Result<String, String>) -> Value {
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

// ---------- tool table ----------

fn project_arg() -> Value {
    json!({"type": "string", "description": "Project name, or a unique suffix of its path."})
}

fn service_arg() -> Value {
    json!({"type": "string", "description": "Compose service name — mast_status lists them."})
}

fn lifecycle_spec(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {"project": project_arg(), "service": service_arg()},
            "required": ["project"],
        },
    })
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "mast_status",
            "description": "The full picture of every Mast-managed project: docker connectivity, per-service container state and health, ports, app URLs, git branch, warnings, workspaces, and the saved commands and Laravel processes each project offers. Call this first to orient yourself; pass `project` for one project in detail.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": project_arg()},
                "required": [],
            },
        },
        lifecycle_spec("mast_start", "Start a project's containers (compose `up -d`), or one service with `service`. Streams the compose output and returns it with the outcome; follow with mast_wait if you need the project healthy before hitting it."),
        lifecycle_spec("mast_stop", "Stop a project's containers, or one service with `service`."),
        lifecycle_spec("mast_restart", "Restart a project's containers, or one service with `service`. Restart reuses images and containers — after a compose file or image change use mast_rebuild instead."),
        lifecycle_spec("mast_rebuild", "Rebuild a project: rebuild images, pull newer ones and recreate the containers, dropping orphans — the recovery when compose config changed underneath running containers (e.g. after a git pull). With `service`, refresh just that container."),
        {
            "name": "mast_logs",
            "description": "Recent container log lines for one service (stderr lines marked). Use it to see why a service is crashing or what it just printed; for application-level errors prefer mast_laravel_log.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_arg(),
                    "service": service_arg(),
                    "tail": {"type": "integer", "description": "How many lines of backlog to fetch (default 100)."},
                },
                "required": ["project", "service"],
            },
        },
        {
            "name": "mast_laravel_log",
            "description": "Parsed entries from the project's storage/logs/laravel.log, newest first — level, timestamp, message, and the stack traces of the most recent errors. The place to look for HTTP 500s, exceptions and failed jobs, i.e. when the app misbehaves rather than the containers.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_arg(),
                    "errors_only": {"type": "boolean", "description": "Only ERROR-and-worse entries (default false)."},
                    "limit": {"type": "integer", "description": "How many entries to return (default 20)."},
                },
                "required": ["project"],
            },
        },
        {
            "name": "mast_captures",
            "description": "Stored last-words captures: the tail of a container's output, saved at the moment it exited, went unhealthy or was torn down. The place to look when a container died and its live logs are already gone.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many captures to list (default 5)."},
                },
                "required": [],
            },
        },
        {
            "name": "mast_diagnose",
            "description": "Run Mast's diagnostic checks — docker daemon, ports, env files, and the rest — across everything, or one project with `project`. Findings carry a severity and say when a one-click repair exists (repairs are applied from the Mast desktop app, not from here).",
            "inputSchema": {
                "type": "object",
                "properties": {"project": project_arg()},
                "required": [],
            },
        },
        {
            "name": "mast_run_command",
            "description": "Run one of the project's saved commands by name — mast_status lists them; arbitrary command strings are deliberately not accepted. Returns when the command finishes, or after `wait_secs` with the output so far if it is still running (dev servers never exit — that is the normal case, and the command is left running).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_arg(),
                    "name": {"type": "string", "description": "Saved command name, exactly as mast_status lists it."},
                    "wait_secs": {"type": "integer", "description": "How long to wait before reporting a still-running command (default 30)."},
                },
                "required": ["project", "name"],
            },
        },
        {
            "name": "mast_wait",
            "description": "Wait for a project to reach running — every container up with health checks passing — polling until `timeout_secs`. Use after mast_start or mast_restart, before hitting the app.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": project_arg(),
                    "timeout_secs": {"type": "integer", "description": "Give up after this long (default 120)."},
                },
                "required": ["project"],
            },
        },
        {
            "name": "mast_history",
            "description": "What Mast actually ran and wrote recently on the user's behalf: every command with its argv and outcome, every config file write. Check it to see what just happened before acting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many entries to show (default 20)."},
                },
                "required": [],
            },
        },
        {
            "name": "mast_snapshots",
            "description": "List a project's data snapshots — point-in-time copies of a service's named volumes, stored as labeled docker volumes. Restoring one overwrites live data, so restores happen in the Mast app, never from here.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": project_arg()},
                "required": ["project"],
            },
        },
        {
            "name": "mast_snapshot_data",
            "description": "Take a data snapshot of one service's named volumes BEFORE running anything destructive — a migration, a seed, a volume reset. The container is stopped for a consistent copy and restarted after. Safe: it only ever adds a copy.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": project_arg(), "service": service_arg()},
                "required": ["project", "service"],
            },
        },
    ])
}

// ---------- arguments ----------
//
// Argument problems go back as tool results, not protocol errors — the agent
// can read a sentence and correct itself; a bare -32602 it often cannot.

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(value.to_string()),
        _ => Err(format!("the \"{key}\" argument is required")),
    }
}

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_u64(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn optional_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

// ---------- tools ----------

async fn status_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let snap = settled_snapshot(client).await;
    match optional_str(args, "project") {
        Some(wanted) => Ok(project_block(find_project(&snap, &wanted)?, true).join("\n")),
        None => Ok(render_overview(&snap)),
    }
}

async fn lifecycle_tool(
    client: &dyn MastClient,
    args: &Value,
    verb: &str,
) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let service = optional_str(args, "service");
    let snap = settled_snapshot(client).await;
    let (id, name) = resolve_project(&snap, &wanted)?;
    let target = match &service {
        Some(service) => format!("{name}/{service}"),
        None => name.clone(),
    };
    let action = match (verb, service) {
        ("start", None) => Action::StartProject { id },
        ("stop", None) => Action::StopProject { id },
        ("restart", None) => Action::RestartProject { id },
        ("rebuild", None) => Action::RebuildProject { id },
        ("start", Some(service)) => Action::StartService { id, service },
        ("stop", Some(service)) => Action::StopService { id, service },
        ("restart", Some(service)) => Action::RestartService { id, service },
        ("rebuild", Some(service)) => Action::RebuildService { id, service },
        _ => unreachable!(),
    };
    let op = client
        .dispatch(action)
        .await
        .map_err(|e| dispatch_error(&target, &e.to_string(), snap.read_only))?;
    let mut events = client
        .operation_events(op)
        .await
        .map_err(|e| format!("{target}: cannot follow the operation: {e}"))?;
    let mut output = Vec::new();
    let mut fix = None;
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => output.push(line),
            OperationEventKind::FixAvailable { repair, .. } => fix = Some(repair.title),
            OperationEventKind::Completed => {
                let mut out = vec![format!("{target}: {verb} completed")];
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Ok(out.join("\n"));
            }
            OperationEventKind::Failed { error } => {
                let mut out = vec![format!("{target}: {verb} failed — {error}")];
                if let Some(fix) = fix {
                    out.push(format!(
                        "a one-click repair for this exists in the Mast desktop app: {fix}"
                    ));
                }
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Err(out.join("\n"));
            }
            OperationEventKind::Cancelled => return Err(format!("{target}: {verb} was cancelled")),
            _ => {}
        }
    }
    Err(format!("{target}: the event stream ended before the operation finished"))
}

async fn logs_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let service = required_str(args, "service")?;
    let tail = optional_u64(args, "tail", 100).clamp(1, 1000) as u32;
    let snap = settled_snapshot(client).await;
    let (id, name) = resolve_project(&snap, &wanted)?;
    let mut stream = client
        .service_logs(id, service.clone(), tail)
        .await
        .map_err(|e| format!("cannot read {name}/{service} logs: {e}"))?;
    // The stream is live and would follow forever. Take the backlog and get
    // out: lines until `tail` have arrived and the stream goes quiet for
    // 300ms, bounded at 3s for a service with less to say than was asked.
    let mut lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let wait = if lines.len() >= tail as usize {
            remaining.min(Duration::from_millis(300))
        } else {
            remaining
        };
        match tokio::time::timeout(wait, stream.next()).await {
            Ok(Some(line)) => lines.push(if line.stderr {
                format!("[stderr] {}", line.message)
            } else {
                line.message
            }),
            Ok(None) | Err(_) => break,
        }
    }
    if lines.is_empty() {
        return Ok(format!(
            "no log output from {name}/{service} — the container may not have started yet"
        ));
    }
    let mut out = vec![format!("{name}/{service} — last {} line(s):", lines.len())];
    out.append(&mut lines);
    Ok(out.join("\n"))
}

async fn laravel_log_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let errors_only = optional_bool(args, "errors_only");
    let limit = optional_u64(args, "limit", 20).clamp(1, 200) as usize;
    let snap = settled_snapshot(client).await;
    let (id, name) = resolve_project(&snap, &wanted)?;
    let report =
        client.laravel_log(id).await.map_err(|e| format!("cannot read {name}'s app log: {e}"))?;
    if !report.exists {
        return Ok(format!(
            "{name} has no storage/logs/laravel.log — a fresh app, or LOG_CHANNEL points somewhere else"
        ));
    }
    let shown: Vec<_> = report
        .entries
        .iter()
        .filter(|entry| !errors_only || is_error_level(&entry.level))
        .take(limit)
        .collect();
    if shown.is_empty() {
        return Ok(if errors_only {
            format!("no error-level entries in the tail of {name}'s laravel.log")
        } else {
            format!("{name}'s laravel.log is empty")
        });
    }
    let mut out = vec![format!("{name} laravel.log — {} entries, newest first:", shown.len())];
    let mut traces = 0;
    for entry in &shown {
        out.push(format!(
            "[{}] {}.{}: {}",
            entry.timestamp, entry.environment, entry.level, entry.message
        ));
        if let Some(detail) = &entry.detail {
            if traces < TRACES_SHOWN && is_error_level(&entry.level) {
                traces += 1;
                for line in keep_head(detail, TRACE_CAP).lines() {
                    out.push(format!("    {line}"));
                }
            } else {
                out.push(format!("    ({} line(s) of detail omitted)", detail.lines().count()));
            }
        }
    }
    if report.truncated {
        out.push("(the file outgrew the read window — older entries were left behind)".into());
    }
    Ok(out.join("\n"))
}

async fn captures_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let limit = optional_u64(args, "limit", 5).clamp(1, 50) as u32;
    let captures =
        client.log_captures(limit).await.map_err(|e| format!("cannot read log captures: {e}"))?;
    if captures.is_empty() {
        return Ok("no log captures stored — Mast saves the tail of a container's output when \
                   it exits, goes unhealthy or is torn down, and none of that has happened yet"
            .into());
    }
    let now = now_unix_ms();
    let mut out = Vec::new();
    for (index, capture) in captures.iter().enumerate() {
        let truncated = if capture.truncated { ", truncated" } else { "" };
        out.push(format!(
            "#{} {}/{} — {}, {} ({} line(s) from a {}s window{})",
            capture.id,
            capture.project_name,
            capture.service,
            reason_phrase(&capture.reason),
            age(capture.at_unix_ms, now),
            capture.lines.len(),
            capture.window_secs,
            truncated,
        ));
        if index == 0 {
            let lines: Vec<String> = capture
                .lines
                .iter()
                .map(|line| {
                    if line.stderr {
                        format!("  [stderr] {}", line.message)
                    } else {
                        format!("  {}", line.message)
                    }
                })
                .collect();
            out.extend(keep_tail(lines, OUTPUT_CAP));
            if captures.len() > 1 {
                out.push(String::new());
                out.push("older captures (headers only):".into());
            }
        }
    }
    Ok(out.join("\n"))
}

async fn diagnose_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let snap = settled_snapshot(client).await;
    let scope = match optional_str(args, "project") {
        Some(wanted) => Some(resolve_project(&snap, &wanted)?.0),
        None => None,
    };
    let report =
        client.run_diagnostics(scope).await.map_err(|e| format!("diagnostics failed: {e}"))?;
    if report.findings.is_empty() {
        return Ok(format!("{} checks run — everything looks healthy", report.checks_run));
    }
    let mut out =
        vec![format!("{} checks run, {} finding(s):", report.checks_run, report.findings.len())];
    for finding in &report.findings {
        let tag = match finding.severity {
            DiagSeverity::Error => "ERROR",
            DiagSeverity::Warning => "WARN",
            DiagSeverity::Info => "info",
        };
        let scope = finding
            .project_name
            .as_ref()
            .map(|name| format!(" [{name}]"))
            .unwrap_or_default();
        out.push(format!("{tag}{scope} {}", finding.title));
        out.push(format!("  {}", finding.detail));
        if let Some(repair) = &finding.repair {
            out.push(format!(
                "  a one-click repair exists, applied from the Mast desktop app: {}",
                repair.title
            ));
        }
    }
    Ok(out.join("\n"))
}

async fn run_command_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let name = required_str(args, "name")?;
    let wait_secs = optional_u64(args, "wait_secs", 30).clamp(1, 600);
    let snap = settled_snapshot(client).await;
    let project = find_project(&snap, &wanted)?;
    if !project.commands.iter().any(|command| command.name == name) {
        return Err(unknown_command_message(project, &name));
    }
    let target = format!("run {name} in {}", project.name);
    let op = client
        .dispatch(Action::RunProjectCommand { id: project.id.clone(), name: name.clone() })
        .await
        .map_err(|e| dispatch_error(&target, &e.to_string(), snap.read_only))?;
    let mut events = client
        .operation_events(op)
        .await
        .map_err(|e| format!("{target}: cannot follow the operation: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    let mut output = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = match tokio::time::timeout(remaining, events.next()).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(format!("{target}: the event stream ended before the command finished"));
            }
            // Deliberately not cancelled: a dev server hitting this timeout
            // is the expected case, not a failure.
            Err(_) => {
                let mut out = vec![format!(
                    "\"{name}\" is still running after {wait_secs}s and was left running — \
                     dev servers never exit, so this is usually the success case. Output so far:"
                )];
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Ok(out.join("\n"));
            }
        };
        match event.kind {
            OperationEventKind::Output { line, .. } => output.push(line),
            OperationEventKind::Completed => {
                let mut out = vec![format!("\"{name}\" completed")];
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Ok(out.join("\n"));
            }
            OperationEventKind::Failed { error } => {
                let mut out = vec![format!("\"{name}\" failed — {error}")];
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Err(out.join("\n"));
            }
            OperationEventKind::Cancelled => return Err(format!("\"{name}\" was cancelled")),
            _ => {}
        }
    }
}

async fn wait_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let timeout_secs = optional_u64(args, "timeout_secs", 120).clamp(1, 900);
    let snap = settled_snapshot(client).await;
    let (id, name) = resolve_project(&snap, &wanted)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let snap = client.snapshot().await.map_err(|e| format!("cannot read engine state: {e}"))?;
        let Some(project) = snap.projects.iter().find(|p| p.id == id) else {
            return Err(format!("{name} is no longer known to the engine"));
        };
        match project.status {
            ProjectStatus::Running => {
                return Ok(format!(
                    "{name} is running ({} of {} services up)",
                    running_count(project),
                    project.services.len()
                ));
            }
            // Failed cannot progress on its own — waiting out the clock
            // would just cost the agent two minutes to learn the same thing.
            ProjectStatus::Failed => {
                return Err(format!(
                    "{name} is failed{} — mast_logs or mast_captures will say why",
                    unhealthy_note(project)
                ));
            }
            status => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "{name} is still {} after {timeout_secs}s{}",
                        status_word(status),
                        unhealthy_note(project)
                    ));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn history_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let limit = optional_u64(args, "limit", 20).clamp(1, 200) as usize;
    let entries =
        client.history_recent().await.map_err(|e| format!("cannot read history: {e}"))?;
    let shown: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|entry| entry.origin == HistoryOrigin::User)
        .rev()
        .take(limit)
        .collect();
    if shown.is_empty() {
        return Ok("no history yet — history lives in the running engine and fills as Mast \
                   runs things"
            .into());
    }
    let now = now_unix_ms();
    let mut out = Vec::new();
    for entry in shown.iter().rev() {
        let took = entry
            .duration_ms
            .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
            .unwrap_or_default();
        out.push(format!(
            "{}  {} — {}{took}",
            age(entry.at_unix_ms, now),
            entry.label,
            outcome_word(&entry.outcome)
        ));
        match &entry.detail {
            HistoryDetail::Command { argv, cwd, .. } => {
                let argv =
                    argv.iter().map(|arg| shell_quote(arg)).collect::<Vec<_>>().join(" ");
                out.push(match cwd {
                    Some(cwd) => format!("  $ {argv}  (in {cwd})"),
                    None => format!("  $ {argv}"),
                });
            }
            HistoryDetail::FileWrite { path, summary } => {
                out.push(format!("  wrote {path}"));
                for line in summary {
                    out.push(format!("    {line}"));
                }
            }
        }
    }
    Ok(out.join("\n"))
}

// ---------- rendering ----------

fn find_project<'snap>(
    snap: &'snap EngineSnapshot,
    wanted: &str,
) -> Result<&'snap ProjectSummary, String> {
    let (id, _) = resolve_project(snap, wanted)?;
    snap.projects
        .iter()
        .find(|project| project.id == id)
        .ok_or_else(|| format!("no project named \"{wanted}\""))
}

fn render_overview(snap: &EngineSnapshot) -> String {
    let mut out = Vec::new();
    out.push(match (snap.docker.available, &snap.docker.error) {
        (true, _) => format!(
            "docker: connected ({})",
            snap.docker.context_name.as_deref().unwrap_or("default")
        ),
        (false, Some(e)) => format!("docker: unavailable — {e}"),
        (false, None) => "docker: connecting…".to_string(),
    });
    if snap.read_only {
        out.push("read-only: another Mast instance owns mutation (the desktop app?)".into());
    }
    if snap.projects.is_empty() {
        out.push("no projects imported — add them in the Mast desktop app".into());
        return out.join("\n");
    }
    for project in &snap.projects {
        out.push(String::new());
        out.extend(project_block(project, false));
    }
    if !snap.workspaces.is_empty() {
        let names: HashMap<&str, &str> = snap
            .projects
            .iter()
            .map(|project| (project.id.0.as_str(), project.name.as_str()))
            .collect();
        out.push(String::new());
        out.push("workspaces:".into());
        for workspace in &snap.workspaces {
            let members = workspace
                .members
                .iter()
                .map(|member| {
                    names.get(member.project.0.as_str()).copied().unwrap_or(member.project.0.as_str())
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!(
                "  {} — {} ({members})",
                workspace.name,
                status_word(workspace.status)
            ));
            if let Some(e) = &workspace.graph_error {
                out.push(format!("    graph error: {e}"));
            }
            for warning in &workspace.warnings {
                out.push(format!("    warning: {warning}"));
            }
        }
    }
    out.join("\n")
}

fn project_block(project: &ProjectSummary, detailed: bool) -> Vec<String> {
    let mut out = Vec::new();
    let branch = match (&project.git_branch, project.git_dirty) {
        (Some(branch), Some(true)) => format!("  [{branch}, dirty]"),
        (Some(branch), _) => format!("  [{branch}]"),
        _ => String::new(),
    };
    out.push(format!(
        "{} — {}, {}/{} services up{branch}",
        project.name,
        status_word(project.status),
        running_count(project),
        project.services.len(),
    ));
    out.push(format!("  path: {}", project.path));
    if let Some(url) = &project.app_url {
        out.push(format!("  app: {url}"));
    }
    if detailed {
        if let Some(compose) = &project.compose_project_name {
            out.push(format!("  compose project: {compose}"));
        }
        if let Some(php) = &project.php {
            out.push(format!("  php: {} (available: {})", php.current, php.available.join(", ")));
        }
        if let Some(url) = &project.share_url {
            out.push(format!("  share: {url}"));
        }
        if let Some(domain) = &project.local_domain {
            out.push(format!("  local domain: {domain}"));
        }
        if !project.services.is_empty() {
            out.push("  services:".into());
            for service in &project.services {
                out.push(format!("    {}", service_line(service)));
            }
        }
        if !project.commands.is_empty() {
            out.push("  commands (runnable via mast_run_command):".into());
            for command in &project.commands {
                let mut notes = Vec::new();
                if command.auto_start {
                    notes.push("auto-start");
                }
                if command.from_manifest {
                    notes.push("from mast.yml");
                }
                let notes = if notes.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", notes.join(", "))
                };
                out.push(format!("    {} — `{}`{notes}", command.name, command.command));
            }
        }
        if !project.processes.is_empty() {
            out.push("  laravel processes:".into());
            for process in &project.processes {
                out.push(format!(
                    "    {} — {}",
                    process.title,
                    if process.running { "running" } else { "stopped" }
                ));
            }
        }
    } else {
        if !project.services.is_empty() {
            out.push(format!(
                "  services: {}",
                project.services.iter().map(service_line).collect::<Vec<_>>().join("; ")
            ));
        }
        if !project.commands.is_empty() {
            out.push(format!(
                "  commands: {}",
                project
                    .commands
                    .iter()
                    .map(|command| {
                        if command.auto_start {
                            format!("{} (auto-start)", command.name)
                        } else {
                            command.name.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !project.processes.is_empty() {
            out.push(format!(
                "  processes: {}",
                project
                    .processes
                    .iter()
                    .map(|process| {
                        format!(
                            "{} ({})",
                            process.title,
                            if process.running { "running" } else { "stopped" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for warning in &project.warnings {
        out.push(format!("  warning: {warning}"));
    }
    if let Some(e) = &project.resolution_error {
        out.push(format!("  resolution error: {e}"));
    }
    out
}

fn service_line(service: &ServiceState) -> String {
    let mut line = format!("{} {}", service.name, state_word(service.state));
    match service.health {
        ServiceHealth::Healthy => line.push_str(" (healthy)"),
        ServiceHealth::Unhealthy => line.push_str(" (UNHEALTHY)"),
        ServiceHealth::Starting => line.push_str(" (health: starting)"),
        ServiceHealth::Unknown => {}
    }
    if let Some(port) = service.db_port {
        line.push_str(&format!(", db port {port}"));
    }
    if let Some(url) = &service.ui_url {
        line.push_str(&format!(", ui {url}"));
    }
    if service.orphaned {
        line.push_str(" — orphaned (no longer in the compose file; rebuild reaps it)");
    }
    line
}

fn state_word(state: Option<ContainerState>) -> &'static str {
    match state {
        None => "not created",
        Some(ContainerState::Created) => "created",
        Some(ContainerState::Running) => "running",
        Some(ContainerState::Restarting) => "restarting",
        Some(ContainerState::Paused) => "paused",
        Some(ContainerState::Exited) => "exited",
        Some(ContainerState::Dead) => "dead",
        Some(ContainerState::Removing) => "removing",
        Some(ContainerState::Unknown) => "unknown",
    }
}

fn status_word(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Stopped => "stopped",
        ProjectStatus::Starting => "starting",
        ProjectStatus::Running => "running",
        ProjectStatus::Degraded => "degraded",
        ProjectStatus::Failed => "failed",
    }
}

fn running_count(project: &ProjectSummary) -> usize {
    project
        .services
        .iter()
        .filter(|service| service.state == Some(ContainerState::Running))
        .count()
}

fn unhealthy_note(project: &ProjectSummary) -> String {
    let down: Vec<&str> = project
        .services
        .iter()
        .filter(|service| service.state != Some(ContainerState::Running))
        .map(|service| service.name.as_str())
        .collect();
    let unhealthy: Vec<&str> = project
        .services
        .iter()
        .filter(|service| service.health == ServiceHealth::Unhealthy)
        .map(|service| service.name.as_str())
        .collect();
    let mut notes = Vec::new();
    if !down.is_empty() {
        notes.push(format!("not running: {}", down.join(", ")));
    }
    if !unhealthy.is_empty() {
        notes.push(format!("unhealthy: {}", unhealthy.join(", ")));
    }
    if notes.is_empty() { String::new() } else { format!(" — {}", notes.join("; ")) }
}

fn reason_phrase(reason: &CaptureReason) -> String {
    match reason {
        CaptureReason::Teardown { verb } => format!("taken before {verb}"),
        CaptureReason::Exited { status: Some(status) } => format!("exited (status {status})"),
        CaptureReason::Exited { status: None } => "exited".into(),
        CaptureReason::Unhealthy => "went unhealthy".into(),
        CaptureReason::ReadyTimeout => "readiness wait gave up".into(),
        CaptureReason::Manual => "captured on request".into(),
    }
}

fn outcome_word(outcome: &HistoryOutcome) -> String {
    match outcome {
        HistoryOutcome::Running => "running".into(),
        HistoryOutcome::Exited { status: 0 } => "ok".into(),
        HistoryOutcome::Exited { status } => format!("exit {status}"),
        HistoryOutcome::Cancelled => "cancelled".into(),
        HistoryOutcome::Failed { error } => format!("failed: {error}"),
        HistoryOutcome::Detached => "launched".into(),
        HistoryOutcome::Applied => "applied".into(),
    }
}

fn unknown_command_message(project: &ProjectSummary, name: &str) -> String {
    let available: Vec<&str> =
        project.commands.iter().map(|command| command.name.as_str()).collect();
    if available.is_empty() {
        format!(
            "{} has no saved commands — define them in the desktop app or a committed mast.yml",
            project.name
        )
    } else {
        format!(
            "no saved command named \"{name}\" in {} — available: {}",
            project.name,
            available.join(", ")
        )
    }
}

/// The one read-only hint worth adding: an embedded engine behind the
/// desktop app's ownership lock refuses mutation, and "read-only" alone does
/// not tell an agent who to blame. When the daemon serves this session,
/// dispatch simply succeeds and none of this fires.
/// Data snapshots for one project, newest first. Restore is deliberately
/// absent from this surface — overwriting live data is a decision for the
/// person, in the app, behind its armed confirm.
async fn snapshots_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let snap = settled_snapshot(client).await;
    let (id, name) = resolve_project(&snap, &wanted)?;
    let snapshots = client
        .volume_snapshots(id)
        .await
        .map_err(|e| format!("cannot list {name}'s snapshots: {e}"))?;
    if snapshots.is_empty() {
        return Ok(format!("{name}: no data snapshots — take one with mast_snapshot_data"));
    }
    let now = now_unix_ms();
    let mut out =
        vec![format!("{name} — {} data snapshot(s), newest first:", snapshots.len())];
    for snapshot in snapshots {
        out.push(format!(
            "  {} — {}, {} ago, volumes: {}",
            snapshot.group,
            snapshot.service,
            age(snapshot.at_unix_ms, now),
            snapshot.volumes.join(", ")
        ));
    }
    out.push(
        "Restoring overwrites live data, so restores happen in the Mast app, not from here."
            .into(),
    );
    Ok(out.join("\n"))
}

/// Take a snapshot of one service's data volumes and stream the copy to
/// completion — the pre-flight for anything destructive an agent is about
/// to run against the database.
async fn snapshot_take_tool(client: &dyn MastClient, args: &Value) -> Result<String, String> {
    let wanted = required_str(args, "project")?;
    let service = required_str(args, "service")?;
    let snap = settled_snapshot(client).await;
    let (id, name) = resolve_project(&snap, &wanted)?;
    let target = format!("{name}/{service}");
    let op = client
        .dispatch(Action::SnapshotServiceData { id, service: service.clone() })
        .await
        .map_err(|e| dispatch_error(&target, &e.to_string(), snap.read_only))?;
    let mut events = client
        .operation_events(op)
        .await
        .map_err(|e| format!("{target}: cannot follow the operation: {e}"))?;
    let mut output = Vec::new();
    while let Some(event) = events.next().await {
        match event.kind {
            OperationEventKind::Output { line, .. } => output.push(line),
            OperationEventKind::Completed => {
                let mut out = vec![format!("{target}: snapshot saved")];
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Ok(out.join("\n"));
            }
            OperationEventKind::Failed { error } => {
                let mut out = vec![format!("{target}: snapshot failed — {error}")];
                out.extend(keep_tail(output, OUTPUT_CAP));
                return Err(out.join("\n"));
            }
            OperationEventKind::Cancelled => {
                return Err(format!("{target}: snapshot cancelled"));
            }
            _ => {}
        }
    }
    Err(format!("{target}: the operation stream ended without a result"))
}

fn dispatch_error(target: &str, error: &str, read_only: bool) -> String {
    if read_only {
        format!("{target}: {error} (the desktop app owns mutation — control it from there for now)")
    } else {
        format!("{target}: {error}")
    }
}

fn is_error_level(level: &str) -> bool {
    matches!(level, "ERROR" | "CRITICAL" | "ALERT" | "EMERGENCY")
}

fn keep_tail(lines: Vec<String>, cap: usize) -> Vec<String> {
    if lines.len() <= cap {
        return lines;
    }
    let dropped = lines.len() - cap;
    let mut kept = vec![format!("… {dropped} earlier line(s) omitted")];
    kept.extend(lines.into_iter().skip(dropped));
    kept
}

fn keep_head(text: &str, cap: usize) -> String {
    let total = text.lines().count();
    if total <= cap {
        text.to_string()
    } else {
        let head: Vec<&str> = text.lines().take(cap).collect();
        format!("{}\n… {} more line(s) omitted", head.join("\n"), total - cap)
    }
}

/// Rough relative age. An agent reading a capture needs "how stale is this",
/// not a calendar date it would have to do arithmetic on.
fn age(at_unix_ms: u64, now_unix_ms: u64) -> String {
    let secs = now_unix_ms.saturating_sub(at_unix_ms) / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_contract::{ProjectCommand, ProjectId};

    fn project(name: &str) -> ProjectSummary {
        ProjectSummary {
            id: ProjectId(format!("id-{name}")),
            name: name.into(),
            path: format!("/x/{name}"),
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
            php: None,
            share_url: None,
            share_dashboard_url: None,
            local_domain: None,
        }
    }

    fn snapshot_with(projects: Vec<ProjectSummary>) -> EngineSnapshot {
        EngineSnapshot {
            protocol_version: mast_contract::PROTOCOL_VERSION,
            seq: 0,
            read_only: false,
            docker: Default::default(),
            integrations: mast_contract::IntegrationSettings::default(),
            watched_directories: Vec::new(),
            discovered: Vec::new(),
            projects,
            workspaces: Vec::new(),
        }
    }

    #[test]
    fn the_tool_table_is_well_formed() {
        let specs = tool_specs();
        let tools = specs.as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "mast_status",
                "mast_start",
                "mast_stop",
                "mast_restart",
                "mast_rebuild",
                "mast_logs",
                "mast_laravel_log",
                "mast_captures",
                "mast_diagnose",
                "mast_run_command",
                "mast_wait",
                "mast_history",
                "mast_snapshots",
                "mast_snapshot_data",
            ]
        );
        for tool in tools {
            assert!(!tool["description"].as_str().unwrap().is_empty());
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object");
            let properties = schema["properties"].as_object().unwrap();
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    properties.contains_key(required.as_str().unwrap()),
                    "{} requires undeclared {}",
                    tool["name"],
                    required
                );
            }
        }
    }

    #[test]
    fn initialize_echoes_the_clients_protocol_version() {
        let result = initialize_result(&json!({"protocolVersion": "2024-11-05"}));
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "mast");
        assert_eq!(result["serverInfo"]["version"], mast_contract::BUILD_VERSION);
    }

    #[test]
    fn initialize_defaults_the_protocol_version() {
        assert_eq!(initialize_result(&Value::Null)["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn missing_arguments_come_back_as_sentences() {
        let err = required_str(&json!({}), "project").unwrap_err();
        assert!(err.contains("project"), "{err}");
        assert_eq!(required_str(&json!({"project": "api"}), "project").unwrap(), "api");
    }

    #[test]
    fn numeric_and_bool_arguments_default_sensibly() {
        assert_eq!(optional_u64(&json!({}), "tail", 100), 100);
        assert_eq!(optional_u64(&json!({"tail": 5}), "tail", 100), 5);
        assert!(!optional_bool(&json!({}), "errors_only"));
        assert!(optional_bool(&json!({"errors_only": true}), "errors_only"));
    }

    #[test]
    fn tool_failures_are_iserror_results_not_protocol_errors() {
        let failed = tool_result(Err("the container is gone".into()));
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["content"][0]["text"], "the container is gone");
        assert_eq!(tool_result(Ok("fine".into()))["isError"], false);
    }

    #[test]
    fn error_responses_are_json_rpc_shaped() {
        let raw = error_response(Value::Null, -32700, "parse error");
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], Value::Null);
        assert_eq!(value["error"]["code"], -32700);
    }

    #[test]
    fn keep_tail_drops_the_front_and_says_so() {
        let lines: Vec<String> = (1..=5).map(|i| format!("line {i}")).collect();
        let kept = keep_tail(lines.clone(), 2);
        assert_eq!(kept, ["… 3 earlier line(s) omitted", "line 4", "line 5"]);
        assert_eq!(keep_tail(lines.clone(), 10), lines);
    }

    #[test]
    fn keep_head_clips_stack_traces_from_the_bottom() {
        let trace = "top\nmiddle\nbottom";
        assert_eq!(keep_head(trace, 2), "top\nmiddle\n… 1 more line(s) omitted");
        assert_eq!(keep_head(trace, 3), trace);
    }

    #[test]
    fn ages_pick_the_largest_readable_unit() {
        assert_eq!(age(0, 30_000), "30s ago");
        assert_eq!(age(0, 90_000), "1m ago");
        assert_eq!(age(0, 7_200_000), "2h ago");
        assert_eq!(age(0, 172_800_000), "2d ago");
    }

    #[test]
    fn unknown_commands_list_what_is_available() {
        let mut p = project("api");
        p.commands.push(ProjectCommand {
            name: "dev".into(),
            command: "npm run dev".into(),
            auto_start: false,
            cwd: None,
            after: None,
            ready_when: None,
            auto_restart: false,
            restart_when_changed: Vec::new(),
            from_manifest: false,
        });
        let err = unknown_command_message(&p, "serve");
        assert!(err.contains("serve") && err.contains("dev"), "{err}");
        p.commands.clear();
        assert!(unknown_command_message(&p, "serve").contains("no saved commands"));
    }

    #[test]
    fn the_overview_names_projects_and_their_services() {
        let mut p = project("api");
        p.services.push(ServiceState {
            name: "mysql".into(),
            container_id: None,
            state: Some(ContainerState::Running),
            health: ServiceHealth::Healthy,
            ui_url: None,
            db_port: Some(3306),
            orphaned: false,
            data_volumes: Vec::new(),
        });
        let text = render_overview(&snapshot_with(vec![p]));
        assert!(text.contains("api — stopped, 1/1 services up"), "{text}");
        assert!(text.contains("mysql running (healthy), db port 3306"), "{text}");
    }

    #[test]
    fn read_only_dispatch_errors_name_the_owner() {
        let err = dispatch_error("api", "read-only: refused", true);
        assert!(err.contains("desktop app owns mutation"), "{err}");
        assert!(!dispatch_error("api", "conflict", false).contains("desktop"), "no hint expected");
    }
}
