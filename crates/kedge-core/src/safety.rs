//! Tool-safety classification — the taxonomy Shadow-Guard uses to decide what an
//! agent is allowed to execute for real.
//!
//! This lives in `kedge-core` (not `kedge-audit`) deliberately: it is pure,
//! dependency-free logic depended on by the audit executor, the HITL gate, the
//! Python bridge, *and* the WebAssembly demo. Keeping it in the wasm-clean core
//! lets every one of those use the exact same classifier — the browser demo runs
//! the real code path, not a copy.

use serde::Serialize;

/// How dangerous a mutating tool is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Medium,
    High,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

/// A tool's safety boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSafety {
    /// Safe to execute for real (reads, queries, compaction).
    ReadOnly,
    /// Has side effects — intercepted in audit mode.
    Mutating { risk: Risk },
}

impl ToolSafety {
    pub fn is_mutating(self) -> bool {
        matches!(self, ToolSafety::Mutating { .. })
    }
}

/// Verbs whose tools only read state — these run for real even in audit mode.
const READ_VERBS: &[&str] = &[
    "read",
    "get",
    "list",
    "search",
    "find",
    "query",
    "fetch",
    "show",
    "cat",
    "grep",
    "view",
    "describe",
    "inspect",
    "compact",
    "outline",
    "ls",
    "stat",
    "head",
    "tail",
    "count",
    "diff",
    "status",
    "log",
    "help",
    "summarize",
    "analyze",
    "lookup",
    "check",
];

/// Explicitly dangerous verbs — destructive, privileged, or externally-visible.
const HIGH_RISK_VERBS: &[&str] = &[
    "exec",
    "execute",
    "shell",
    "run",
    "eval",
    "spawn",
    "delete",
    "rm",
    "drop",
    "kill",
    "destroy",
    "remove",
    "wipe",
    "erase",
    "purge",
    "unlink",
    "sudo",
    "chmod",
    "chown",
    "format",
    "truncate",
    "overwrite",
    "deploy",
    "publish",
    "release",
    "send",
    "post",
    "email",
    "charge",
    "transfer",
    "pay",
    "revoke",
    "shutdown",
    "reboot",
    "restart",
];

/// Clearly side-effecting but non-destructive verbs — still mutating, so still
/// intercepted in audit mode, just a lower risk label.
const MUTATING_VERBS: &[&str] = &[
    "write",
    "create",
    "update",
    "set",
    "modify",
    "insert",
    "append",
    "save",
    "edit",
    "patch",
    "rename",
    "move",
    "mv",
    "copy",
    "cp",
    "upload",
    "install",
    "commit",
    "push",
    "merge",
    "grant",
    "enable",
    "disable",
    "start",
    "stop",
    "register",
    "unregister",
    "subscribe",
    "unsubscribe",
    "mkdir",
    "put",
    "add",
    "replace",
    "apply",
    "sync",
    "provision",
    "invoke",
    "trigger",
    "submit",
];

/// Classify a tool by its name. **Fail-safe and deny-wins:** anything not
/// recognized as clearly read-only is treated as mutating.
///
/// Unlike a head-verb-only check, this scans *every* token in the name, so a
/// compound like `get_and_delete` or `list_then_wipe` — which reads as read-only
/// if you only look at the first word — is correctly caught as mutating. A name is
/// classified read-only only when its head is a read verb **and** no token
/// anywhere in the name is a mutating/dangerous verb.
///
/// Note: this is still *name*-based and cannot see arguments. A generic tool like
/// `fetch`/`request` that mutates via a `method` argument classifies read-only by
/// name; declare such a tool's capability explicitly to gate it correctly.
pub fn classify(tool_name: &str) -> ToolSafety {
    let tokens: Vec<String> = tool_name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();

    // Deny-wins: a dangerous token anywhere makes the whole tool high-risk, even
    // when the name *starts* with a read verb.
    if tokens.iter().any(|t| HIGH_RISK_VERBS.contains(&t.as_str())) {
        return ToolSafety::Mutating { risk: Risk::High };
    }
    // Any other clearly side-effecting token → mutating (medium risk).
    if tokens.iter().any(|t| MUTATING_VERBS.contains(&t.as_str())) {
        return ToolSafety::Mutating { risk: Risk::Medium };
    }
    // Read-only only if the head is a read verb and nothing above tripped.
    match tokens.first() {
        Some(head) if READ_VERBS.contains(&head.as_str()) => ToolSafety::ReadOnly,
        // Unknown / empty → assume it can mutate. Safety over convenience.
        _ => ToolSafety::Mutating { risk: Risk::Medium },
    }
}

