//! # kedge-mesh
//!
//! Bounded subagent supervision. A parent Kedge agent can delegate a sub-task to
//! a child agent that runs in an isolated Tokio task under hard token/step/wall-
//! clock bounds. If the child panics, times out, is cancelled, or blows its
//! budget, the failure is contained: it's journaled, surfaced as a structured
//! [`SubagentResult::Error`], and **never propagates into the parent's task or
//! context** — Tokio's task isolation plus an inner `spawn` guarantee the parent
//! keeps running.

use std::sync::Arc;
use std::time::Duration;

use kedge_core::{
    Action, Budget, Outcome, ReActEngine, Reasoner, Step, StepObserver, Task, TaskId, ToolCall,
    ToolExecutor,
};
use kedge_ledger::Ledger;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error("the subagent is no longer running")]
    ChildGone,
}

/// Hard cap on subagent nesting depth. A subagent that can itself delegate would
/// otherwise let a run fan out as `breadth^depth` tasks — a fork bomb. Delegation
/// beyond this depth is refused outright.
pub const MAX_SUBAGENT_DEPTH: u32 = 4;

/// How long the supervisor waits past a subagent's own budget before killing it
/// outright.
///
/// A subagent that respects its budget stops itself at `timeout_secs` and
/// returns a proper outcome. This margin is for the one that does not: a
/// reasoner or tool that blocks its thread rather than awaiting cannot be
/// cancelled by the engine's `tokio::time::timeout`, and only the supervisor's
/// `abort()` will end it.
///
/// Half a second is long enough to make the ordering unambiguous and short
/// enough that a genuinely stuck child is not left running.
pub const SUPERVISOR_GRACE: Duration = Duration::from_millis(500);

/// Hard bounds and identity for a spawned subagent.
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub name: String,
    pub max_tokens: u64,
    pub max_steps: u64,
    pub timeout_secs: u64,
    pub parent_run_id: TaskId,
    /// Nesting depth: a top-level delegation is 0, its children 1, and so on.
    /// Delegation past [`MAX_SUBAGENT_DEPTH`] is refused (fork-bomb guard).
    pub depth: u32,
}

impl SubagentConfig {
    /// A conservative default: 20k tokens, 8 steps, 60s, depth 0.
    pub fn new(name: impl Into<String>, parent_run_id: TaskId) -> Self {
        SubagentConfig {
            name: name.into(),
            max_tokens: 20_000,
            max_steps: 8,
            timeout_secs: 60,
            parent_run_id,
            depth: 0,
        }
    }

    /// Derive a child config one level deeper. A subagent that spawns its own
    /// subagents MUST use this so the depth counter propagates and the fork-bomb
    /// guard actually fires.
    pub fn child(&self, name: impl Into<String>) -> Self {
        SubagentConfig {
            name: name.into(),
            max_tokens: self.max_tokens,
            max_steps: self.max_steps,
            timeout_secs: self.timeout_secs,
            parent_run_id: self.parent_run_id,
            depth: self.depth.saturating_add(1),
        }
    }
}

/// Parent → child control messages.
#[derive(Debug, Clone)]
pub enum SubagentCommand {
    /// Cooperative pause request (best-effort; not enforced mid-step in this MVP).
    Pause,
    /// Cancel the child immediately.
    Cancel,
    /// Ask the child to emit its current status on the event stream.
    RequestStatus,
}

/// Child → parent streaming events. The parent renders these live.
#[derive(Debug, Clone, PartialEq)]
pub enum SubagentEvent {
    StepCompleted { index: u32 },
    ToolInvoked { name: String },
    Finished { answer: String, tokens: u64 },
    Failed { reason: String, tokens: u64 },
}

/// The terminal outcome handed back to the parent.
#[derive(Debug, Clone, PartialEq)]
pub enum SubagentResult {
    Ok { summary: String, tokens_used: u64 },
    Error { reason: String, tokens_used: u64 },
}

impl SubagentResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, SubagentResult::Ok { .. })
    }
}

