//! Where skills live: versions, lineage, and the history of what was promoted.
//!
//! SQLite in WAL mode, the same idiom as `kedge-ledger`. Two tables — the skills
//! themselves, and an append-only log of every promotion and rollback with the
//! gate's reasoning attached.
//!
//! ## No clocks
//!
//! Ordering comes from an autoincrementing sequence, not a timestamp. The rest
//! of this pipeline is deterministic on purpose — `kedge-bench` derives its run
//! ids from task names rather than `Uuid::new_v4()` so a corpus is reproducible
//! — and a wall-clock column would put a source of run-to-run variance right in
//! the middle of it. Callers that want timestamps can record them alongside.
//!
//! ## Promotion is one transaction
//!
//! Promoting means demoting whatever was current *and* marking the new record,
//! and a crash between those leaves a skill name with either two current
//! versions or none. Both are silent: nothing about a later read makes it
//! obvious. So it is a single transaction, and there is a test that kills it
//! half-way and asserts nothing moved.

use std::path::Path;

use kedge_core::TaskId;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::gate::{GateNote, GateReason, GateVerdict};
use crate::observe::ObservedAuthority;
use crate::Reach;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("registry database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("no skill with id {0}")]
    NotFound(SkillId),
    #[error("`{name}` has no promoted version to roll back")]
    NothingToRollBack { name: String },
    #[error("refusing to promote: {0}")]
    Refused(String),
}

/// Identity of one skill version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkillId(pub Uuid);

impl SkillId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        SkillId(Uuid::new_v4())
    }
}

impl std::fmt::Display for SkillId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One version of one skill.
#[derive(Debug, Clone)]
pub struct SkillRecord {
    pub id: SkillId,
    pub name: String,
    pub version: String,
    /// The version this was derived from, if any.
    pub parent: Option<SkillId>,
    pub manifest_toml: String,
    /// The trajectory it was learned from.
    pub origin_run: TaskId,
    pub reach: Reach,
    /// Calls the origin run made that its own manifest would refuse.
    pub violations: Vec<String>,
    /// Effects in the origin run that could not be named.
    pub indeterminate: usize,
    pub promoted: bool,
}

impl SkillRecord {
    /// Build a candidate from an observation and its measured authority.
    pub fn from_observation(
        observed: &ObservedAuthority,
        name: impl Into<String>,
        version: impl Into<String>,
        reach: Reach,
    ) -> Self {
        let name = name.into();
        let version = version.into();
        let violations = match &observed.verification {
            crate::Verification::Failed { violations, .. } => violations.clone(),
            // An unverified observation is treated as failing, not as passing.
            // `Skipped` means nobody checked, and nobody-checked is not a pass.
            crate::Verification::Skipped => {
                vec!["the observation was never verified".to_string()]
            }
            crate::Verification::Exact => Vec::new(),
        };
        SkillRecord {
            id: SkillId::new(),
            manifest_toml: observed.manifest(&name, &version),
            name,
            version,
            parent: None,
            origin_run: observed.task,
            reach,
            violations,
            indeterminate: observed.unobservable.len(),
            promoted: false,
        }
    }

    pub fn with_parent(mut self, parent: SkillId) -> Self {
        self.parent = Some(parent);
        self
    }
}

/// What happened to a skill, and why.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub seq: i64,
    pub skill: SkillId,
    pub action: String,
    pub detail: String,
}

/// The SQLite-backed skill registry.
pub struct Registry {
    conn: Connection,
}

