//! Command observation: a process-wide hook every subprocess reports to.
//!
//! Mast's promise is that terminal commands are the mechanism (ADR-0001), so
//! the user is entitled to see the argv Mast actually ran. Rather than thread
//! a recorder through every call site, the three spawn primitives in
//! [`crate::command`] notify an installed observer — which means coverage is
//! automatic: a new shell-out anywhere in the workspace is recorded without
//! its author remembering to record it.

use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// A command about to be spawned. Borrowed — the observer copies what it keeps.
#[derive(Debug)]
pub struct CommandStart<'a> {
    pub argv: &'a [String],
    pub cwd: Option<&'a Path>,
    pub env_overlay: &'a [(String, String)],
    /// Streamed (lifecycle/long-running) rather than captured.
    pub streaming: bool,
    /// Launched detached — it outlives Mast, so there is no outcome to await.
    pub detached: bool,
}

/// How a recorded command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandFinish {
    Exited(i32),
    Cancelled,
    /// Never ran, or died in a way that produced no status (spawn failure,
    /// timeout).
    Failed(String),
    /// Detached launch: handed to the OS, outcome unknowable.
    Detached,
}

pub trait CommandObserver: Send + Sync {
    /// Called before spawning. The returned id is handed back to
    /// [`CommandObserver::finished`]; the observer chooses its meaning.
    fn started(&self, start: &CommandStart<'_>) -> u64;
    /// Called once the command is over. `output_tail` is the last few lines of
    /// combined output, already truncated by the caller.
    fn finished(&self, id: u64, finish: CommandFinish, output_tail: &[String]);
}

type Registry = RwLock<Vec<Weak<dyn CommandObserver>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register an observer. Held weakly and additively rather than as a single
/// replaceable slot: a second engine in the same process (tests do this) must
/// not silently switch off the first engine's history, and an engine that is
/// dropped must stop being notified.
pub fn register_command_observer(observer: &Arc<dyn CommandObserver>) {
    if let Ok(mut guard) = registry().write() {
        guard.retain(|weak| weak.strong_count() > 0);
        guard.push(Arc::downgrade(observer));
    }
}

pub(crate) fn observers() -> Vec<Arc<dyn CommandObserver>> {
    registry()
        .read()
        .map(|guard| guard.iter().filter_map(Weak::upgrade).collect())
        .unwrap_or_default()
}

/// One in-flight command, per observer that asked to see it.
pub(crate) type Watchers = Vec<(Arc<dyn CommandObserver>, u64)>;

pub(crate) fn start_all(start: &CommandStart<'_>) -> Watchers {
    observers()
        .into_iter()
        .map(|observer| {
            let id = observer.started(start);
            (observer, id)
        })
        .collect()
}

pub(crate) fn finish_all(watchers: Watchers, finish: CommandFinish, output_tail: &[String]) {
    for (observer, id) in watchers {
        observer.finished(id, finish.clone(), output_tail);
    }
}

/// Lines kept per command, and their per-line clip. Bounded twice over: the
/// history ring holds hundreds of entries and must not grow without limit.
pub(crate) const TAIL_LINES: usize = 25;
pub(crate) const TAIL_LINE_CHARS: usize = 240;

/// Last [`TAIL_LINES`] lines of `text`, each clipped, oldest first.
pub(crate) fn tail_of(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].iter().map(|line| clip(line)).collect()
}

pub(crate) fn clip(line: &str) -> String {
    if line.chars().count() <= TAIL_LINE_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(TAIL_LINE_CHARS).collect();
    out.push('…');
    out
}
