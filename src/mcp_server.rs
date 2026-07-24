//! `kedge mcp` — an MCP (Model Context Protocol) **server** over stdio.
//!
//! Kedge is normally an MCP *client* (it consumes tools); this flips it around so
//! any MCP client — Claude Code, in particular — can call Kedge's own
//! capabilities as native tools:
//!
//! * `kedge_compact` — AST-aware token compaction of a source file (deterministic)
//! * `kedge_audit`   — forensic cost/security report from a ledger (deterministic)
//! * `kedge_run`     — a bounded, journaled ReAct agent driven by a Groq model
//!
//! Transport is newline-delimited JSON-RPC 2.0 on stdin/stdout, per the MCP stdio
//! spec. **stdout carries the protocol** — so the handlers here call the crates
//! directly and *return* values; they never print. (All logging goes to stderr;
//! see `telemetry`.)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};

use kedge_compact::Compactor;
use kedge_core::{Budget, ReActEngine, Reasoner, Task, ToolExecutor};
use kedge_ledger::Ledger;
use kedge_llm::ChatReasoner;

use crate::{parse_lang, shell_tool_spec, ShellTool};

/// MCP protocol revision we advertise when the client asks for something we
/// don't speak.
const PROTOCOL_VERSION: &str = "2024-11-05";
/// Revisions we're actually compatible with over the `tools/*` surface. We echo
/// the client's version only if it's in here — claiming to speak a revision we
/// haven't implemented is worse than negotiating down.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
/// Groq's OpenAI-compatible endpoint. `kedge_run` is wired to it; the key comes
/// from `GROQ_API_KEY` at call time and is never persisted.
const GROQ_API_BASE: &str = "https://api.groq.com/openai/v1";
const DEFAULT_GROQ_MODEL: &str = "llama-3.3-70b-versatile";

/// The Groq key, read from the environment **once** at startup and then removed
/// from our own process environment. The agent's `shell` tool already gets an
/// allowlist-scrubbed env, but that alone doesn't stop a child from reading the
/// *parent's* environment via `/proc/<ppid>/environ`; removing the key here closes
/// that path. Cached because the MCP server is long-lived and serves many runs.
static GROQ_KEY: OnceLock<Option<String>> = OnceLock::new();

fn groq_key() -> Option<&'static str> {
    GROQ_KEY
        .get_or_init(|| {
            let k = std::env::var("GROQ_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty());
            std::env::remove_var("GROQ_API_KEY");
            k
        })
        .as_deref()
}

/// Negotiate the protocol revision: echo the client's only if we speak it.
fn negotiate_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|s| **s == v)
            .copied()
            .unwrap_or(PROTOCOL_VERSION),
        None => PROTOCOL_VERSION,
    }
}

/// Shared, serialized handle to stdout. Concurrent request tasks all write
/// through this, so two responses can never interleave mid-line.
type SharedOut = Arc<tokio::sync::Mutex<Stdout>>;

