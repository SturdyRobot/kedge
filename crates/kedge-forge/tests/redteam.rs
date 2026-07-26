//! Red-team regressions.
//!
//! Each of these asserted a *confirmed hole* when it was written (commit
//! `46fb532`); each now asserts the fix. The failure messages name the finding,
//! so a regression is recognisable rather than merely red.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kedge_core::{Action, Observation, Step, TaskId, Thought, ToolCall, ToolExecutor, Trajectory};
use kedge_forge::{observe_verified, reach, Reach};
use kedge_skill::{Capability, Manifest, SkillGuard};

fn compile(t: &str) -> Manifest {
    Manifest::from_toml_str(t, &HashMap::new()).expect("manifest")
}

fn one_call(name: &str, args: serde_json::Value) -> Trajectory {
    let mut t = Trajectory::new(TaskId::new());
    t.steps.push(Step {
        index: 0,
        thought: Thought("t".into()),
        action: Action::Tool(ToolCall::new(name, args)),
        observation: Some(Observation::ok("x")),
        tokens: 1,
        elapsed_ms: 0,
    });
    t
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

/// **A1** — an argument key outside the recognized table defeated the read grant
/// entirely, because "no known key" was read as "needs nothing".
#[tokio::test]
async fn a1_an_unknown_argument_key_cannot_read_outside_the_grant() {
    let m = compile(
        "[skill]\nname=\"s\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nread=[\"/repo/src/lib.rs\"]\n",
    );
    let spy = Arc::new(Spy::default());
    let g = SkillGuard::new(Arc::new(m), "/repo", spy.clone() as Arc<dyn ToolExecutor>);

    for key in ["resource", "target_uri", "blob"] {
        let obs = g
            .execute(&ToolCall::new(
                "read_file",
                serde_json::json!({ key: "/loot/shadow" }),
            ))
            .await
            .unwrap();
        assert!(obs.is_error, "A1 REGRESSION: `{key}` was permitted");
    }
    assert!(
        spy.0.lock().unwrap().is_empty(),
        "A1 REGRESSION: a call reached the executor"
    );
}

/// **A1, residue** — the value can be a bare token that reveals nothing, so the
/// tool *name* has to carry the refusal.
#[tokio::test]
async fn a1b_a_tool_promising_io_that_names_nothing_is_refused() {
    let m = compile(
        "[skill]\nname=\"s\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nread=[\"/repo/**\"]\n",
    );
    let spy = Arc::new(Spy::default());
    let g = SkillGuard::new(Arc::new(m), "/repo", spy.clone() as Arc<dyn ToolExecutor>);

    let obs = g
        .execute(&ToolCall::new(
            "read_file",
            serde_json::json!({"resource": "shadow"}),
        ))
        .await
        .unwrap();
    assert!(
        obs.is_error,
        "A1b REGRESSION: permitted while naming nothing"
    );
    assert!(spy.0.lock().unwrap().is_empty());
}

/// **A2** — a filename containing a glob character widened the learned manifest,
/// and the round-trip check could not see it: the observed path still matched.
#[tokio::test]
async fn a2_a_glob_char_in_a_filename_does_not_widen_the_learned_manifest() {
    let t = one_call(
        "read_file",
        serde_json::json!({"path": "report[1]*draft.md"}),
    );
    let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0")
        .await
        .unwrap();
    let m = compile(&o.manifest("s", "0.1.0"));

    assert!(
        m.permits(&Capability::FsRead(PathBuf::from(
            "/repo/report[1]*draft.md"
        ))),
        "A2 REGRESSION: the observed file is no longer granted"
    );
    assert!(
        !m.permits(&Capability::FsRead(PathBuf::from(
            "/repo/report[1]SECRETdraft.md"
        ))),
        "A2 REGRESSION: the manifest grants a file the run never touched"
    );
}

/// **A3** — `Reach` compared commands by entry count while the gate compared them
/// by containment. Two implementations, two answers.
#[test]
fn a3_reach_does_not_compare_commands_at_all() {
    let base = Reach {
        writable: 100,
        readable: 100,
        commands: 1,
        hosts: 0,
        wildcard_grants: 1,
        escapes_root: false,
        truncated: false,
        files_scanned: 100,
    };
    let narrower = Reach {
        writable: 3,
        readable: 3,
        commands: 2, // two specific commands instead of one blanket one
        wildcard_grants: 0,
        ..base
    };
    assert!(
        narrower.is_filesystem_reduction_of(&base),
        "A3 REGRESSION: command entry count leaked back into the comparison"
    );
}

/// **A4** — a `**` grant and a literal path measured identically in a directory
/// holding one file, and diverged the moment a file was added.
#[test]
fn a4_a_wildcard_grant_is_never_a_reduction_of_a_literal_one() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.rs"), "x").unwrap();
    let root = d.path().canonicalize().unwrap();
    let r = root.to_string_lossy();

    let glob = compile(&format!(
        "[skill]\nname=\"g\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nwrite=[\"{r}/**\"]\n"
    ));
    let literal = compile(&format!(
        "[skill]\nname=\"l\"\nversion=\"0.1.0\"\n\
         [capabilities.filesystem]\nwrite=[\"{r}/a.rs\"]\n"
    ));

    let g = reach(&glob, &root).unwrap();
    let l = reach(&literal, &root).unwrap();

    // Identical file counts today.
    assert_eq!(g.writable, l.writable);
    // The wildcard is what makes the difference visible, and it decides.
    assert_eq!(g.wildcard_grants, 1);
    assert_eq!(l.wildcard_grants, 0);
    assert!(
        l.is_filesystem_reduction_of(&g),
        "A4 REGRESSION: the literal grant is not scored as tighter"
    );
    assert!(
        !g.is_filesystem_reduction_of(&l),
        "A4 REGRESSION: a wildcard scored as a reduction of a literal grant"
    );
}

