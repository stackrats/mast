//! Log captures (M10): a container's last words, kept after the container is
//! gone.
//!
//! `Engine::service_logs` follows one container id and ends when that
//! container does. That is the right shape for watching a running service and
//! the wrong shape for understanding a dead one: by the time a developer knows
//! they wanted the output, `up -d --force-recreate` has removed the container
//! and Docker has started a fresh log.
//!
//! So captures are read at the two moments the evidence still exists:
//!
//! - **before Mast destroys it** — ahead of stop/restart/rebuild, since a
//!   recreate removes the container;
//! - **after Mast observes a death** — on the reconcile pass that first sees
//!   an unexpected exit or an unhealthy transition. Docker keeps an exited
//!   container's log until removal, so this read is not a race.
//!
//! Nothing is followed and nothing is buffered continuously: one bounded read
//! per capture, not a stream per service.
//!
//! Captures are **persisted and copyable**, which is why every line passes
//! through the union redactor first. That is the opposite of the rule for live
//! streams (see `redact.rs`) and the difference is exactly persistence — see
//! ADR-0005.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use mast_contract::{CaptureReason, LogCapture, LogCaptureLine, ProjectId};
use rusqlite::Connection;

use crate::{internal_err, Engine};

/// How far back a capture reaches. "The last minute before it went down" is
/// the ask; a minute of a chatty dev container is also about as much as a
/// person will read.
pub const CAPTURE_WINDOW_SECS: u32 = 60;

/// Ceiling on lines kept per capture. A `vite` container can emit thousands a
/// minute; the tail is the part that explains the ending.
pub const CAPTURE_MAX_LINES: u32 = 200;

/// Captures kept before the oldest are pruned.
pub const CAPTURE_RETENTION_COUNT: u32 = 200;

/// Age beyond which a capture is pruned regardless of count. Long enough to
/// cover "it died over the weekend", short enough that a forgotten database
/// does not accumulate a developer's application output indefinitely.
pub const CAPTURE_RETENTION_DAYS: u64 = 14;

/// Repeat-capture suppression window, keyed by container. Stops a teardown
/// capture and the reconcile that observes the same teardown from recording
/// twice, and stops a container flapping in and out of `unhealthy` from
/// filling the tab with near-identical rows.
const DEDUP_WINDOW_MS: u64 = 30_000;

/// How long a capture may take before the thing it is delaying gives up on it.
/// A teardown waits for its capture — a missing post-mortem is a worse UI, a
/// blocked restart is a broken app.
pub(crate) const CAPTURE_TIMEOUT_SECS: u64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("captures db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("captures db: {0}")]
    Encode(#[from] serde_json::Error),
}

/// One capture request, resolved against engine state but not yet read. The
/// reconcile fold produces these inside a lock where it cannot await, so
/// resolution and I/O are deliberately separate steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureRequest {
    pub project: ProjectId,
    pub project_name: String,
    pub service: String,
    pub container_id: String,
    pub reason: CaptureReason,
}

pub(crate) struct CaptureDb {
    conn: Connection,
}

