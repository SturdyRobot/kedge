//! The promotion gate. Deny-by-default, like everything else here.
//!
//! A skill is a thing that will be handed authority on future runs, so the
//! question the gate answers is not "is this good?" but "is there any reason
//! this should not be trusted?" — and every reason it finds is reported.
//!
//! ## The one invariant
//!
//! ```text
//! verdict.promote == verdict.reasons.is_empty()
//! ```
//!
//! A denial with no reason is a bug, and a promotion carrying a blocking reason
//! is a worse one. Informational findings go in [`GateVerdict::notes`] instead,
//! which is why `NoBaseline` — worth recording, not worth blocking — does not
//! live in `reasons`. Mixing the two is how a gate quietly stops gating.

use kedge_skill::{Capability, Manifest};

use crate::registry::SkillRecord;

/// A blocking finding. Any one of these refuses promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateReason {
    /// The origin run had calls the manifest would refuse.
    ConformanceViolation { detail: String },
    /// The run contained effects that could not be named.
    Incomplete { indeterminate: usize },
    /// Authority grew in some countable dimension relative to the baseline.
    AuthorityWidened {
        dimension: &'static str,
        from: usize,
        to: usize,
    },
    /// The candidate claims a command or host the baseline never permitted.
    ///
    /// Separate from [`GateReason::AuthorityWidened`] because counting entries
    /// is the wrong test here. A baseline of `["cargo"]` is *one* entry and
    /// permits every cargo subcommand; a candidate of `["cargo check -q",
    /// "cargo test -q"]` is *two* entries and permits strictly less. By count
    /// the candidate looks like a widening, and it is the opposite. So commands
    /// and hosts are compared by asking the baseline manifest whether it would
    /// have permitted each of the candidate's entries.
    GrantNotCoveredByBaseline { kind: String, entry: String },
    /// A manifest could not be compiled, so nothing about it can be checked.
    UnparseableManifest { whose: &'static str, detail: String },
    /// A grant reaches outside the workspace.
    EscapesRoot,
    /// The authority measurement was truncated, so it is a lower bound.
    ReachTruncated,
    /// A regression suite failed.
    EvalRegressed { metric: String, detail: String },
}

impl std::fmt::Display for GateReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateReason::ConformanceViolation { detail } => {
                write!(f, "the origin run was not conformant: {detail}")
            }
            GateReason::Incomplete { indeterminate } => write!(
                f,
                "{indeterminate} effect(s) in the origin run could not be named, so the \
                 manifest cannot describe what this skill does"
            ),
            GateReason::AuthorityWidened {
                dimension,
                from,
                to,
            } => write!(f, "authority widened: {dimension} {from} → {to}"),
            GateReason::EscapesRoot => {
                write!(f, "a grant reaches outside the workspace")
            }
            GateReason::ReachTruncated => write!(
                f,
                "the authority measurement was truncated, so it is a lower bound \
                 and cannot be compared"
            ),
            GateReason::GrantNotCoveredByBaseline { kind, entry } => write!(
                f,
                "the baseline never permitted {kind} `{entry}` — this widens authority"
            ),
            GateReason::UnparseableManifest { whose, detail } => {
                write!(f, "the {whose} manifest does not compile: {detail}")
            }
            GateReason::EvalRegressed { metric, detail } => {
                write!(f, "eval `{metric}` regressed: {detail}")
            }
        }
    }
}

/// A non-blocking finding, recorded so the audit trail says the gate was thin
/// rather than that it was clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateNote {
    /// Nothing to compare against; the authority check could not run.
    NoBaseline,
    /// No regression suite was supplied.
    NoEval,
}

impl std::fmt::Display for GateNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateNote::NoBaseline => write!(
                f,
                "no baseline to compare against — the authority check did not run"
            ),
            GateNote::NoEval => write!(f, "no regression suite was supplied"),
        }
    }
}

/// The gate's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    pub promote: bool,
    /// Blocking. Non-empty exactly when `promote` is false.
    pub reasons: Vec<GateReason>,
    /// Informational. Never affects `promote`.
    pub notes: Vec<GateNote>,
}

impl GateVerdict {
    pub fn report(&self) -> String {
        let mut s = if self.promote {
            "promote: YES".to_string()
        } else {
            "promote: NO".to_string()
        };
        for r in &self.reasons {
            s.push_str(&format!("\n  ✘ {r}"));
        }
        for n in &self.notes {
            s.push_str(&format!("\n  · {n}"));
        }
        s
    }
}