/// A live handle to a supervised subagent.
pub struct SubagentHandle {
    events: mpsc::Receiver<SubagentEvent>,
    commands: mpsc::Sender<SubagentCommand>,
    join: JoinHandle<SubagentResult>,
}

impl SubagentHandle {
    /// Send a control command to the child.
    pub async fn command(&self, cmd: SubagentCommand) -> Result<(), MeshError> {
        self.commands
            .send(cmd)
            .await
            .map_err(|_| MeshError::ChildGone)
    }

    /// Fire-and-forget cancel (safe even if the child already exited).
    pub fn cancel(&self) {
        let _ = self.commands.try_send(SubagentCommand::Cancel);
    }

    /// Await the next streamed event, or `None` once the child is done.
    pub async fn next_event(&mut self) -> Option<SubagentEvent> {
        self.events.recv().await
    }

    /// Block until the child terminates and collect its result. Always resolves
    /// to a value — a panicked child yields a structured error, never a panic.
    pub async fn wait(self) -> SubagentResult {
        match self.join.await {
            Ok(result) => result,
            Err(_) => SubagentResult::Error {
                reason: "subagent supervisor task failed".into(),
                tokens_used: 0,
            },
        }
    }
}

/// Spawn `reasoner`/`tools` as a bounded subagent working on `task_prompt`.
///
/// # Safety obligations for the caller (integration contract)
///
/// This primitive supervises a run but does **not** impose the parent's safety
/// posture on it. Whoever wires delegation into the engine MUST:
///
/// - **Wrap `tools` in the same guard chain as the parent** (audit / policy /
///   HITL). This function runs whatever executor it is given, raw — pass a
///   guard-wrapped executor, or a delegated mutation escapes shadow-audit.
/// - **Bound aggregate budget.** Each subagent gets its *own* [`SubagentConfig`]
///   budget; N delegations do not draw down a shared parent ceiling here. Cap the
///   number of delegations and/or derive child budgets from the parent's
///   remaining, or the parent's token/step ceiling is not a real total.
/// - **Propagate depth** via [`SubagentConfig::child`] so the [`MAX_SUBAGENT_DEPTH`]
///   fork-bomb guard (enforced below) actually fires.
pub fn spawn_subagent(
    config: SubagentConfig,
    task_prompt: impl Into<String>,
    reasoner: Arc<dyn Reasoner>,
    tools: Arc<dyn ToolExecutor>,
    ledger: Option<Arc<Ledger>>,
) -> SubagentHandle {
    let (ev_tx, ev_rx) = mpsc::channel(128);
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let prompt = task_prompt.into();
    let join = tokio::spawn(supervise(
        config, prompt, reasoner, tools, ledger, ev_tx, cmd_rx,
    ));
    SubagentHandle {
        events: ev_rx,
        commands: cmd_tx,
        join,
    }
}

/// Streams engine steps to the parent as [`SubagentEvent`]s.
struct ChannelObserver {
    tx: mpsc::Sender<SubagentEvent>,
}

