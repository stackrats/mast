//! Effect history (M9): every subprocess Mast spawns, plus every config file
//! it writes, recorded with its outcome so the user can see — and copy — what
//! actually ran.
//!
//! Coverage comes from the observer hook in `mast-docker` rather than from
//! call sites remembering to record, so a shell-out added anywhere in the
//! workspace shows up here for free. What a call site *does* supply is
//! context: a task-local [`CommandContext`] naming the user action behind the
//! command. Without one, a command is background upkeep — resolution,
//! probes, reconciliation — and clients hide it by default.
//!
//! Everything recorded is redacted first: argv, env overlay, and output all
//! pass through the union of every project's `.env` redactor, because a
//! background command belongs to no single project but can still echo one's
//! secrets.

use std::sync::{Arc, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mast_contract::{
    HistoryDetail, HistoryEntry, HistoryEnvVar, HistoryOrigin, HistoryOutcome, OperationId,
    ProjectId,
};
use mast_docker::{CommandFinish, CommandObserver, CommandStart};

use crate::{Engine, Inner};

/// Entries kept. Beyond this the oldest are dropped; a client that missed
/// them missed them (history is a transparency aid, not an audit log).
pub(crate) const HISTORY_CAPACITY: usize = 300;

/// What a user action was, for commands spawned underneath it.
#[derive(Debug, Clone)]
pub(crate) struct CommandContext {
    pub label: String,
    pub project: Option<ProjectId>,
    pub operation: Option<OperationId>,
}

tokio::task_local! {
    static CONTEXT: CommandContext;
}

/// Run `fut` with every command it spawns attributed to `ctx`. Task-locals do
/// not cross `tokio::spawn`, so this wraps the task that awaits the command,
/// not the dispatch that created it.
pub(crate) async fn with_context<F: std::future::Future>(ctx: CommandContext, fut: F) -> F::Output {
    CONTEXT.scope(ctx, fut).await
}

pub(crate) fn current_context() -> Option<CommandContext> {
    CONTEXT.try_with(|ctx| ctx.clone()).ok()
}

/// Bridges `mast-docker`'s process-wide hook to this engine. Holds a weak
/// reference so an engine that goes away stops recording instead of leaking.
pub(crate) struct EngineObserver {
    inner: Weak<Inner>,
}

impl EngineObserver {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        Self { inner: Arc::downgrade(inner) }
    }

    fn engine(&self) -> Option<Engine> {
        self.inner.upgrade().map(|inner| Engine { inner })
    }
}

impl CommandObserver for EngineObserver {
    fn started(&self, start: &CommandStart<'_>) -> u64 {
        let Some(engine) = self.engine() else { return 0 };
        engine.record_command_start(start)
    }