/// Serve the MCP protocol on stdio until stdin closes.
///
/// Requests are dispatched onto their own tasks, so a long `kedge_run` (up to a
/// 120s wall-clock budget) can't stall `tools/list`, `ping`, or a concurrent
/// `kedge_compact`. JSON-RPC correlates by `id`, so out-of-order replies are
/// expected and fine.
pub async fn serve_stdio() -> Result<()> {
    // Read + scrub the Groq key from our own env immediately, before any run can
    // spawn a shell child that might read it back out of `/proc/self/environ`.
    let _ = groq_key();
    let mut reader = BufReader::new(tokio::io::stdin()).lines();
    let out: SharedOut = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    // The MCP handshake must precede any tool traffic.
    let initialized = Arc::new(AtomicBool::new(false));
    // Handles for tool calls still running. On EOF we drain these before
    // returning — otherwise the runtime shuts down and cancels them, silently
    // dropping a response for work that had already been done.
    let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    eprintln!(
        "kedge mcp: ready on stdio · tools: kedge_compact, kedge_audit, kedge_run \
         (run needs GROQ_API_KEY)"
    );

    while let Some(line) = reader.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_msg(
                    &out,
                    &json!({
                        "jsonrpc": "2.0", "id": null,
                        "error": { "code": -32700, "message": format!("parse error: {e}") }
                    }),
                )
                .await?;
                continue;
            }
        };

        // A request has an `id`; a notification does not (and gets no reply).
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                let version =
                    negotiate_version(params.get("protocolVersion").and_then(Value::as_str));
                initialized.store(true, Ordering::SeqCst);
                let result = json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "kedge", "version": env!("CARGO_PKG_VERSION") },
                });
                reply(&out, id, Ok(result)).await?;
            }
            "ping" => reply(&out, id, Ok(json!({}))).await?,
            "tools/list" => {
                if !initialized.load(Ordering::SeqCst) {
                    reply(&out, id, Err(not_initialized())).await?;
                } else {
                    reply(&out, id, Ok(json!({ "tools": tool_specs() }))).await?;
                }
            }
            "tools/call" => {
                if !initialized.load(Ordering::SeqCst) {
                    reply(&out, id, Err(not_initialized())).await?;
                    continue;
                }
                // Run off-loop so a slow tool doesn't block the reader. A tool
                // failure is a *successful* JSON-RPC result carrying
                // `isError: true`, per MCP — not a protocol-level error.
                let out = Arc::clone(&out);
                inflight.retain(|h| !h.is_finished()); // keep the list bounded
                inflight.push(tokio::spawn(async move {
                    let result = handle_tool_call(&params).await;
                    if let Err(e) = reply(&out, id, Ok(result)).await {
                        eprintln!("kedge mcp: failed to write response: {e}");
                    }
                }));
            }
            // Notifications and anything we don't implement.
            "notifications/initialized" | "initialized" => {}
            other => {
                if id.is_some() {
                    reply(
                        &out,
                        id,
                        Err((-32601, format!("method not found: {other}"))),
                    )
                    .await?;
                }
            }
        }
    }

    // stdin closed. Let any in-flight tool call finish writing its response
    // rather than having the runtime cancel it out from under us. Each tool
    // carries its own budget, so this wait is bounded by construction.
    for handle in inflight {
        if let Err(e) = handle.await {
            eprintln!("kedge mcp: in-flight tool call did not finish cleanly: {e}");
        }
    }
    Ok(())
}

/// JSON-RPC error for tool traffic that arrives before the handshake.
fn not_initialized() -> (i64, String) {
    (
        -32002,
        "server not initialized: send `initialize` first".to_string(),
    )
}

/// Write one newline-delimited JSON message and flush, holding the stdout lock
/// only for the duration of the write.
async fn write_msg(out: &SharedOut, msg: &Value) -> Result<()> {
    let mut s = serde_json::to_string(msg)?;
    s.push('\n');
    let mut guard = out.lock().await;
    guard.write_all(s.as_bytes()).await?;
    guard.flush().await?;
    Ok(())
}

/// Reply to a request (no-op for notifications, which have no `id`).
async fn reply(
    out: &SharedOut,
    id: Option<Value>,
    result: std::result::Result<Value, (i64, String)>,
) -> Result<()> {
    let Some(id) = id else { return Ok(()) };
    let msg = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    write_msg(out, &msg).await
}

