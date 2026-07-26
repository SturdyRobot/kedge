//! What a tool call *asks for*, derived from its name and arguments.
//!
//! A manifest grants capabilities; this module decides which ones a given call
//! is requesting. The whole guard is only as good as this derivation, so it has
//! one rule above all others:
//!
//! > **If we cannot name what a call would do, we do not permit it.**
//!
//! That is why [`Requirement::Indeterminate`] exists. The tempting alternative —
//! "no recognized arguments, so it needs nothing, so let it through" — turns the
//! allow-list into a blocklist of argument names, which is exactly the failure
//! mode `kedge-policy`'s `blocked_tools` has and this crate exists to fix.
//!
//! The one deliberate exception: a call the classifier reports as read-only,
//! carrying no capability-bearing argument, requests nothing enumerable (think
//! `list_tools`, `get_time`). Denying those would make every manifest a list of
//! trivia. A *mutating* call in the same shape is always indeterminate, because
//! an unnamed effect is still an effect.
//!
//! ## Three layers, because a name table is not enough
//!
//! The first version of this module recognized capability arguments **by key
//! name only**, and that exception above swallowed a real bypass: a server that
//! calls its path argument `resource` produced a `read_file` call matching no
//! known key, therefore "requiring nothing", therefore permitted under a
//! manifest granting one single file. The allow-list had quietly become a
//! blocklist of argument names — the failure this module's own header warns
//! about, present in the module itself.
//!
//! So there are three layers now, and the last two exist because the first is
//! not sound:
//!
//! 1. **Key name** — `path`, `command`, `url`, `token`, … Precise when it hits.
//! 2. **Value shape** — a string under *any* key that looks like a path
//!    (`/etc/shadow`, `src/lib.rs`) or a URL is treated as one. An unknown
//!    vocabulary now costs precision, not soundness.
//! 3. **Tool name** — a tool calling itself `read_file` or `fetch` while naming
//!    nothing it would touch is refused, because no manifest can constrain it.
//!    This catches the residue that layer 2 cannot: a bare token like
//!    `{"resource": "shadow"}` reveals nothing about what it is.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kedge_core::{classify, ToolCall};
use serde_json::Value;

/// A single thing a call wants to touch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    FsRead(PathBuf),
    FsWrite(PathBuf),
    /// A command line, exactly as the call wrote it.
    Process(String),
    /// A URL or host the call would reach.
    Network(String),
    /// A named credential the call would read.
    Secret(String),
}

impl Capability {
    /// A short, stable label for reports and manifest minimization.
    pub fn kind(&self) -> &'static str {
        match self {
            Capability::FsRead(_) => "filesystem.read",
            Capability::FsWrite(_) => "filesystem.write",
            Capability::Process(_) => "process",
            Capability::Network(_) => "network",
            Capability::Secret(_) => "secrets",
        }
    }

    /// The subject of the capability, for reports.
    pub fn subject(&self) -> String {
        match self {
            Capability::FsRead(p) | Capability::FsWrite(p) => p.to_string_lossy().into_owned(),
            Capability::Process(c) => c.clone(),
            Capability::Network(u) => u.clone(),
            Capability::Secret(k) => k.clone(),
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} `{}`", self.kind(), self.subject())
    }
}

/// The outcome of inspecting a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    /// Exactly these capabilities, and nothing else.
    Known(BTreeSet<Capability>),
    /// The call's effect could not be enumerated. Always a deny.
    Indeterminate(String),
}

// Argument keys that name a filesystem path.
const PATH_KEYS: &[&str] = &[
    "path",
    "paths",
    "file",
    "files",
    "file_path",
    "filepath",
    "filename",
    "file_name",
    "dir",
    "directory",
    "folder",
    "src",
    "source",
    "source_path",
    "dest",
    "destination",
    "dest_path",
    "target",
    "target_file",
    "output",
    "output_path",
    "input",
    "input_path",
];

// Argument keys that name a command to execute.
const COMMAND_KEYS: &[&str] = &["command", "cmd", "argv", "script", "shell", "exec", "run"];

// Argument keys that name a network destination.
const NETWORK_KEYS: &[&str] = &[
    "url", "uri", "endpoint", "href", "host", "hostname", "address",
];

// Argument keys that name a credential.
const SECRET_KEYS: &[&str] = &[
    "token",
    "api_key",
    "apikey",
    "secret",
    "password",
    "passwd",
    "credential",
    "credentials",
    "private_key",
    "access_key",
    "auth",
    "authorization",
];

