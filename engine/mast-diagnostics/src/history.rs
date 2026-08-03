//! Diagnostics history (plan §8): every run and every applied repair is
//! recorded in `diagnostics.db` (rusqlite, bundled). Callers wrap calls in
//! `spawn_blocking` — connections are cheap and short-lived here.

use std::path::Path;

use rusqlite::Connection;

use crate::{Finding, RiskTier, Severity};

#[derive(Debug, thiserror::Error)]
pub enum DiagnosticsError {
    #[error("diagnostics db: {0}")]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: i64,
    pub taken_unix: i64,
    pub checks_run: u32,
    pub errors: u32,
    pub warnings: u32,
    pub infos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairAudit {
    pub applied_unix: i64,
    pub repair: String,
    pub project_name: Option<String>,
    pub risk: String,
    pub outcome: String,
}

fn risk_str(risk: RiskTier) -> &'static str {
    match risk {
        RiskTier::Safe => "safe",
        RiskTier::Caution => "caution",
        RiskTier::HighRisk => "high-risk",
    }
}

pub struct DiagnosticsDb {
    conn: Connection,
}

impl DiagnosticsDb {
    pub fn open(path: &Path) -> Result<Self, DiagnosticsError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
                 id INTEGER PRIMARY KEY,
                 taken_unix INTEGER NOT NULL,
                 checks_run INTEGER NOT NULL,
                 errors INTEGER NOT NULL,
                 warnings INTEGER NOT NULL,
                 infos INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS findings (
                 run_id INTEGER NOT NULL REFERENCES runs(id),
                 check_id TEXT NOT NULL,
                 severity TEXT NOT NULL,
                 title TEXT NOT NULL,
                 detail TEXT NOT NULL,
                 project TEXT
             );
             CREATE TABLE IF NOT EXISTS repairs (
                 id INTEGER PRIMARY KEY,
                 applied_unix INTEGER NOT NULL,
                 repair TEXT NOT NULL,
                 project_name TEXT,
                 risk TEXT NOT NULL,
                 outcome TEXT NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    pub fn record_run(
        &self,
        taken_unix: i64,
        checks_run: usize,
        findings: &[Finding],
    ) -> Result<i64, DiagnosticsError> {
        let count = |s: Severity| findings.iter().filter(|f| f.severity == s).count() as i64;
        self.conn.execute(
            "INSERT INTO runs (taken_unix, checks_run, errors, warnings, infos) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                taken_unix,
                checks_run as i64,
                count(Severity::Error),
                count(Severity::Warning),
                count(Severity::Info),
            ),
        )?;
        let run_id = self.conn.last_insert_rowid();
        let mut stmt = self.conn.prepare(
            "INSERT INTO findings (run_id, check_id, severity, title, detail, project) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for f in findings {
            let severity = match f.severity {
                Severity::Info => "info",
                Severity::Warning => "warning",
                Severity::Error => "error",
            };
            stmt.execute((run_id, f.check, severity, &f.title, &f.detail, &f.project))?;
        }
        Ok(run_id)
    }

    pub fn recent_runs(&self, limit: u32) -> Result<Vec<RunSummary>, DiagnosticsError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, taken_unix, checks_run, errors, warnings, infos \
             FROM runs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(RunSummary {
                id: row.get(0)?,
                taken_unix: row.get(1)?,
                checks_run: row.get(2)?,
                errors: row.get(3)?,
                warnings: row.get(4)?,
                infos: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn record_repair(
        &self,
        applied_unix: i64,
        repair: &str,
        project_name: Option<&str>,
        risk: RiskTier,
        outcome: &str,
    ) -> Result<(), DiagnosticsError> {
        self.conn.execute(
            "INSERT INTO repairs (applied_unix, repair, project_name, risk, outcome) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (applied_unix, repair, project_name, risk_str(risk), outcome),
        )?;
        Ok(())
    }

    pub fn recent_repairs(&self, limit: u32) -> Result<Vec<RepairAudit>, DiagnosticsError> {
        let mut stmt = self.conn.prepare(
            "SELECT applied_unix, repair, project_name, risk, outcome \
             FROM repairs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(RepairAudit {
                applied_unix: row.get(0)?,
                repair: row.get(1)?,
                project_name: row.get(2)?,
                risk: row.get(3)?,
                outcome: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_finding(severity: Severity) -> Finding {
        Finding {
            check: "docker-running",
            severity,
            title: "t".into(),
            detail: "d".into(),
            project: Some("p1".into()),
            repair: None,
        }
    }

    #[test]
    fn run_and_repair_history_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let db = DiagnosticsDb::open(&dir.path().join("diagnostics.db")).unwrap();

        let id = db
            .record_run(
                1_700_000_000,
                12,
                &[sample_finding(Severity::Error), sample_finding(Severity::Warning)],
            )
            .unwrap();
        assert!(id > 0);
        db.record_run(1_700_000_100, 12, &[]).unwrap();

        let runs = db.recent_runs(10).unwrap();
        assert_eq!(runs.len(), 2);
        // Newest first.
        assert_eq!(runs[0].taken_unix, 1_700_000_100);
        assert_eq!(runs[1].errors, 1);
        assert_eq!(runs[1].warnings, 1);
        assert_eq!(runs[1].checks_run, 12);

        db.record_repair(1_700_000_200, "set-wwwuser", Some("api"), RiskTier::Safe, "applied")
            .unwrap();
        let repairs = db.recent_repairs(10).unwrap();
        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0].repair, "set-wwwuser");
        assert_eq!(repairs[0].risk, "safe");
        assert_eq!(repairs[0].project_name.as_deref(), Some("api"));
    }

    #[test]
    fn reopening_preserves_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.db");
        DiagnosticsDb::open(&path).unwrap().record_run(1, 5, &[]).unwrap();
        let db = DiagnosticsDb::open(&path).unwrap();
        assert_eq!(db.recent_runs(10).unwrap().len(), 1);
    }
}
