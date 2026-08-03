//! Workspace snapshots (M6): git refs + config-file hashes per member,
//! report-only restore — a drift report, never an automatic apply.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mast_contract::{
    ErrorInfo, ProjectId,
    WorkspaceId, WorkspaceSnapshot,
};

use crate::{Engine, internal_err};

impl Engine {
    /// Capture the snapshot-relevant state of one member: git branch/commit/
    /// dirty via shell git (plan: shell-git fallback; gix chips land M8) and
    /// hashes of the invocation's compose files + .env.
    pub(crate) async fn capture_member_state(
        &self,
        project_id: &str,
    ) -> Option<mast_project::SnapshotMemberRecord> {
        let (name, dir, mut paths) = {
            let st = self.inner.state.lock().unwrap();
            let entry = st.projects.get(project_id)?;
            let mut paths: Vec<PathBuf> = entry
                .invocation
                .as_ref()
                .map(|i| i.files.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default();
            paths.push(entry.record.path.join(".env"));
            (entry.summary.name.clone(), entry.record.path.clone(), paths)
        };
        paths.retain(|p| p.is_file());

        let git = |args: &[&str]| {
            let argv: Vec<String> =
                std::iter::once("git".to_string()).chain(args.iter().map(|s| s.to_string())).collect();
            let dir = dir.clone();
            async move {
                mast_docker::run_command(&argv, Some(&dir), &[], Duration::from_secs(10), 64 * 1024)
                    .await
                    .ok()
                    .filter(|o| o.success())
                    .map(|o| o.stdout.trim().to_string())
            }
        };
        let git_branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).await;
        let git_commit = git(&["rev-parse", "HEAD"]).await;
        let git_dirty =
            git(&["status", "--porcelain"]).await.map(|out| !out.is_empty());

        let files = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .filter_map(|p| {
                    mast_project::file_sha256(p)
                        .map(|h| (p.to_string_lossy().into_owned(), h))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();

        Some(mast_project::SnapshotMemberRecord {
            project_id: project_id.to_string(),
            project_name: name,
            git_branch,
            git_commit,
            git_dirty,
            files,
        })
    }

    pub(crate) fn snapshot_to_contract(record: &mast_project::SnapshotRecord) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            id: record.id.clone(),
            workspace: WorkspaceId(record.workspace_id.clone()),
            name: record.name.clone(),
            taken_unix: record.taken_unix,
            members: record
                .members
                .iter()
                .map(|m| mast_contract::SnapshotMemberState {
                    project: ProjectId(m.project_id.clone()),
                    project_name: m.project_name.clone(),
                    git_branch: m.git_branch.clone(),
                    git_commit: m.git_commit.clone(),
                    git_dirty: m.git_dirty,
                    file_hashes: m
                        .files
                        .iter()
                        .map(|(path, sha256)| mast_contract::SnapshotFileHash {
                            path: path.clone(),
                            sha256: sha256.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub async fn list_snapshots(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<WorkspaceSnapshot>, ErrorInfo> {
        let mut all = self
            .inner
            .deps
            .store
            .load_snapshots()
            .map_err(internal_err)?;
        all.retain(|s| s.workspace_id == workspace.0);
        all.sort_by_key(|s| std::cmp::Reverse(s.taken_unix));
        Ok(all.iter().map(Self::snapshot_to_contract).collect())
    }

    /// Compare current member state against a snapshot — a report, never an
    /// automatic restore.
    pub async fn snapshot_report(
        &self,
        snapshot_id: &str,
    ) -> Result<mast_contract::SnapshotReport, ErrorInfo> {
        let record = self
            .inner
            .deps
            .store
            .load_snapshots()
            .map_err(internal_err)?
            .into_iter()
            .find(|s| s.id == snapshot_id)
            .ok_or(ErrorInfo::NotFound { what: format!("snapshot {snapshot_id}") })?;

        let mut deltas = Vec::new();
        for then in &record.members {
            let mut changes = Vec::new();
            match self.capture_member_state(&then.project_id).await {
                None => changes.push("project is no longer imported".to_string()),
                Some(now) => {
                    if now.git_branch != then.git_branch {
                        changes.push(format!(
                            "branch {} → {}",
                            then.git_branch.as_deref().unwrap_or("?"),
                            now.git_branch.as_deref().unwrap_or("?")
                        ));
                    }
                    if now.git_commit != then.git_commit {
                        let short = |c: &Option<String>| {
                            c.as_deref().map(|s| s[..s.len().min(8)].to_string())
                                .unwrap_or_else(|| "?".into())
                        };
                        changes.push(format!(
                            "commit {} → {}",
                            short(&then.git_commit),
                            short(&now.git_commit)
                        ));
                    }
                    if then.git_dirty == Some(false) && now.git_dirty == Some(true) {
                        changes.push("working tree is now dirty".to_string());
                    }
                    for (path, then_hash) in &then.files {
                        match now.files.iter().find(|(p, _)| p == path) {
                            Some((_, now_hash)) if now_hash != then_hash => {
                                let file = Path::new(path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.clone());
                                changes.push(format!("{file} changed"));
                            }
                            None => changes.push(format!("{path} is missing")),
                            _ => {}
                        }
                    }
                }
            }
            deltas.push(mast_contract::SnapshotDelta {
                project_name: then.project_name.clone(),
                changes,
            });
        }
        let clean = deltas.iter().all(|d| d.changes.is_empty());
        Ok(mast_contract::SnapshotReport {
            snapshot: Self::snapshot_to_contract(&record),
            deltas,
            clean,
        })
    }
}