/// **A5** — an `argv` array became one command capability *per element*, so no
/// manifest could ever permit the call.
#[test]
fn a5_an_argv_array_is_one_command() {
    let call = ToolCall::new("run", serde_json::json!({"argv": ["cargo", "test"]}));
    let kedge_skill::Requirement::Known(caps) = kedge_skill::required(&call, Path::new("/repo"))
    else {
        panic!("A5 REGRESSION: argv became indeterminate");
    };
    assert_eq!(caps.len(), 1, "A5 REGRESSION: {caps:?}");
    assert!(caps.contains(&Capability::Process("cargo test".into())));

    let m = compile(
        "[skill]\nname=\"s\"\nversion=\"0.1.0\"\n\
         [capabilities.process]\nallow=[\"cargo test\"]\n",
    );
    assert!(
        m.permits(&Capability::Process("cargo test".into())),
        "A5 REGRESSION: still unpermittable by any manifest"
    );
}

/// **A7** — the composite, and the one that mattered.
///
/// A read of a secret through an unrecognized key was observed as exercising
/// nothing, verified `Exact`, reported complete, and promoted with an empty
/// manifest. `is_complete()` requires two signals and the docs claimed that
/// meant neither could be load-bearing alone — but both were built on the same
/// derivation, so they shared the blind spot and agreed with each other while
/// being blind together.
#[tokio::test]
async fn a7_a_hidden_read_is_observed_and_blocks_promotion() {
    use kedge_forge::{gate, SkillRecord};

    let t = one_call("read_file", serde_json::json!({"resource": "/loot/shadow"}));
    let o = observe_verified(&t, Path::new("/repo"), "s", "0.1.0")
        .await
        .unwrap();

    assert!(
        o.exercised
            .contains_key(&Capability::FsRead(PathBuf::from("/loot/shadow"))),
        "A7 REGRESSION: the read is still invisible — {:?}",
        o.exercised
    );
    // Declared rather than hidden.
    assert!(o.manifest("s", "0.1.0").contains("/loot/shadow"));

    // And because it leaves the workspace, the gate refuses it outright.
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let m = compile(&o.manifest("s", "0.1.0"));
    let r = reach(&m, &root).unwrap();
    assert!(r.escapes_root, "A7 REGRESSION: the escape is not flagged");

    let rec = SkillRecord::from_observation(&o, "s", "0.1.0", r);
    let v = gate(&rec, None, None);
    assert!(
        !v.promote,
        "A7 REGRESSION: promoted anyway:\n{}",
        v.report()
    );
}
