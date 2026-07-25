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
    // Deliberately excluded as ambiguous: "open" (open_file may create) and
    // "convert" (may write its output). Servers that declare `readOnlyHint`
    // cover those without us guessing.
    "screenshot",
    "echo",
    "tree",
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
    // Read-only ONLY if the head token is a read verb.
    //
    // 0.3.0 briefly widened this to a two-token window so a namespace prefix
    // (`puppeteer_screenshot`) would not hide the verb. Adversarial testing
    // showed that inverted the engine's premise: a known-safe verb anywhere in
    // the window validated whatever followed it, so `ns_get_frobnicate` and
    // `x_get_nuke` were forwarded on names whose actual action is unknown. That
    // turns a fail-safe default into a blocklist, which is precisely the thing
    // this classifier exists not to be.
    //
    // Namespaces are a real problem, but they cannot be solved by guessing from
    // a single name in isolation. It needs corroboration across a server's whole
    // catalogue, which is context this stateless function does not have and
    // should not acquire.
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

/// Regression tests from an ecosystem sweep of 10 real MCP servers (80 tools),
/// scored against the `readOnlyHint` those servers publish about themselves.
///
/// The sweep found zero false negatives and six false positives, all traced to
/// two causes fixed here: read verbs were only recognised in head position, so
/// every namespaced tool failed safe, and a few plainly read-only verbs were
/// missing from the vocabulary.
#[cfg(test)]
mod ecosystem {
    use super::*;

    /// The bypass that got the two-token window reverted in 0.3.1.
    ///
    /// The window let a known-safe verb validate whatever followed it, so a name
    /// whose real action is unknown was forwarded: `ns_get_frobnicate`,
    /// `x_get_nuke`. That converts a fail-safe default into a blocklist, which
    /// is the opposite of what this classifier is for. These names must stay
    /// mutating, permanently.
    #[test]
    fn a_read_verb_never_validates_an_unknown_suffix() {
        for name in [
            "ns_get_frobnicate",
            "ns_read_obliterate",
            "x_get_nuke",
            "svc_list_purge",
            "api_search_incinerate",
            "prefix_fetch_unknownaction",
        ] {
            assert!(
                classify(name).is_mutating(),
                "BYPASS: {name} has an unknown action behind a read verb and must fail safe"
            );
        }
    }

    /// The cost of that revert, recorded honestly rather than hidden. Namespaced
    /// read-only tools are false positives again until Foreguard can corroborate
    /// a prefix across a server's whole catalogue. A false positive is noise; the
    /// bypass was a hole.
    #[test]
    fn namespaced_reads_are_knowingly_false_positive() {
        for name in ["puppeteer_screenshot", "github_get_file", "directory_tree"] {
            assert!(
                classify(name).is_mutating(),
                "{name} is expected to fail safe while namespace handling lives elsewhere"
            );
        }
    }

    /// Deny-wins is untouched by any of this.
    #[test]
    fn the_window_never_defeats_deny_wins() {
        for name in [
            "get_and_delete",
            "read_and_write_file",
            "github_delete_repo",
            "list_then_rm",
            "search_and_drop_table",
            "fetch_and_execute",
        ] {
            assert!(
                classify(name).is_mutating(),
                "ESCAPE: {name} contains a dangerous verb and must stay mutating"
            );
        }
    }

    /// An unrecognised head still fails safe regardless of what follows.
    #[test]
    fn a_read_verb_beyond_the_head_does_not_rescue_an_unknown_name() {
        for name in [
            "frobnicate_and_get",
            "nuke_the_thing_list",
            "unknownverb_something_read",
        ] {
            assert!(
                classify(name).is_mutating(),
                "{name} has an unrecognised head; a later read verb must not downgrade it"
            );
        }
    }

    /// Vocabulary additions, and the ones deliberately left out.
    #[test]
    fn newly_recognised_read_verbs() {
        for name in ["screenshot", "echo", "tree"] {
            assert_eq!(classify(name), ToolSafety::ReadOnly, "{name} is read-only");
        }
        // Ambiguous on purpose: these can write, so they stay fail-safe and are
        // left to a server's own declared hints.
        for name in ["open_nodes", "convert_time"] {
            assert!(
                classify(name).is_mutating(),
                "{name} is ambiguous and must stay fail-safe"
            );
        }
    }

    /// The real filesystem and github catalogues, which the sweep scored by hand.
    #[test]
    fn real_server_catalogues_classify_correctly() {
        let read_only = [
            "read_file",
            "read_text_file",
            "read_media_file",
            "read_multiple_files",
            "list_directory",
            "list_directory_with_sizes",
            "search_files",
            "get_file_info",
            "list_allowed_directories",
            "search_repositories",
            "get_file_contents",
            "list_commits",
            "list_issues",
            "search_code",
            "get_issue",
            "get_pull_request",
            "list_pull_requests",
            "read_query",
            "list_tables",
            "describe_table",
        ];
        for n in read_only {
            assert_eq!(classify(n), ToolSafety::ReadOnly, "{n} should pass");
        }
        let mutating = [
            "write_file",
            "edit_file",
            "create_directory",
            "move_file",
            "create_or_update_file",
            "create_repository",
            "push_files",
            "create_issue",
            "create_pull_request",
            "fork_repository",
            "create_branch",
            "update_issue",
            "merge_pull_request",
            "write_query",
            "create_table",
            "puppeteer_click",
            "puppeteer_fill",
            "puppeteer_evaluate",
        ];
        for n in mutating {
            assert!(
                classify(n).is_mutating(),
                "ESCAPE: {n} should be intercepted"
            );
        }
    }
}
