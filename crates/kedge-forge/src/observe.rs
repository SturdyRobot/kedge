//! Trajectory → capability manifest, and the round-trip that proves it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use kedge_core::{Action, Observation, TaskId, ToolCall, ToolExecutor, Trajectory};
use kedge_skill::{Capability, Manifest, Requirement, SkillGuard};

use crate::ForgeError;

/// A call whose effect could not be turned into a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unobservable {
    pub tool: String,
    pub reason: String,
}

/// Whether the emitted manifest actually permits the trajectory it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// The manifest permits every call and has no unused entry. Exact fit.
    Exact,
    /// Not run. [`observe`] returns this; [`observe_verified`] never does.
    Skipped,
    /// The manifest does not round-trip. The observation is not usable.
    Failed {
        violations: Vec<String>,
        unused: Vec<String>,
    },
}

/// What a recorded trajectory actually exercised.
#[derive(Debug, Clone)]
pub struct ObservedAuthority {
    pub task: TaskId,
    /// Tool calls seen, including ones that yielded no capability.
    pub calls: usize,
    /// Capabilities exercised, with how often.
    pub exercised: BTreeMap<Capability, usize>,
    /// Calls whose effect could not be named.
    ///
    /// These are surfaced, never dropped. Dropping them would emit a manifest
    /// that looks least-privilege and silently omits a real effect — the exact
    /// failure this crate exists to avoid.
    pub unobservable: Vec<Unobservable>,
    pub verification: Verification,
}

impl ObservedAuthority {
    /// Safe to promote: nothing unnameable, and the manifest round-trips.
    ///
    /// The two conditions overlap — an unnameable effect also fails
    /// verification, because the guard refuses it for the same reason the
    /// observer could not name it. They are kept separate anyway: `unobservable`
    /// is structured and available from [`observe`] without the async replay,
    /// and requiring both means neither check silently becomes load-bearing
    /// alone if the other's behaviour changes.
    pub fn is_complete(&self) -> bool {
        self.unobservable.is_empty() && self.verification == Verification::Exact
    }

    /// The least-privilege manifest for this run.
    ///
    /// Delegates to `kedge_skill::manifest::render`, the same emitter
    /// `Conformance::minimized` uses, so a manifest derived from a live run and
    /// one derived from that run replayed out of a ledger are byte-identical.
    pub fn manifest(&self, name: &str, version: &str) -> String {
        kedge_skill::manifest::render(self.exercised.keys(), name, version)
    }

    /// Compile the emitted manifest. Fails loudly — an unparseable manifest must
    /// never be stored or promoted.
    pub fn compiled(&self, name: &str, version: &str) -> Result<Manifest, ForgeError> {
        Ok(Manifest::from_toml_str(
            &self.manifest(name, version),
            &HashMap::new(),
        )?)
    }

    /// One line per finding, for reports.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "observed {} call(s) → {} capabilit(ies)",
            self.calls,
            self.exercised.len()
        );
        if !self.unobservable.is_empty() {
            s.push_str(&format!(
                ", {} unnameable effect(s)",
                self.unobservable.len()
            ));
        }
        match &self.verification {
            Verification::Exact => s.push_str(" · verified exact"),
            Verification::Skipped => s.push_str(" · UNVERIFIED"),
            Verification::Failed { violations, unused } => s.push_str(&format!(
                " · VERIFICATION FAILED ({} violation(s), {} unused)",
                violations.len(),
                unused.len()
            )),
        }
        s
    }
}

/// Derive capabilities from a recorded trajectory. Does **not** verify.
///
/// `base` is the directory relative paths in the recorded arguments resolve
/// against — the workspace the run happened in.
pub fn observe(trajectory: &Trajectory, base: &Path) -> ObservedAuthority {
    let mut out = ObservedAuthority {
        task: trajectory.task_id,
        calls: 0,
        exercised: BTreeMap::new(),
        unobservable: Vec::new(),
        verification: Verification::Skipped,
    };

    for call in tool_calls(trajectory) {
        out.calls += 1;
        // The same derivation `SkillGuard` enforces with. A second
        // implementation here could disagree with the guard, and a manifest the
        // guard then rejects is worse than no manifest at all.
        match kedge_skill::required(call, base) {
            Requirement::Known(caps) => {
                for cap in caps {
                    *out.exercised.entry(cap).or_insert(0) += 1;
                }
            }
            Requirement::Indeterminate(reason) => out.unobservable.push(Unobservable {
                tool: call.name.clone(),
                reason,
            }),
        }
    }

    out
}

