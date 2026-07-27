//! The tools a bench run actually calls.
//!
//! Deliberately plain: `read_file`, `write_file`, `list_files`, `run_command`.
//! The point of the corpus is that the *capabilities* a trajectory exercises are
//! real — a recorded `write_file` with a real path is what `kedge-forge observe`
//! will later derive a manifest from. A mock that returned canned strings would
//! produce a corpus with nothing to observe.
//!
//! Every path is resolved against the workspace root and refused if it escapes.
//! That is not the security boundary — `kedge-skill` is — it is here so a buggy
//! scripted plan cannot write outside its own scratch directory during a suite
//! run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use kedge_core::{Observation, ToolCall, ToolExecutor};

/// Filesystem and process tools scoped to one workspace.
pub struct WorkspaceTools {
    root: PathBuf,
    command_timeout: Duration,
}

impl WorkspaceTools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        WorkspaceTools {
            root: root.into(),
            command_timeout: Duration::from_secs(120),
        }
    }

    /// Resolve a tool-supplied path inside the workspace, or `None` if it
    /// escapes. Lexical only — same approach as `kedge-skill`, without the
    /// symlink pass, because these workspaces are ours and contain no links.
    fn resolve(&self, raw: &str) -> Option<PathBuf> {
        let joined = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.root.join(raw)
        };
        let mut out = PathBuf::new();
        let mut depth = 0usize;
        for c in joined.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if depth == 0 {
                        return None;
                    }
                    out.pop();
                    depth -= 1;
                }
                std::path::Component::RootDir => out.push(c.as_os_str()),
                std::path::Component::Prefix(p) => out.push(p.as_os_str()),
                std::path::Component::Normal(seg) => {
                    out.push(seg);
                    depth += 1;
                }
            }
        }
        out.starts_with(&self.root).then_some(out)
    }

    fn arg<'a>(call: &'a ToolCall, key: &str) -> Option<&'a str> {
        call.arguments.get(key).and_then(|v| v.as_str())
    }
}

#[async_trait]
impl ToolExecutor for WorkspaceTools {
    async fn execute(&self, call: &ToolCall) -> kedge_core::Result<Observation> {
        match call.name.as_str() {
            "read_file" => {
                let Some(raw) = Self::arg(call, "path") else {
                    return Ok(Observation::error("read_file needs a `path` argument"));
                };
                let Some(path) = self.resolve(raw) else {
                    return Ok(Observation::error(format!(
                        "path `{raw}` is outside the workspace"
                    )));
                };
                Ok(match std::fs::read_to_string(&path) {
                    Ok(text) => Observation::ok(text),
                    Err(e) => Observation::error(format!("read_file `{raw}`: {e}")),
                })
            }

            "write_file" => {
                let (Some(raw), Some(content)) =
                    (Self::arg(call, "path"), Self::arg(call, "content"))
                else {
                    return Ok(Observation::error(
                        "write_file needs `path` and `content` arguments",
                    ));
                };
                let Some(path) = self.resolve(raw) else {
                    return Ok(Observation::error(format!(
                        "path `{raw}` is outside the workspace"
                    )));
                };
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                Ok(match std::fs::write(&path, content) {
                    Ok(()) => Observation::ok(format!("wrote {} bytes to {raw}", content.len())),
                    Err(e) => Observation::error(format!("write_file `{raw}`: {e}")),
                })
            }

            "list_files" => {
                let raw = Self::arg(call, "path").unwrap_or(".");
                let Some(path) = self.resolve(raw) else {
                    return Ok(Observation::error(format!(
                        "path `{raw}` is outside the workspace"
                    )));
                };
                let mut names = Vec::new();
                match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        for e in entries.flatten() {
                            names.push(e.file_name().to_string_lossy().into_owned());
                        }
                    }
                    Err(e) => return Ok(Observation::error(format!("list_files `{raw}`: {e}"))),
                }
                names.sort(); // deterministic: readdir order is not
                Ok(Observation::ok(names.join("\n")))
            }

            "run_command" => {
                let Some(cmd) = Self::arg(call, "command") else {
                    return Ok(Observation::error("run_command needs a `command` argument"));
                };
                let mut parts = cmd.split_whitespace();
                let Some(program) = parts.next() else {
                    return Ok(Observation::error("empty command"));
                };
                let args: Vec<&str> = parts.collect();

                let mut c = tokio::process::Command::new(program);
                c.args(&args)
                    .current_dir(&self.root)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true);
                c.env(
                    "CARGO_TARGET_DIR",
                    crate::fixture::target_dir_for(&self.root),
                );

                match tokio::time::timeout(self.command_timeout, c.output()).await {
                    Err(_) => Ok(Observation::error(format!(
                        "`{cmd}` timed out after {:?}",
                        self.command_timeout
                    ))),
                    Ok(Err(e)) => Ok(Observation::error(format!("spawning `{program}`: {e}"))),
                    Ok(Ok(out)) => {
                        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                        text.push_str(&String::from_utf8_lossy(&out.stderr));
                        if text.len() > MAX_OUTPUT {
                            let mut cut = MAX_OUTPUT;
                            while cut > 0 && !text.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            text.truncate(cut);
                            text.push_str("\n… (truncated)");
                        }
                        Ok(if out.status.success() {
                            Observation::ok(text)
                        } else {
                            Observation::error(text)
                        })
                    }
                }
            }

            other => Ok(Observation::error(format!("unknown tool `{other}`"))),
        }
    }
}

const MAX_OUTPUT: usize = 16 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ws() -> (tempfile::TempDir, WorkspaceTools) {
        let dir = tempfile::tempdir().unwrap();
        let tools = WorkspaceTools::new(dir.path());
        (dir, tools)
    }

    #[tokio::test]
    async fn a_write_then_read_round_trips() {
        let (_d, t) = ws();
        let w = t
            .execute(&ToolCall::new(
                "write_file",
                json!({"path": "a/b.txt", "content": "hello"}),
            ))
            .await
            .unwrap();
        assert!(!w.is_error, "{}", w.content);

        let r = t
            .execute(&ToolCall::new("read_file", json!({"path": "a/b.txt"})))
            .await
            .unwrap();
        assert_eq!(r.content, "hello");
    }

    #[tokio::test]
    async fn paths_that_escape_the_workspace_are_refused() {
        let (_d, t) = ws();
        for raw in ["../escape.txt", "/etc/passwd", "a/../../escape.txt"] {
            let r = t
                .execute(&ToolCall::new(
                    "write_file",
                    json!({"path": raw, "content": "x"}),
                ))
                .await
                .unwrap();
            assert!(r.is_error, "`{raw}` was not refused");
        }
    }

    #[tokio::test]
    async fn listing_is_sorted_so_trajectories_are_reproducible() {
        let (d, t) = ws();
        for name in ["c.txt", "a.txt", "b.txt"] {
            std::fs::write(d.path().join(name), "x").unwrap();
        }
        let r = t
            .execute(&ToolCall::new("list_files", json!({})))
            .await
            .unwrap();
        assert_eq!(r.content, "a.txt\nb.txt\nc.txt");
    }

    #[tokio::test]
    async fn a_failing_command_is_a_recoverable_error_not_a_fatal_one() {
        let (_d, t) = ws();
        let r = t
            .execute(&ToolCall::new("run_command", json!({"command": "false"})))
            .await
            .unwrap();
        assert!(r.is_error);
    }
}
