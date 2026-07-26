//! What a manifest learned from a recorded run actually looks like.
//!
//! ```sh
//! cargo run -p kedge-forge --example observe_corpus
//! ```

use std::sync::Arc;

use kedge_bench::{fixtures_dir, runner, suite, ScriptedReasoner};
use kedge_forge::observe_verified;
use kedge_ledger::Ledger;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let suite = suite();
    let ledger = Ledger::in_memory()?;
    let scratch = std::env::temp_dir()
        .canonicalize()?
        .join("kedge-forge-example");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;

    let reasoner = Arc::new(ScriptedReasoner::for_suite(&suite));
    let report = runner::run_suite(&suite, reasoner, &ledger, &fixtures_dir(), &scratch).await?;
    println!(
        "corpus: {}/{} runs recorded and solved\n",
        report.solved(),
        report.outcomes.len()
    );

    println!("── every run, observed ──\n");
    for task in &suite.tasks {
        let trajectory = ledger.replay(kedge_bench::stable_task_id(task.id))?;
        let observed =
            observe_verified(&trajectory, &scratch.join(task.id), task.family, "0.1.0").await?;
        println!(
            "  {}  {:<10} {}",
            if observed.is_complete() { "✔" } else { "✘" },
            task.id,
            observed.summary()
        );
    }

    // One in full. `cart-*` has the longest plan, so it exercises the most.
    let task = suite.get("cart-002").unwrap();
    let trajectory = ledger.replay(kedge_bench::stable_task_id(task.id))?;
    let observed =
        observe_verified(&trajectory, &scratch.join(task.id), task.family, "0.1.0").await?;

    println!("\n── {} in full ──\n", task.id);
    println!("  the trajectory called:");
    for (cap, n) in &observed.exercised {
        println!("      {cap}  ×{n}");
    }

    println!("\n  the manifest it learned:\n");
    // Paths are absolute into a temp workspace; shorten for legibility only.
    let base = scratch.join(task.id);
    let text = observed
        .manifest(task.family, "0.1.0")
        .replace(&format!("{}/", base.to_string_lossy()), "${workspace}/");
    for line in text.lines() {
        println!("  {line}");
    }

    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}