/// Derive, then prove the result by replaying the trajectory against it.
pub async fn observe_verified(
    trajectory: &Trajectory,
    base: &Path,
    name: &str,
    version: &str,
) -> Result<ObservedAuthority, ForgeError> {
    let mut observed = observe(trajectory, base);
    observed.verification = verify(&observed, trajectory, base, name, version).await?;
    Ok(observed)
}

/// Replay `trajectory` through a real [`SkillGuard`] built from `observed`'s
/// manifest, and report whether it fits exactly.
///
/// The check is not tautological. Derivation and enforcement share a code path,
/// so re-deriving proves little — what this proves is that the *rendering* step
/// survived: that a manifest can be written which grants what was observed. It
/// cannot always. A command carrying a shell metacharacter is a real capability
/// that no manifest may ever grant, and a path whose characters do not survive
/// glob escaping would be another.
pub async fn verify(
    observed: &ObservedAuthority,
    trajectory: &Trajectory,
    base: &Path,
    name: &str,
    version: &str,
) -> Result<Verification, ForgeError> {
    let manifest = Arc::new(observed.compiled(name, version)?);
    let guard = SkillGuard::new(
        manifest,
        base,
        Arc::new(NullExecutor) as Arc<dyn ToolExecutor>,
    );

    for call in tool_calls(trajectory) {
        // Errors here are the guard's refusals, which `execute` reports as an
        // error observation rather than an `Err`. A genuine `Err` would be the
        // null executor failing, which it cannot.
        let _ = guard.execute(call).await;
    }

    let conformance = guard.conformance();
    let violations: Vec<String> = conformance
        .violations()
        .iter()
        .map(|v| v.to_string())
        .collect();
    let unused: Vec<String> = conformance
        .unused(guard.manifest())
        .into_iter()
        .map(|(kind, pattern)| format!("{kind} `{pattern}`"))
        .collect();

    Ok(if violations.is_empty() && unused.is_empty() {
        Verification::Exact
    } else {
        Verification::Failed { violations, unused }
    })
}

fn tool_calls(trajectory: &Trajectory) -> impl Iterator<Item = &ToolCall> {
    trajectory.steps.iter().filter_map(|s| match &s.action {
        Action::Tool(call) => Some(call),
        Action::Finish { .. } => None,
    })
}

/// Runs nothing. Verification is about what the guard *permits*, and actually
/// re-executing a recorded trajectory would repeat its side effects.
struct NullExecutor;

