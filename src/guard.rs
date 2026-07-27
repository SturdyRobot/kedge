//! The single, canonical guard chain.
//!
//! Both entry points — the CLI (`kedge run`) and the MCP server (`kedge_run`) —
//! build their tool executor here, so no path can drift to a weaker wiring than
//! another. Before this existed, the CLI defaulted to an *unguarded* shell while
//! MCP defaulted to audit, and `kedge-policy` (blocked tools + PII redaction) was
//! wired into neither — it enforced nothing in the shipped binary.
//!
//! Layering, outermost first:
//!
//! ```text
//!   PolicyGuard  →  SkillGuard  →  mode guard (audit|hitl|deny|live)  →  tools
//! ```
//!
//! Three layers, three different questions, deliberately in this order.
//!
//! **PolicyGuard** is the operator's hard rule and applies regardless of mode: a
//! blocked tool is refused and PII in tool output is redacted. It is a
//! *blocklist*, so anything not named is allowed through.
//!
//! **SkillGuard** is the opposite default. It is a deny-by-default capability
//! manifest scoped to *this task*: the skill declares what it may read, write,
//! run and reach, and anything undeclared is refused. Without it, kedge shipped
//! two policy systems with opposite defaults that did not know about each other,
//! and only the weaker one was wired in.
//!
//! Order matters even though both deny-win. The operator's ban is outermost so
//! it cannot be widened by a skill manifest, and so its refusal is the reason a
//! user sees. The manifest sits above the mode guard so a call the task never
//! declared is refused before the approval machinery is asked about it: there is
//! no point prompting a human to approve something the skill was never scoped to
//! do.
//!
//! Passing `manifest: None` means no per-task scoping, which is the pre-existing
//! behaviour and is *less* safe. It is not the default anywhere new.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use kedge_core::{TaskId, ToolExecutor, ToolSafety};
use kedge_hitl::{ApprovalGate, Approver, DenyingApprover};
use kedge_ledger::Ledger;
use kedge_policy::{Policy, PolicyGuard};
use kedge_skill::{Manifest, SkillGuard};

/// Per-tool safety resolved from declared capabilities (e.g. MCP annotations),
/// consulted before name-based classification.
pub type Capabilities = Arc<HashMap<String, ToolSafety>>;

/// The safety posture a run executes under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardMode {
    /// Shadow-Guard dry-run: read-only tools run for real, mutating tools are
    /// intercepted and journaled but never executed. The safe default.
    Audit,
    /// Human-in-the-loop: mutating tools require an approver's `Approve`.
    Hitl,
    /// Read-only lockdown: mutating tools are refused outright.
    Deny,
    /// No guard — an unrestricted shell. Explicit opt-in only.
    Live,
}

/// The built chain: the executor to hand the engine, plus optional handles the
/// caller can query for a safety summary (how many mutations were intercepted /
/// denied).
pub struct GuardChain {
    pub tools: Arc<dyn ToolExecutor>,
    pub auditor: Option<Arc<kedge_audit::AuditExecutor>>,
    pub gate: Option<Arc<ApprovalGate>>,
    /// Present when a manifest scoped the run. Query it afterwards for what the
    /// task actually exercised versus what it declared.
    pub skill: Option<Arc<SkillGuard>>,
}

/// The mode guard's output: the wrapped executor plus optional reporting handles.
type ModeChain = (
    Arc<dyn ToolExecutor>,
    Option<Arc<kedge_audit::AuditExecutor>>,
    Option<Arc<ApprovalGate>>,
);