impl StepObserver for ChannelObserver {
    fn on_step(&self, _task: &Task, step: &Step) {
        // try_send: streaming is best-effort; a full/closed channel never blocks
        // or fails the agent loop.
        let _ = self
            .tx
            .try_send(SubagentEvent::StepCompleted { index: step.index });
        if let Action::Tool(call) = &step.action {
            let _ = self.tx.try_send(SubagentEvent::ToolInvoked {
                name: call.name.clone(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    config: SubagentConfig,
    prompt: String,
    reasoner: Arc<dyn Reasoner>,
    tools: Arc<dyn ToolExecutor>,
    ledger: Option<Arc<Ledger>>,
    ev_tx: mpsc::Sender<SubagentEvent>,
    mut cmd_rx: mpsc::Receiver<SubagentCommand>,
) -> SubagentResult {
    // Fork-bomb guard: refuse to run past the maximum nesting depth. Enforced here
    // (not just in the caller) so it holds no matter who invokes the primitive.
    if config.depth > MAX_SUBAGENT_DEPTH {
        let reason = format!(
            "subagent `{}` refused: nesting depth {} exceeds the limit of {}",
            config.name, config.depth, MAX_SUBAGENT_DEPTH
        );
        tracing::warn!(subagent = %config.name, depth = config.depth, "subagent depth limit hit");
        let _ = ev_tx.try_send(SubagentEvent::Failed {
            reason: reason.clone(),
            tokens: 0,
        });
        return SubagentResult::Error {
            reason,
            tokens_used: 0,
        };
    }
    // `timeout_secs` is the engine's budget. The supervisor's hard bound is this
    // plus SUPERVISOR_GRACE, so a subagent that respects its own budget always
    // terminates itself first. See the `hard_bound` comment below.
    let budget = Budget {
        max_tokens: config.max_tokens,
        max_steps: config.max_steps,
        wall_clock: Duration::from_secs(config.timeout_secs),
    };
    let tracker = budget.tracker();
    // A clone lets us read token usage even after aborting the inner task.
    let tracker_read = tracker.clone();
    let engine = ReActEngine::new(reasoner, tools, tracker)
        .with_observer(Arc::new(ChannelObserver { tx: ev_tx.clone() }));
    let task = Task::new(prompt);

    // Run the agent in its OWN task: a panic there surfaces as a JoinError here
    // instead of unwinding the supervisor (and, in turn, the parent).
    let mut inner: JoinHandle<(Outcome, _)> = tokio::spawn(async move { engine.run(&task).await });

    // The supervisor's bound fires strictly AFTER the engine's own budget, and
    // the gap is the entire point.
    //
    // Both used to be `timeout_secs`, which meant the backstop and the thing it
    // backs up were scheduled for the same instant. Which one won was decided by
    // tokio's timer and select! ordering, so an identical hang reported
    // "budget exhausted" on one machine and "timed out (hard wall-clock bound)"
    // on another. CI caught it as a test that passed 25 times locally and failed
    // on a macOS runner.
    //
    // Worse than the flaky message: the outer branch is the one that calls
    // `inner.abort()`. A backstop that only maybe runs is not a backstop. With a
    // grace period the engine always gets first refusal, stops itself cleanly,
    // and keeps its trajectory; the supervisor now only fires when the engine
    // genuinely could not stop itself, which is exactly the case abort exists for.
    let hard_bound = Duration::from_secs(config.timeout_secs) + SUPERVISOR_GRACE;
    let outer_timeout = tokio::time::sleep(hard_bound);
    tokio::pin!(outer_timeout);

    let result = loop {
        tokio::select! {
            joined = &mut inner => break match joined {
                Ok((outcome, _traj)) => outcome_to_result(outcome, tracker_read.tokens_used()),
                Err(e) => {
                    let reason = if e.is_panic() { "subagent panicked" } else { "subagent aborted" };
                    SubagentResult::Error { reason: reason.into(), tokens_used: tracker_read.tokens_used() }
                }
            },
            _ = &mut outer_timeout => {
                inner.abort();
                break SubagentResult::Error {
                    // Report the bound that actually fired, not `timeout_secs`.
                    // Those are no longer the same number, and a supervisor kill
                    // that claims to have happened half a second before it did
                    // is the kind of detail that wastes an hour later.
                    reason: format!(
                        "timed out after {:.1}s (hard wall-clock bound, {}s budget + {}ms supervisor grace)",
                        hard_bound.as_secs_f64(),
                        config.timeout_secs,
                        SUPERVISOR_GRACE.as_millis(),
                    ),
                    tokens_used: tracker_read.tokens_used(),
                };
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(SubagentCommand::Cancel) => {
                    inner.abort();
                    break SubagentResult::Error {
                        reason: "cancelled by parent".into(),
                        tokens_used: tracker_read.tokens_used(),
                    };
                }
                // Pause/RequestStatus are acknowledged; enforcement is a follow-on.
                Some(SubagentCommand::RequestStatus) => {
                    let _ = ev_tx.try_send(SubagentEvent::StepCompleted { index: tracker_read.steps_used() as u32 });
                    continue;
                }
                Some(SubagentCommand::Pause) => continue,
                None => continue, // parent dropped the command channel; keep running
            }
        }
    };

    // Terminal bookkeeping: stream a final event and journal any failure. Failures
    // are contained here — the parent only ever sees the returned `SubagentResult`.
    match &result {
        SubagentResult::Ok {
            summary,
            tokens_used,
        } => {
            let _ = ev_tx.try_send(SubagentEvent::Finished {
                answer: summary.clone(),
                tokens: *tokens_used,
            });
        }
        SubagentResult::Error {
            reason,
            tokens_used,
        } => {
            let _ = ev_tx.try_send(SubagentEvent::Failed {
                reason: reason.clone(),
                tokens: *tokens_used,
            });
            if let Some(ledger) = &ledger {
                if let Err(e) = ledger.record_event(
                    config.parent_run_id,
                    &kedge_ledger::Event::SubagentFailed {
                        name: config.name.clone(),
                        reason: reason.clone(),
                        tokens_used: *tokens_used,
                    },
                ) {
                    // Not side-effect-gating like the audit/HITL events, so we
                    // don't abort — but never drop it silently.
                    tracing::error!(subagent = %config.name, error = %e, "failed to journal SubagentFailed");
                }
            }
        }
    }
    result
}

fn outcome_to_result(outcome: Outcome, tokens: u64) -> SubagentResult {
    match outcome {
        Outcome::Finished { answer } => SubagentResult::Ok {
            summary: answer,
            tokens_used: tokens,
        },
        Outcome::BudgetExhausted { reason } => SubagentResult::Error {
            reason: format!("budget exhausted: {reason}"),
            tokens_used: tokens,
        },
        Outcome::Failed { reason } | Outcome::Interrupted { reason } => SubagentResult::Error {
            reason,
            tokens_used: tokens,
        },
    }
}

// ── parent-side delegation tool ──

/// What a [`SubagentFactory`] produces for a delegated task: the child's bounds
/// plus the reasoner and tools to run it with.
pub type SubagentBuild = (SubagentConfig, Arc<dyn Reasoner>, Arc<dyn ToolExecutor>);

/// Builds the reasoner/tools/config for a named subagent type. Lets a parent's
/// `kedge_delegate_task` tool spin up the right kind of child on demand.
pub trait SubagentFactory: Send + Sync {
    fn build(&self, subagent_type: &str, parent_run_id: TaskId) -> Option<SubagentBuild>;
}

/// A [`ToolExecutor`] a parent agent can be given so its ReAct loop can call
/// `kedge_delegate_task({"subagent_type": "...", "prompt": "..."})` and receive a
/// summarized result — with all subagent failures contained.
pub struct DelegateTool {
    factory: Arc<dyn SubagentFactory>,
    parent_run_id: TaskId,
    ledger: Option<Arc<Ledger>>,
}

impl DelegateTool {
    pub const TOOL_NAME: &'static str = "kedge_delegate_task";

    pub fn new(
        factory: Arc<dyn SubagentFactory>,
        parent_run_id: TaskId,
        ledger: Option<Arc<Ledger>>,
    ) -> Self {
        DelegateTool {
            factory,
            parent_run_id,
            ledger,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for DelegateTool {
    async fn execute(&self, call: &ToolCall) -> kedge_core::Result<kedge_core::Observation> {
        let subagent_type = call
            .arguments
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        let prompt = call
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let Some((config, reasoner, tools)) = self.factory.build(subagent_type, self.parent_run_id)
        else {
            return Ok(kedge_core::Observation::error(format!(
                "unknown subagent type `{subagent_type}`"
            )));
        };

        let handle = spawn_subagent(config, prompt, reasoner, tools, self.ledger.clone());
        // Delegation is contained: a child failure becomes a tool *observation*
        // the parent can react to, not a fault in the parent's run.
        Ok(match handle.wait().await {
            SubagentResult::Ok { summary, .. } => kedge_core::Observation::ok(summary),
            SubagentResult::Error { reason, .. } => kedge_core::Observation::error(reason),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kedge_core::{Decision, Observation, Reasoner, Thought, Trajectory};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn subagent_past_max_depth_is_refused_without_running() {
        // A tool that flips a flag if it ever executes.
        struct SpyTool(Arc<AtomicBool>);
        #[async_trait]
        impl ToolExecutor for SpyTool {
            async fn execute(&self, _c: &ToolCall) -> kedge_core::Result<Observation> {
                self.0.store(true, Ordering::SeqCst);
                Ok(Observation::ok("ran"))
            }
        }
        let ran = Arc::new(AtomicBool::new(false));
        let mut config = SubagentConfig::new("too-deep", TaskId::new());
        config.depth = MAX_SUBAGENT_DEPTH + 1;

        let handle = spawn_subagent(
            config,
            "do something",
            Arc::new(LoopingReasoner),
            Arc::new(SpyTool(ran.clone())),
            None,
        );
        let result = handle.wait().await;
        assert!(
            matches!(result, SubagentResult::Error { .. }),
            "over-depth subagent must be refused"
        );
        assert!(!ran.load(Ordering::SeqCst), "its tools must never execute");
    }

    /// A reasoner that never finishes — always issues another tool call. Drives the
    /// engine straight into its step budget (a classic runaway loop).
    struct LoopingReasoner;
    #[async_trait]
    impl Reasoner for LoopingReasoner {
        async fn next_action(&self, _t: &Task, _tr: &Trajectory) -> kedge_core::Result<Decision> {
            Ok(Decision {
                thought: Thought("loop forever".into()),
                action: Action::Tool(ToolCall::new("noop", serde_json::json!({}))),
                tokens: 1,
            })
        }
    }

    /// A reasoner that hangs on its first think — the step budget never triggers,
    /// so only the hard wall-clock timeout can rescue the parent.
    struct HangingReasoner;
    #[async_trait]
    impl Reasoner for HangingReasoner {
        async fn next_action(&self, _t: &Task, _tr: &Trajectory) -> kedge_core::Result<Decision> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    struct NoopTool;
    #[async_trait]
    impl ToolExecutor for NoopTool {
        async fn execute(&self, _c: &ToolCall) -> kedge_core::Result<Observation> {
            Ok(Observation::ok("ok"))
        }
    }

    #[tokio::test]
    async fn runaway_subagent_is_capped_at_max_steps_and_parent_survives() {
        let ledger = Arc::new(Ledger::in_memory().unwrap());
        let parent = TaskId::new();
        let mut cfg = SubagentConfig::new("runaway", parent);
        cfg.max_steps = 5;
        cfg.timeout_secs = 60; // won't trigger; the step budget bites first

        let handle = spawn_subagent(
            cfg,
            "loop please",
            Arc::new(LoopingReasoner),
            Arc::new(NoopTool),
            Some(ledger.clone()),
        );
        let result = handle.wait().await;

        // Terminated by the step budget, not by hanging the runtime.
        match &result {
            SubagentResult::Error { reason, .. } => assert!(reason.contains("budget")),
            other => panic!("expected budget error, got {other:?}"),
        }
        // The failure was journaled…
        let events = ledger.events(parent).unwrap();
        assert!(events.iter().any(
            |e| matches!(e, kedge_ledger::Event::SubagentFailed { name, .. } if name == "runaway")
        ));

        // …and the PARENT runtime is completely unharmed.
        let alive = tokio::spawn(async { 2 + 2 }).await.unwrap();
        assert_eq!(alive, 4);
    }

    /// A subagent whose reasoner awaits forever is stopped by its OWN budget,
    /// not by the supervisor. That ordering is the contract, so it is asserted
    /// exactly rather than with a "contains a time word" check.
    ///
    /// This test used to assert `"timed out"`, the supervisor's message, and it
    /// was a coin flip: the engine budget and the supervisor bound were both set
    /// to `timeout_secs`, so two timers came due at the same instant and tokio
    /// picked one. It passed 25 consecutive local runs and failed on a macOS CI
    /// runner. The failure message now includes the reason, because "assertion
    /// failed: reason.contains(...)" told us nothing about which layer had won.
    #[tokio::test(start_paused = true)]
    async fn a_hanging_reasoner_is_stopped_by_the_engines_own_budget() {
        let parent = TaskId::new();
        let mut cfg = SubagentConfig::new("hanger", parent);
        cfg.timeout_secs = 2;
        cfg.max_steps = 1_000_000; // never reached

        let handle = spawn_subagent(
            cfg,
            "hang",
            Arc::new(HangingReasoner),
            Arc::new(NoopTool),
            None,
        );
        // With paused time, the runtime auto-advances to the timer instantly.
        let result = handle.wait().await;
        match result {
            SubagentResult::Error { reason, .. } => assert!(
                reason.contains("budget exhausted") && reason.contains("wall-clock"),
                "the engine budget should win by SUPERVISOR_GRACE, got: {reason}"
            ),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// The backstop, on the only child that needs one.
    ///
    /// The engine bounds a slow call with `tokio::time::timeout`, which cancels
    /// by dropping the future. A reasoner that blocks its thread instead of
    /// awaiting cannot be dropped, so the engine's budget is powerless and only
    /// the supervisor's `abort()` ends the run. Multi-threaded on purpose: on a
    /// current-thread runtime a blocking reasoner would wedge the supervisor too
    /// and there would be nothing left to do the aborting.
    ///
    /// Real time, not paused, because `start_paused` cannot advance a clock past
    /// a thread that is asleep in `std::thread::sleep`. That is the whole point
    /// of the scenario.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_supervisor_backstop_fires_when_the_engine_cannot_stop_itself() {
        struct ThreadBlockingReasoner;
        #[async_trait]
        impl Reasoner for ThreadBlockingReasoner {
            async fn next_action(
                &self,
                _t: &Task,
                _tr: &Trajectory,
            ) -> kedge_core::Result<Decision> {
                // Blocks the worker thread: uncancellable by design. Bounded so a
                // failing test cannot hold the runtime open for long.
                std::thread::sleep(Duration::from_secs(3));
                unreachable!()
            }
        }

        let parent = TaskId::new();
        let mut cfg = SubagentConfig::new("blocker", parent);
        cfg.timeout_secs = 1;
        cfg.max_steps = 1_000_000;

        let started = std::time::Instant::now();
        let handle = spawn_subagent(
            cfg,
            "block",
            Arc::new(ThreadBlockingReasoner),
            Arc::new(NoopTool),
            None,
        );
        let result = handle.wait().await;
        let elapsed = started.elapsed();

        match result {
            SubagentResult::Error { reason, .. } => assert!(
                reason.contains("timed out") && reason.contains("hard wall-clock bound"),
                "the supervisor should be the one to end this, got: {reason}"
            ),
            other => panic!("expected a supervisor timeout, got {other:?}"),
        }
        // It returned on the supervisor's schedule, not the blocking sleep's.
        assert!(
            elapsed < Duration::from_secs(4),
            "supervisor waited for the blocked thread instead of aborting: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn delegate_tool_contains_child_failure_as_observation() {
        struct Factory;
        impl SubagentFactory for Factory {
            fn build(&self, subagent_type: &str, parent: TaskId) -> Option<SubagentBuild> {
                if subagent_type != "looper" {
                    return None;
                }
                let mut cfg = SubagentConfig::new("looper", parent);
                cfg.max_steps = 3;
                Some((cfg, Arc::new(LoopingReasoner), Arc::new(NoopTool)))
            }
        }

        let tool = DelegateTool::new(Arc::new(Factory), TaskId::new(), None);
        let call = ToolCall::new(
            DelegateTool::TOOL_NAME,
            serde_json::json!({ "subagent_type": "looper", "prompt": "go" }),
        );
        let obs = tool.execute(&call).await.unwrap();
        // A runaway child yields an *error observation* the parent can react to —
        // it does not fault the parent's run.
        assert!(format!("{obs:?}").to_lowercase().contains("budget"));
    }
}
