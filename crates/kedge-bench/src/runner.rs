//! Running the suite and recording the corpus.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kedge_core::{Budget, ReActEngine, Reasoner, Task, TaskId, ToolExecutor};
use kedge_ledger::Ledger;
use serde::{Deserialize, Serialize};

use crate::{fixture, BenchError, BenchSuite, BenchTask, TaskRef, WorkspaceTools};

/// What happened on one task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BenchOutcome {
    pub task: TaskRef,
    /// Ledger key. Deterministic — see [`crate::stable_task_id`].
    pub run: String,
    /// Per the fixture's own tests, not per anything the solver reported.
    pub solved: bool,
    pub steps: u32,
    pub tools: Vec<String>,
    /// Excluded from [`BenchReport::fingerprint`]: it is timing, not behaviour.
    pub elapsed_ms: u64,
    /// Set when the task could not even be set up. Distinct from "unsolved".
    pub setup_error: Option<String>,
}

/// The suite result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub suite: String,
    pub outcomes: Vec<BenchOutcome>,
}

impl BenchReport {
    pub fn solved(&self) -> usize {
        self.outcomes.iter().filter(|o| o.solved).count()
    }

    pub fn solve_rate(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 0.0;
        }
        self.solved() as f64 / self.outcomes.len() as f64
    }

    /// Everything about the run except how long it took.
    ///
    /// Two invocations of the same suite must produce the same fingerprint.
    /// `elapsed_ms` is excluded because wall-clock is not behaviour; run ids are
    /// *included* precisely because they are supposed to be deterministic, and a
    /// fingerprint that hid them would not catch it if they stopped being so.
    pub fn fingerprint(&self) -> String {
        let mut s = String::from(&self.suite);
        for o in &self.outcomes {
            s.push_str(&format!(
                "\n{}|{}|{}|{}|{}|{}|{}",
                o.task.id,
                o.task.family,
                o.run,
                o.solved,
                o.steps,
                o.tools.join(","),
                o.setup_error.as_deref().unwrap_or("")
            ));
        }
        s
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn to_pretty(&self) -> String {
        let mut s = format!(
            "kedge-bench — {} · {}/{} solved ({:.0}%)\n\n",
            self.suite,
            self.solved(),
            self.outcomes.len(),
            self.solve_rate() * 100.0
        );
        for o in &self.outcomes {
            s.push_str(&format!(
                "  {}  {:<12} {:<18} {} step(s){}\n",
                if o.solved { "✔" } else { "✘" },
                o.task.id,
                o.task.family,
                o.steps,
                o.setup_error
                    .as_ref()
                    .map(|e| format!("  · setup failed: {e}"))
                    .unwrap_or_default(),
            ));
        }
        s
    }
}

/// Run every task: materialize, break, solve, then ask the oracle.
///
/// The ledger is the point. Each task is journalled as a real run so
/// `kedge-forge` has trajectories to observe.
pub async fn run_suite(
    suite: &BenchSuite,
    reasoner: Arc<dyn Reasoner>,
    ledger: &Ledger,
    fixtures: &Path,
    scratch: &Path,
) -> Result<BenchReport, BenchError> {
    std::fs::create_dir_all(scratch)?;
    let mut outcomes = Vec::with_capacity(suite.tasks.len());

    for task in &suite.tasks {
        outcomes.push(run_one(task, reasoner.clone(), ledger, fixtures, scratch).await?);
    }

    Ok(BenchReport {
        suite: suite.name.to_string(),
        outcomes,
    })
}

async fn run_one(
    task: &BenchTask,
    reasoner: Arc<dyn Reasoner>,
    ledger: &Ledger,
    fixtures: &Path,
    scratch: &Path,
) -> Result<BenchOutcome, BenchError> {
    let id = crate::stable_task_id(task.id);
    let started = Instant::now();

    let mut outcome = BenchOutcome {
        task: TaskRef::from(task),
        run: id.to_string(),
        solved: false,
        steps: 0,
        tools: Vec::new(),
        elapsed_ms: 0,
        setup_error: None,
    };

    // Setup failures are recorded, never conflated with an unsolved task: one
    // means the harness is broken, the other means the agent is.
    let ws = match fixture::materialize(task, fixtures, scratch) {
        Ok(ws) => ws,
        Err(e) => {
            outcome.setup_error = Some(e.to_string());
            outcome.elapsed_ms = started.elapsed().as_millis() as u64;
            return Ok(outcome);
        }
    };
    if let Err(e) = fixture::apply_breakage(task, &ws.root) {
        outcome.setup_error = Some(e.to_string());
        outcome.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(outcome);
    }

    let core_task = Task {
        id,
        goal: task.goal.to_string(),
        workspace: Some(ws.root.to_string_lossy().into_owned()),
    };

    ledger.begin_run(&core_task)?;

    let tools: Arc<dyn ToolExecutor> = Arc::new(WorkspaceTools::new(&ws.root));
    let engine = ReActEngine::new(
        reasoner,
        tools,
        Budget {
            max_tokens: 100_000,
            max_steps: 20,
            wall_clock: Duration::from_secs(180),
        }
        .tracker(),
    )
    .with_observer(ledger.observer());

    let (engine_outcome, trajectory) = engine.run(&core_task).await;

    outcome.steps = trajectory.steps.len() as u32;
    outcome.tools = trajectory
        .steps
        .iter()
        .filter_map(|s| match &s.action {
            kedge_core::Action::Tool(c) => Some(c.name.clone()),
            kedge_core::Action::Finish { .. } => None,
        })
        .collect();

    ledger.finalize(id, &engine_outcome)?;

    // The independent oracle. Note this runs regardless of what the engine
    // reported: an agent that says it succeeded and did not must show up as
    // unsolved.
    match fixture::check_acceptance(task, &ws.root).await {
        Ok(v) => outcome.solved = v.passed,
        Err(e) => outcome.setup_error = Some(e.to_string()),
    }

    outcome.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(outcome)
}