/// Derive the capabilities a call requires. `base` is the directory relative
/// paths are resolved against.
pub fn required(call: &ToolCall, base: &Path) -> Requirement {
    let mutating = classify(&call.name).is_mutating();
    let promises_io = name_promises_io(&call.name);
    let mut found = BTreeSet::new();
    let mut saw_capability_key = false;

    if let Err(reason) = walk(
        &call.arguments,
        None,
        base,
        mutating,
        promises_io,
        &mut found,
        &mut saw_capability_key,
    ) {
        return Requirement::Indeterminate(reason);
    }

    if found.is_empty() && !saw_capability_key {
        if mutating {
            return Requirement::Indeterminate(format!(
                "`{}` is classified mutating but names no path, command, URL or credential \
                 in its arguments, so its effect cannot be granted",
                call.name
            ));
        }
        if promises_io {
            return Requirement::Indeterminate(format!(
                "`{}` names a filesystem or network operation but no argument identifies \
                 what it would touch, so no manifest can constrain it",
                call.name
            ));
        }
        // Read-only, promises no I/O, and touches nothing nameable.
        return Requirement::Known(BTreeSet::new());
    }

    Requirement::Known(found)
}

/// Recursively scan arguments. `key` is the enclosing object key, if any.
#[allow(clippy::too_many_arguments)]
fn walk(
    value: &Value,
    key: Option<&str>,
    base: &Path,
    mutating: bool,
    promises_io: bool,
    out: &mut BTreeSet<Capability>,
    saw: &mut bool,
) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                walk(v, Some(k.as_str()), base, mutating, promises_io, out, saw)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            // An argv array is ONE command line, not one command per element.
            // Deriving `["cargo", "test"]` as two separate `Process`
            // capabilities made the call unpermittable by any manifest: neither
            // bare `cargo` nor bare `test` matches an allow-entry of
            // `cargo test`. It failed safe, and it also failed to work.
            if let Some(k) = key {
                if COMMAND_KEYS.contains(&k.to_ascii_lowercase().as_str()) {
                    if let Some(parts) = items
                        .iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                    {
                        *saw = true;
                        out.insert(Capability::Process(parts.join(" ")));
                        return Ok(());
                    }
                }
            }
            for item in items {
                // Otherwise an array inherits its parent's key: `paths: ["a", "b"]`.
                walk(item, key, base, mutating, promises_io, out, saw)?;
            }
            Ok(())
        }
        Value::String(s) => {
            let Some(key) = key else { return Ok(()) };
            let lower = key.to_ascii_lowercase();

            if SECRET_KEYS.contains(&lower.as_str()) {
                *saw = true;
                out.insert(Capability::Secret(lower));
                return Ok(());
            }
            if COMMAND_KEYS.contains(&lower.as_str()) {
                *saw = true;
                out.insert(Capability::Process(s.clone()));
                return Ok(());
            }
            if NETWORK_KEYS.contains(&lower.as_str()) {
                *saw = true;
                out.insert(Capability::Network(s.clone()));
                return Ok(());
            }
            if PATH_KEYS.contains(&lower.as_str()) {
                *saw = true;
                // A URL is never a path, whatever the key is called. `target`
                // and `dest` are in both vocabularies, and resolving
                // `https://evil.com/x` against the workspace produced the
                // nonsense capability `FsWrite("/repo/https:/evil.com/x")` —
                // which no manifest would grant, so it failed safe, and which
                // also described the wrong effect entirely.
                if looks_like_url(s) {
                    out.insert(Capability::Network(s.clone()));
                    return Ok(());
                }
                return insert_path(s, key, base, mutating, out);
            }

            // The key is not one we know. Fall back to the *shape of the value*.
            //
            // Without this, a server naming its path argument `resource` or
            // `target_uri` produced a call that "requires nothing" — and a
            // read-only-sounding tool would then read any file on the machine
            // under a manifest granting one. The allow-list silently degraded
            // into a blocklist of argument names, which is the exact failure
            // this module's header warns about and which it nonetheless had.
            //
            // Matching on shape rather than name means an unknown vocabulary
            // costs precision, not soundness.
            if looks_like_url(s) {
                *saw = true;
                out.insert(Capability::Network(s.clone()));
            } else if looks_like_path(s, promises_io) {
                *saw = true;
                return insert_path(s, key, base, mutating, out);
            }
            Ok(())
        }
        // Numbers, bools and nulls carry no capability.
        _ => Ok(()),
    }
}

