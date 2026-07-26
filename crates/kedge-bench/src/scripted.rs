//! The reference solver: a [`kedge_core::Reasoner`] that emits a fixed plan.
//!
//! It implements `Reasoner` rather than some bespoke trait so the corpus is
//! produced by the real `ReActEngine` and journalled by the real
//! `LedgerObserver`. A trajectory in this corpus is shaped exactly like one an
//! LLM would have produced, which is the only reason anything learned from it
//! could transfer ([ADR-0002](../../../docs/adr/0002-benchmark-before-distiller.md)).
//!
//! ## It computes the fix, it does not memorize it
//!
//! The plan does not carry the repaired file as a literal. At the write step it
//! reaches back into the trajectory for the content the *previous* `read_file`
//! returned and applies the inverse splice to it. That matters for two reasons:
//! the trajectory contains a genuine read→transform→write dependency for
//! `kedge-forge observe` to find, and the solver cannot accidentally "solve" a
//! task whose fixture it never actually read.
//!
//! ## Honest limitation
//!
//! The step shapes below are *authored*. A scripted corpus therefore cannot
//! answer whether real agent trajectories share repeated structure — the
//! structure in it is whatever was written here. Plans are varied per family to
//! avoid a degenerate all-identical corpus, but that variation is also authored.
//! See `docs/RISKS.md` R9.

use std::collections::HashMap;

use async_trait::async_trait;
use kedge_core::{Action, Decision, Reasoner, Task, Thought, ToolCall, Trajectory};
use serde_json::json;

use crate::{BenchSuite, BenchTask, Breakage};

/// A single scripted step. Resolved against the trajectory at emit time.
#[derive(Debug, Clone)]
enum PlanStep {
    Tool {
        name: &'static str,
        args: serde_json::Value,
    },
    /// Write `file`, repairing it from the last `read_file` observation.
    Repair {
        file: String,
        from: String,
        to: String,
    },
    Finish(String),
}

/// Emits a fixed plan per task. No LLM, no network, no cost.
pub struct ScriptedReasoner {
    plans: HashMap<kedge_core::TaskId, Vec<PlanStep>>,
    /// Tokens attributed per step, so budgets and token metrics are exercised
    /// without pretending to a real count.
    tokens_per_step: u64,
}

impl ScriptedReasoner {
    /// Build plans for every task in a suite.
    pub fn for_suite(suite: &BenchSuite) -> Self {
        let mut plans = HashMap::new();
        for task in &suite.tasks {
            plans.insert(crate::stable_task_id(task.id), plan_for(task));
        }
        ScriptedReasoner {
            plans,
            tokens_per_step: 100,
        }
    }

    pub fn known_tasks(&self) -> usize {
        self.plans.len()
    }
}

/// The plan for one task, shaped by family.
///
/// The variation is deliberate: an all-identical corpus would make any
/// structure-mining result a measurement of this function rather than of
/// agent behaviour.
fn plan_for(task: &BenchTask) -> Vec<PlanStep> {
    let Breakage::Splice {
        file,
        find,
        replace,
        ..
    } = &task.breakage;

    let read = PlanStep::Tool {
        name: "read_file",
        args: json!({ "path": file }),
    };
    let test = PlanStep::Tool {
        name: "run_command",
        args: json!({ "command": "cargo test -q" }),
    };
    let repair = PlanStep::Repair {
        file: file.to_string(),
        // Inverse of the breakage: put back what the splice took out.
        from: replace.to_string(),
        to: find.to_string(),
    };
    let done = PlanStep::Finish(format!("repaired {file}"));

    match task.family {
        // Reproduce, inspect, fix, confirm.
        "numeric-bounds" => vec![test.clone(), read, repair, test, done],

        // Orient in the tree first, then the same loop.
        "config-parse" => vec![
            PlanStep::Tool {
                name: "list_files",
                args: json!({ "path": "src" }),
            },
            read,
            test.clone(),
            repair,
            test,
            done,
        ],

        // Check the manifest before touching money code, and typecheck before
        // running the full suite.
        "money-arithmetic" => vec![
            PlanStep::Tool {
                name: "read_file",
                args: json!({ "path": "Cargo.toml" }),
            },
            test.clone(),
            read,
            repair,
            PlanStep::Tool {
                name: "run_command",
                args: json!({ "command": "cargo check -q" }),
            },
            test,
            done,
        ],

        // Shortest loop: straight to the source.
        _ => vec![read, repair, test, done],
    }
}

/// The content the most recent successful `read_file` returned.
fn last_read(trajectory: &Trajectory) -> Option<&str> {
    trajectory.steps.iter().rev().find_map(|step| {
        let Action::Tool(call) = &step.action else {
            return None;
        };
        if call.name != "read_file" {
            return None;
        }
        step.observation
            .as_ref()
            .filter(|o| !o.is_error)
            .map(|o| o.content.as_str())
    })
}

