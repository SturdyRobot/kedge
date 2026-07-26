//! # kedge-bench
//!
//! A reproducible repair-task suite. Its job is **not** to grade Forge — it is
//! to *feed* it.
//!
//! [Spike 000](../../../docs/spikes/000-trajectory-corpus.md) measured kedge's
//! ledger at 0 runs, 0 steps, 0 events. Every Forge component consumes recorded
//! trajectories, so until something produces them, nothing downstream can be
//! built or measured. This crate is that something.
//!
//! ## Three properties that make the corpus trustworthy
//!
//! **The oracle is independent.** A task is solved when the fixture's own
//! `cargo test` exits 0 — the compiler and the crate's own tests decide, not a
//! predicate written alongside the solver. Checking "did the solver write the
//! string we expected" would be an oracle derived from the thing under test,
//! which can only ever confirm it (`WORKFLOW.md` principle 8).
//!
//! **Every breakage is proven to break.** A task whose breakage does not
//! actually fail the tests is a no-op, and its solve rate is a lie. This is not
//! hypothetical: the first candidate breakage written for this crate changed
//! `v > hi` to `v >= hi` in `clamp_upper`, which is *behaviourally identical* —
//! both return `hi` at the boundary. It passed.
//! [`checks::every_breakage_actually_breaks`] is the test that caught it, and it
//! runs over the whole suite.
//!
//! **Runs are reproducible.** Fixtures are copied fresh, task ordering is fixed,
//! `TaskId`s are derived deterministically from the task name rather than
//! randomly, and nothing consults the clock or an RNG.
//! [`BenchReport::fingerprint`] excludes only timing.
//!
//! ## Scope, stated plainly
//!
//! The reference solver is a [`ScriptedReasoner`], not an LLM: the corpus costs
//! $0, runs in CI, and is byte-reproducible. That is deliberate
//! ([ADR-0002](../../../docs/adr/0002-benchmark-before-distiller.md)) and it has
//! a consequence worth stating up front — a scripted corpus **cannot** answer
//! whether real agent trajectories share repeated structure, because the
//! structure in it is whatever the author scripted. It can validate the
//! machinery that looks for structure. It cannot discover any. See `RISKS.md` R9.

pub mod checks;
pub mod fixture;
pub mod runner;
pub mod scripted;
pub mod suite;
pub mod tools;

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use fixture::FixtureError;
pub use runner::{run_suite, BenchOutcome, BenchReport};
pub use scripted::ScriptedReasoner;
pub use suite::suite;
pub use tools::WorkspaceTools;

/// One reproducible repair task.
#[derive(Debug, Clone)]
pub struct BenchTask {
    /// Stable across runs; evals and the skill registry join on this.
    pub id: &'static str,
    /// The generalization unit — a skill is learned per family.
    pub family: &'static str,
    /// Directory name under `fixtures/`.
    pub fixture: &'static str,
    /// What the agent is told to do.
    pub goal: &'static str,
    pub breakage: Breakage,
    pub acceptance: Acceptance,
}

/// How a fixture is broken. Deterministic, reversible, self-describing.
#[derive(Debug, Clone)]
pub enum Breakage {
    /// Replace `find` with `replace` in `file`.
    ///
    /// `find` must occur **exactly once**. Matching twice would edit somewhere
    /// the author did not intend; matching zero times would silently produce an
    /// unbroken task — the failure mode this crate exists to prevent. Both are
    /// hard errors, asserted by [`checks::every_splice_matches_exactly_once`].
    Splice {
        file: &'static str,
        find: &'static str,
        replace: &'static str,
    },
}

/// How we know a task is solved. Never consults the solver's own output.
#[derive(Debug, Clone)]
pub enum Acceptance {
    /// The command exits 0 in the workspace.
    CommandSucceeds {
        program: &'static str,
        args: &'static [&'static str],
        timeout: Duration,
    },
}

impl Acceptance {
    /// The standard oracle: the fixture's own test suite.
    pub const fn cargo_test() -> Self {
        Acceptance::CommandSucceeds {
            program: "cargo",
            args: &["test", "-q"],
            timeout: Duration::from_secs(120),
        }
    }
}

/// An ordered, named set of tasks.
#[derive(Debug, Clone)]
pub struct BenchSuite {
    pub name: &'static str,
    pub tasks: Vec<BenchTask>,
}

impl BenchSuite {
    pub fn families(&self) -> Vec<&'static str> {
        let mut f: Vec<_> = self.tasks.iter().map(|t| t.family).collect();
        f.sort_unstable();
        f.dedup();
        f
    }

    pub fn get(&self, id: &str) -> Option<&BenchTask> {
        self.tasks.iter().find(|t| t.id == id)
    }
}

/// Where fixtures live, resolved from this crate's manifest directory so it
/// works regardless of the caller's cwd.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("fixture: {0}")]
    Fixture(#[from] FixtureError),
    #[error("ledger: {0}")]
    Ledger(#[from] kedge_ledger::LedgerError),
    #[error("engine: {0}")]
    Engine(#[from] kedge_core::HarnessError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A [`kedge_core::TaskId`] derived from a stable string rather than a random
/// v4 UUID.
///
/// `TaskId::new()` calls `Uuid::new_v4()`, which would make every ledger row and
/// every report differ between two otherwise identical runs — and a corpus whose
/// identity changes on each run cannot serve as a fixed baseline. FNV-1a is used
/// rather than `DefaultHasher` because `DefaultHasher`'s output is explicitly
/// not stable across Rust releases, and this value is written to a database.
pub fn stable_task_id(name: &str) -> kedge_core::TaskId {
    const OFFSET_A: u64 = 0xcbf2_9ce4_8422_2325;
    const OFFSET_B: u64 = 0x9dcf_1a2b_3c4d_5e6f;
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&fnv1a64(name.as_bytes(), OFFSET_A).to_be_bytes());
    bytes[8..].copy_from_slice(&fnv1a64(name.as_bytes(), OFFSET_B).to_be_bytes());
    kedge_core::TaskId(uuid::Uuid::from_bytes(bytes))
}

fn fnv1a64(data: &[u8], seed: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = seed;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Serialized reference to a task, for reports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRef {
    pub id: String,
    pub family: String,
}

impl From<&BenchTask> for TaskRef {
    fn from(t: &BenchTask) -> Self {
        TaskRef {
            id: t.id.to_string(),
            family: t.family.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_task_ids_are_stable_and_distinct() {
        assert_eq!(stable_task_id("clamp-001"), stable_task_id("clamp-001"));
        assert_ne!(stable_task_id("clamp-001"), stable_task_id("clamp-002"));
        // Not the nil UUID, and not v4-random-looking by accident.
        assert_ne!(stable_task_id("x").0, uuid::Uuid::nil());
    }

    #[test]
    fn every_task_id_in_the_suite_is_unique() {
        let s = suite();
        let mut ids: Vec<_> = s.tasks.iter().map(|t| t.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate task id in the suite");

        // And the derived UUIDs must not collide either.
        let mut uuids: Vec<_> = s.tasks.iter().map(|t| stable_task_id(t.id)).collect();
        uuids.sort_unstable_by_key(|u| u.0);
        uuids.dedup_by_key(|u| u.0);
        assert_eq!(uuids.len(), before, "stable_task_id collision");
    }
}