/// A regression-suite result, reduced to what the gate needs.
///
/// Deliberately not `kedge_eval::EvalReport`: the gate uses two fields, and
/// taking a dependency on `kedge-eval` to get them would tie this crate to a
/// crate it otherwise has no relationship with. Map at the call site.
#[derive(Debug, Clone)]
pub struct EvalOutcome {
    pub suite: String,
    pub passed: bool,
    pub detail: String,
}

/// Decide whether `candidate` may be promoted.
///
/// Every condition is checked even after one fails, so a denial reports
/// *everything* wrong rather than the first thing wrong. Fixing findings one
/// round-trip at a time is how a review turns into a slog.
pub fn gate(
    candidate: &SkillRecord,
    baseline: Option<&SkillRecord>,
    eval: Option<&EvalOutcome>,
) -> GateVerdict {
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    // 1. The origin run must have been conformant.
    for v in &candidate.violations {
        reasons.push(GateReason::ConformanceViolation { detail: v.clone() });
    }

    // 2. Nothing unnameable.
    if candidate.indeterminate > 0 {
        reasons.push(GateReason::Incomplete {
            indeterminate: candidate.indeterminate,
        });
    }

    // 3 & 4. The measurement must be sound before it can be compared.
    if candidate.reach.escapes_root {
        reasons.push(GateReason::EscapesRoot);
    }
    if candidate.reach.truncated {
        reasons.push(GateReason::ReachTruncated);
    }

    // 5. Authority may never widen. Equal is fine — an equally tight skill is
    //    not a regression — but any single dimension growing is a refusal, even
    //    when every other dimension shrank. That is a trade, and a human decides
    //    trades.
    match baseline {
        None => notes.push(GateNote::NoBaseline),
        Some(base) => {
            if base.reach.truncated {
                reasons.push(GateReason::ReachTruncated);
            }
            // Filesystem is compared by *count*, because files under the root
            // are enumerable and a count is exact.
            for (dimension, from, to) in [
                (
                    "filesystem.write",
                    base.reach.writable,
                    candidate.reach.writable,
                ),
                (
                    "filesystem.read",
                    base.reach.readable,
                    candidate.reach.readable,
                ),
            ] {
                if to > from {
                    reasons.push(GateReason::AuthorityWidened {
                        dimension,
                        from,
                        to,
                    });
                }
            }

            // Commands and hosts are compared by *containment*, because there
            // is no finite set to count. Ask the baseline manifest whether it
            // would have permitted each of the candidate's grants.
            reasons.extend(uncovered_grants(candidate, base));
        }
    }

    // 6. Regressions block.
    match eval {
        None => notes.push(GateNote::NoEval),
        Some(e) if !e.passed => reasons.push(GateReason::EvalRegressed {
            metric: e.suite.clone(),
            detail: e.detail.clone(),
        }),
        Some(_) => {}
    }

    GateVerdict {
        promote: reasons.is_empty(),
        reasons,
        notes,
    }
}