#[async_trait]
impl Reasoner for ScriptedReasoner {
    async fn next_action(
        &self,
        task: &Task,
        trajectory: &Trajectory,
    ) -> kedge_core::Result<Decision> {
        let Some(plan) = self.plans.get(&task.id) else {
            // Fail loudly. A silent `Finish` here would record a trajectory that
            // solved nothing and looked complete.
            return Ok(Decision {
                thought: Thought("no plan for this task".into()),
                action: Action::Finish {
                    answer: format!("no scripted plan for task {}", task.id),
                },
                tokens: 0,
            });
        };

        let idx = trajectory.steps.len();
        let Some(step) = plan.get(idx) else {
            return Ok(Decision {
                thought: Thought("plan exhausted".into()),
                action: Action::Finish {
                    answer: "plan exhausted".into(),
                },
                tokens: 0,
            });
        };

        let (thought, action) = match step {
            PlanStep::Tool { name, args } => (
                Thought(format!("step {idx}: {name}")),
                Action::Tool(ToolCall::new(*name, args.clone())),
            ),

            PlanStep::Repair { file, from, to } => {
                let Some(current) = last_read(trajectory) else {
                    return Ok(Decision {
                        thought: Thought("cannot repair without having read the file".into()),
                        action: Action::Finish {
                            answer: "no prior read_file observation to repair from".into(),
                        },
                        tokens: self.tokens_per_step,
                    });
                };
                // One replacement. If the pattern is not present the write is a
                // no-op and the final `cargo test` will fail the task — which is
                // the correct outcome, not something to paper over.
                let repaired = current.replacen(from.as_str(), to.as_str(), 1);
                (
                    Thought(format!("step {idx}: repair {file}")),
                    Action::Tool(ToolCall::new(
                        "write_file",
                        json!({ "path": file, "content": repaired }),
                    )),
                )
            }

            PlanStep::Finish(answer) => (
                Thought(format!("step {idx}: done")),
                Action::Finish {
                    answer: answer.clone(),
                },
            ),
        };

        Ok(Decision {
            thought,
            action,
            tokens: self.tokens_per_step,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite;

    #[test]
    fn every_task_in_the_suite_has_a_plan() {
        let s = suite();
        let r = ScriptedReasoner::for_suite(&s);
        assert_eq!(r.known_tasks(), s.tasks.len());
    }

    #[test]
    fn plans_are_not_all_the_same_shape() {
        // If they were, mining the corpus for repeated structure would be
        // measuring `plan_for`, not the domain.
        let s = suite();
        let mut shapes: Vec<Vec<String>> = s
            .tasks
            .iter()
            .map(|t| {
                plan_for(t)
                    .iter()
                    .map(|p| match p {
                        PlanStep::Tool { name, .. } => (*name).to_string(),
                        PlanStep::Repair { .. } => "write_file".into(),
                        PlanStep::Finish(_) => "finish".into(),
                    })
                    .collect()
            })
            .collect();
        shapes.sort();
        shapes.dedup();
        assert!(
            shapes.len() >= 4,
            "only {} distinct plan shapes across 4 families",
            shapes.len()
        );
    }

    #[tokio::test]
    async fn the_repair_step_transforms_what_it_read() {
        let s = suite();
        let task = s.get("clamp-001").unwrap();
        let r = ScriptedReasoner::for_suite(&s);
        let id = crate::stable_task_id(task.id);

        let mut traj = Trajectory::new(id);
        let core_task = Task {
            id,
            goal: task.goal.into(),
            workspace: None,
        };

        // Feed the plan until the write step appears, recording observations the
        // way the engine would.
        for i in 0..6 {
            let d = r.next_action(&core_task, &traj).await.unwrap();
            let observation = match &d.action {
                Action::Tool(c) if c.name == "read_file" => Some(kedge_core::Observation::ok(
                    "pub fn clamp_upper(v: i32, hi: i32) -> i32 { if v > hi { v } else { hi } }",
                )),
                Action::Tool(c) if c.name == "write_file" => {
                    // The assertion: it wrote back the *repaired* source.
                    let content = c.arguments.get("content").unwrap().as_str().unwrap();
                    assert!(
                        content.contains("if v > hi { hi } else { v }"),
                        "repair did not invert the breakage: {content}"
                    );
                    return;
                }
                _ => Some(kedge_core::Observation::ok("ok")),
            };
            traj.steps.push(kedge_core::Step {
                index: i,
                thought: d.thought,
                action: d.action,
                observation,
                tokens: d.tokens,
                elapsed_ms: 0,
            });
        }
        panic!("never reached the write step");
    }
}