fn insert_path(
    s: &str,
    key: &str,
    base: &Path,
    mutating: bool,
    out: &mut BTreeSet<Capability>,
) -> Result<(), String> {
    let Some(resolved) = super::path::resolve(base, s) else {
        return Err(format!(
            "argument `{key}` = `{s}` does not resolve to a path inside the \
             filesystem (it escapes the root)"
        ));
    };
    out.insert(if mutating {
        Capability::FsWrite(resolved)
    } else {
        Capability::FsRead(resolved)
    });
    Ok(())
}

/// Whether a string is shaped like a filesystem path.
///
/// An unambiguous prefix (`/`, `./`, `../`, `~/`) counts on its own, whitespace
/// and all. A bare `contains('/')` only counts when there is no whitespace, so
/// prose like `"see /docs/readme for details"` is not mistaken for a path.
fn looks_like_path(s: &str, promises_io: bool) -> bool {
    // No newlines, not enormous. These guards are what stop a file's *contents*
    // being shape-matched as a location — Rust source begins `//!`, which starts
    // with `/`. An earlier attempt used a denylist of content-carrying key names
    // instead, and the generic entries in it (`value`, `data`, `text`) reopened
    // the very bypass the shape check exists to close. A value-shape problem
    // cannot be fixed with a list of names.
    if s.is_empty() || s.len() > MAX_PATH_LEN || s.contains(['\n', '\r']) {
        return false;
    }
    // A rooted path is unambiguous under any key.
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.starts_with("~/") {
        return true;
    }
    // A bare `a/b/c` is ambiguous — it is equally a cache key, a repo slug or a
    // topic. It counts only when the tool's own name says it does filesystem
    // work, which is the difference between `read_file {"resource": "src/x.rs"}`
    // and `query_records {"cache_key": "org/repo/main"}`.
    promises_io && s.contains('/') && !s.chars().any(char::is_whitespace)
}

/// Longer than any real path on any platform we target.
const MAX_PATH_LEN: usize = 4096;

fn looks_like_url(s: &str) -> bool {
    s.contains("://")
}

/// Tool names that promise filesystem or network access.
///
/// The second half of the A1 fix. Value-shape catches `{"resource":
/// "/etc/shadow"}`; this catches `{"resource": "shadow"}`, where the value is a
/// bare token that reveals nothing. A tool calling itself `read_file` while
/// naming no path it will read cannot be granted, so it is refused.
fn name_promises_io(name: &str) -> bool {
    const TOKENS: &[&str] = &[
        "file",
        "files",
        "dir",
        "dirs",
        "directory",
        "folder",
        "folders",
        "path",
        "paths",
        "read",
        "write",
        "fetch",
        "http",
        "url",
        "download",
        "upload",
        "load",
        "save",
        "copy",
        "move",
        "open",
        "cat",
        "stat",
    ];
    // Whole tokens, not substrings. Substring matching refused `get_profile`
    // (contains `file`), `get_payload` (contains `load`) and `list_openings`
    // (contains `open`) — a security check that refuses ordinary tools is one
    // that gets switched off.
    name_tokens(name)
        .iter()
        .any(|tok| TOKENS.contains(&tok.as_str()))
}

