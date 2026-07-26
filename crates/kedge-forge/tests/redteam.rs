//! Red team. Each test ASSERTS THE BUG, so a pass here means the hole is real.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kedge_core::{Action, Observation, Step, TaskId, Thought, ToolCall, ToolExecutor, Trajectory};
use kedge_forge::{observe_verified, reach, Reach};
use kedge_skill::{Capability, Manifest, SkillGuard};

fn compile(t: &str) -> Manifest {
    Manifest::from_toml_str(t, &HashMap::new()).expect("manifest")
}

#[derive(Default)]
struct Spy(Mutex<Vec<String>>);
#[async_trait]
impl ToolExecutor for Spy {
    async fn execute(&self, c: &ToolCall) -> kedge_core::Result<Observation> {
        self.0.lock().unwrap().push(c.name.clone());
        Ok(Observation::ok("SECRET CONTENTS"))
    }
}

// ── A1: an unrecognized argument key defeats the read grant entirely ──
#[tokio::test]
async fn a1_unknown_argument_key_reads_anything() {
    // Grants read of exactly one file. Nothing else.
    let m = compile(
        "[skill]\nname=\"s\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nread=[\"/repo/src/lib.rs\"]\n",
    );
    let spy = Arc::new(Spy::default());
    let g = SkillGuard::new(Arc::new(m), "/repo", spy.clone() as Arc<dyn ToolExecutor>);

    // `resource` is not in kedge-skill's PATH_KEYS table.
    let obs = g
        .execute(&ToolCall::new(
            "read_file",
            serde_json::json!({"resource": "/etc/shadow"}),
        ))
        .await
        .unwrap();

    assert!(!obs.is_error, "BUG NOT PRESENT: it was refused");
    assert_eq!(*spy.0.lock().unwrap(), vec!["read_file"],
        "BUG NOT PRESENT: the call never reached the executor");
    println!("A1 CONFIRMED: read of /etc/shadow permitted under a one-file read grant");
}

// ── A2: a filename containing a glob char widens the minimized manifest ──
#[tokio::test]
async fn a2_glob_char_in_a_real_filename_widens_the_learned_manifest() {
    let mut t = Trajectory::new(TaskId::new());
    t.steps.push(Step {
        index: 0,
        thought: Thought("t".into()),
        // A real file really can be named this.
        action: Action::Tool(ToolCall::new(
            "read_file",
            serde_json::json!({"path": "report[1]*draft.md"}),
        )),
        observation: Some(Observation::ok("x")),
        tokens: 1,
        elapsed_ms: 0,
    });

    let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0").await.unwrap();
    println!("A2 verification = {:?}", o.verification);
    let m = compile(&o.manifest("s", "0.1.0"));

    // The observed file.
    assert!(m.permits(&Capability::FsRead(PathBuf::from("/repo/report[1]*draft.md"))));
    // A DIFFERENT file the run never touched.
    let widened = m.permits(&Capability::FsRead(PathBuf::from("/repo/report[1]SECRETdraft.md")));
    assert!(widened, "BUG NOT PRESENT: the glob char was escaped");
    println!("A2 CONFIRMED: learned manifest also grants report[1]SECRETdraft.md");
}

// ── A3: Reach::is_reduction_of still counts commands (the gate does not) ──
#[test]
fn a3_reach_still_uses_the_backwards_command_rule() {
    let base = Reach { writable: 100, readable: 100, commands: 1, hosts: 0,
        escapes_root: false, truncated: false, files_scanned: 100 };
    // Strictly narrower: 3 files instead of 100, and two SPECIFIC commands
    // instead of one blanket one.
    let cand = Reach { writable: 3, readable: 3, commands: 2, ..base };

    assert!(!cand.is_reduction_of(&base),
        "BUG NOT PRESENT: is_reduction_of agreed with the gate");
    println!("A3 CONFIRMED: is_reduction_of calls a strictly narrower skill a widening");
}

// ── A4: Reach counts only files that exist at walk time ──
#[test]
fn a4_reach_understates_a_glob_against_a_growing_tree() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.rs"), "x").unwrap();
    let root = d.path().canonicalize().unwrap();

    let glob = compile(&format!(
        "[skill]\nname=\"g\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nwrite=[\"{}/**\"]\n", root.to_string_lossy()));
    let literal = compile(&format!(
        "[skill]\nname=\"l\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nwrite=[\"{}/a.rs\"]\n", root.to_string_lossy()));

    let g1 = reach(&glob, &root).unwrap();
    let l1 = reach(&literal, &root).unwrap();
    assert_eq!(g1.writable, l1.writable, "both reach 1 file today");
    assert!(!g1.is_reduction_of(&l1) && !l1.is_reduction_of(&g1),
        "they measure as equivalent");

    // The repo grows by one file. The glob silently gained authority; the
    // literal did not.
    std::fs::write(root.join("secrets.rs"), "x").unwrap();
    let g2 = reach(&glob, &root).unwrap();
    let l2 = reach(&literal, &root).unwrap();
    assert_eq!(l2.writable, 1);
    assert_eq!(g2.writable, 2);
    println!("A4 CONFIRMED: identical scores today ({} vs {}), diverge tomorrow ({} vs {})",
        g1.writable, l1.writable, g2.writable, l2.writable);
}