    fn finished(&self, id: u64, finish: CommandFinish, output_tail: &[String]) {
        let Some(engine) = self.engine() else { return };
        engine.record_command_finish(id, finish, output_tail);
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Label for a command nobody claimed: the program plus the subcommand verbs
/// that follow it — `docker compose -f /x/compose.yaml config` → "docker
/// compose config".
fn label_from_argv(argv: &[String]) -> String {
    let Some(program) = argv.first() else { return "command".to_string() };
    let program = program.rsplit('/').next().unwrap_or(program);
    let mut rest: Vec<&str> = Vec::new();
    // Flags and the values that follow them are noise; the verbs are the
    // point. A flag with no value costs at most one verb from the label.
    let mut after_flag = false;
    for arg in &argv[1..] {
        if arg.starts_with('-') {
            after_flag = true;
            continue;
        }
        if std::mem::take(&mut after_flag) {
            continue;
        }
        // The first operand ends the verbs. Labelling a container scan
        // "docker exec 4eda4849e1ef…" buries the one word that mattered.
        if !is_verb(arg) || rest.len() == 2 {
            break;
        }
        rest.push(arg);
    }
    if rest.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", rest.join(" "))
    }
}

/// A subcommand word rather than an operand: short, lowercase, no path or id
/// shape.
fn is_verb(arg: &str) -> bool {
    !arg.is_empty()
        && arg.len() <= 16
        && arg.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

impl Engine {
    /// Start recording the subprocesses this process spawns. The registry
    /// holds observers weakly, so the engine must keep its own alive.
    pub(crate) fn install_command_observer(&self) {
        let observer: Arc<dyn CommandObserver> = Arc::new(EngineObserver::new(&self.inner));
        mast_docker::register_command_observer(&observer);
        *self.inner.command_observer.lock().unwrap() = Some(observer);
    }

    fn record(&self, entry: HistoryEntry) {
        let mut history = self.inner.history.lock().unwrap();
        history.push_back(entry.clone());
        while history.len() > HISTORY_CAPACITY {
            history.pop_front();
        }
        drop(history);
        let _ = self.inner.history_tx.send(entry);
    }

    pub(crate) fn record_command_start(&self, start: &CommandStart<'_>) -> u64 {
        let id = self.inner.next_history.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let context = current_context();
        let redactor = self.inner.state.lock().unwrap().redactor_all.clone();
        // Argv is a list of tokens, not prose: redact per token so a path that
        // merely contains a short secret survives intact (see `redact_token`).
        let argv: Vec<String> =
            start.argv.iter().map(|arg| redactor.redact_token(arg)).collect();
        let entry = HistoryEntry {
            id,
            at_unix_ms: now_ms(),
            // Derived from the already-redacted argv, so the label cannot leak
            // what the command line does not.
            label: context
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_else(|| label_from_argv(&argv)),
            project: context.as_ref().and_then(|c| c.project.clone()),
            operation: context.as_ref().and_then(|c| c.operation),
            origin: if context.is_some() { HistoryOrigin::User } else { HistoryOrigin::Background },
            detail: HistoryDetail::Command {
                argv,
                cwd: start.cwd.map(|p| p.to_string_lossy().into_owned()),
                env: start
                    .env_overlay
                    .iter()
                    .map(|(key, value)| {
                        let masked = mast_laravel::is_secret_key(key);
                        HistoryEnvVar {
                            key: key.clone(),
                            value: if masked {
                                crate::redact::REDACTED.to_string()
                            } else {
                                redactor.redact_token(value)
                            },
                            masked,
                        }
                    })
                    .collect(),
                streaming: start.streaming,
            },
            // A detached launch is over the moment it is handed to the OS.
            outcome: if start.detached {
                HistoryOutcome::Detached
            } else {
                HistoryOutcome::Running
            },
            duration_ms: None,
            output: Vec::new(),
        };
        if !start.detached {
            self.inner.history_started.lock().unwrap().insert(id, Instant::now());
        }
        self.record(entry);
        id
    }

    pub(crate) fn record_command_finish(
        &self,
        id: u64,
        finish: CommandFinish,
        output_tail: &[String],
    ) {
        let started = self.inner.history_started.lock().unwrap().remove(&id);
        // A detached launch reports its own start as terminal; nothing to do.
        if matches!(finish, CommandFinish::Detached) {
            return;
        }
        let redactor = self.inner.state.lock().unwrap().redactor_all.clone();
        let outcome = match finish {
            CommandFinish::Exited(status) => HistoryOutcome::Exited { status },
            CommandFinish::Cancelled => HistoryOutcome::Cancelled,
            CommandFinish::Failed(error) => HistoryOutcome::Failed { error: redactor.redact(&error) },
            CommandFinish::Detached => HistoryOutcome::Detached,
        };
        let mut history = self.inner.history.lock().unwrap();
        // The entry can have aged out of the ring under a long-running
        // command; there is then nothing left to update.
        let Some(entry) = history.iter_mut().find(|entry| entry.id == id) else { return };
        entry.outcome = outcome;
        entry.duration_ms = started.map(|at| at.elapsed().as_millis() as u64);
        entry.output = output_tail.iter().map(|line| redactor.redact(line)).collect();
        let updated = entry.clone();
        drop(history);
        let _ = self.inner.history_tx.send(updated);
    }

    /// Record a config write. Not a subprocess, but the same promise applies:
    /// a change Mast made to the developer's machine is visible and named.
    pub(crate) fn record_file_write(
        &self,
        label: &str,
        project: Option<ProjectId>,
        path: &str,
        summary: Vec<String>,
        error: Option<String>,
    ) {
        let redactor = self.inner.state.lock().unwrap().redactor_all.clone();
        let id = self.inner.next_history.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let context = current_context();
        self.record(HistoryEntry {
            id,
            at_unix_ms: now_ms(),
            label: label.to_string(),
            project,
            operation: context.as_ref().and_then(|c| c.operation),
            origin: HistoryOrigin::User,
            detail: HistoryDetail::FileWrite {
                path: path.to_string(),
                summary: summary.iter().map(|line| redactor.redact(line)).collect(),
            },
            outcome: match error {
                Some(error) => HistoryOutcome::Failed { error: redactor.redact(&error) },
                None => HistoryOutcome::Applied,
            },
            duration_ms: None,
            output: Vec::new(),
        });
    }

    /// Record a config write under the current action's label — the shape
    /// every `.env` and compose edit uses.
    pub(crate) fn record_config_write<T>(
        &self,
        path: &std::path::Path,
        summary: Vec<String>,
        result: &Result<T, mast_contract::ErrorInfo>,
    ) {
        let context = current_context();
        self.record_file_write(
            &context.as_ref().map(|c| c.label.clone()).unwrap_or_else(|| "Config write".into()),
            context.and_then(|c| c.project),
            &path.to_string_lossy(),
            summary,
            result.as_ref().err().map(|e| e.to_string()),
        );
    }

    /// The current history ring, oldest first.
    pub fn history_recent(&self) -> Vec<HistoryEntry> {
        self.inner.history.lock().unwrap().iter().cloned().collect()
    }

    /// Live history: new entries and updates to existing ones, both delivered
    /// as whole entries. Clients upsert by [`HistoryEntry::id`].
    pub fn subscribe_history(&self) -> futures::stream::BoxStream<'static, HistoryEntry> {
        use futures::StreamExt;
        let mut rx = self.inner.history_tx.subscribe();
        let (tx, out_rx) = tokio::sync::mpsc::channel::<HistoryEntry>(256);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(entry) => {
                        if tx.send(entry).await.is_err() {
                            return;
                        }
                    }
                    // History is a transparency aid, not an audit log: a
                    // lagging subscriber skips entries rather than resyncing.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|entry| (entry, rx))
        })
        .boxed()
    }
}

