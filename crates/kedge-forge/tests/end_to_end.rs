//! The deterministic skeleton, end to end.
//!
//! ```text
//! kedge-bench   runs a task, kedge-ledger records it
//!       ↓
//! observe       what did it actually touch?
//!       ↓
//! reach         how much authority is that, in files?
//!       ↓
//! registry      store it as a candidate, with lineage
//!       ↓
//! gate          may it be promoted?
//! ```
//!
//! No LLM anywhere in the path. Every step is a function of the recorded run.

use std::sync::Arc;

use kedge_bench::{fixture, fixtures_dir, runner, suite, ScriptedReasoner};
use kedge_forge::{
    gate, general_agent_manifest, observe_verified, reach, EvalOutcome, GateReason, Registry,
    SkillRecord,
};
use kedge_ledger::Ledger;
use kedge_skill::Manifest;

fn compile(toml: &str) -> Manifest {
    Manifest::from_toml_str(toml, &std::collections::HashMap::new()).expect("manifest")
}

fn passing_eval() -> EvalOutcome {
    EvalOutcome {
        suite: "repair-v1".into(),
        passed: true,
        detail: "all metrics passed".into(),
    }
}

#[tokio::test]
async fn a_recorded_run_becomes_a_promoted_least_privilege_skill() {
    let suite = suite();
    let ledger = Ledger::in_memory().expect("ledger");
    let registry = Registry::in_memory().expect("registry");
    let scratch = std::env::temp_dir()
        .canonicalize()
        .expect("temp")
        .join("kedge-forge-e2e");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");

    // 1. Run the suite; the ledger records every trajectory.
    let reasoner = Arc::new(ScriptedReasoner::for_suite(&suite));
    let report = runner::run_suite(&suite, reasoner, &ledger, &fixtures_dir(), &scratch)
        .await
        .expect("bench");
    assert_eq!(report.solved(), 20, "{}", report.to_pretty());

    let mut promoted = 0;

    for task in &suite.tasks {
        // 2. Observe what the run actually needed.
        let trajectory = ledger
            .replay(kedge_bench::stable_task_id(task.id))
            .expect("replay");
        let observed = observe_verified(&trajectory, &scratch.join(task.id), task.id, "0.1.0")
            .await
            .expect("observe");

        // 3. Measure it, against the workspace it ran in.
        let ws = fixture::materialize(task, &fixtures_dir(), &scratch).expect("materialize");
        let learned = compile(&observed.manifest(task.id, "0.1.0"));
        let learned_reach = reach(&learned, &ws.root).expect("reach");

        // The baseline: what a general-purpose agent holds today.
        let general = compile(&general_agent_manifest(&ws.root, &["cargo"]));
        let general_reach = reach(&general, &ws.root).expect("reach");

        // 4. Store both — the baseline as the parent, so lineage is real.
        //
        // Red-team A6: this used to be built with `from_observation`, which sets
        // `manifest_toml` to the *learned* manifest. The record therefore had
        // general-agent reach numbers attached to a tight manifest, and the
        // gate's containment check compared the candidate's commands against
        // its own. The test passed for the wrong reason. The baseline now
        // carries the general-agent manifest it claims to be.
        let baseline_record = SkillRecord {
            manifest_toml: general_agent_manifest(&ws.root, &["cargo"]),
            ..SkillRecord::from_observation(&observed, task.id, "0.0.0-general", general_reach)
        };
        assert!(
            baseline_record.manifest_toml.contains("/**"),
            "A6 REGRESSION: the baseline is not a general-agent manifest"
        );
        registry
            .insert_candidate(&baseline_record)
            .expect("insert baseline");
        registry
            .promote(
                baseline_record.id,
                &gate(&baseline_record, None, Some(&passing_eval())),
            )
            .expect("promote baseline");

        let candidate = SkillRecord::from_observation(&observed, task.id, "0.1.0", learned_reach)
            .with_parent(baseline_record.id);
        registry.insert_candidate(&candidate).expect("insert");

        // 5. Gate it against the baseline.
        let verdict = gate(&candidate, Some(&baseline_record), Some(&passing_eval()));
        assert!(
            verdict.promote,
            "{} was refused:\n{}",
            task.id,
            verdict.report()
        );
        registry.promote(candidate.id, &verdict).expect("promote");
        promoted += 1;

        // The learned skill is now current, and its lineage records where it
        // came from.
        let current = registry.current(task.id).expect("current").expect("some");
        assert_eq!(current.id, candidate.id);
        assert_eq!(current.version, "0.1.0");

        let chain = registry.lineage(candidate.id).expect("lineage");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].version, "0.0.0-general");
        assert!(
            chain[1].reach.writable < chain[0].reach.writable,
            "{}: the learned skill is not tighter than what it replaced",
            task.id
        );
    }

    assert_eq!(promoted, 20);

    // Every promotion is in the history, in order, with the gate's reasoning.
    let history = registry.history().expect("history");
    assert_eq!(history.len(), 40, "20 baselines + 20 learned skills");
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));

    let _ = std::fs::remove_dir_all(&scratch);
}

