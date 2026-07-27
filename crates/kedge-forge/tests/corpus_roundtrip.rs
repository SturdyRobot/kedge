//! S2 acceptance: the round-trip invariant, over the whole corpus.
//!
//! > For every run in the S1 corpus, replaying it under
//! > `observe(run).manifest()` yields 0 violations and 0 unused entries.
//!
//! Over all 20 runs, not a sample. If the observer and the enforcer disagree on
//! even one trajectory, the observer is wrong by definition — a manifest that
//! rejects the run it was derived from is worse than no manifest, because it
//! looks authoritative.
//!
//! The corpus is generated in-process rather than read from a checked-in
//! database, so this test cannot pass against a stale artifact.

use std::sync::Arc;

use kedge_bench::{fixtures_dir, runner, suite, ScriptedReasoner};
use kedge_forge::{observe_verified, Verification};
use kedge_ledger::Ledger;

#[tokio::test]
async fn every_recorded_run_round_trips_through_its_own_manifest() {
    let suite = suite();
    let ledger = Ledger::in_memory().expect("ledger");

    // Canonicalized so the base used here matches the one the run used: on
    // macOS the temp dir is reached through a symlink (`/var` → `/private/var`)
    // and the path resolver follows it.
    let scratch = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join("kedge-forge-roundtrip");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");

    let reasoner = Arc::new(ScriptedReasoner::for_suite(&suite));
    let report = runner::run_suite(&suite, reasoner, &ledger, &fixtures_dir(), &scratch)
        .await
        .expect("bench run");

    assert_eq!(report.outcomes.len(), 20, "corpus is not the full suite");
    assert_eq!(
        report.solved(),
        20,
        "the corpus must be of *successful* runs:\n{}",
        report.to_pretty()
    );

    let mut failures = Vec::new();
    let mut total_caps = 0usize;

    for task in &suite.tasks {
        let run = kedge_bench::stable_task_id(task.id);
        let trajectory = ledger.replay(run).expect("replay");
        assert!(
            !trajectory.steps.is_empty(),
            "{}: empty trajectory — the ledger did not record the run",
            task.id
        );

        let base = scratch.join(task.id);
        let observed = observe_verified(&trajectory, &base, task.family, "0.1.0")
            .await
            .expect("observe");

        total_caps += observed.exercised.len();

        if !observed.is_complete() {
            failures.push(format!(
                "  {} ({})\n      {}\n      {:?}",
                task.id,
                task.family,
                observed.summary(),
                observed.verification
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of 20 runs did not round-trip:\n{}",
        failures.len(),
        failures.join("\n")
    );

    // Non-vacuity: a bug that observed nothing at all would satisfy every
    // assertion above, because an empty manifest permits an empty run exactly.
    assert!(
        total_caps >= 40,
        "only {total_caps} capabilities across 20 runs — the observer is \
         probably seeing nothing"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// The observed manifests must actually be *tight*, not just self-consistent.
///
/// Round-tripping proves the manifest permits its own run. It does not prove the
/// manifest is small — `read = ["**"]` would round-trip too, with the unused
/// check being the only thing standing against it. This asserts the shape
/// directly.
#[tokio::test]
async fn an_observed_manifest_names_literal_paths_and_no_wildcards() {
    let suite = suite();
    let ledger = Ledger::in_memory().expect("ledger");
    let scratch = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join("kedge-forge-tightness");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");

    let one = kedge_bench::BenchSuite {
        name: "tightness",
        tasks: vec![suite.get("clamp-001").unwrap().clone()],
    };
    let reasoner = Arc::new(ScriptedReasoner::for_suite(&suite));
    runner::run_suite(&one, reasoner, &ledger, &fixtures_dir(), &scratch)
        .await
        .expect("bench run");

    let run = kedge_bench::stable_task_id("clamp-001");
    let trajectory = ledger.replay(run).expect("replay");
    let base = scratch.join("clamp-001");
    let observed = observe_verified(&trajectory, &base, "numeric-bounds", "0.1.0")
        .await
        .expect("observe");

    assert_eq!(observed.verification, Verification::Exact);

    let manifest = observed.manifest("numeric-bounds", "0.1.0");
    assert!(
        !manifest.contains('*'),
        "the observer invented a glob — widening is a human's call:\n{manifest}"
    );
    // The one file the plan touched, and the one command it ran.
    assert!(manifest.contains("src/lib.rs"), "{manifest}");
    assert!(manifest.contains("cargo test -q"), "{manifest}");
    // It never read Cargo.toml on this family's plan, so it must not appear.
    assert!(!manifest.contains("Cargo.toml"), "{manifest}");

    let _ = std::fs::remove_dir_all(&scratch);
}