/// The tools advertised to the client via `tools/list`.
fn tool_specs() -> Value {
    json!([
        {
            "name": "kedge_compact",
            "description": "AST-aware token compaction. Parses a source file with Tree-sitter and returns its structural skeleton — signatures and types kept, function bodies elided — so a large file fits a token budget. Deterministic, no LLM. Pass `path` (read from disk, language auto-detected) OR `code` + `lang`. SIDE EFFECT: the token saving is journaled to the SQLite ledger at `db` (default ./kedge.sqlite, created if absent) so kedge_audit can report a cumulative total; set db to redirect it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to a source file; language detected from its extension." },
                    "code": { "type": "string", "description": "Raw source text (use together with `lang` instead of `path`)." },
                    "lang": { "type": "string", "enum": ["rust", "python", "javascript", "typescript", "go"], "description": "Force or declare the language." },
                    "max_tokens": { "type": "integer", "description": "Elide only enough bodies to fit this token budget. Omit for a full outline (all bodies elided)." },
                    "db": { "type": "string", "description": "Ledger to journal the token savings into (default: $KEDGE_LEDGER_PATH, else ./kedge.sqlite). Point KEDGE_LEDGER_PATH at one absolute file to accumulate lifetime totals across every project." }
                }
            }
        },
        {
            "name": "kedge_expand",
            "description": "The inverse of kedge_compact: return the full source of the named function(s) a skeleton elided, bodies included. Use this after orienting on a compacted skeleton when you need to EDIT a specific function whose body you never saw — fetch just that body instead of re-reading the whole file. The skeleton shows each name above its `/* … elided */` marker, so you already know what to ask for. Deterministic, no LLM.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "The function or method name to expand (as shown in the skeleton signature)." },
                    "path": { "type": "string", "description": "Path to the source file; language detected from its extension." },
                    "code": { "type": "string", "description": "Raw source text (use together with `lang` instead of `path`)." },
                    "lang": { "type": "string", "enum": ["rust", "python", "javascript", "typescript", "go"], "description": "Force or declare the language." }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "kedge_audit",
            "description": "Forensic report from a Kedge SQLite ledger: total runs, tokens consumed, intercepted mutations (Shadow-Guard dry-runs), and — when pricing is supplied — a cost projection. Deterministic, no LLM.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "db": { "type": "string", "description": "Ledger path (default: $KEDGE_LEDGER_PATH, else ./kedge.sqlite)." },
                    "price_per_1k": { "type": "number", "description": "USD per 1k tokens, to compute a cost figure." },
                    "runs_per_day": { "type": "integer", "description": "Expected runs/day, for a projection (needs price_per_1k)." }
                }
            }
        },
        {
            "name": "kedge_run",
            "description": "Run a bounded, fully-journaled ReAct agent on a natural-language goal, driven by a Groq model. Enforces hard budgets (steps / tokens / wall-clock) and records every step to a SQLite ledger you can later replay or audit. Returns the final answer plus the full trajectory. The agent gets a `shell` tool scoped to `cwd`. Requires GROQ_API_KEY in the environment. SAFETY: defaults to mode=\"audit\" (Shadow-Guard dry-run) — mutating tool calls are intercepted and journaled, never executed. Pass mode=\"live\" only when the caller genuinely intends real side effects.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": { "type": "string", "description": "What the agent should accomplish." },
                    "mode": { "type": "string", "enum": ["audit", "deny", "live"], "description": "Safety posture. audit (DEFAULT) = Shadow-Guard dry-run: read-only tools run for real, mutating tools are intercepted and journaled but NOT executed. deny = mutating tools are refused outright. live = execute everything, no guard (explicit opt-in — the agent has an arbitrary shell)." },
                    "model": { "type": "string", "description": "Groq model id (default: llama-3.3-70b-versatile)." },
                    "cwd": { "type": "string", "description": "Working directory for the shell tool (default: '.')." },
                    "max_steps": { "type": "integer", "description": "Max ReAct steps (default: 12)." },
                    "max_tokens": { "type": "integer", "description": "Max cumulative tokens (default: 100000)." },
                    "max_secs": { "type": "integer", "description": "Wall-clock budget in seconds (default: 120)." },
                    "db": { "type": "string", "description": "Ledger path (default: $KEDGE_LEDGER_PATH, else ./kedge.sqlite)." }
                },
                "required": ["goal"]
            }
        }
    ])
}