/// Command and host grants the candidate claims that the baseline never had.
///
/// Uses `Manifest::permits` — the real enforcement path — rather than
/// re-deriving containment, so the gate cannot disagree with the guard about
/// what a grant means.
fn uncovered_grants(candidate: &SkillRecord, base: &SkillRecord) -> Vec<GateReason> {
    let vars = std::collections::HashMap::new();
    let compiled = |toml: &str, whose: &'static str| {
        Manifest::from_toml_str(toml, &vars).map_err(|e| GateReason::UnparseableManifest {
            whose,
            detail: e.to_string(),
        })
    };

    let cand = match compiled(&candidate.manifest_toml, "candidate") {
        Ok(m) => m,
        Err(r) => return vec![r],
    };
    let baseline = match compiled(&base.manifest_toml, "baseline") {
        Ok(m) => m,
        Err(r) => return vec![r],
    };

    cand.declared()
        .into_iter()
        .filter_map(|(kind, entry)| {
            let cap = match kind {
                "process" => Capability::Process(entry.clone()),
                "network" => Capability::Network(entry.clone()),
                // Filesystem is handled by the count comparison above; secrets
                // are compared by name.
                "secrets" => Capability::Secret(entry.clone()),
                _ => return None,
            };
            (!baseline.permits(&cap)).then(|| GateReason::GrantNotCoveredByBaseline {
                kind: kind.to_string(),
                entry,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::SkillId;
    use crate::Reach;
    use kedge_core::TaskId;

    fn reach(writable: usize, readable: usize, commands: usize, hosts: usize) -> Reach {
        Reach {
            writable,
            readable,
            commands,
            hosts,
            escapes_root: false,
            truncated: false,
            files_scanned: 100,
        }
    }

    /// A record whose manifest actually compiles. `commands` matters: the
    /// process/network check reads the manifest, not the counts.
    fn record_with(reach: Reach, commands: &[&str]) -> SkillRecord {
        let mut toml = String::from("[skill]\nname = \"s\"\nversion = \"0.1.0\"\n");
        if !commands.is_empty() {
            toml.push_str("\n[capabilities.process]\nallow = [");
            for c in commands {
                toml.push_str(&format!("\n  {c:?},"));
            }
            toml.push_str("\n]\n");
        }
        SkillRecord {
            id: SkillId::new(),
            name: "s".into(),
            version: "0.1.0".into(),
            parent: None,
            manifest_toml: toml,
            origin_run: TaskId::new(),
            reach,
            violations: Vec::new(),
            indeterminate: 0,
            promoted: false,
        }
    }

    fn record(reach: Reach) -> SkillRecord {
        record_with(reach, &["cargo test"])
    }

    fn eval_ok() -> EvalOutcome {
        EvalOutcome {
            suite: "repair-v1".into(),
            passed: true,
            detail: "all metrics passed".into(),
        }
    }

    #[test]
    fn a_clean_candidate_with_a_tighter_baseline_promotes() {
        let base = record(reach(100, 100, 2, 0));
        let cand = record(reach(3, 5, 2, 0));
        let v = gate(&cand, Some(&base), Some(&eval_ok()));
        assert!(v.promote, "{}", v.report());
        assert!(v.reasons.is_empty());
        assert!(v.notes.is_empty());
    }

    #[test]
    fn equal_authority_is_not_a_regression() {
        let base = record(reach(3, 5, 2, 0));
        let cand = record(reach(3, 5, 2, 0));
        assert!(gate(&cand, Some(&base), Some(&eval_ok())).promote);
    }

    /// Acceptance: widening in **any** single dimension is denied, including
    /// when the candidate narrows in every other.
    #[test]
    fn widening_one_dimension_is_denied_even_while_narrowing_every_other() {
        let base = record(reach(100, 100, 2, 0));
        for (label, cand_reach) in [
            ("write", reach(101, 1, 1, 0)),
            ("read", reach(1, 101, 1, 0)),
        ] {
            let v = gate(&record(cand_reach), Some(&base), Some(&eval_ok()));
            assert!(!v.promote, "{label} widening was allowed:\n{}", v.report());
            assert!(v
                .reasons
                .iter()
                .any(|r| matches!(r, GateReason::AuthorityWidened { .. })));
        }

        // A command the baseline never permitted, while shrinking every file
        // count to almost nothing.
        let sneaky = record_with(reach(1, 1, 1, 0), &["cargo test", "curl evil.com"]);
        let v = gate(&sneaky, Some(&base), Some(&eval_ok()));
        assert!(!v.promote, "{}", v.report());
        assert!(v
            .reasons
            .iter()
            .any(|r| matches!(r, GateReason::GrantNotCoveredByBaseline { .. })));
    }

    /// The bug that a real end-to-end run found.
    ///
    /// A baseline of `["cargo"]` is one entry permitting every cargo
    /// subcommand. A candidate of `["cargo check -q", "cargo test -q"]` is two
    /// entries permitting strictly less. By entry count that reads as a
    /// widening — exactly backwards — so commands are compared by asking the
    /// baseline whether it would have permitted each grant.
    #[test]
    fn more_command_entries_is_not_more_authority() {
        let base = record_with(reach(100, 100, 1, 0), &["cargo"]);
        let cand = record_with(reach(3, 3, 2, 0), &["cargo check -q", "cargo test -q"]);
        let v = gate(&cand, Some(&base), Some(&eval_ok()));
        assert!(
            v.promote,
            "narrower commands were called a widening:\n{}",
            v.report()
        );
    }

    #[test]
    fn a_manifest_that_does_not_compile_blocks_promotion() {
        let mut cand = record(reach(1, 1, 1, 0));
        cand.manifest_toml = "this is not toml [[[".into();
        let v = gate(&cand, Some(&record(reach(9, 9, 9, 0))), Some(&eval_ok()));
        assert!(!v.promote);
        assert!(v
            .reasons
            .iter()
            .any(|r| matches!(r, GateReason::UnparseableManifest { .. })));
    }

    // ── one adversarial case per blocking reason ──

    #[test]
    fn a_non_conformant_origin_run_is_denied() {
        let mut cand = record(reach(1, 1, 0, 0));
        cand.violations = vec!["`write_file`: manifest does not grant …".into()];
        let v = gate(&cand, None, Some(&eval_ok()));
        assert!(!v.promote);
        assert!(matches!(
            v.reasons[0],
            GateReason::ConformanceViolation { .. }
        ));
    }

    #[test]
    fn an_unnameable_effect_is_denied() {
        let mut cand = record(reach(1, 1, 0, 0));
        cand.indeterminate = 2;
        let v = gate(&cand, None, Some(&eval_ok()));
        assert!(!v.promote);
        assert!(v
            .reasons
            .contains(&GateReason::Incomplete { indeterminate: 2 }));
    }

    #[test]
    fn a_grant_outside_the_workspace_is_denied() {
        let mut r = reach(0, 0, 0, 0);
        r.escapes_root = true;
        // Note it reaches *zero* files in-workspace, so a naive count would
        // call this the tightest manifest possible.
        let v = gate(&record(r), None, Some(&eval_ok()));
        assert!(!v.promote);
        assert!(v.reasons.contains(&GateReason::EscapesRoot));
    }

    #[test]
    fn a_truncated_measurement_is_denied_on_either_side() {
        let mut r = reach(1, 1, 0, 0);
        r.truncated = true;
        assert!(!gate(&record(r), None, Some(&eval_ok())).promote);

        // And when it is the *baseline* that is unmeasurable.
        let mut base_r = reach(100, 100, 2, 0);
        base_r.truncated = true;
        let v = gate(
            &record(reach(1, 1, 1, 0)),
            Some(&record(base_r)),
            Some(&eval_ok()),
        );
        assert!(!v.promote);
        assert!(v.reasons.contains(&GateReason::ReachTruncated));
    }

    #[test]
    fn a_failing_regression_suite_is_denied() {
        let eval = EvalOutcome {
            suite: "repair-v1".into(),
            passed: false,
            detail: "tool_call_reduction: 11 → 18".into(),
        };
        let v = gate(&record(reach(1, 1, 0, 0)), None, Some(&eval));
        assert!(!v.promote);
        assert!(matches!(v.reasons[0], GateReason::EvalRegressed { .. }));
    }

    #[test]
    fn a_missing_baseline_is_recorded_but_does_not_block() {
        let v = gate(&record(reach(1, 1, 0, 0)), None, Some(&eval_ok()));
        assert!(v.promote, "{}", v.report());
        assert!(v.notes.contains(&GateNote::NoBaseline));
        // The audit trail must say the gate was thin, not that it was clean.
        assert!(v.report().contains("no baseline"));
    }

    #[test]
    fn a_missing_eval_is_recorded_but_does_not_block() {
        let v = gate(
            &record(reach(1, 1, 0, 0)),
            Some(&record(reach(9, 9, 9, 9))),
            None,
        );
        assert!(v.promote);
        assert!(v.notes.contains(&GateNote::NoEval));
    }

    #[test]
    fn every_finding_is_reported_not_just_the_first() {
        let mut r = reach(500, 500, 9, 9);
        r.escapes_root = true;
        r.truncated = true;
        let mut cand = record(r);
        cand.violations = vec!["v1".into(), "v2".into()];
        cand.indeterminate = 1;

        let v = gate(
            &cand,
            Some(&record(reach(1, 1, 1, 1))),
            Some(&EvalOutcome {
                suite: "s".into(),
                passed: false,
                detail: "d".into(),
            }),
        );
        assert!(!v.promote);
        // 2 violations + incomplete + escapes + truncated + 2 file widenings + eval
        assert_eq!(v.reasons.len(), 8, "{}", v.report());
    }

    /// The invariant, over generated combinations rather than examples.
    #[test]
    fn promote_is_true_exactly_when_there_are_no_blocking_reasons() {
        let mut checked = 0;
        for w in [0usize, 1, 5] {
            for r in [0usize, 1, 5] {
                for escapes in [false, true] {
                    for trunc in [false, true] {
                        for indet in [0usize, 1] {
                            for viol in [0usize, 1] {
                                for eval_pass in [true, false] {
                                    for with_base in [true, false] {
                                        let mut reach_v = reach(w, r, 1, 0);
                                        reach_v.escapes_root = escapes;
                                        reach_v.truncated = trunc;
                                        let mut cand = record(reach_v);
                                        cand.indeterminate = indet;
                                        cand.violations =
                                            (0..viol).map(|i| format!("v{i}")).collect();

                                        let base = record(reach(3, 3, 1, 0));
                                        let ev = EvalOutcome {
                                            suite: "s".into(),
                                            passed: eval_pass,
                                            detail: "d".into(),
                                        };
                                        let v = gate(&cand, with_base.then_some(&base), Some(&ev));
                                        assert_eq!(
                                            v.promote,
                                            v.reasons.is_empty(),
                                            "invariant broken: {}",
                                            v.report()
                                        );
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 3 * 3 * 2 * 2 * 2 * 2 * 2 * 2);
    }
}