/// Classify honoring optional capability hints (e.g. MCP tool `annotations`),
/// **fail-safe**: a hint may only make a tool *more* restricted, never less.
///
/// This is the safe way to incorporate declared capabilities from a source you
/// don't fully trust (a remote MCP server controls its own tool metadata). A
/// server claiming `read_only_hint = true` on a name that looks mutating is **not**
/// trusted to downgrade it — otherwise a hostile server could label a destructive
/// tool read-only to get it executed for real in audit mode. Only *upgrades*
/// (`destructive_hint = true`, or `read_only_hint = false`) are honored.
pub fn classify_annotated(
    name: &str,
    read_only_hint: Option<bool>,
    destructive_hint: Option<bool>,
) -> ToolSafety {
    let base = classify(name);
    // Explicitly destructive → high risk, always (upgrade).
    if destructive_hint == Some(true) {
        return ToolSafety::Mutating { risk: Risk::High };
    }
    // Explicitly not-read-only → at least mutating (upgrade a name that looked read).
    if read_only_hint == Some(false) {
        return match base {
            ToolSafety::ReadOnly => ToolSafety::Mutating { risk: Risk::Medium },
            other => other,
        };
    }
    // A `read_only_hint = true` is deliberately NOT trusted to downgrade a
    // mutating-looking name; it can only leave an already-read-only name as-is.
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_verbs_are_read_only() {
        for v in ["read_file", "list-dir", "grep", "search_code", "compact"] {
            assert_eq!(classify(v), ToolSafety::ReadOnly, "{v} should be read-only");
        }
    }

    #[test]
    fn dangerous_verbs_are_high_risk() {
        for v in ["rm", "shell", "delete_all", "sudo_thing", "charge_card"] {
            assert_eq!(
                classify(v),
                ToolSafety::Mutating { risk: Risk::High },
                "{v} should be high-risk"
            );
        }
    }

    #[test]
    fn unknown_verbs_fail_safe_to_mutating() {
        // The whole safety argument: an unrecognized tool is assumed dangerous.
        assert_eq!(
            classify("frobnicate_the_database"),
            ToolSafety::Mutating { risk: Risk::Medium }
        );
        assert_eq!(classify(""), ToolSafety::Mutating { risk: Risk::Medium });
    }

    /// Red-team regression: a mutating tool whose NAME STARTS with a read verb
    /// must NOT slip through as read-only (the head-token-only bypass). Every one
    /// of these executed for real in audit mode before the deny-wins scan.
    #[test]
    fn read_verb_prefix_does_not_hide_a_mutation() {
        for name in [
            "get_and_delete",
            "list_then_wipe",
            "search_and_destroy",
            "read_and_remove",
            "fetch_and_post",
            "get_or_create", // classic MCP pattern
            "status_update", // read verb head, but it UPDATES
            "read_write",
            "find_and_replace",
            "search_and_replace",
            "list_exec",
        ] {
            assert!(
                classify(name).is_mutating(),
                "`{name}` must be treated as mutating, not read-only"
            );
        }
    }

    #[test]
    fn annotations_can_only_upgrade_safety_never_downgrade() {
        // A hostile server can't relabel a destructive tool as read-only to slip it
        // past the audit guard.
        assert_eq!(
            classify_annotated("delete_everything", Some(true), None),
            ToolSafety::Mutating { risk: Risk::High },
            "read_only_hint=true must NOT downgrade a mutating name"
        );
        // But a hint CAN upgrade an innocuously-named tool that actually mutates.
        assert!(
            classify_annotated("get_weather", None, Some(true)).is_mutating(),
            "destructive_hint=true must upgrade a read-looking name"
        );
        assert!(
            classify_annotated("lookup", Some(false), None).is_mutating(),
            "read_only_hint=false must upgrade a read-looking name"
        );
        // No hints → identical to plain name classification.
        assert_eq!(
            classify_annotated("read_file", None, None),
            classify("read_file")
        );
        // A truthful read-only hint on a read-only name leaves it read-only.
        assert_eq!(
            classify_annotated("get_status", Some(true), None),
            ToolSafety::ReadOnly
        );
    }

    #[test]
    fn compound_read_only_names_still_read_only() {
        // Genuinely read-only compounds must not become false-positive mutations.
        for name in [
            "get_user",
            "list_directory",
            "search_code",
            "read_file_contents",
            "describe_table",
            "fetch_status",
        ] {
            assert_eq!(
                classify(name),
                ToolSafety::ReadOnly,
                "`{name}` should stay read-only"
            );
        }
    }
}
