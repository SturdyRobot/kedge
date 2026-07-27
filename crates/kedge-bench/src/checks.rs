//! Suite integrity. These are the checks that decide whether the corpus means
//! anything, and they run before any trajectory is recorded.
//!
//! A benchmark can be wrong in two directions, and only one of them is loud:
//!
//! - A task that is **impossible** shows up as a low solve rate. Annoying, and
//!   obvious the first time you look.
//! - A task that is **already solved** shows up as a high solve rate. It looks
//!   like success. Nothing in a normal test run distinguishes "the agent fixed
//!   it" from "there was nothing to fix."
//!
//! The second is the dangerous one, and [`every_breakage_actually_breaks`] is
//! the only thing standing between it and a published number.

use std::path::Path;

use crate::fixture;
use crate::{BenchSuite, BenchTask, Breakage, FixtureError};

/// One task's integrity verdict.
#[derive(Debug, Clone)]
pub struct TaskIntegrity {
    pub id: String,
    /// The pristine fixture passes its own tests.
    pub pristine_passes: bool,
    /// The breakage pattern matched exactly once.
    pub splice_unique: bool,
    /// The broken fixture *fails* its own tests. The load-bearing one.
    pub breakage_bites: bool,
    pub detail: String,
}

impl TaskIntegrity {
    pub fn ok(&self) -> bool {
        self.pristine_passes && self.splice_unique && self.breakage_bites
    }
}

/// Check every task. Returns one verdict per task, in suite order.
pub async fn verify_suite(
    suite: &BenchSuite,
    fixtures: &Path,
    scratch: &Path,
) -> Result<Vec<TaskIntegrity>, FixtureError> {
    let mut out = Vec::with_capacity(suite.tasks.len());
    for task in &suite.tasks {
        out.push(verify_task(task, fixtures, scratch).await?);
    }
    Ok(out)
}

/// Check one task end to end: pristine passes, splice is unique, broken fails.
pub async fn verify_task(
    task: &BenchTask,
    fixtures: &Path,
    scratch: &Path,
) -> Result<TaskIntegrity, FixtureError> {
    let mut integrity = TaskIntegrity {
        id: task.id.to_string(),
        pristine_passes: false,
        splice_unique: false,
        breakage_bites: false,
        detail: String::new(),
    };

    // 1. Pristine must pass. Otherwise the task is unsolvable for reasons that
    //    have nothing to do with its breakage.
    let ws = fixture::materialize(task, fixtures, scratch)?;
    let pristine = fixture::check_acceptance(task, &ws.root).await?;
    integrity.pristine_passes = pristine.passed;
    if !pristine.passed {
        integrity.detail = format!(
            "pristine fixture already fails:\n{}",
            tail(&pristine.output)
        );
        return Ok(integrity);
    }

    // 2. The splice must be unambiguous. `apply_breakage` is the authority —
    //    re-implementing the count here could disagree with it.
    match fixture::apply_breakage(task, &ws.root) {
        Ok(()) => integrity.splice_unique = true,
        Err(e @ FixtureError::SpliceNotUnique { .. }) => {
            integrity.detail = e.to_string();
            return Ok(integrity);
        }
        Err(other) => return Err(other),
    }

    // 3. The breakage must actually change observable behaviour.
    let broken = fixture::check_acceptance(task, &ws.root).await?;
    integrity.breakage_bites = !broken.passed;
    if broken.passed {
        let Breakage::Splice { find, replace, .. } = &task.breakage;
        integrity.detail = format!(
            "breakage is a no-op: the fixture still passes after replacing\n  {find:?}\nwith\n  {replace:?}\n\
             The task would report as solved without the agent doing anything."
        );
    } else if broken.timed_out {
        // A timeout is a failure, but not the *kind* of failure we want: it
        // proves nothing about the breakage.
        integrity.breakage_bites = false;
        integrity.detail = "breakage caused a timeout, not a test failure".to_string();
    }

    Ok(integrity)
}

fn tail(s: &str) -> String {
    s.lines().rev().take(12).collect::<Vec<_>>().join("\n")
}