/// Dispatch a `tools/call`, wrapping the outcome as an MCP `CallToolResult`.
async fn handle_tool_call(params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let outcome: Result<String> = match name {
        "kedge_compact" => tool_compact(&args),
        "kedge_expand" => tool_expand(&args),
        "kedge_audit" => tool_audit(&args),
        "kedge_run" => tool_run(&args).await,
        other => Err(anyhow::anyhow!("unknown tool `{other}`")),
    };

    match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
        Err(e) => {
            json!({ "content": [{ "type": "text", "text": format!("error: {e:#}") }], "isError": true })
        }
    }
}

/// Cap on a file the compaction tools will slurp into memory. Prevents a hostile
/// `path` (`/dev/zero`, a multi-GB file) from OOM-killing the server.
const MAX_COMPACT_FILE_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB

/// The directory MCP file operations (`path`, `db`) are confined to. Set via
/// `$KEDGE_MCP_ROOT`, else the server's current working directory. Canonicalized
/// once at first use, so an operator who launches the server in a project root
/// scopes every file the model can read/write to that tree.
static MCP_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn mcp_root() -> &'static Path {
    MCP_ROOT.get_or_init(|| {
        let raw = std::env::var_os("KEDGE_MCP_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        std::fs::canonicalize(&raw).unwrap_or(raw)
    })
}

/// Resolve a caller-supplied path *for reading*, confined under `root`. Rejects
/// traversal (`..`), symlink escapes, and absolute paths outside the root. The
/// file must exist (so `canonicalize` fully resolves it).
fn resolve_read_under(root: &Path, p: &str) -> Result<PathBuf> {
    let c = Path::new(p);
    let target = if c.is_absolute() {
        c.to_path_buf()
    } else {
        root.join(c)
    };
    let canon = std::fs::canonicalize(&target).with_context(|| format!("resolving path `{p}`"))?;
    if !canon.starts_with(root) {
        anyhow::bail!(
            "`{p}` resolves outside the allowed root {} — set KEDGE_MCP_ROOT to widen access",
            root.display()
        );
    }
    Ok(canon)
}

/// Resolve a caller-supplied path *for writing* (e.g. a ledger db), confined under
/// `root`. The parent directory must exist and be inside the root; the file itself
/// need not exist yet.
fn resolve_write_under(root: &Path, p: &str) -> Result<PathBuf> {
    let c = Path::new(p);
    let target = if c.is_absolute() {
        c.to_path_buf()
    } else {
        root.join(c)
    };
    let parent = target
        .parent()
        .filter(|pp| !pp.as_os_str().is_empty())
        .unwrap_or(root);
    let canon_parent =
        std::fs::canonicalize(parent).with_context(|| format!("resolving parent of `{p}`"))?;
    if !canon_parent.starts_with(root) {
        anyhow::bail!("`{p}` is outside the allowed root {}", root.display());
    }
    let name = target.file_name().context("path has no filename")?;
    Ok(canon_parent.join(name))
}

/// Resolve a ledger `db` argument: a caller-supplied path is confined under
/// [`mcp_root`]; absent, the operator's default (`resolve_ledger_path`, which
/// honors `$KEDGE_LEDGER_PATH`) is trusted.
fn resolve_db(args: &Value, for_read: bool) -> Result<PathBuf> {
    match args.get("db").and_then(Value::as_str) {
        Some(p) if for_read => resolve_read_under(mcp_root(), p),
        Some(p) => resolve_write_under(mcp_root(), p),
        None => Ok(kedge_ledger::resolve_ledger_path(None)),
    }
}

