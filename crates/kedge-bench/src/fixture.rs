//! Materializing a task: copy the fixture, break it, and ask the oracle.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Acceptance, BenchTask, Breakage};

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("fixture `{0}` not found at {1}")]
    Missing(String, PathBuf),
    #[error("io at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "breakage for `{task}` matched {found} times in {file}, expected exactly 1 \
         (pattern: {pattern:?})"
    )]
    SpliceNotUnique {
        task: String,
        file: String,
        pattern: String,
        found: usize,
    },
    #[error("running the acceptance command `{program}`: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
}

fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> FixtureError {
    let path = path.into();
    move |source| FixtureError::Io { path, source }
}

/// A materialized workspace for one task. Removed on drop unless `keep` is set.
#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    keep: bool,
}

impl Workspace {
    pub fn keep(mut self) -> Self {
        self.keep = true;
        self
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Copy `fixtures/<task.fixture>` into `scratch/<task.id>`, pristine.
pub fn materialize(
    task: &BenchTask,
    fixtures: &Path,
    scratch: &Path,
) -> Result<Workspace, FixtureError> {
    let src = fixtures.join(task.fixture);
    if !src.is_dir() {
        return Err(FixtureError::Missing(task.fixture.to_string(), src));
    }
    let root = scratch.join(task.id);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).map_err(io(&root))?;
    copy_tree(&src, &root)?;
    Ok(Workspace { root, keep: false })
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), FixtureError> {
    for entry in std::fs::read_dir(src).map_err(io(src))? {
        let entry = entry.map_err(io(src))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        // `target/` is never copied: it is build output, it is enormous, and a
        // stale one would make a "fresh" workspace anything but.
        if from.file_name().is_some_and(|n| n == "target") {
            continue;
        }
        if from.is_dir() {
            std::fs::create_dir_all(&to).map_err(io(&to))?;
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(io(&to))?;
        }
    }
    Ok(())
}

/// Apply the task's breakage to a materialized workspace.
///
/// Fails loudly when the pattern does not match exactly once. A zero-match
/// splice would produce a task that is already solved, and the suite would
/// report a solve rate that means nothing.
pub fn apply_breakage(task: &BenchTask, ws: &Path) -> Result<(), FixtureError> {
    match &task.breakage {
        Breakage::Splice {
            file,
            find,
            replace,
        } => {
            let path = ws.join(file);
            let text = std::fs::read_to_string(&path).map_err(io(&path))?;
            let found = text.matches(find).count();
            if found != 1 {
                return Err(FixtureError::SpliceNotUnique {
                    task: task.id.to_string(),
                    file: file.to_string(),
                    pattern: find.to_string(),
                    found,
                });
            }
            std::fs::write(&path, text.replacen(find, replace, 1)).map_err(io(&path))?;
            Ok(())
        }
    }
}

/// The result of asking the oracle.
#[derive(Debug, Clone)]
pub struct OracleVerdict {
    pub passed: bool,
    pub timed_out: bool,
    /// Trimmed, for diagnostics only — never fed back into a decision.
    pub output: String,
}

/// Run the acceptance check. This is the independent oracle: the fixture's own
/// compiler and tests decide, not anything this crate wrote.
pub async fn check_acceptance(task: &BenchTask, ws: &Path) -> Result<OracleVerdict, FixtureError> {
    let Acceptance::CommandSucceeds {
        program,
        args,
        timeout,
    } = &task.acceptance;

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(*args)
        .current_dir(ws)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    // Per-workspace, always set explicitly so an ambient CARGO_TARGET_DIR from
    // the parent `cargo test` cannot leak in. See `target_dir_for`.
    cmd.env("CARGO_TARGET_DIR", target_dir_for(ws));

    let child = cmd.output();
    let out = match tokio::time::timeout(*timeout, child).await {
        Err(_) => {
            return Ok(OracleVerdict {
                passed: false,
                timed_out: true,
                output: format!("timed out after {:?}", timeout),
            })
        }
        Ok(Err(source)) => {
            return Err(FixtureError::Spawn {
                program: program.to_string(),
                source,
            })
        }
        Ok(Ok(out)) => out,
    };

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    truncate_in_place(&mut text, MAX_ORACLE_OUTPUT);

    Ok(OracleVerdict {
        passed: out.status.success(),
        timed_out: false,
        output: text,
    })
}

const MAX_ORACLE_OUTPUT: usize = 8 * 1024;

fn truncate_in_place(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    // Truncate on a char boundary, never mid-codepoint.
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n… (truncated)");
}

/// Build artifacts for one task, isolated from every other task.
///
/// **This must be per-task, and the reason is a real bug, not caution.** The
/// first version of this crate shared one `CARGO_TARGET_DIR` across the whole
/// suite, because measuring it showed 0.10s per task instead of 0.15s. Every
/// fixture copy has the same package name and version (`slug 0.0.0`), so cargo
/// resolved them to the same artifact: a *pristine* copy of a fixture would run
/// the compiled binary of a previously-broken one and report `FAILED`.
///
/// The corpus would have been silently wrong in both directions — tasks failing
/// that were fine, tasks passing that were broken — and nothing about a passing
/// test run would have shown it. [`crate::checks`] caught it.
///
/// The isolation costs 0.05s per task: ~3s for the suite against a 30s budget.
///
/// It is keyed on the **workspace path**, not the task id. Keying on the task id
/// was the first fix and it was still wrong: the id is a global name, so two
/// concurrent runs of `slug-001` — which is exactly what happens when cargo runs
/// the integrity check and the contamination regression test in parallel —
/// collided again. A workspace path is unique by construction. Putting the
/// directory *inside* the workspace also means it is removed with it, so a
/// suite run leaves nothing behind.
///
/// Any ambient `CARGO_TARGET_DIR` is deliberately **ignored** rather than used
/// as a base, because inheriting the parent `cargo test`'s target dir is one of
/// the ways artifacts leaked between tasks.
pub fn target_dir_for(ws_root: &Path) -> PathBuf {
    ws_root.join(".kedge-target")
}

/// A scratch root for materialized workspaces.
pub fn scratch_root() -> PathBuf {
    std::env::temp_dir().join("kedge-bench-ws")
}

/// Convenience for tests and checks: materialize, break, and ask the oracle.
pub async fn materialize_broken(
    task: &BenchTask,
    fixtures: &Path,
    scratch: &Path,
) -> Result<(Workspace, OracleVerdict), FixtureError> {
    let ws = materialize(task, fixtures, scratch)?;
    apply_breakage(task, &ws.root)?;
    let verdict = check_acceptance(task, &ws.root).await?;
    Ok((ws, verdict))
}

/// The acceptance timeout, exposed so checks can budget against it.
pub fn acceptance_timeout(task: &BenchTask) -> Duration {
    let Acceptance::CommandSucceeds { timeout, .. } = &task.acceptance;
    *timeout
}
