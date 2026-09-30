use crate::database::{timestamp_ms, Database};
use crate::error::{AppError, Result};
use crate::workspace::{content_hash, mutation_guard, normalize_path, Workspace, MAX_TEXT_BYTES};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use similar::TextDiff;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;
const MAX_PATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_PATCH_FILES: usize = 100;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchProposal {
    pub path: String,
    pub content: Option<String>,
    pub expected_hash: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub original_hash: Option<String>,
    pub result_hash: Option<String>,
    pub diff: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchSet {
    pub id: String,
    pub repository_id: String,
    pub status: String,
    pub source: String,
    pub created_at: i64,
    pub changes: Vec<PatchChange>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalItem {
    path: String,
    before: Option<String>,
    after: Option<String>,
}
pub struct PatchService {
    database: Arc<Database>,
}
impl PatchService {
    pub fn new(database: Arc<Database>) -> Self {
        Self { database }
    }
    pub fn propose(
        &self,
        workspace: &Workspace,
        proposals: Vec<PatchProposal>,
        source: &str,
    ) -> Result<PatchSet> {
        let _guard = mutation_guard()?;
        self.ensure_no_recovery(workspace)?;
        if proposals.is_empty() || proposals.len() > MAX_PATCH_FILES || source.len() > 1024 {
            return Err(AppError::new(
                "PATCH_LIMIT",
                "A patch needs 1–100 files and a source label of at most 1024 bytes",
            ));
        }
        let mut seen = HashSet::new();
        let mut total = 0usize;
        let mut changes = Vec::with_capacity(proposals.len());
        for proposal in proposals {
            let path = normalize_path(&proposal.path)?;
            workspace.resolve(&path)?;
            let identity = if cfg!(windows) {
                path.to_lowercase()
            } else {
                path.clone()
            };
            if !seen.insert(identity) {
                return Err(AppError::new(
                    "PATCH_DUPLICATE",
                    "A patch cannot contain the same path more than once",
                ));
            }
            if proposal
                .content
                .as_ref()
                .is_some_and(|content| content.len() > MAX_TEXT_BYTES || content.contains('\0'))
            {
                return Err(AppError::new(
                    "PATCH_CONTENT_LIMIT",
                    "Patch content must be UTF-8 text up to 2 MiB per file without NUL bytes",
                ));
            }
            let original = workspace.snapshot(&path)?;
            if original.as_ref().map(|file| &file.hash) != proposal.expected_hash.as_ref() {
                return Err(Self::stale(
                    &path,
                    proposal.expected_hash.as_deref(),
                    original.as_ref().map(|file| file.hash.as_str()),
                ));
            }
            let before = original.map(|file| file.content);
            if before == proposal.content {
                return Err(AppError::new(
                    "PATCH_NO_CHANGE",
                    "Each proposal must change, create, or delete a file",
                )
                .context(serde_json::json!({"path": path})));
            }
            total = total
                .saturating_add(before.as_ref().map_or(0, String::len))
                .saturating_add(proposal.content.as_ref().map_or(0, String::len));
            if total > MAX_PATCH_BYTES {
                return Err(AppError::new(
                    "PATCH_TOTAL_LIMIT",
                    "Combined patch snapshots exceed 16 MiB",
                ));
            }
            let before_label = if before.is_some() {
                format!("a/{path}")
            } else {
                "/dev/null".into()
            };
            let after_label = if proposal.content.is_some() {
                format!("b/{path}")
            } else {
                "/dev/null".into()
            };
            let diff = TextDiff::configure()
                .timeout(std::time::Duration::from_secs(2))
                .diff_lines(
                    before.as_deref().unwrap_or(""),
                    proposal.content.as_deref().unwrap_or(""),
                )
                .unified_diff()
                .context_radius(3)
                .header(&before_label, &after_label)
                .to_string();
            changes.push(PatchChange {
                path,
                original_hash: before.as_deref().map(content_hash),
                result_hash: proposal.content.as_deref().map(content_hash),
                before,
                after: proposal.content,
                diff,
            });
        }
        let patch = PatchSet {
            id: Uuid::new_v4().to_string(),
            repository_id: workspace.id.clone(),
            status: "proposed".into(),
            source: source.into(),
            created_at: timestamp_ms(),
            changes,
        };
        self.database.with(|connection| {
            connection.execute("INSERT INTO patch_sets(id,repository_id,status,source,created_at,changes) VALUES(?1,?2,?3,?4,?5,?6)", params![patch.id, patch.repository_id, patch.status, patch.source, patch.created_at, serde_json::to_string(&patch.changes)?])?;
            Ok(())
        })?;
        Ok(patch)
    }
    pub fn list(&self, workspace: &Workspace) -> Result<Vec<PatchSet>> {
        self.database.with(|connection| {
            let mut statement = connection.prepare("SELECT id,repository_id,status,source,created_at,changes FROM patch_sets WHERE repository_id=?1 ORDER BY created_at DESC,id DESC LIMIT 100")?;
            let rows = statement.query_map([&workspace.id], Self::row)?;
            let mut patches = Vec::new();
            let mut bytes = 0usize;
            for row in rows {
                let row = row?;
                if !patches.is_empty() && bytes.saturating_add(row.5.len()) > 32 * 1024 * 1024 {
                    break;
                }
                bytes = bytes.saturating_add(row.5.len());
                patches.push(Self::decode(row)?);
            }
            Ok(patches)
        })
    }
    pub fn apply(&self, workspace: &Workspace, id: &str) -> Result<PatchSet> {
        self.transition(workspace, id, false)
    }
    pub fn revert(&self, workspace: &Workspace, id: &str) -> Result<PatchSet> {
        self.transition(workspace, id, true)
    }
    pub fn reject(&self, workspace: &Workspace, id: &str) -> Result<PatchSet> {
        let _guard = mutation_guard()?;
        let mut patch = self.get(workspace, id)?;
        if patch.status != "proposed" {
            return Err(AppError::new(
                "PATCH_STATE",
                "Only a proposed patch can be rejected",
            ));
        }
        self.database.with(|connection| {
            connection.execute("UPDATE patch_sets SET status='rejected' WHERE id=?1 AND repository_id=?2 AND status='proposed'", params![id, workspace.id])?;
            Ok(())
        })?;
        patch.status = "rejected".into();
        Ok(patch)
    }
    pub fn recover(&self, workspace: &Workspace) -> Result<Vec<String>> {
        let _guard = mutation_guard()?;
        let journals = self.database.with(|connection| {
            let mut statement = connection.prepare("SELECT patch_id,prior_status,items FROM patch_journal WHERE repository_id=?1 ORDER BY started_at DESC")?;
            let rows = statement.query_map([&workspace.id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })?;
        let mut recovered = Vec::new();
        let mut failures = Vec::new();
        for (id, prior_status, items) in journals {
            let items: Vec<JournalItem> = serde_json::from_str(&items)?;
            match self.rollback(workspace, &id, &prior_status, &items) {
                Ok(()) => recovered.push(id),
                Err(error) => failures.push(error),
            }
        }
        if failures.is_empty() {
            Ok(recovered)
        } else {
            Err(AppError::new("PATCH_RECOVERY_REQUIRED", "Interrupted patch recovery found changed files; preserve and resolve them before applying further patches")
                .context(serde_json::json!({"recovered": recovered, "conflicts": failures})))
        }
    }
    fn transition(&self, workspace: &Workspace, id: &str, reverting: bool) -> Result<PatchSet> {
        let _guard = mutation_guard()?;
        self.ensure_no_recovery(workspace)?;
        let mut patch = self.get(workspace, id)?;
        let required = if reverting { "applied" } else { "proposed" };
        if patch.status != required {
            return Err(AppError::new(
                "PATCH_STATE",
                format!("The patch must be {required} before this operation"),
            ));
        }
        let items: Vec<JournalItem> = patch
            .changes
            .iter()
            .map(|change| JournalItem {
                path: change.path.clone(),
                before: if reverting {
                    change.after.clone()
                } else {
                    change.before.clone()
                },
                after: if reverting {
                    change.before.clone()
                } else {
                    change.after.clone()
                },
            })
            .collect();
        for item in &items {
            let current = workspace.snapshot(&item.path)?;
            let expected_hash = item.before.as_deref().map(content_hash);
            if current.as_ref().map(|file| file.hash.as_str()) != expected_hash.as_deref() {
                return Err(Self::stale(
                    &item.path,
                    expected_hash.as_deref(),
                    current.as_ref().map(|file| file.hash.as_str()),
                ));
            }
        }
        let intermediate = if reverting { "reverting" } else { "applying" };
        let final_status = if reverting { "reverted" } else { "applied" };
        self.database.with(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute("INSERT INTO patch_journal(patch_id,repository_id,prior_status,operation,items,started_at) VALUES(?1,?2,?3,?4,?5,?6)", params![id, workspace.id, patch.status, intermediate, serde_json::to_string(&items)?, timestamp_ms()])?;
            transaction.execute("UPDATE patch_sets SET status=?1 WHERE id=?2", params![intermediate, id])?;
            transaction.commit()?;
            Ok(())
        })?;
        let operation = (|| -> Result<()> {
            for item in &items {
                let expected = item.before.as_deref().map(content_hash);
                workspace.replace(&item.path, item.after.as_deref(), expected.as_deref())?;
            }
            for item in &items {
                let current = workspace.snapshot(&item.path)?;
                let expected = item.after.as_deref().map(content_hash);
                if current.as_ref().map(|file| file.hash.as_str()) != expected.as_deref() {
                    return Err(Self::stale(
                        &item.path,
                        expected.as_deref(),
                        current.as_ref().map(|file| file.hash.as_str()),
                    ));
                }
            }
            self.database.with(|connection| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "UPDATE patch_sets SET status=?1 WHERE id=?2",
                    params![final_status, id],
                )?;
                transaction.execute("DELETE FROM patch_journal WHERE patch_id=?1", [id])?;
                transaction.commit()?;
                Ok(())
            })
        })();
        if let Err(error) = operation {
            return match self.rollback(workspace, id, &patch.status, &items) {
                Ok(()) => Err(AppError::new(
                    "PATCH_ROLLED_BACK",
                    "The patch could not complete; all touched files were restored",
                )
                .context(serde_json::json!({"patchId": id, "operation": error}))),
                Err(rollback) => Err(AppError::new(
                    "PATCH_RECOVERY_REQUIRED",
                    "The patch failed and requires recovery before further patch operations",
                )
                .context(
                    serde_json::json!({"patchId": id, "operation": error, "recovery": rollback}),
                )),
            };
        }
        patch.status = final_status.into();
        Ok(patch)
    }
    fn rollback(
        &self,
        workspace: &Workspace,
        id: &str,
        prior_status: &str,
        items: &[JournalItem],
    ) -> Result<()> {
        let mut failures = Vec::new();
        for item in items.iter().rev() {
            let restored = (|| -> Result<()> {
                let current = workspace.snapshot(&item.path)?;
                let current_hash = current.as_ref().map(|file| file.hash.as_str());
                let before_hash = item.before.as_deref().map(content_hash);
                let after_hash = item.after.as_deref().map(content_hash);
                if current_hash == before_hash.as_deref() {
                    return Ok(());
                }
                if current_hash != after_hash.as_deref() {
                    return Err(Self::stale(&item.path, after_hash.as_deref(), current_hash));
                }
                workspace.replace(&item.path, item.before.as_deref(), after_hash.as_deref())
            })();
            if let Err(error) = restored {
                failures.push(error);
            }
        }
        self.database.with(|connection| {
            let transaction = connection.transaction()?;
            if failures.is_empty() {
                transaction.execute("UPDATE patch_sets SET status=?1 WHERE id=?2 AND repository_id=?3", params![prior_status, id, workspace.id])?;
                transaction.execute("DELETE FROM patch_journal WHERE patch_id=?1", [id])?;
            } else {
                transaction.execute("UPDATE patch_sets SET status='recovery_required' WHERE id=?1 AND repository_id=?2", params![id, workspace.id])?;
            }
            transaction.commit()?;
            Ok(())
        })?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::new(
                "PATCH_ROLLBACK_CONFLICT",
                "Recovery preserved externally changed files instead of overwriting them",
            )
            .context(serde_json::json!({"patchId": id, "files": failures})))
        }
    }
    fn ensure_no_recovery(&self, workspace: &Workspace) -> Result<()> {
        let count: i64 = self.database.with(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM patch_journal WHERE repository_id=?1",
                [&workspace.id],
                |row| row.get(0),
            )?)
        })?;
        if count > 0 {
            Err(AppError::new(
                "PATCH_RECOVERY_REQUIRED",
                "Recover the interrupted patch before creating or applying more changes",
            ))
        } else {
            Ok(())
        }
    }
    fn get(&self, workspace: &Workspace, id: &str) -> Result<PatchSet> {
        self.database.with(|connection| {
            let row = connection.query_row("SELECT id,repository_id,status,source,created_at,changes FROM patch_sets WHERE id=?1 AND repository_id=?2", params![id, workspace.id], Self::row).optional()?;
            Self::decode(row.ok_or_else(|| AppError::new("PATCH_NOT_FOUND", "The patch does not belong to this repository or does not exist"))?)
        })
    }
    fn row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<(String, String, String, String, i64, String)> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    }
    fn decode(row: (String, String, String, String, i64, String)) -> Result<PatchSet> {
        Ok(PatchSet {
            id: row.0,
            repository_id: row.1,
            status: row.2,
            source: row.3,
            created_at: row.4,
            changes: serde_json::from_str(&row.5)?,
        })
    }
    fn stale(path: &str, expected: Option<&str>, actual: Option<&str>) -> AppError {
        AppError::new("PATCH_STALE", "The patch conflicts with current file content; propose it again using the current file")
            .context(serde_json::json!({"path": path, "expectedHash": expected, "actualHash": actual}))
    }
}