/// A human label for an action, derived from its wire tag so new actions get
/// a sensible label without anyone updating a match arm. `startService` on
/// project "acme" with service "redis" → "Start service — acme (redis)".
pub(crate) fn describe_action(
    action: &mast_contract::Action,
    project_name: impl Fn(&str) -> Option<String>,
) -> (String, Option<ProjectId>) {
    let value = serde_json::to_value(action).unwrap_or(serde_json::Value::Null);
    let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("action");
    let mut label = humanize(kind);

    // `id` is a project id only when it names a project we know — workspace
    // actions carry a workspace id in the same field.
    let id = value.get("id").and_then(|v| v.as_str());
    let name = id.and_then(&project_name);
    let project = match (&name, id) {
        (Some(_), Some(id)) => Some(ProjectId(id.to_string())),
        _ => None,
    };
    if let Some(name) = name {
        label.push_str(" — ");
        label.push_str(&name);
    }
    let detail: Vec<&str> = ["service", "name", "key", "process", "path"]
        .iter()
        .filter_map(|field| value.get(*field).and_then(|v| v.as_str()))
        .collect();
    if !detail.is_empty() {
        label.push_str(&format!(" ({})", detail.join(" ")));
    }
    (label, project)
}

/// "startService" → "Start service".
fn humanize(camel: &str) -> String {
    let mut out = String::new();
    for (i, ch) in camel.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            out.push(' ');
            out.push(ch.to_ascii_lowercase());
        } else if i == 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unclaimed_commands_are_labelled_from_their_argv() {
        let argv: Vec<String> =
            ["docker", "compose", "-f", "/x/compose.yaml", "config"].map(String::from).to_vec();
        assert_eq!(label_from_argv(&argv), "docker compose config");
        assert_eq!(label_from_argv(&["/usr/bin/git".to_string()]), "git");
        assert_eq!(label_from_argv(&[]), "command");

        // An operand ends the verbs: a container id is not part of the name.
        let scan: Vec<String> =
            ["docker", "exec", "4eda4849e1ef5f6b6c1e1ccc422f", "sh", "-c", "for p in /proc"]
                .map(String::from)
                .to_vec();
        assert_eq!(label_from_argv(&scan), "docker exec");

        // Profiles are flag values, not verbs.
        let profiled: Vec<String> =
            ["docker", "compose", "-f", "/x/compose.yaml", "--profile", "debug", "up", "-d"]
                .map(String::from)
                .to_vec();
        assert_eq!(label_from_argv(&profiled), "docker compose up");
    }

    #[test]
    fn action_labels_carry_the_project_name_and_detail() {
        let names = |id: &str| (id == "p1").then(|| "acme".to_string());
        let (label, project) = describe_action(
            &mast_contract::Action::StartService {
                id: ProjectId("p1".into()),
                service: "redis".into(),
            },
            names,
        );
        assert_eq!(label, "Start service — acme (redis)");
        assert_eq!(project, Some(ProjectId("p1".into())));

        // A workspace id is not a project id, so it must not be attributed.
        let (label, project) = describe_action(
            &mast_contract::Action::StartWorkspace {
                id: mast_contract::WorkspaceId("w1".into()),
            },
            names,
        );
        assert_eq!(label, "Start workspace");
        assert_eq!(project, None);
    }
}