impl Registry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, RegistryError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self, RegistryError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Registry { conn })
    }

    /// Store a candidate. Candidates are never promoted on insert.
    pub fn insert_candidate(&self, rec: &SkillRecord) -> Result<SkillId, RegistryError> {
        self.conn.execute(
            "INSERT INTO skills (id, name, version, parent, manifest_toml, origin_run,
                 writable, readable, commands, hosts, wildcard_grants, escapes_root,
                 truncated, files_scanned, violations, indeterminate, promoted)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0)",
            params![
                rec.id.to_string(),
                rec.name,
                rec.version,
                rec.parent.map(|p| p.to_string()),
                rec.manifest_toml,
                rec.origin_run.to_string(),
                rec.reach.writable as i64,
                rec.reach.readable as i64,
                rec.reach.commands as i64,
                rec.reach.hosts as i64,
                rec.reach.wildcard_grants as i64,
                rec.reach.escapes_root as i64,
                rec.reach.truncated as i64,
                rec.reach.files_scanned as i64,
                serde_json::to_string(&rec.violations).unwrap_or_else(|_| "[]".into()),
                rec.indeterminate as i64,
            ],
        )?;
        Ok(rec.id)
    }

    pub fn get(&self, id: SkillId) -> Result<SkillRecord, RegistryError> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM skills WHERE id = ?1"),
                params![id.to_string()],
                row_to_record,
            )
            .optional()?
            .ok_or(RegistryError::NotFound(id))
    }

    /// The currently promoted version of `name`, if any.
    pub fn current(&self, name: &str) -> Result<Option<SkillRecord>, RegistryError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM skills WHERE name = ?1 AND promoted = 1"),
                params![name],
                row_to_record,
            )
            .optional()?)
    }

    /// The full parent chain, oldest first, ending at `id`.
    pub fn lineage(&self, id: SkillId) -> Result<Vec<SkillRecord>, RegistryError> {
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = cursor {
            // A cycle would be corruption, but hanging is a worse response to it
            // than stopping.
            if !seen.insert(current) {
                break;
            }
            let rec = self.get(current)?;
            cursor = rec.parent;
            chain.push(rec);
        }
        chain.reverse();
        Ok(chain)
    }

    /// Apply a gate verdict. A refusal is recorded and changes nothing.
    ///
    /// The whole thing is one transaction: demote the old current, promote the
    /// new record, append history. A crash part-way through would otherwise
    /// leave a name with two current versions or none.
    pub fn promote(&self, id: SkillId, verdict: &GateVerdict) -> Result<(), RegistryError> {
        self.promote_inner(id, verdict, |_| Ok(()))
    }

    fn promote_inner(
        &self,
        id: SkillId,
        verdict: &GateVerdict,
        probe: impl Fn(&str) -> Result<(), RegistryError>,
    ) -> Result<(), RegistryError> {
        let rec = self.get(id)?;

        if !verdict.promote {
            // Refusals are still history. A gate whose denials leave no trace is
            // indistinguishable from a gate that was never run.
            self.record_history(id, "refused", &verdict_detail(verdict))?;
            return Err(RegistryError::Refused(
                verdict
                    .reasons
                    .iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE skills SET promoted = 0 WHERE name = ?1 AND promoted = 1",
            params![rec.name],
        )?;
        probe("after-demote")?;
        tx.execute(
            "UPDATE skills SET promoted = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        probe("after-promote")?;
        tx.execute(
            "INSERT INTO history (skill_id, action, detail) VALUES (?1, 'promoted', ?2)",
            params![id.to_string(), verdict_detail(verdict)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Undo the current promotion of `name`, restoring its parent.
    pub fn rollback(&self, name: &str, why: &str) -> Result<Option<SkillId>, RegistryError> {
        let current = self
            .current(name)?
            .ok_or_else(|| RegistryError::NothingToRollBack {
                name: name.to_string(),
            })?;

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE skills SET promoted = 0 WHERE id = ?1",
            params![current.id.to_string()],
        )?;
        if let Some(parent) = current.parent {
            tx.execute(
                "UPDATE skills SET promoted = 1 WHERE id = ?1",
                params![parent.to_string()],
            )?;
        }
        tx.execute(
            "INSERT INTO history (skill_id, action, detail) VALUES (?1, 'rolled-back', ?2)",
            params![current.id.to_string(), why],
        )?;
        tx.commit()?;
        Ok(current.parent)
    }

    /// Everything that has happened, oldest first.
    pub fn history(&self) -> Result<Vec<HistoryEntry>, RegistryError> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, skill_id, action, detail FROM history ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok(HistoryEntry {
                seq: row.get(0)?,
                skill: SkillId(Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or(Uuid::nil())),
                action: row.get(2)?,
                detail: row.get(3)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    fn record_history(&self, id: SkillId, action: &str, detail: &str) -> Result<(), RegistryError> {
        self.conn.execute(
            "INSERT INTO history (skill_id, action, detail) VALUES (?1, ?2, ?3)",
            params![id.to_string(), action, detail],
        )?;
        Ok(())
    }
}

fn verdict_detail(v: &GateVerdict) -> String {
    let mut parts: Vec<String> = v.reasons.iter().map(GateReason::to_string).collect();
    parts.extend(v.notes.iter().map(GateNote::to_string));
    if parts.is_empty() {
        "clean".into()
    } else {
        parts.join("; ")
    }
}

const COLUMNS: &str = "id, name, version, parent, manifest_toml, origin_run,
     writable, readable, commands, hosts, wildcard_grants, escapes_root, truncated,
     files_scanned, violations, indeterminate, promoted";

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillRecord> {
    let parse_uuid = |s: String| Uuid::parse_str(&s).unwrap_or(Uuid::nil());
    Ok(SkillRecord {
        id: SkillId(parse_uuid(row.get(0)?)),
        name: row.get(1)?,
        version: row.get(2)?,
        parent: row
            .get::<_, Option<String>>(3)?
            .map(|s| SkillId(parse_uuid(s))),
        manifest_toml: row.get(4)?,
        origin_run: TaskId(parse_uuid(row.get(5)?)),
        reach: Reach {
            writable: row.get::<_, i64>(6)? as usize,
            readable: row.get::<_, i64>(7)? as usize,
            commands: row.get::<_, i64>(8)? as usize,
            hosts: row.get::<_, i64>(9)? as usize,
            wildcard_grants: row.get::<_, i64>(10)? as usize,
            escapes_root: row.get::<_, i64>(11)? != 0,
            truncated: row.get::<_, i64>(12)? != 0,
            files_scanned: row.get::<_, i64>(13)? as usize,
        },
        violations: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
        indeterminate: row.get::<_, i64>(15)? as usize,
        promoted: row.get::<_, i64>(16)? != 0,
    })
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS skills (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    version       TEXT NOT NULL,
    parent        TEXT REFERENCES skills(id),
    manifest_toml TEXT NOT NULL,
    origin_run    TEXT NOT NULL,
    writable      INTEGER NOT NULL,
    readable      INTEGER NOT NULL,
    commands      INTEGER NOT NULL,
    hosts         INTEGER NOT NULL,
    wildcard_grants INTEGER NOT NULL,
    escapes_root  INTEGER NOT NULL,
    truncated     INTEGER NOT NULL,
    files_scanned INTEGER NOT NULL,
    violations    TEXT NOT NULL,
    indeterminate INTEGER NOT NULL,
    promoted      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS skills_name ON skills(name);
-- At most one promoted version per name, enforced by the database rather than
-- by the code being careful. A partial unique index is the only thing that
-- survives a bug in `promote`.
CREATE UNIQUE INDEX IF NOT EXISTS skills_one_current
    ON skills(name) WHERE promoted = 1;

CREATE TABLE IF NOT EXISTS history (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_id TEXT NOT NULL REFERENCES skills(id),
    action   TEXT NOT NULL,
    detail   TEXT NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{gate, EvalOutcome};

    fn reach(writable: usize) -> Reach {
        Reach {
            writable,
            readable: writable,
            commands: 1,
            hosts: 0,
            escapes_root: false,
            truncated: false,
            files_scanned: 100,
            wildcard_grants: 0,
        }
    }

    fn candidate(name: &str, version: &str, writable: usize) -> SkillRecord {
        SkillRecord {
            id: SkillId::new(),
            name: name.into(),
            version: version.into(),
            parent: None,
            manifest_toml: "[skill]\nname=\"x\"\nversion=\"0.1.0\"\n".into(),
            origin_run: TaskId::new(),
            reach: reach(writable),
            violations: Vec::new(),
            indeterminate: 0,
            promoted: false,
        }
    }

    fn clean() -> GateVerdict {
        GateVerdict {
            promote: true,
            reasons: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn lineage_returns_the_whole_chain_oldest_first() {
        let r = Registry::in_memory().unwrap();
        let v1 = candidate("repair", "0.1.0", 10);
        r.insert_candidate(&v1).unwrap();
        let v2 = candidate("repair", "0.2.0", 5).with_parent(v1.id);
        r.insert_candidate(&v2).unwrap();
        let v3 = candidate("repair", "0.3.0", 3).with_parent(v2.id);
        r.insert_candidate(&v3).unwrap();

        let chain = r.lineage(v3.id).unwrap();
        let versions: Vec<&str> = chain.iter().map(|s| s.version.as_str()).collect();
        assert_eq!(versions, ["0.1.0", "0.2.0", "0.3.0"]);
    }

    #[test]
    fn promotion_replaces_the_previous_current() {
        let r = Registry::in_memory().unwrap();
        let v1 = candidate("repair", "0.1.0", 10);
        r.insert_candidate(&v1).unwrap();
        r.promote(v1.id, &clean()).unwrap();
        assert_eq!(r.current("repair").unwrap().unwrap().id, v1.id);

        let v2 = candidate("repair", "0.2.0", 5).with_parent(v1.id);
        r.insert_candidate(&v2).unwrap();
        r.promote(v2.id, &clean()).unwrap();

        assert_eq!(r.current("repair").unwrap().unwrap().id, v2.id);
        assert!(!r.get(v1.id).unwrap().promoted);
    }

    /// Acceptance: a failed promotion leaves no partial write — asserted by
    /// killing the transaction mid-flight, not by inspection.
    #[test]
    fn a_promotion_killed_half_way_leaves_nothing_moved() {
        let r = Registry::in_memory().unwrap();
        let v1 = candidate("repair", "0.1.0", 10);
        r.insert_candidate(&v1).unwrap();
        r.promote(v1.id, &clean()).unwrap();

        let v2 = candidate("repair", "0.2.0", 5).with_parent(v1.id);
        r.insert_candidate(&v2).unwrap();

        // Die between demoting the old and promoting the new: the window where
        // the name has *no* current version.
        let err = r.promote_inner(v2.id, &clean(), |stage| {
            if stage == "after-demote" {
                Err(RegistryError::Refused("simulated crash".into()))
            } else {
                Ok(())
            }
        });
        assert!(err.is_err());

        // The old version is still current and the new one is not promoted.
        assert_eq!(
            r.current("repair").unwrap().map(|s| s.id),
            Some(v1.id),
            "the rollback did not restore the previous current"
        );
        assert!(!r.get(v2.id).unwrap().promoted);
        // And no history was written for the promotion that did not happen.
        assert!(r
            .history()
            .unwrap()
            .iter()
            .all(|h| !(h.skill == v2.id && h.action == "promoted")));
    }

    #[test]
    fn the_database_itself_refuses_two_current_versions() {
        // Belt and braces: even if `promote` had a bug, the partial unique index
        // makes the corrupt state unrepresentable rather than merely unlikely.
        let r = Registry::in_memory().unwrap();
        let v1 = candidate("repair", "0.1.0", 10);
        let v2 = candidate("repair", "0.2.0", 5);
        r.insert_candidate(&v1).unwrap();
        r.insert_candidate(&v2).unwrap();
        r.conn
            .execute(
                "UPDATE skills SET promoted = 1 WHERE id = ?1",
                params![v1.id.to_string()],
            )
            .unwrap();
        let second = r.conn.execute(
            "UPDATE skills SET promoted = 1 WHERE id = ?1",
            params![v2.id.to_string()],
        );
        assert!(second.is_err(), "the database allowed two current versions");
    }

    #[test]
    fn rollback_restores_the_parent_and_records_why() {
        let r = Registry::in_memory().unwrap();
        let v1 = candidate("repair", "0.1.0", 10);
        r.insert_candidate(&v1).unwrap();
        r.promote(v1.id, &clean()).unwrap();
        let v2 = candidate("repair", "0.2.0", 5).with_parent(v1.id);
        r.insert_candidate(&v2).unwrap();
        r.promote(v2.id, &clean()).unwrap();

        let restored = r.rollback("repair", "broke the nightly build").unwrap();
        assert_eq!(restored, Some(v1.id));
        assert_eq!(r.current("repair").unwrap().unwrap().id, v1.id);

        let h = r.history().unwrap();
        let last = h.last().unwrap();
        assert_eq!(last.action, "rolled-back");
        assert_eq!(last.detail, "broke the nightly build");
        // History is ordered and complete: promote, promote, rollback.
        assert_eq!(h.len(), 3);
        assert!(h.windows(2).all(|w| w[0].seq < w[1].seq));
    }

    #[test]
    fn a_refused_candidate_changes_nothing_but_is_recorded() {
        let r = Registry::in_memory().unwrap();
        let base = candidate("repair", "0.1.0", 5);
        r.insert_candidate(&base).unwrap();
        r.promote(base.id, &clean()).unwrap();

        // Wider than the baseline: the gate must refuse it.
        let wider = candidate("repair", "0.2.0", 50).with_parent(base.id);
        r.insert_candidate(&wider).unwrap();
        let verdict = gate(
            &wider,
            Some(&base),
            Some(&EvalOutcome {
                suite: "s".into(),
                passed: true,
                detail: String::new(),
            }),
        );
        assert!(!verdict.promote);

        let err = r.promote(wider.id, &verdict);
        assert!(matches!(err, Err(RegistryError::Refused(_))));

        assert_eq!(r.current("repair").unwrap().unwrap().id, base.id);
        assert!(!r.get(wider.id).unwrap().promoted);

        // A gate whose denials leave no trace is indistinguishable from one that
        // was never run.
        let h = r.history().unwrap();
        assert!(h.iter().any(|e| e.skill == wider.id
            && e.action == "refused"
            && e.detail.contains("authority widened")));
    }

    #[test]
    fn an_unverified_observation_is_treated_as_failing_not_passing() {
        use crate::observe::{ObservedAuthority, Verification};
        let obs = ObservedAuthority {
            task: TaskId::new(),
            calls: 3,
            exercised: Default::default(),
            unobservable: Vec::new(),
            verification: Verification::Skipped,
        };
        let rec = SkillRecord::from_observation(&obs, "s", "0.1.0", reach(1));
        assert!(!rec.violations.is_empty(), "nobody-checked is not a pass");
        assert!(!gate(&rec, None, None).promote);
    }
}