/// Convenience: run the canonical suite with the scripted solver.
pub async fn run_default(ledger: &Ledger) -> Result<BenchReport, BenchError> {
    let suite = crate::suite();
    let reasoner = Arc::new(crate::ScriptedReasoner::for_suite(&suite));
    run_suite(
        &suite,
        reasoner,
        ledger,
        &crate::fixtures_dir(),
        &fixture::scratch_root().join("run"),
    )
    .await
}

/// The ledger ids this suite will write, without running it.
pub fn expected_run_ids(suite: &BenchSuite) -> Vec<TaskId> {
    suite
        .tasks
        .iter()
        .map(|t| crate::stable_task_id(t.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kedge_core::{Action, Decision, Thought, Trajectory};

    /// A solver that does nothing but declare victory.
    struct LiarReasoner;

    #[async_trait]
    impl Reasoner for LiarReasoner {
        async fn next_action(
            &self,
            _task: &Task,
            _trajectory: &Trajectory,
        ) -> kedge_core::Result<Decision> {
            Ok(Decision {
                thought: Thought("nothing to do".into()),
                action: Action::Finish {
                    answer: "fixed it".into(),
                },
                tokens: 1,
            })
        }
    }

    /// The positive control for the *runner*, not the suite.
    ///
    /// The scripted solver scores 100%, which on its own is indistinguishable
    /// from an oracle that always says yes. This proves the difference: a solver
    /// that touches nothing and claims success scores **zero**, because the
    /// verdict comes from the fixture's own tests and not from what the agent
    /// reported.
    ///
    /// Without this, "20/20 solved" would be an unfalsifiable number.
    #[tokio::test]
    async fn a_solver_that_does_nothing_but_claim_success_scores_zero() {
        let suite = crate::suite();
        let ledger = Ledger::in_memory().unwrap();
        let scratch = fixture::scratch_root().join("liar");
        let _ = std::fs::remove_dir_all(&scratch);

        // Three tasks is enough to make the point and keeps the test quick.
        let small = BenchSuite {
            name: "liar-control",
            tasks: suite.tasks.iter().take(3).cloned().collect(),
        };

        let report = run_suite(
            &small,
            Arc::new(LiarReasoner),
            &ledger,
            &crate::fixtures_dir(),
            &scratch,
        )
        .await
        .unwrap();

        assert_eq!(report.solved(), 0, "{}", report.to_pretty());
        assert_eq!(report.solve_rate(), 0.0);
        // And they failed by being unsolved, not by failing to set up.
        assert!(report.outcomes.iter().all(|o| o.setup_error.is_none()));
        // The engine still recorded real runs — the corpus captures failures too.
        assert_eq!(ledger.list_runs().unwrap().len(), 3);
    }

    /// Reproducibility, asserted rather than eyeballed.
    #[tokio::test]
    async fn two_runs_of_the_same_suite_fingerprint_identically() {
        let suite = crate::suite();
        let small = BenchSuite {
            name: "repro",
            tasks: suite.tasks.iter().take(2).cloned().collect(),
        };
        let reasoner = Arc::new(crate::ScriptedReasoner::for_suite(&suite));

        let mut fingerprints = Vec::new();
        for tag in ["repro-a", "repro-b"] {
            let ledger = Ledger::in_memory().unwrap();
            let scratch = fixture::scratch_root().join(tag);
            let _ = std::fs::remove_dir_all(&scratch);
            let r = run_suite(
                &small,
                reasoner.clone(),
                &ledger,
                &crate::fixtures_dir(),
                &scratch,
            )
            .await
            .unwrap();
            fingerprints.push(r.fingerprint());
        }
        assert_eq!(fingerprints[0], fingerprints[1]);
        // Non-trivially: the fingerprint actually contains the run ids, so a
        // random TaskId would have broken this.
        assert!(fingerprints[0].contains("clamp-001"));
    }
}