impl CaptureDb {
    pub fn open(path: &Path) -> Result<Self, CaptureError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS captures (
                 id INTEGER PRIMARY KEY,
                 at_unix_ms INTEGER NOT NULL,
                 project TEXT NOT NULL,
                 project_name TEXT NOT NULL,
                 service TEXT NOT NULL,
                 container_id TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 window_secs INTEGER NOT NULL,
                 truncated INTEGER NOT NULL,
                 lines TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS captures_at ON captures (at_unix_ms);
             CREATE INDEX IF NOT EXISTS captures_container \
                 ON captures (container_id, at_unix_ms);",
        )?;
        Ok(Self { conn })
    }

    /// Has this container been captured since `since_unix_ms`? The in-memory
    /// suppression window dies with the process, and the whole point of this
    /// store is that captures do not — without this, stopping a project and
    /// reopening Mast a moment later records the same lines twice.
    pub fn captured_since(
        &self,
        container_id: &str,
        since_unix_ms: u64,
    ) -> Result<bool, CaptureError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM captures WHERE container_id = ?1 AND at_unix_ms >= ?2",
            (container_id, since_unix_ms as i64),
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Insert a capture and return it with its assigned id. Lines are stored
    /// as JSON: they are read and written whole, never queried into, and a
    /// second table would buy nothing but joins.
    pub fn insert(&self, capture: &LogCapture) -> Result<u64, CaptureError> {
        self.conn.execute(
            "INSERT INTO captures \
             (at_unix_ms, project, project_name, service, container_id, reason, \
              window_secs, truncated, lines) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                capture.at_unix_ms as i64,
                &capture.project.0,
                &capture.project_name,
                &capture.service,
                &capture.container_id,
                serde_json::to_string(&capture.reason)?,
                capture.window_secs as i64,
                capture.truncated as i64,
                serde_json::to_string(&capture.lines)?,
            ),
        )?;
        Ok(self.conn.last_insert_rowid() as u64)
    }

    /// Drop everything past the count cap or the age cap. Runs after each
    /// insert: retention that only runs at startup is retention that does not
    /// hold for the session that actually produced the rows.
    pub fn prune(&self, now_unix_ms: u64) -> Result<(), CaptureError> {
        let oldest_kept = now_unix_ms.saturating_sub(CAPTURE_RETENTION_DAYS * 24 * 60 * 60 * 1000);
        self.conn
            .execute("DELETE FROM captures WHERE at_unix_ms < ?1", [oldest_kept as i64])?;
        self.conn.execute(
            "DELETE FROM captures WHERE id NOT IN \
             (SELECT id FROM captures ORDER BY id DESC LIMIT ?1)",
            [CAPTURE_RETENTION_COUNT as i64],
        )?;
        Ok(())
    }

    /// Newest first — the tab reads top-down and the newest capture is the one
    /// that explains what just happened.
    pub fn recent(&self, limit: u32) -> Result<Vec<LogCapture>, CaptureError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, at_unix_ms, project, project_name, service, container_id, \
                    reason, window_secs, truncated, lines \
             FROM captures ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            let reason: String = row.get(6)?;
            let lines: String = row.get(9)?;
            Ok((
                LogCapture {
                    id: row.get::<_, i64>(0)? as u64,
                    at_unix_ms: row.get::<_, i64>(1)? as u64,
                    project: ProjectId(row.get(2)?),
                    project_name: row.get(3)?,
                    service: row.get(4)?,
                    container_id: row.get(5)?,
                    // Replaced below; the closure cannot fail on serde.
                    reason: CaptureReason::Manual,
                    window_secs: row.get::<_, i64>(7)? as u32,
                    lines: Vec::new(),
                    truncated: row.get::<_, i64>(8)? != 0,
                },
                reason,
                lines,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (mut capture, reason, lines) = row?;
            // A row written by a newer build can carry a reason this build
            // does not know. Losing the whole capture over its label would be
            // the wrong trade — keep the lines, call it manual.
            capture.reason = serde_json::from_str(&reason).unwrap_or(CaptureReason::Manual);
            capture.lines = serde_json::from_str(&lines).unwrap_or_default();
            out.push(capture);
        }
        Ok(out)
    }

    pub fn clear(&self) -> Result<(), CaptureError> {
        self.conn.execute("DELETE FROM captures", [])?;
        Ok(())
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Engine {
    /// Resolve a capture request for one service, or `None` when there is
    /// nothing to read (no such project/service, or no container).
    pub(crate) fn capture_request(
        &self,
        project: &ProjectId,
        service: &str,
        reason: CaptureReason,
    ) -> Option<CaptureRequest> {
        let st = self.inner.state.lock().unwrap();
        let entry = st.projects.get(&project.0)?;
        let container_id =
            entry.summary.services.iter().find(|s| s.name == service)?.container_id.clone()?;
        Some(CaptureRequest {
            project: project.clone(),
            project_name: entry.summary.name.clone(),
            service: service.to_string(),
            container_id,
            reason,
        })
    }

    /// Every service of a project that currently has a container — used when a
    /// whole-project verb tears everything down at once.
    pub(crate) fn capture_requests_for_project(
        &self,
        project: &ProjectId,
        reason: CaptureReason,
    ) -> Vec<CaptureRequest> {
        let st = self.inner.state.lock().unwrap();
        let Some(entry) = st.projects.get(&project.0) else {
            return Vec::new();
        };
        entry
            .summary
            .services
            .iter()
            .filter_map(|s| {
                Some(CaptureRequest {
                    project: project.clone(),
                    project_name: entry.summary.name.clone(),
                    service: s.name.clone(),
                    container_id: s.container_id.clone()?,
                    reason: reason.clone(),
                })
            })
            .collect()
    }

    /// Has this container been captured recently enough that another capture
    /// would just be the same lines again?
    fn recently_captured(&self, container_id: &str, now_ms: u64) -> bool {
        let seen = self.inner.captures_seen.lock().unwrap();
        seen.get(container_id)
            .is_some_and(|last| now_ms.saturating_sub(*last) < DEDUP_WINDOW_MS)
    }

    /// Start the suppression window for a container. Called only once a
    /// capture has actually been **recorded** — an attempt that found nothing
    /// must not suppress the next one. A container can go unhealthy while
    /// quiet and crash noisily ten seconds later, and that second capture is
    /// the one worth having.
    fn mark_captured(&self, container_id: &str, now_ms: u64) {
        let mut seen = self.inner.captures_seen.lock().unwrap();
        seen.insert(container_id.to_string(), now_ms);
        // The map is keyed by container id and containers are replaced, not
        // reused, so stale keys accumulate. Cheap to sweep here.
        seen.retain(|_, at| now_ms.saturating_sub(*at) < DEDUP_WINDOW_MS * 10);
    }

    /// Capture every service of a project that has a container and is not
    /// known-healthy — the suspects when a readiness wait ran out. A project
    /// with no healthcheck at all reports `Unknown` for everything, so this
    /// deliberately takes those too rather than capturing nothing.
    pub(crate) async fn capture_stalled_services(&self, project_id: &str) {
        let requests: Vec<CaptureRequest> = {
            let st = self.inner.state.lock().unwrap();
            let Some(entry) = st.projects.get(project_id) else {
                return;
            };
            entry
                .summary
                .services
                .iter()
                .filter(|s| s.health != mast_contract::ServiceHealth::Healthy)
                .filter_map(|s| {
                    Some(CaptureRequest {
                        project: ProjectId(project_id.to_string()),
                        project_name: entry.summary.name.clone(),
                        service: s.name.clone(),
                        container_id: s.container_id.clone()?,
                        reason: CaptureReason::ReadyTimeout,
                    })
                })
                .collect()
        };
        for request in requests {
            self.run_capture(request).await;
        }
    }

    /// Was this container captured within the suppression window by an engine
    /// that has since gone away? The in-memory window covers this process; the
    /// store covers the ones before it.
    async fn captured_before_restart(&self, container_id: &str, now_ms: u64) -> bool {
        let db_path = self.inner.deps.store.captures_db_path();
        let container_id = container_id.to_string();
        let since = now_ms.saturating_sub(DEDUP_WINDOW_MS);
        tokio::task::spawn_blocking(move || {
            CaptureDb::open(&db_path)
                .and_then(|db| db.captured_since(&container_id, since))
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }

    /// Read, redact, store and broadcast one capture. Errors are logged, never
    /// propagated: a capture is diagnostic garnish on whatever the caller was
    /// really doing.
    pub(crate) async fn run_capture(&self, request: CaptureRequest) {
        self.run_capture_inner(request, true).await
    }

    /// As [`Engine::run_capture`], ignoring repeat suppression. Only the manual
    /// capture uses this: asking twice means wanting a second look.
    pub(crate) async fn run_capture_forced(&self, request: CaptureRequest) {
        self.run_capture_inner(request, false).await
    }

    async fn run_capture_inner(&self, request: CaptureRequest, suppress_repeats: bool) {
        let now_ms = now_unix_ms();
        if suppress_repeats
            && (self.recently_captured(&request.container_id, now_ms)
                || self.captured_before_restart(&request.container_id, now_ms).await)
        {
            return;
        }
        let Some(adapter) = self.inner.adapter.lock().unwrap().clone() else {
            return;
        };

        let since = (now_ms / 1000).saturating_sub(CAPTURE_WINDOW_SECS as u64) as i64;
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(CAPTURE_TIMEOUT_SECS),
            adapter.container_log_tail(&request.container_id, since, CAPTURE_MAX_LINES + 1),
        )
        .await;
        let mut lines = match read {
            Ok(Ok(lines)) => lines,
            Ok(Err(e)) => {
                tracing::warn!("log capture for {} failed: {e}", request.service);
                return;
            }
            Err(_) => {
                tracing::warn!("log capture for {} timed out", request.service);
                return;
            }
        };
        if lines.is_empty() {
            return;
        }

        // Read one over the cap so a full window is distinguishable from an
        // exactly-full one.
        let truncated = lines.len() > CAPTURE_MAX_LINES as usize;
        if truncated {
            lines.drain(..lines.len() - CAPTURE_MAX_LINES as usize);
        }

        // Persisted and copyable, so redacted — the union redactor, because a
        // container can echo another project's secret just as history can.
        let redactor = self.inner.state.lock().unwrap().redactor_all.clone();
        let lines: Vec<LogCaptureLine> = lines
            .into_iter()
            .map(|l| LogCaptureLine {
                at: l.at,
                message: redactor.redact(&l.message),
                stderr: l.stderr,
            })
            .collect();

        let mut capture = LogCapture {
            id: 0,
            at_unix_ms: now_ms,
            project: request.project,
            project_name: request.project_name,
            service: request.service,
            container_id: request.container_id,
            reason: request.reason,
            window_secs: CAPTURE_WINDOW_SECS,
            lines,
            truncated,
        };

        let db_path = self.inner.deps.store.captures_db_path();
        let to_store = capture.clone();
        let stored = tokio::task::spawn_blocking(move || {
            let db = CaptureDb::open(&db_path)?;
            let id = db.insert(&to_store)?;
            db.prune(now_ms)?;
            Ok::<u64, CaptureError>(id)
        })
        .await;

        match stored {
            Ok(Ok(id)) => {
                self.mark_captured(&capture.container_id, now_ms);
                capture.id = id;
                let _ = self.inner.captures_tx.send(capture);
            }
            Ok(Err(e)) => tracing::warn!("failed to store log capture: {e}"),
            Err(e) => tracing::warn!("log capture task failed: {e}"),
        }
    }

    /// Fire capture requests without making the caller wait. Used by the
    /// reconcile pass, which notices deaths inside a lock it cannot await in.
    pub(crate) fn spawn_captures(&self, requests: Vec<CaptureRequest>) {
        if requests.is_empty() {
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            for request in requests {
                engine.run_capture(request).await;
            }
        });
    }

    /// Stored captures, newest first.
    pub async fn log_captures(&self, limit: u32) -> Result<Vec<LogCapture>, mast_contract::ErrorInfo> {
        let db_path = self.inner.deps.store.captures_db_path();
        tokio::task::spawn_blocking(move || {
            CaptureDb::open(&db_path).and_then(|db| db.recent(limit)).map_err(internal_err)
        })
        .await
        .map_err(internal_err)?
    }

    /// Live captures. Unlike history this is append-only — a capture is never
    /// updated after it is written — so clients only ever prepend.
    pub fn subscribe_log_captures(&self) -> futures::stream::BoxStream<'static, LogCapture> {
        use futures::StreamExt;
        let mut rx = self.inner.captures_tx.subscribe();
        let (tx, out_rx) = tokio::sync::mpsc::channel::<LogCapture>(64);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(capture) => {
                        if tx.send(capture).await.is_err() {
                            return;
                        }
                    }
                    // The db is the record; a lagging subscriber refetches
                    // rather than forcing a resync.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|capture| (capture, rx))
        })
        .boxed()
    }

    pub(crate) async fn clear_log_captures(&self) -> Result<(), mast_contract::ErrorInfo> {
        let db_path = self.inner.deps.store.captures_db_path();
        tokio::task::spawn_blocking(move || {
            CaptureDb::open(&db_path).and_then(|db| db.clear()).map_err(internal_err)
        })
        .await
        .map_err(internal_err)?
    }
}

