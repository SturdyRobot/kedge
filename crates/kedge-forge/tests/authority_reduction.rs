//! S3 acceptance: does a learned manifest actually reduce authority?
//!
//! This carries the project's kill criterion. From `docs/SLICES.md`:
//!
//! > **KILL: learned-skill authority is not measurably smaller than the general
//! > agent's.** The security half is the load-bearing half.
//!
//! If this test cannot be made to pass honestly, the correct response is to stop
//! and publish that, not to weaken the assertion.

use std::sync::Arc;

use kedge_bench::{fixture, fixtures_dir, runner, suite, ScriptedReasoner};
use kedge_forge::{general_agent_manifest, observe_verified, reach};
use kedge_ledger::Ledger;
use kedge_skill::Manifest;

fn compile(toml: &str) -> Manifest {
    Manifest::from_toml_str(toml, &std::collections::HashMap::new()).expect("manifest")
}

#[tokio::test]
async fn every_learned_manifest_reduces_authority_against_the_general_agent() {
    let suite = suite();
    let ledger = Ledger::in_memory().expect("ledger");
    let scratch = std::env::temp_dir()
        .canonicalize()
        .expect("temp")
        .join("kedge-forge-authority");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");

    let reasoner = Arc::new(ScriptedReasoner::for_suite(&suite));
    let report = runner::run_suite(&suite, reasoner, &ledger, &fixtures_dir(), &scratch)
        .await
        .expect("bench");
    assert_eq!(report.solved(), 20, "{}", report.to_pretty());

    let mut rows = Vec::new();
    let mut not_reduced = Vec::new();

    for task in &suite.tasks {
        let trajectory = ledger
            .replay(kedge_bench::stable_task_id(task.id))
            .expect("replay");
        let root = scratch.join(task.id);
        let observed = observe_verified(&trajectory, &root, task.family, "0.1.0")
            .await
            .expect("observe");
        assert!(observed.is_complete(), "{}", observed.summary());

        // The workspace is removed when a run finishes, so recreate the same
        // tree at the same path to measure against.
        let ws = fixture::materialize(task, &fixtures_dir(), &scratch).expect("materialize");

        let learned = compile(&observed.manifest(task.family, "0.1.0"));

        // The general agent gets the *same* commands the skill used. That
        // understates its real authority — a general agent can run anything, and
        // `Reach` cannot enumerate an unbounded command space — so the reduction
        // measured here is a floor, not a ceiling. Understating our own result is
        // the right direction to be wrong in.
        let declared = learned.declared();
        let commands: Vec<&str> = declared
            .iter()
            .filter(|(k, _)| *k == "process")
            .map(|(_, c)| c.as_str())
            .collect();
        let general = compile(&general_agent_manifest(&ws.root, &commands));

        let l = reach(&learned, &ws.root).expect("reach learned");
        let g = reach(&general, &ws.root).expect("reach general");

        if !l.is_filesystem_reduction_of(&g) {
            not_reduced.push(format!(
                "  {}\n      general: {}\n      learned: {}",
                task.id,
                g.summary(),
                l.summary()
            ));
        }
        rows.push((task.id, g.writable, l.writable, g.readable, l.readable));
    }

    assert!(
        not_reduced.is_empty(),
        "{} of 20 learned manifests did not reduce authority:\n{}",
        not_reduced.len(),
        not_reduced.join("\n")
    );

    // The aggregate, printed so the number is visible in CI output rather than
    // only asserted.
    let gw: usize = rows.iter().map(|r| r.1).sum();
    let lw: usize = rows.iter().map(|r| r.2).sum();
    let gr: usize = rows.iter().map(|r| r.3).sum();
    let lr: usize = rows.iter().map(|r| r.4).sum();
    println!(
        "\nacross 20 tasks — writable {gw} → {lw} ({:.0}% cut), readable {gr} → {lr} ({:.0}% cut)\n",
        (1.0 - lw as f64 / gw as f64) * 100.0,
        (1.0 - lr as f64 / gr as f64) * 100.0,
    );

    // Non-vacuity: a bug that measured zero everywhere would pass every
    // `is_reduction_of` above by accident.
    assert!(
        gw > 0 && gr > 0,
        "the general agent measured as reaching nothing"
    );
    assert!(lw > 0, "the learned manifests measured as writing nothing");
    assert!(lw < gw, "no aggregate reduction in writable files");

    let _ = std::fs::remove_dir_all(&scratch);
}

/// Scale, measured on a real repository rather than a four-file fixture.
///
/// Reported separately and never mixed with the numbers above: this says how
/// much authority a general agent holds in a codebase of this size, not how much
/// any learned skill saved. `Reach` is filesystem-dependent by design, so the
/// two are not comparable and must not be presented as one figure.
#[test]
fn the_general_agent_surface_on_this_repository() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf();

    let general = compile(&general_agent_manifest(&repo, &["cargo test"]));
    let r = reach(&general, &repo).expect("reach");

    println!("\nkedge repo — general agent: {}\n", r.summary());
    assert!(!r.escapes_root);
    assert!(
        r.files_scanned > 50,
        "only {} files scanned — the walk is not seeing the repo",
        r.files_scanned
    );
    // Everything it can read it can also write, which is the point.
    assert_eq!(r.writable, r.files_scanned);
}
