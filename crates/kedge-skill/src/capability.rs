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
    let mut found = BTreeSet::new();
    let mut saw_capability_key = false;

    if let Err(reason) = walk(
        &call.arguments,
        None,
        base,
        mutating,
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
        // Read-only and touches nothing nameable: requests no capability.
        return Requirement::Known(BTreeSet::new());
    }

    Requirement::Known(found)
}

/// Recursively scan arguments. `key` is the enclosing object key, if any.
fn walk(
    value: &Value,
    key: Option<&str>,
    base: &Path,
    mutating: bool,
    out: &mut BTreeSet<Capability>,
    saw: &mut bool,
) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                walk(v, Some(k.as_str()), base, mutating, out, saw)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                // An array inherits its parent's key: `paths: ["a", "b"]`.
                walk(item, key, base, mutating, out, saw)?;
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
            }
            Ok(())
        }
        // Numbers, bools and nulls carry no capability.
        _ => Ok(()),
    }
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