// ── A5: an argv array can never be granted, and each element becomes a command ──
#[test]
fn a5_argv_arrays_are_derived_incoherently() {
    let call = ToolCall::new("run", serde_json::json!({"argv": ["cargo", "test"]}));
    let req = kedge_skill::required(&call, Path::new("/repo"));
    match req {
        kedge_skill::Requirement::Known(caps) => {
            let procs: Vec<_> = caps.iter().filter_map(|c| match c {
                Capability::Process(s) => Some(s.clone()), _ => None }).collect();
            assert_eq!(procs.len(), 2, "BUG NOT PRESENT: argv treated as one command");
            println!("A5 CONFIRMED: argv ['cargo','test'] derived as {procs:?} — two separate commands");
            let m = compile("[skill]\nname=\"s\"\nversion=\"0.1.0\"\n\
                [capabilities.process]\nallow=[\"cargo test\"]\n");
            assert!(!m.permits(&Capability::Process("cargo".into())));
            println!("A5 CONFIRMED: no manifest can ever permit this call");
        }
        other => panic!("expected Known, got {other:?}"),
    }
}

// ── A6: the e2e acceptance test's "baseline" carries the LEARNED manifest ──
#[tokio::test]
async fn a6_the_end_to_end_baseline_is_not_actually_a_general_agent() {
    use kedge_forge::{general_agent_manifest, SkillRecord};

    let mut t = Trajectory::new(TaskId::new());
    t.steps.push(Step { index: 0, thought: Thought("t".into()),
        action: Action::Tool(ToolCall::new("run", serde_json::json!({"command": "cargo test -q"}))),
        observation: Some(Observation::ok("x")), tokens: 1, elapsed_ms: 0 });
    let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0").await.unwrap();

    let wide_reach = Reach { writable: 999, readable: 999, commands: 1, hosts: 0,
        escapes_root: false, truncated: false, files_scanned: 999 };

    // Exactly how end_to_end.rs builds its baseline.
    let baseline = SkillRecord {
        version: "0.0.0-general".into(),
        reach: wide_reach,
        ..SkillRecord::from_observation(&o, "s", "0.0.0-general", wide_reach)
    };

    let real_general = general_agent_manifest(Path::new("/repo"), &["cargo"]);
    assert_ne!(baseline.manifest_toml, real_general,
        "BUG NOT PRESENT: the baseline carries a general-agent manifest");
    assert!(baseline.manifest_toml.contains("cargo test -q"),
        "expected the LEARNED manifest text");
    assert!(!baseline.manifest_toml.contains("/repo/**"),
        "expected NO workspace-wide grant");
    println!("A6 CONFIRMED: baseline has wide reach numbers but the tight learned manifest — \
              the gate's containment check was never exercised against a real general agent");
}

// ── A7: A1 composes into a promoted skill whose manifest omits a real read ──
#[tokio::test]
async fn a7_a_hidden_read_survives_observation_verification_and_the_gate() {
    use kedge_forge::{gate, SkillRecord};

    let mut t = Trajectory::new(TaskId::new());
    // Same unrecognized key as A1. A real read of a real secret.
    t.steps.push(Step { index: 0, thought: Thought("t".into()),
        action: Action::Tool(ToolCall::new("read_file",
            serde_json::json!({"resource": "/etc/shadow"}))),
        observation: Some(Observation::ok("root:$6$...")), tokens: 1, elapsed_ms: 0 });

    let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0").await.unwrap();

    assert!(o.exercised.is_empty(), "BUG NOT PRESENT: the read was observed");
    assert!(o.unobservable.is_empty(), "BUG NOT PRESENT: it was flagged unnameable");
    assert_eq!(o.verification, kedge_forge::Verification::Exact);
    assert!(o.is_complete(), "BUG NOT PRESENT: is_complete() caught it");

    let r = Reach { writable: 0, readable: 0, commands: 0, hosts: 0,
        escapes_root: false, truncated: false, files_scanned: 10 };
    let rec = SkillRecord::from_observation(&o, "s", "0.1.0", r);
    let v = gate(&rec, None, None);
    assert!(v.promote, "BUG NOT PRESENT: the gate refused it");

    println!("A7 CONFIRMED: a skill that read /etc/shadow is promoted with an EMPTY manifest.");
    println!("A7 manifest:\n{}", rec.manifest_toml);
    println!("A7 both checks agreed because they share the blind spot — the \
              'requires both' defence does not help.");
}