/// The gate refusing a real regression, not a synthetic one.
#[tokio::test]
async fn a_skill_that_widens_authority_is_refused_and_the_old_one_stays_current() {
    let suite = suite();
    let ledger = Ledger::in_memory().expect("ledger");
    let registry = Registry::in_memory().expect("registry");
    let scratch = std::env::temp_dir()
        .canonicalize()
        .expect("temp")
        .join("kedge-forge-e2e-refuse");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");

    let task = suite.get("clamp-001").unwrap();
    let one = kedge_bench::BenchSuite {
        name: "one",
        tasks: vec![task.clone()],
    };
    let reasoner = Arc::new(ScriptedReasoner::for_suite(&suite));
    runner::run_suite(&one, reasoner, &ledger, &fixtures_dir(), &scratch)
        .await
        .expect("bench");

    let trajectory = ledger
        .replay(kedge_bench::stable_task_id(task.id))
        .expect("replay");
    let observed = observe_verified(&trajectory, &scratch.join(task.id), "repair", "0.1.0")
        .await
        .expect("observe");

    let ws = fixture::materialize(task, &fixtures_dir(), &scratch).expect("materialize");
    let tight = compile(&observed.manifest("repair", "0.1.0"));
    let tight_reach = reach(&tight, &ws.root).expect("reach");

    let good = SkillRecord::from_observation(&observed, "repair", "0.1.0", tight_reach);
    registry.insert_candidate(&good).expect("insert");
    registry
        .promote(good.id, &gate(&good, None, Some(&passing_eval())))
        .expect("promote");

    // A successor that grants the whole workspace — the exact shape of a
    // "generalization" that quietly hands back the authority it removed.
    let wide = compile(&general_agent_manifest(&ws.root, &["cargo"]));
    let wide_reach = reach(&wide, &ws.root).expect("reach");
    assert!(wide_reach.writable > tight_reach.writable);

    let bad = SkillRecord {
        version: "0.2.0".into(),
        reach: wide_reach,
        ..SkillRecord::from_observation(&observed, "repair", "0.2.0", wide_reach)
    }
    .with_parent(good.id);
    registry.insert_candidate(&bad).expect("insert");

    let verdict = gate(&bad, Some(&good), Some(&passing_eval()));
    assert!(!verdict.promote, "{}", verdict.report());
    assert!(verdict
        .reasons
        .iter()
        .any(|r| matches!(r, GateReason::AuthorityWidened { .. })));

    assert!(registry.promote(bad.id, &verdict).is_err());

    // The tight version is still what runs.
    assert_eq!(registry.current("repair").unwrap().unwrap().id, good.id);
    // And the refusal is on the record.
    assert!(registry
        .history()
        .unwrap()
        .iter()
        .any(|h| h.skill == bad.id && h.action == "refused"));

    let _ = std::fs::remove_dir_all(&scratch);
}