/// Build the canonical chain. `approver` is only consulted in [`GuardMode::Hitl`];
/// if it is `None` there, the chain fails **safe** by denying every mutating tool.
#[allow(clippy::too_many_arguments)]
pub fn build(
    mode: GuardMode,
    policy: Option<Arc<Policy>>,
    approver: Option<Arc<dyn Approver>>,
    caps: Option<Capabilities>,
    base: Arc<dyn ToolExecutor>,
    ledger: Option<Arc<Ledger>>,
    run_id: TaskId,
    manifest: Option<(Arc<Manifest>, std::path::PathBuf)>,
) -> GuardChain {
    // ── inner: the mode guard ──
    let (mode_tools, auditor, gate): ModeChain = match mode {
        GuardMode::Live => (base, None, None),
        GuardMode::Deny => {
            let g = Arc::new(
                ApprovalGate::new(base, Arc::new(DenyingApprover), ledger, run_id)
                    .with_capabilities(caps),
            );
            (g.clone() as Arc<dyn ToolExecutor>, None, Some(g))
        }
        GuardMode::Hitl => {
            // No approver supplied → deny everything (fail-safe), never fall
            // through to unguarded execution.
            let appr: Arc<dyn Approver> = approver.unwrap_or_else(|| Arc::new(DenyingApprover));
            let g = Arc::new(ApprovalGate::new(base, appr, ledger, run_id).with_capabilities(caps));
            (g.clone() as Arc<dyn ToolExecutor>, None, Some(g))
        }
        GuardMode::Audit => {
            let ae = Arc::new(
                kedge_audit::AuditExecutor::new(base, ledger, run_id).with_capabilities(caps),
            );
            (ae.clone() as Arc<dyn ToolExecutor>, Some(ae), None)
        }
    };

    // ── middle: the per-task capability manifest (deny-by-default) ──
    let (skilled, skill) = match manifest {
        Some((m, workspace)) => {
            let g = Arc::new(SkillGuard::new(m, workspace, mode_tools));
            (g.clone() as Arc<dyn ToolExecutor>, Some(g))
        }
        None => (mode_tools, None),
    };

    // ── outer: the policy guard (operator's hard rule, applied first at runtime) ──
    let tools = match policy {
        Some(p) => Arc::new(PolicyGuard::new(p, skilled)) as Arc<dyn ToolExecutor>,
        None => skilled,
    };

    GuardChain {
        tools,
        auditor,
        gate,
        skill,
    }
}