/// Split a tool name into lowercase tokens on separators and camelCase humps.
fn name_tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in name.chars() {
        if matches!(c, '_' | '-' | '.' | '/' | ' ' | ':') {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else if c.is_uppercase() && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur.push(c.to_ascii_lowercase());
        } else {
            cur.push(c.to_ascii_lowercase());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(name: &str, args: Value) -> Requirement {
        required(&ToolCall::new(name, args), Path::new("/repo"))
    }

    fn caps(name: &str, args: Value) -> BTreeSet<Capability> {
        match req(name, args) {
            Requirement::Known(c) => c,
            Requirement::Indeterminate(r) => panic!("expected Known, got Indeterminate: {r}"),
        }
    }

    #[test]
    fn a_read_names_a_read_capability_and_a_write_names_a_write() {
        let r = caps("read_file", json!({"path": "src/main.rs"}));
        assert!(r.contains(&Capability::FsRead(PathBuf::from("/repo/src/main.rs"))));

        let w = caps("write_file", json!({"path": "src/main.rs"}));
        assert!(w.contains(&Capability::FsWrite(PathBuf::from("/repo/src/main.rs"))));
    }

    #[test]
    fn a_mutating_call_naming_nothing_is_indeterminate() {
        // The load-bearing fail-safe: `deploy_to_prod` with no arguments has a
        // real effect that no manifest can describe, so it cannot be granted.
        let r = req("deploy_to_prod", json!({}));
        assert!(matches!(r, Requirement::Indeterminate(_)), "{r:?}");
    }

    #[test]
    fn a_read_only_call_naming_nothing_requests_nothing() {
        assert_eq!(caps("list_tools", json!({})).len(), 0);
        assert_eq!(caps("get_time", json!({"format": "iso"})).len(), 0);
    }

    /// Red-team A1: an argument key outside the table used to defeat the read
    /// grant entirely, because "no known key" was read as "needs nothing".
    #[test]
    fn an_unknown_key_carrying_a_path_still_yields_the_capability() {
        for key in ["resource", "target_uri", "blob", "whatever"] {
            let r = caps("read_file", json!({ key: "/loot/shadow" }));
            assert!(
                r.contains(&Capability::FsRead(PathBuf::from("/loot/shadow"))),
                "`{key}` slipped through: {r:?}"
            );
        }
        // And the mutating direction.
        let w = caps("write_file", json!({"resource": "/loot/shadow"}));
        assert!(w.contains(&Capability::FsWrite(PathBuf::from("/loot/shadow"))));
    }

    /// Red-team A1, residue: the value reveals nothing, so the *name* has to.
    #[test]
    fn a_tool_promising_io_that_names_nothing_is_indeterminate() {
        for name in ["read_file", "fetch", "download_blob", "load_config"] {
            let r = req(name, json!({"resource": "shadow"}));
            assert!(
                matches!(r, Requirement::Indeterminate(_)),
                "`{name}` was permitted while naming nothing: {r:?}"
            );
        }
        // But a tool that promises no I/O is still free.
        assert_eq!(caps("list_tools", json!({"filter": "all"})).len(), 0);
    }

    /// Regression from the A1 fix itself.
    ///
    /// Value-shape detection made a `write_file` payload derive a second,
    /// garbage capability: Rust source begins `//! …`, which starts with `/`.
    /// Every write in the bench corpus grew a phantom filesystem grant.
    ///
    /// The first attempt at this fix was a denylist of content-carrying key
    /// names, and its generic entries (`value`, `data`, `text`) reopened A1 —
    /// so a value-shape problem is now solved with value-shape guards only.
    #[test]
    fn a_file_payload_is_never_mistaken_for_a_location() {
        let source = "//! Numeric bounds.\n\npub fn clamp(v: i32) -> i32 { v }\n";
        let r = caps(
            "write_file",
            json!({"path": "src/lib.rs", "content": source}),
        );
        assert_eq!(r.len(), 1, "the payload produced a phantom grant: {r:?}");
        assert!(r.contains(&Capability::FsWrite(PathBuf::from("/repo/src/lib.rs"))));

        // Multi-line and oversized values are not paths, whatever the key.
        assert!(caps("get_page", json!({"blob": "/etc/shadow\nmore"})).is_empty());
        assert!(caps("get_page", json!({"blob": "/x".repeat(4000)})).is_empty());
    }

    /// Red-team B1: a *rooted* path is unambiguous and counts under any key,
    /// including keys that sound like they carry content. The earlier
    /// `CONTENT_KEYS` denylist skipped these and reopened the A1 bypass.
    #[test]
    fn a_rooted_path_counts_under_any_key_name() {
        for key in ["value", "data", "text", "body", "payload", "message"] {
            let r = caps("query_records", json!({ key: "/loot/shadow" }));
            assert!(
                r.contains(&Capability::FsRead(PathBuf::from("/loot/shadow"))),
                "`{key}` skipped shape detection: {r:?}"
            );
        }
    }

    /// Red-team B3: a bare `a/b/c` is equally a cache key, a repo slug or a
    /// topic. It counts only when the tool's own name does filesystem work.
    #[test]
    fn a_bare_relative_value_counts_only_when_the_tool_does_io() {
        // Not a filesystem tool: this is a cache key, not a path.
        let label = caps("query_records", json!({"cache_key": "org/repo/main"}));
        assert!(label.is_empty(), "a cache key became a grant: {label:?}");

        // A filesystem tool: the same shape is a real relative path.
        let real = caps("read_file", json!({"resource": "src/lib.rs"}));
        assert!(
            real.contains(&Capability::FsRead(PathBuf::from("/repo/src/lib.rs"))),
            "{real:?}"
        );
    }

    /// Red-team B2: token boundaries, not substrings.
    ///
    /// Tested on the predicate directly rather than through `required`, because
    /// `kedge_core::classify` reaches its own verdict first — every camelCase
    /// name is *mutating* to it (it does not split humps, and unknown shapes
    /// fail safe). Going through `required` would have asserted that rule while
    /// claiming to assert this one.
    #[test]
    fn name_matching_is_on_token_boundaries_not_substrings() {
        // The traps: `profile` contains `file`, `payload` contains `load`,
        // `openings` contains `open`, `balancer` contains… nothing, but
        // `load_balancer` does.
        for name in [
            "get_profile",
            "get_payload",
            "list_openings",
            "getProfileCard",
            "reader_stats",
        ] {
            assert!(
                !name_promises_io(name),
                "`{name}` was treated as an I/O tool"
            );
        }
        // And the real ones still are, camelCase included.
        for name in [
            "read_file",
            "readFile",
            "fetch",
            "download_blob",
            "load_config",
            "listFiles",
            "writePath",
        ] {
            assert!(name_promises_io(name), "`{name}` was not treated as I/O");
        }
    }

    #[test]
    fn tool_names_split_on_separators_and_camel_humps() {
        assert_eq!(name_tokens("read_file"), ["read", "file"]);
        assert_eq!(name_tokens("readFile"), ["read", "file"]);
        assert_eq!(name_tokens("get-profile.card"), ["get", "profile", "card"]);
        assert_eq!(
            name_tokens("puppeteer_screenshot"),
            ["puppeteer", "screenshot"]
        );
    }

    #[test]
    fn prose_containing_a_slash_is_not_mistaken_for_a_path() {
        // Fail-safe must not mean fail-noisy: an unknown key holding a sentence
        // is not a filesystem access.
        let r = caps("get_page", json!({"body": "see /docs/readme for details"}));
        assert!(r.is_empty(), "{r:?}");
    }

    #[test]
    fn a_url_is_a_network_capability_even_under_a_path_shaped_key() {
        // `target` is in PATH_KEYS, so this used to resolve to the nonsense
        // path `/repo/https:/evil.com/x`.
        let r = caps("get_page", json!({"target": "https://evil.com/x"}));
        assert!(
            r.contains(&Capability::Network("https://evil.com/x".into())),
            "{r:?}"
        );
        // And under a key in no table at all.
        let r = caps("get_page", json!({"whatever": "https://evil.com/x"}));
        assert!(
            r.contains(&Capability::Network("https://evil.com/x".into())),
            "{r:?}"
        );
    }

    /// Red-team A5: an argv array is one command line.
    #[test]
    fn an_argv_array_is_a_single_command() {
        let r = caps("run", json!({"argv": ["cargo", "test", "--lib"]}));
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r.contains(&Capability::Process("cargo test --lib".into())));
    }

    #[test]
    fn traversal_in_an_argument_yields_the_resolved_path() {
        // Resolved, not matched as text — so a `/repo/**` grant will not cover
        // it. Non-existent paths keep this assertion platform-independent.
        let r = caps("read_file", json!({"path": "src/../../loot/x"}));
        assert!(
            r.contains(&Capability::FsRead(PathBuf::from("/loot/x"))),
            "{r:?}"
        );
    }

    #[test]
    fn a_path_that_escapes_the_root_is_indeterminate() {
        let r = req("read_file", json!({"path": "/../../../../loot/x"}));
        assert!(matches!(r, Requirement::Indeterminate(_)), "{r:?}");
        // `/repo/../..` is also above the root: one `..` per component, no more.
        let r = req("read_file", json!({"path": "../../loot/x"}));
        assert!(matches!(r, Requirement::Indeterminate(_)), "{r:?}");
    }

    #[test]
    fn nested_and_array_arguments_are_scanned() {
        let r = caps("read_file", json!({"opts": {"paths": ["a.rs", "b.rs"]}}));
        assert!(r.contains(&Capability::FsRead(PathBuf::from("/repo/a.rs"))));
        assert!(r.contains(&Capability::FsRead(PathBuf::from("/repo/b.rs"))));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn commands_urls_and_secrets_are_each_their_own_capability() {
        assert!(caps("run_tests", json!({"command": "cargo test"}))
            .contains(&Capability::Process("cargo test".into())));
        assert!(caps("fetch", json!({"url": "https://api.example.com/x"}))
            .contains(&Capability::Network("https://api.example.com/x".into())));
        assert!(caps("get_config", json!({"api_key": "sk-live-abc"}))
            .contains(&Capability::Secret("api_key".into())));
    }

    #[test]
    fn a_single_call_can_request_several_capabilities_at_once() {
        // Copying between two places is two grants, not one.
        let r = caps(
            "copy_file",
            json!({"source": "a.txt", "destination": "b.txt"}),
        );
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn argument_keys_are_matched_case_insensitively() {
        // A server declaring `filePath` must not slip past a lowercase table.
        let r = caps("read_file", json!({"filePath": "src/main.rs"}));
        assert!(r.contains(&Capability::FsRead(PathBuf::from("/repo/src/main.rs"))));
    }
}
