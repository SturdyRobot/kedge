//! Generate the corpus.
//!
//! ```sh
//! cargo run -p kedge-bench                    # run the suite, write the ledger
//! cargo run -p kedge-bench -- --check         # integrity only, no trajectories
//! cargo run -p kedge-bench -- --json          # machine-readable report
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use kedge_bench::{checks, fixture, fixtures_dir, runner, suite, ScriptedReasoner};
use kedge_ledger::Ledger;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let check_only = args.iter().any(|a| a == "--check");
    let ledger_path = args
        .iter()
        .position(|a| a == "--ledger")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bench.sqlite"));

    let s = suite();
    let scratch = fixture::scratch_root().join("cli");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;

    // Integrity first, always. A corpus generated from an unsound suite is
    // worse than no corpus: it looks usable.
    let started = Instant::now();
    let verdicts = checks::verify_suite(&s, &fixtures_dir(), &scratch).await?;
    let unsound = verdicts.iter().filter(|v| !v.ok()).count();
    if !json {
        println!("{}", checks::report(&verdicts));
    }
    if unsound > 0 {
        eprintln!("refusing to generate a corpus from {unsound} unsound task(s)");
        std::process::exit(2);
    }
    if check_only {
        println!(
            "integrity: {} task(s) sound in {:?}",
            verdicts.len(),
            started.elapsed()
        );
        return Ok(());
    }

    let ledger = Ledger::open(&ledger_path)?;
    let reasoner = Arc::new(ScriptedReasoner::for_suite(&s));
    let report = runner::run_suite(&s, reasoner, &ledger, &fixtures_dir(), &scratch).await?;

    if json {
        println!("{}", report.to_json());
    } else {
        println!("{}", report.to_pretty());
        println!(
            "  ledger:      {}\n  fingerprint: {:016x}\n  wall-clock:  {:?}",
            ledger_path.display(),
            fnv(report.fingerprint().as_bytes()),
            started.elapsed()
        );
    }

    // A run that solved nothing is a broken harness, not a hard benchmark.
    if report.solved() == 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn fnv(data: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