/// Resolve a policy for a run: an explicit `--policy <path>` (error if missing),
/// otherwise `kedge-policy.toml` in `trusted_dir` if present, otherwise none.
///
/// `trusted_dir` must be an **operator-controlled** directory (the invocation cwd
/// / the MCP server's own cwd) — never the agent's target `--cwd`, which may be an
/// untrusted repo that could ship a hostile policy file (only ever *more*
/// restrictive, but still a denial-of-function / bad-regex startup-DoS vector).
pub fn load_policy(explicit: Option<&Path>, trusted_dir: &Path) -> Result<Option<Arc<Policy>>> {
    if let Some(p) = explicit {
        let policy = Policy::from_toml_file(p)
            .with_context(|| format!("loading policy file {}", p.display()))?;
        return Ok(Some(Arc::new(policy)));
    }
    let default = trusted_dir.join("kedge-policy.toml");
    if default.exists() {
        let policy = Policy::from_toml_file(&default)
            .with_context(|| format!("loading policy file {}", default.display()))?;
        return Ok(Some(Arc::new(policy)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kedge_core::{Observation, ToolCall};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A tool that records whether it was actually run.
    struct SpyTool(Arc<AtomicU64>);
    #[async_trait]
    impl ToolExecutor for SpyTool {
        async fn execute(&self, call: &ToolCall) -> kedge_core::Result<Observation> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Observation::ok(format!("RAN {}", call.name)))
        }
    }

    fn spy() -> (Arc<dyn ToolExecutor>, Arc<AtomicU64>) {
        let hits = Arc::new(AtomicU64::new(0));
        (Arc::new(SpyTool(hits.clone())), hits)
    }

    #[tokio::test]
    async fn audit_default_intercepts_mutations() {
        let (base, hits) = spy();
        let chain = build(
            GuardMode::Audit,
            None,
            None,
            None,
            base,
            None,
            TaskId::new(),
            None,
        );
        let obs = chain
            .tools
            .execute(&ToolCall::new("delete_file", serde_json::json!({})))
            .await
            .unwrap();
        assert!(!obs.content.contains("RAN"));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "mutation must not execute");
        assert_eq!(chain.auditor.unwrap().intercepted(), 1);
    }

    #[tokio::test]
    async fn policy_blocks_tool_even_in_live_mode() {
        // A blocked tool is refused by the outer PolicyGuard before the (unguarded)
        // live mode inner ever sees it.
        let (base, hits) = spy();
        let policy = Arc::new(Policy::from_toml_str(r#"blocked_tools = ["shell"]"#).unwrap());
        let chain = build(
            GuardMode::Live,
            Some(policy),
            None,
            None,
            base,
            None,
            TaskId::new(),
            None,
        );
        let obs = chain
            .tools
            .execute(&ToolCall::new("shell", serde_json::json!({})))
            .await
            .unwrap();
        assert!(obs.is_error);
        assert!(obs.content.contains("blocked by policy"));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "blocked tool must not run");
    }

    #[tokio::test]
    async fn declared_capability_overrides_name_classification() {
        // A read-LOOKING tool name (`get_weather`) that a server declares
        // destructive must be intercepted in audit mode, not executed for real.
        let (base, hits) = spy();
        let mut caps = HashMap::new();
        caps.insert(
            "get_weather".to_string(),
            ToolSafety::Mutating {
                risk: kedge_core::Risk::High,
            },
        );
        let chain = build(
            GuardMode::Audit,
            None,
            None,
            Some(Arc::new(caps)),
            base,
            None,
            TaskId::new(),
            None,
        );
        let obs = chain
            .tools
            .execute(&ToolCall::new("get_weather", serde_json::json!({})))
            .await
            .unwrap();
        assert!(!obs.content.contains("RAN"));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "a declared-mutating tool must be intercepted despite its read-only name"
        );
        assert_eq!(chain.auditor.unwrap().intercepted(), 1);
    }

    #[tokio::test]
    async fn hitl_without_approver_fails_safe_to_deny() {
        let (base, hits) = spy();
        let chain = build(GuardMode::Hitl, None, None, None, base, None, TaskId::new(), None);
        let obs = chain
            .tools
            .execute(&ToolCall::new("deploy", serde_json::json!({})))
            .await
            .unwrap();
        assert!(obs.is_error, "missing approver must deny, not execute");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
    // ── the three layers, composed ──────────────────────────────────────
    //
    // kedge shipped two policy systems with opposite defaults that did not know
    // about each other, and only the weaker one was wired in. These assert the
    // composition, not just that it compiles.

    fn manifest_granting(read: &str) -> Arc<Manifest> {
        let toml = format!(
            "[skill]\nname = \"scoped\"\nversion = \"0.1.0\"\n\
             [capabilities.filesystem]\nread = [\"{read}\"]\n"
        );
        Arc::new(Manifest::from_toml_str(&toml, &std::collections::HashMap::new()).unwrap())
    }

    #[tokio::test]
    async fn a_manifest_refuses_a_read_the_mode_guard_would_have_allowed() {
        // Audit mode intercepts *mutations*. A read outside the task's scope is
        // not a mutation, so before SkillGuard was in the chain nothing stopped
        // it: the mode guard asks "is this mutating?", a manifest asks "was this
        // task ever allowed to touch that?".
        let (base, hits) = spy();
        let chain = build(
            GuardMode::Audit,
            None,
            None,
            None,
            base,
            None,
            TaskId::new(),
            Some((manifest_granting("/repo/src/lib.rs"), std::path::PathBuf::from("/repo"))),
        );
        let obs = chain
            .tools
            .execute(&ToolCall::new(
                "read_file",
                serde_json::json!({"path": "/repo/secrets.env"}),
            ))
            .await
            .unwrap();
        assert!(obs.is_error, "an undeclared read must be refused");
        assert_eq!(hits.load(Ordering::SeqCst), 0, "it must not reach the tools");
    }

    #[tokio::test]
    async fn a_declared_read_still_runs() {
        // The other direction: scoping must not break the task it scopes.
        let (base, hits) = spy();
        let chain = build(
            GuardMode::Audit,
            None,
            None,
            None,
            base,
            None,
            TaskId::new(),
            Some((manifest_granting("/repo/src/lib.rs"), std::path::PathBuf::from("/repo"))),
        );
        let obs = chain
            .tools
            .execute(&ToolCall::new(
                "read_file",
                serde_json::json!({"path": "/repo/src/lib.rs"}),
            ))
            .await
            .unwrap();
        assert!(!obs.is_error, "a declared read must run: {}", obs.content);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_operator_blocklist_still_wins_over_a_permissive_manifest() {
        // PolicyGuard is outermost so a manifest can never widen an operator ban.
        let (base, hits) = spy();
        let policy = Arc::new(
            Policy::from_toml_str("blocked_tools = [\"read_file\"]").unwrap(),
        );
        let chain = build(
            GuardMode::Audit,
            Some(policy),
            None,
            None,
            base,
            None,
            TaskId::new(),
            Some((manifest_granting("/repo/**"), std::path::PathBuf::from("/repo"))),
        );
        let obs = chain
            .tools
            .execute(&ToolCall::new(
                "read_file",
                serde_json::json!({"path": "/repo/src/lib.rs"}),
            ))
            .await
            .unwrap();
        assert!(obs.is_error);
        assert!(
            obs.content.contains("blocked by policy"),
            "the operator ban should be the reason, not the manifest: {}",
            obs.content
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn without_a_manifest_the_run_is_unscoped_which_is_the_weaker_default() {
        // Documents the gap rather than hiding it: passing None means only the
        // mode guard applies, and an out-of-scope read runs.
        let (base, hits) = spy();
        let chain = build(GuardMode::Audit, None, None, None, base, None, TaskId::new(), None);
        let obs = chain
            .tools
            .execute(&ToolCall::new(
                "read_file",
                serde_json::json!({"path": "/anywhere/at/all"}),
            ))
            .await
            .unwrap();
        assert!(!obs.is_error);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