/// Resolve the `path` / `code`+`lang` argument pair (shared by compact + expand)
/// into the source text and a `Compactor` for the right language.
fn source_and_compactor(args: &Value) -> Result<(String, Compactor)> {
    let lang = args.get("lang").and_then(Value::as_str);
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        // Confine the read to the allowed root and cap its size before slurping.
        let path = resolve_read_under(mcp_root(), p)?;
        let len = std::fs::metadata(&path)
            .with_context(|| format!("stat `{p}`"))?
            .len();
        if len > MAX_COMPACT_FILE_BYTES {
            anyhow::bail!(
                "`{p}` is {len} bytes; the compaction file limit is {MAX_COMPACT_FILE_BYTES} bytes"
            );
        }
        let src = std::fs::read_to_string(&path).with_context(|| format!("reading {p}"))?;
        let compactor = match lang {
            Some(l) => Compactor::new(parse_lang(l)?)?,
            None => Compactor::for_path(&path)?,
        };
        Ok((src, compactor))
    } else if let Some(code) = args.get("code").and_then(Value::as_str) {
        let l = lang.context("`lang` is required when passing `code`")?;
        Ok((code.to_string(), Compactor::new(parse_lang(l)?)?))
    } else {
        anyhow::bail!("provide `path`, or `code` together with `lang`")
    }
}