/// Compare a service's previous and current observation and decide whether the
/// change is worth a post-mortem. Pure so the reconcile fold can call it under
/// its lock.
///
/// `was_running` is the previous state; `state`/`health` are current.
pub(crate) fn death_reason(
    was_running: bool,
    state: Option<&mast_contract::ContainerState>,
    previous_health: mast_contract::ServiceHealth,
    health: mast_contract::ServiceHealth,
    exit_code: Option<i32>,
) -> Option<CaptureReason> {
    use mast_contract::{ContainerState, ServiceHealth};
    // A container that was running and is now finished died. Whether that was
    // a clean exit or not is the developer's business, not ours to filter:
    // `queue:work` exiting 0 unexpectedly is exactly the confusing case.
    if was_running && matches!(state, Some(ContainerState::Exited) | Some(ContainerState::Dead)) {
        return Some(CaptureReason::Exited { status: exit_code });
    }
    // Only the transition, not the ongoing condition — otherwise every
    // reconcile of a persistently unhealthy container captures again.
    if health == ServiceHealth::Unhealthy && previous_health != ServiceHealth::Unhealthy {
        return Some(CaptureReason::Unhealthy);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_contract::{ContainerState, ServiceHealth};

    fn capture(at_unix_ms: u64, service: &str) -> LogCapture {
        LogCapture {
            id: 0,
            at_unix_ms,
            project: ProjectId("p1".into()),
            project_name: "acme".into(),
            service: service.into(),
            container_id: format!("container-{service}"),
            reason: CaptureReason::Exited { status: Some(1) },
            window_secs: CAPTURE_WINDOW_SECS,
            lines: vec![LogCaptureLine {
                at: Some("2026-08-12T14:22:03.000000000Z".into()),
                message: "boom".into(),
                stderr: true,
            }],
            truncated: false,
        }
    }

    #[test]
    fn captures_round_trip_through_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = CaptureDb::open(&dir.path().join("captures.db")).unwrap();
        let id = db.insert(&capture(1_700_000_000_000, "queue")).unwrap();

        let stored = db.recent(10).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, id);
        assert_eq!(stored[0].service, "queue");
        assert_eq!(stored[0].reason, CaptureReason::Exited { status: Some(1) });
        assert_eq!(stored[0].lines[0].message, "boom");
        assert!(stored[0].lines[0].stderr);
    }

    #[test]
    fn recent_reads_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = CaptureDb::open(&dir.path().join("captures.db")).unwrap();
        db.insert(&capture(1_000, "first")).unwrap();
        db.insert(&capture(2_000, "second")).unwrap();

        let stored = db.recent(10).unwrap();
        assert_eq!(
            stored.iter().map(|c| c.service.as_str()).collect::<Vec<_>>(),
            ["second", "first"]
        );
    }

    #[test]
    fn retention_prunes_by_count_and_by_age() {
        let dir = tempfile::tempdir().unwrap();
        let db = CaptureDb::open(&dir.path().join("captures.db")).unwrap();
        let now = 1_700_000_000_000u64;

        for i in 0..(CAPTURE_RETENTION_COUNT + 20) {
            db.insert(&capture(now, &format!("s{i}"))).unwrap();
        }
        db.prune(now).unwrap();
        assert_eq!(db.recent(1000).unwrap().len(), CAPTURE_RETENTION_COUNT as usize);

        // Older than the age cap goes regardless of how few rows remain.
        db.clear().unwrap();
        let ancient = now - (CAPTURE_RETENTION_DAYS + 1) * 24 * 60 * 60 * 1000;
        db.insert(&capture(ancient, "stale")).unwrap();
        db.insert(&capture(now, "fresh")).unwrap();
        db.prune(now).unwrap();
        assert_eq!(
            db.recent(10).unwrap().iter().map(|c| c.service.as_str()).collect::<Vec<_>>(),
            ["fresh"]
        );
    }

    #[test]
    fn clear_empties_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = CaptureDb::open(&dir.path().join("captures.db")).unwrap();
        db.insert(&capture(1_000, "queue")).unwrap();
        db.clear().unwrap();
        assert!(db.recent(10).unwrap().is_empty());
    }

    #[test]
    fn an_unexpected_exit_is_worth_capturing() {
        assert_eq!(
            death_reason(
                true,
                Some(&ContainerState::Exited),
                ServiceHealth::Unknown,
                ServiceHealth::Unknown,
                Some(137),
            ),
            Some(CaptureReason::Exited { status: Some(137) })
        );
        // A clean exit counts too: `queue:work` finishing on its own is
        // exactly the disappearance a developer cannot otherwise explain.
        assert_eq!(
            death_reason(
                true,
                Some(&ContainerState::Exited),
                ServiceHealth::Unknown,
                ServiceHealth::Unknown,
                Some(0),
            ),
            Some(CaptureReason::Exited { status: Some(0) })
        );
    }

    #[test]
    fn unhealthy_captures_on_the_edge_only() {
        assert_eq!(
            death_reason(
                true,
                Some(&ContainerState::Running),
                ServiceHealth::Healthy,
                ServiceHealth::Unhealthy,
                None,
            ),
            Some(CaptureReason::Unhealthy)
        );
        // Still unhealthy on the next pass — already captured, say nothing.
        assert_eq!(
            death_reason(
                true,
                Some(&ContainerState::Running),
                ServiceHealth::Unhealthy,
                ServiceHealth::Unhealthy,
                None,
            ),
            None
        );
    }

    #[test]
    fn a_container_that_was_already_stopped_is_not_news() {
        assert_eq!(
            death_reason(
                false,
                Some(&ContainerState::Exited),
                ServiceHealth::Unknown,
                ServiceHealth::Unknown,
                Some(1),
            ),
            None
        );
        // Nor is a healthy running one.
        assert_eq!(
            death_reason(
                true,
                Some(&ContainerState::Running),
                ServiceHealth::Healthy,
                ServiceHealth::Healthy,
                None,
            ),
            None
        );
    }
}