/// Format a set of verdicts for a human.
pub fn report(verdicts: &[TaskIntegrity]) -> String {
    let bad: Vec<_> = verdicts.iter().filter(|v| !v.ok()).collect();
    let mut s = format!(
        "kedge-bench — suite integrity: {}/{} tasks sound\n",
        verdicts.len() - bad.len(),
        verdicts.len()
    );
    for v in bad {
        s.push_str(&format!(
            "\n  ✘ {}  pristine={} unique={} bites={}\n      {}\n",
            v.id,
            v.pristine_passes,
            v.splice_unique,
            v.breakage_bites,
            v.detail.replace('\n', "\n      ")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fixtures_dir, suite};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let p = fixture::scratch_root().join(tag);
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The single most important test in this crate.
    ///
    /// It is one test rather than three because the three properties are only
    /// meaningful together, and because checking them together costs one
    /// materialization per task instead of three.
    #[tokio::test]
    async fn every_breakage_actually_breaks() {
        let s = suite();
        let dir = scratch("integrity");
        let verdicts = verify_suite(&s, &fixtures_dir(), &dir).await.unwrap();

        let bad: Vec<_> = verdicts.iter().filter(|v| !v.ok()).collect();
        assert!(bad.is_empty(), "\n{}", report(&verdicts));

        // Belt and braces: the loop above must have actually run.
        assert_eq!(verdicts.len(), 20);
        assert!(verdicts.iter().all(|v| v.pristine_passes));
        assert!(verdicts.iter().all(|v| v.breakage_bites));
    }

    /// Guards the guard: if `verify_task` cannot detect a deliberately broken
    /// task definition, its clean verdicts on the real suite mean nothing.
    #[tokio::test]
    async fn the_integrity_check_detects_a_no_op_breakage() {
        // Replaces a string with itself: a splice that changes nothing.
        let sham = BenchTask {
            id: "sham-noop",
            family: "control",
            fixture: "clamp",
            goal: "control",
            breakage: Breakage::Splice {
                file: "src/lib.rs",
                find: "hi - lo + 1",
                replace: "hi - lo + 1",
            },
            acceptance: crate::Acceptance::cargo_test(),
        };
        let dir = scratch("control-noop");
        let v = verify_task(&sham, &fixtures_dir(), &dir).await.unwrap();
        assert!(v.pristine_passes);
        assert!(v.splice_unique);
        assert!(!v.breakage_bites, "a no-op breakage was reported as sound");
        assert!(v.detail.contains("no-op"), "{}", v.detail);
    }

    /// Regression: the shared-target-dir contamination.
    ///
    /// Every fixture copy is `slug 0.0.0`, so with one `CARGO_TARGET_DIR` across
    /// the suite cargo resolved them all to the same artifact — a *pristine*
    /// copy would execute a previously-broken copy's compiled binary and report
    /// `FAILED`. Both directions of the corpus were wrong and nothing in a
    /// passing test run showed it.
    ///
    /// This runs a broken task and then a pristine one on the same fixture, in
    /// that order, which is the sequence that produced the bug.
    #[tokio::test]
    async fn a_broken_task_does_not_contaminate_the_next_task_on_the_same_fixture() {
        let dir = scratch("contamination");
        let s = suite();

        // 1. A genuinely broken slug task.
        let broken = s.get("slug-001").unwrap();
        let first = verify_task(broken, &fixtures_dir(), &dir).await.unwrap();
        assert!(first.ok(), "{}", first.detail);

        // 2. A no-op splice on the same fixture: pristine must still pass.
        let pristine = BenchTask {
            id: "sham-after-broken",
            family: "control",
            fixture: "slug",
            goal: "control",
            breakage: Breakage::Splice {
                file: "src/lib.rs",
                find: "pub fn slugify",
                replace: "pub fn slugify",
            },
            acceptance: crate::Acceptance::cargo_test(),
        };
        let second = verify_task(&pristine, &fixtures_dir(), &dir).await.unwrap();
        assert!(
            second.pristine_passes,
            "a clean fixture failed after a broken one ran — build artifacts are \
             being shared across tasks again:\n{}",
            second.detail
        );
    }

    #[test]
    fn each_workspace_gets_its_own_build_directory() {
        // Keyed on the workspace path, so even the same task id materialized
        // twice concurrently cannot share artifacts.
        let a = fixture::target_dir_for(std::path::Path::new("/scratch/a/slug-001"));
        let b = fixture::target_dir_for(std::path::Path::new("/scratch/b/slug-001"));
        assert_ne!(a, b);
    }

    /// And that an ambiguous pattern is refused rather than applied arbitrarily.
    #[tokio::test]
    async fn the_integrity_check_detects_an_ambiguous_splice() {
        let sham = BenchTask {
            id: "sham-ambiguous",
            family: "control",
            fixture: "clamp",
            goal: "control",
            // `i32` appears many times in the fixture.
            breakage: Breakage::Splice {
                file: "src/lib.rs",
                find: "i32",
                replace: "i64",
            },
            acceptance: crate::Acceptance::cargo_test(),
        };
        let dir = scratch("control-ambiguous");
        let v = verify_task(&sham, &fixtures_dir(), &dir).await.unwrap();
        assert!(!v.splice_unique, "an ambiguous splice was accepted");
        assert!(v.detail.contains("expected exactly 1"), "{}", v.detail);
    }
}