/// Bring back the full source of the named function(s) a skeleton elided — the
/// "act on it" half of compaction. After orienting on a `kedge_compact`
/// skeleton, call this to fetch just the body you need to edit.
fn tool_expand(args: &Value) -> Result<String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .context("`symbol` is required — the function/method name to expand")?;
    let (source, mut compactor) = source_and_compactor(args)?;
    let matches = compactor.expand(&source, symbol)?;
    let out = json!({
        "symbol": symbol,
        "match_count": matches.len(),
        "matches": matches,
        "note": if matches.is_empty() {
            "no function/method by that name — read the file directly"
        } else {
            "full source of each match, bodies included; safe to edit from these"
        },
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

fn tool_compact(args: &Value) -> Result<String> {
    let (source, mut compactor) = source_and_compactor(args)?;

    let result = match args.get("max_tokens").and_then(Value::as_u64) {
        Some(max) => compactor.compact_to_budget(&source, max as usize)?,
        None => compactor.outline(&source)?,
    };

    // Surface the savings explicitly so callers never have to do the subtraction.
    let saved = result
        .original_tokens
        .saturating_sub(result.compacted_tokens);
    let pct = result.savings() * 100.0;
    let summary = format!(
        "Saved {saved} tokens ({pct:.0}%): {} → {} tokens, {} bodies elided",
        result.original_tokens, result.compacted_tokens, result.elided_bodies
    );
    // Best-effort: journal this saving so `kedge_audit` can report a cumulative
    // "tokens saved" total. A ledger problem never fails the compaction itself.
    let db = resolve_db(args, false)?;
    let cumulative = match Ledger::open(&db) {
        Ok(ledger) => {
            let label = args.get("path").and_then(Value::as_str);
            if let Err(e) = ledger.record_compaction(
                result.original_tokens as u64,
                result.compacted_tokens as u64,
                label,
            ) {
                eprintln!("kedge mcp: compaction not journaled ({e})");
            }
            ledger.compaction_totals().ok()
        }
        Err(e) => {
            eprintln!("kedge mcp: compaction ledger unavailable ({e})");
            None
        }
    };

    let out = json!({
        "tokens_saved": saved,
        "percent_saved": (pct * 10.0).round() / 10.0,
        "original_tokens": result.original_tokens,
        "compacted_tokens": result.compacted_tokens,
        "elided_bodies": result.elided_bodies,
        "summary": summary,
        "cumulative": cumulative.map(|t| json!({
            "compactions": t.compactions,
            "tokens_saved": t.tokens_saved,
            "note": "running total in this ledger — see kedge_audit for the full report",
        })),
        "text": result.text,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

fn tool_audit(args: &Value) -> Result<String> {
    let db = resolve_db(args, true)?;
    let price = args.get("price_per_1k").and_then(Value::as_f64);
    let runs = args.get("runs_per_day").and_then(Value::as_u64);
    let report = kedge_audit::AuditReport::from_ledger(&db, price, runs)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(report.to_json())
}

/// The safety posture `kedge_run` executes under. Parsed from the `mode`
/// argument; anything unrecognized is rejected rather than silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Shadow-Guard dry-run: read-only tools run for real, mutating tools are
    /// intercepted and journaled but never executed. The default.
    Audit,
    /// Read-only lockdown: mutating tools are refused outright.
    Deny,
    /// No guard — the agent gets an unrestricted shell. Explicit opt-in only.
    Live,
}

impl RunMode {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "audit" => Ok(RunMode::Audit),
            "deny" => Ok(RunMode::Deny),
            "live" => Ok(RunMode::Live),
            other => anyhow::bail!(
                "unknown mode `{other}` — expected \"audit\" (default), \"deny\", or \"live\""
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            RunMode::Audit => "audit",
            RunMode::Deny => "deny",
            RunMode::Live => "live",
        }
    }
}

/// Wrap `base` in the guard implied by `mode`. Returns the executor to hand the
/// engine, plus the `AuditExecutor` handle when one is in play (so the caller can
/// report how many mutations were intercepted).
///
/// `CliApprover` is deliberately never used here: it prints to stdout and reads
/// stdin, which are this process's JSON-RPC channel.
impl RunMode {
    fn as_guard_mode(self) -> crate::guard::GuardMode {
        match self {
            RunMode::Audit => crate::guard::GuardMode::Audit,
            RunMode::Deny => crate::guard::GuardMode::Deny,
            RunMode::Live => crate::guard::GuardMode::Live,
        }
    }
}

/// Thin wrapper over the canonical [`crate::guard::build`] (no policy here — the
/// MCP path loads policy at the call site). Kept so the mode-behavior tests read
/// against the same chain the real run uses.
#[cfg(test)]
fn build_guarded_tools(
    mode: RunMode,
    base: Arc<dyn ToolExecutor>,
    ledger: Option<Arc<Ledger>>,
    run_id: kedge_core::TaskId,
) -> (
    Arc<dyn ToolExecutor>,
    Option<Arc<kedge_audit::AuditExecutor>>,
) {
    let chain = crate::guard::build(mode.as_guard_mode(), None, None, None, base, ledger, run_id);
    (chain.tools, chain.auditor)
}

async fn tool_run(args: &Value) -> Result<String> {
    let goal = args
        .get("goal")
        .and_then(Value::as_str)
        .context("`goal` is required")?;
    // Validate arguments before touching the environment, so a bad `mode` is
    // reported as such instead of being masked by a missing API key.
    let mode = RunMode::parse(args.get("mode").and_then(Value::as_str).unwrap_or("audit"))?;
    let key = groq_key()
        .context(
            "GROQ_API_KEY is not set in this process's environment — \
             the run tool needs it to reach Groq. Set it in the MCP server's env.",
        )?
        .to_string();

    let model = args
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_GROQ_MODEL)
        .to_string();
    let cwd = PathBuf::from(args.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let max_tokens = args
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(100_000);
    let max_steps = args.get("max_steps").and_then(Value::as_u64).unwrap_or(12);
    let max_secs = args.get("max_secs").and_then(Value::as_u64).unwrap_or(120);
    let db = resolve_db(args, false)?;

    let budget = Budget {
        max_tokens,
        max_steps,
        wall_clock: Duration::from_secs(max_secs),
    }
    .tracker();

    let ledger = Ledger::open(&db).context("opening ledger")?;
    let task = Task::new(goal).in_workspace(cwd.display().to_string());
    ledger.begin_run(&task)?;

    // ── Safety layering (the caller here is an LLM, so default to safe) ──
    //
    // The agent's `shell` tool executes arbitrary programs. Over MCP there is no
    // human in the loop by default, so a bare ShellTool would hand an unguarded
    // shell to whatever asked. Modes mirror the CLI's `--audit` / `--hitl`:
    //
    //   audit (default) — Shadow-Guard dry-run: read-only tools execute for real,
    //                     every mutating tool is intercepted and journaled but
    //                     NOT executed.
    //   deny            — read-only lockdown: mutating tools are refused outright
    //                     (the agent observes the denial and can adapt).
    //   live            — no guard. Explicit opt-in.
    //
    // NOTE: `CliApprover` is deliberately unavailable here — it prints to stdout
    // and reads stdin, which are this process's JSON-RPC channel. Interactive
    // approval belongs on the `WebhookApprover` + `kedge serve` path.
    let base: Arc<dyn ToolExecutor> = Arc::new(ShellTool {
        cwd: cwd.clone(),
        timeout: Duration::from_secs(30),
    });
    // Same canonical chain as the CLI, and now with policy. Load policy from the
    // MCP SERVER's own working directory (operator-controlled) — never from the
    // run's `cwd` argument, which the caller/model controls and could point at an
    // untrusted repo that ships a hostile kedge-policy.toml.
    let policy_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let policy = crate::guard::load_policy(None, &policy_dir)?;
    let chain = crate::guard::build(
        mode.as_guard_mode(),
        policy,
        None,
        None, // built-in shell tool only; name classification suffices
        base,
        Some(Arc::new(ledger.clone())),
        task.id,
    );
    let (tools, auditor) = (chain.tools, chain.auditor);

    let reasoner: Arc<dyn Reasoner> = Arc::new(ChatReasoner::new(
        GROQ_API_BASE.to_string(),
        model.clone(),
        Some(key),
        vec![shell_tool_spec()],
    ));

    let engine = ReActEngine::new(reasoner, tools, budget.clone()).with_observer(ledger.observer());
    let (outcome, trajectory) = engine.run(&task).await;
    ledger.finalize(task.id, &outcome)?;

    // Report the safety posture alongside the result: a caller must never be able
    // to mistake an intercepted dry-run for work that actually happened.
    let intercepted = auditor.as_ref().map(|a| a.intercepted());
    let note = match (mode, intercepted) {
        (RunMode::Audit, Some(n)) if n > 0 => Some(format!(
            "shadow-audit: {n} mutating tool call(s) were INTERCEPTED and never executed. \
             Nothing was written. Re-run with mode=\"live\" to execute for real."
        )),
        (RunMode::Audit, _) => Some(
            "shadow-audit: no mutating tool calls were attempted; read-only work ran for real."
                .to_string(),
        ),
        (RunMode::Deny, _) => {
            Some("read-only lockdown: mutating tool calls were refused.".to_string())
        }
        (RunMode::Live, _) => None,
    };

    let out = json!({
        "task_id": task.id.to_string(),
        "model": model,
        "mode": mode.as_str(),
        "intercepted_mutations": intercepted,
        "note": note,
        "outcome": outcome,
        "steps": trajectory.len(),
        "tokens_used": budget.tokens_used(),
        "trajectory": trajectory,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kedge_core::{Observation, TaskId, ToolCall};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn confined_read_rejects_escapes_and_allows_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("ok.rs"), "fn a() {}").unwrap();
        // A file inside the root resolves.
        assert!(resolve_read_under(&root, "ok.rs").is_ok());
        // Arbitrary-file-read (the H-A exfil primitive) is blocked:
        assert!(resolve_read_under(&root, "/etc/passwd").is_err());
        assert!(resolve_read_under(&root, "../escape.rs").is_err());
        // A nonexistent path inside the root can't be canonicalized → error.
        assert!(resolve_read_under(&root, "missing.rs").is_err());
    }

    #[test]
    fn confined_write_allows_new_file_inside_but_not_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // A new (not-yet-existing) file whose parent is the root is allowed.
        assert!(resolve_write_under(&root, "run.sqlite").is_ok());
        // …but not one that escapes the root.
        assert!(resolve_write_under(&root, "/tmp/evil.sqlite").is_err());
        assert!(resolve_write_under(&root, "../evil.sqlite").is_err());
    }

    /// Stands in for the real `ShellTool` and records whether it actually ran.
    struct SpyTool(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl ToolExecutor for SpyTool {
        async fn execute(&self, _call: &ToolCall) -> kedge_core::Result<Observation> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Observation::ok("the underlying tool really executed"))
        }
    }

    fn spy() -> (Arc<dyn ToolExecutor>, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        (Arc::new(SpyTool(hits.clone())), hits)
    }

    fn call(name: &str) -> ToolCall {
        ToolCall::new(name, json!({ "cmd": "rm", "args": ["-rf", "/"] }))
    }

    #[test]
    fn unknown_modes_are_rejected_never_silently_downgraded() {
        assert_eq!(RunMode::parse("audit").unwrap(), RunMode::Audit);
        assert_eq!(RunMode::parse("deny").unwrap(), RunMode::Deny);
        assert_eq!(RunMode::parse("live").unwrap(), RunMode::Live);
        // A typo must be an error, not a silent fallback to something permissive.
        assert!(RunMode::parse("Live").is_err());
        assert!(RunMode::parse("").is_err());
        assert!(RunMode::parse("yolo").is_err());
    }

    #[tokio::test]
    async fn audit_mode_intercepts_mutating_tools_without_executing_them() {
        let (base, hits) = spy();
        let (tools, auditor) = build_guarded_tools(RunMode::Audit, base, None, TaskId::new());
        let _ = tools.execute(&call("shell")).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "audit mode MUST NOT execute a mutating tool"
        );
        assert_eq!(
            auditor
                .expect("audit mode exposes the auditor")
                .intercepted(),
            1
        );
    }

    #[tokio::test]
    async fn audit_mode_still_executes_read_only_tools() {
        let (base, hits) = spy();
        let (tools, _) = build_guarded_tools(RunMode::Audit, base, None, TaskId::new());
        let _ = tools.execute(&call("read_file")).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "read-only tools should still run for real in audit mode"
        );
    }

    #[tokio::test]
    async fn deny_mode_refuses_mutating_tools() {
        let (base, hits) = spy();
        let (tools, auditor) = build_guarded_tools(RunMode::Deny, base, None, TaskId::new());
        let _ = tools.execute(&call("shell")).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0, "deny mode MUST NOT execute");
        assert!(auditor.is_none());
    }

    #[tokio::test]
    async fn live_mode_executes_for_real() {
        let (base, hits) = spy();
        let (tools, auditor) = build_guarded_tools(RunMode::Live, base, None, TaskId::new());
        let _ = tools.execute(&call("shell")).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "live mode is the opt-in escape hatch"
        );
        assert!(auditor.is_none());
    }

    #[test]
    fn run_tool_schema_advertises_the_safety_modes() {
        let specs = tool_specs();
        let run = specs
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "kedge_run")
            .expect("kedge_run is advertised");
        assert_eq!(
            run["inputSchema"]["properties"]["mode"]["enum"],
            json!(["audit", "deny", "live"])
        );
        assert!(run["description"].as_str().unwrap().contains("SAFETY"));
    }

    #[test]
    fn every_advertised_tool_has_a_name_and_schema() {
        for t in tool_specs().as_array().unwrap() {
            assert!(t["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["description"].as_str().is_some_and(|d| d.len() > 40));
        }
    }
    #[test]
    fn protocol_version_is_negotiated_not_blindly_echoed() {
        // A revision we speak is echoed back...
        assert_eq!(negotiate_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate_version(Some("2024-11-05")), "2024-11-05");
        // ...one we don't is negotiated down rather than falsely claimed.
        assert_eq!(negotiate_version(Some("1999-01-01")), PROTOCOL_VERSION);
        assert_eq!(negotiate_version(Some("")), PROTOCOL_VERSION);
        assert_eq!(negotiate_version(None), PROTOCOL_VERSION);
    }

    #[test]
    fn compact_tool_declares_its_ledger_side_effect() {
        let specs = tool_specs();
        let c = specs
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "kedge_compact")
            .unwrap();
        let desc = c["description"].as_str().unwrap();
        assert!(
            desc.contains("SIDE EFFECT"),
            "the ledger write must be declared"
        );
    }
}