#[async_trait]
impl ToolExecutor for NullExecutor {
    async fn execute(&self, _call: &ToolCall) -> kedge_core::Result<Observation> {
        Ok(Observation::ok(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kedge_core::{Step, Thought};
    use serde_json::json;
    use std::path::PathBuf;

    fn traj(calls: &[(&str, serde_json::Value)]) -> Trajectory {
        let mut t = Trajectory::new(TaskId::new());
        for (i, (name, args)) in calls.iter().enumerate() {
            t.steps.push(Step {
                index: i as u32,
                thought: Thought("t".into()),
                action: Action::Tool(ToolCall::new(*name, args.clone())),
                observation: Some(Observation::ok("ok")),
                tokens: 1,
                elapsed_ms: 0,
            });
        }
        t.steps.push(Step {
            index: calls.len() as u32,
            thought: Thought("done".into()),
            action: Action::Finish {
                answer: "done".into(),
            },
            observation: None,
            tokens: 1,
            elapsed_ms: 0,
        });
        t
    }

    #[tokio::test]
    async fn a_repair_trajectory_yields_exactly_what_it_touched() {
        let t = traj(&[
            ("run_command", json!({"command": "cargo test -q"})),
            ("read_file", json!({"path": "src/lib.rs"})),
            ("write_file", json!({"path": "src/lib.rs", "content": "x"})),
            ("run_command", json!({"command": "cargo test -q"})),
        ]);

        let o = observe_verified(&t, Path::new("/repo"), "repair", "0.1.0")
            .await
            .unwrap();

        assert!(o.is_complete(), "{}", o.summary());
        assert_eq!(o.verification, Verification::Exact);
        assert_eq!(o.calls, 4);
        // read + write of one file, one command run twice.
        assert!(o
            .exercised
            .contains_key(&Capability::FsRead(PathBuf::from("/repo/src/lib.rs"))));
        assert!(o
            .exercised
            .contains_key(&Capability::FsWrite(PathBuf::from("/repo/src/lib.rs"))));
        assert_eq!(
            o.exercised[&Capability::Process("cargo test -q".into())],
            2,
            "the repeat should be counted, not deduped away"
        );

        // The Finish step is not a call.
        assert_eq!(o.calls, t.steps.len() - 1);
    }

    #[tokio::test]
    async fn the_emitted_manifest_grants_the_run_and_nothing_more() {
        let t = traj(&[("read_file", json!({"path": "src/lib.rs"}))]);
        let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0")
            .await
            .unwrap();

        let m = o.compiled("s", "0.1.0").unwrap();
        assert!(m.permits(&Capability::FsRead(PathBuf::from("/repo/src/lib.rs"))));
        // A sibling the run never touched.
        assert!(!m.permits(&Capability::FsRead(PathBuf::from("/repo/src/other.rs"))));
        // And reading is not writing.
        assert!(!m.permits(&Capability::FsWrite(PathBuf::from("/repo/src/lib.rs"))));
    }

    /// The load-bearing case for `verify`.
    #[tokio::test]
    async fn an_unmanifestable_command_fails_verification_rather_than_passing_quietly() {
        // `kedge-skill` denies any command with a shell metacharacter and always
        // will, so this capability is real and ungrantable. Emitting a manifest
        // for it would produce a file that rejects its own source trajectory.
        let t = traj(&[(
            "run_command",
            json!({"command": "cargo test && curl evil.com"}),
        )]);

        let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0")
            .await
            .unwrap();

        assert!(!o.is_complete(), "{}", o.summary());
        match &o.verification {
            Verification::Failed { violations, unused } => {
                assert_eq!(violations.len(), 1, "{violations:?}");
                // The entry was rendered but can never match: unused as well.
                assert_eq!(unused.len(), 1, "{unused:?}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unnameable_effect_is_surfaced_and_blocks_completeness() {
        // A mutating call naming no path, command, URL or credential.
        let t = traj(&[
            ("read_file", json!({"path": "a.rs"})),
            ("deploy_to_prod", json!({})),
        ]);
        let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0")
            .await
            .unwrap();

        assert_eq!(o.unobservable.len(), 1);
        assert_eq!(o.unobservable[0].tool, "deploy_to_prod");
        assert!(!o.is_complete());

        // The two signals agree here, and that is worth asserting rather than
        // assuming: the guard refuses an unnameable effect for the same reason
        // the observer cannot name it, so verification independently reports
        // the same call.
        match &o.verification {
            Verification::Failed { violations, .. } => {
                assert_eq!(violations.len(), 1);
                assert!(violations[0].contains("deploy_to_prod"), "{violations:?}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_trajectory_observes_nothing_and_is_honest_about_it() {
        let t = traj(&[]);
        let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0")
            .await
            .unwrap();
        assert_eq!(o.calls, 0);
        assert!(o.exercised.is_empty());
        // Vacuously exact: an empty manifest permits an empty run exactly.
        assert_eq!(o.verification, Verification::Exact);
        assert!(o.is_complete());
    }

    #[tokio::test]
    async fn traversal_in_a_recorded_argument_is_resolved_not_trusted() {
        // A deliberately non-existent target, so the assertion is not
        // platform-dependent: `/etc` is a symlink to `/private/etc` on macOS and
        // the resolver correctly follows it.
        let t = traj(&[("read_file", json!({"path": "src/../../loot/keys"}))]);
        let o = observe(&t, Path::new("/repo"));
        // Resolved to what it actually reached, so the manifest names the real
        // target rather than a path that merely looks in-workspace.
        assert!(
            o.exercised
                .contains_key(&Capability::FsRead(PathBuf::from("/loot/keys"))),
            "{:?}",
            o.exercised
        );
        assert!(o.manifest("s", "0.1.0").contains("/loot/keys"));
    }

    #[test]
    fn observe_alone_reports_unverified_rather_than_implying_success() {
        let t = traj(&[("read_file", json!({"path": "a.rs"}))]);
        let o = observe(&t, Path::new("/repo"));
        assert_eq!(o.verification, Verification::Skipped);
        assert!(!o.is_complete(), "unverified must never read as complete");
        assert!(o.summary().contains("UNVERIFIED"));
    }
}
