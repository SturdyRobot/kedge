# Security Policy

Kedge executes tool calls on behalf of language models. That makes its security
posture part of the product, not an afterthought — so this file describes both
how to report a problem and what the tool actually guarantees.

## Reporting a vulnerability

Email **noeljacksonjs@gmail.com** with `[kedge security]` in the subject, or open
a [private security advisory](https://github.com/nlj3/kedge/security/advisories/new).

Please **do not open a public issue** for anything exploitable.

Include what you need to make it reproducible: the version or commit, the
platform, and the smallest sequence of steps that triggers it. If it involves a
tool call being executed when it should have been intercepted, the run's task id
is especially useful — `kedge replay <id>` reconstructs the exact trajectory
from the ledger.

Expect an initial response within a few days. This is a solo-maintained project,
so please be patient; I'd rather reply properly than quickly.

## Threat model

Kedge assumes the **model is untrusted** and the **operator is trusted**. Its job
is to keep a model's chosen actions inside bounds the operator set.

**What Kedge is designed to do**

- Enforce token, step, and wall-clock budgets as ceilings, checked *before* work
  happens rather than after
- Intercept mutating tool calls in `audit` (Shadow-Guard) mode so an intended
  side effect is journaled without being executed. **Safe by default:** both the
  CLI (`kedge run`) and the MCP `kedge_run` tool default to `audit` — an unguarded
  shell requires an explicit `--live` / `mode="live"` opt-in
- Fail *safe* and *deny-wins* when classifying tools: the classifier scans every
  token in a tool's name, so a compound like `get_and_delete` or `list_then_wipe`
  is caught as mutating even though it starts with a read verb. Anything not
  clearly read-only is treated as mutating
- Scrub the environment of any child process a tool spawns: only an allowlist of
  build-relevant, non-secret vars is inherited, so a model-driven `shell` can't
  read the harness's API keys (`$OPENAI_API_KEY`, …) out of its own environment.
  The harness also **removes its own LLM API key from its process environment**
  after reading it, so a child can't recover it via `/proc/<ppid>/environ` either
- Enforce `kedge-policy.toml` (`blocked_tools`, `pii_redaction`) on both the CLI
  and MCP run paths
- Journal every step to SQLite so any run can be replayed and audited after the
  fact — and **fail the run loudly** rather than proceed if a security-relevant
  event (an interception or an approval decision) can't be journaled

**What Kedge does not claim**

- It is **not a sandbox.** In `live` mode the agent's `shell` tool executes
  arbitrary programs with the privileges of the process that launched it (though
  their environment is scrubbed of secrets by default). If you need containment,
  run Kedge inside a **container or VM** — the guards are policy, not isolation.
  (`kedge-probe` is an **experimental, observe-only eBPF prototype**: it does not
  enforce, is not wired into a normal run, and must not be relied on for
  containment. See `crates/kedge-probe`.)
- Tool classification is **name-based**, augmented by declared capabilities. It
  scans every token and fails safe, and it honors an MCP tool's `readOnlyHint` /
  `destructiveHint` annotations — but only ever to make a tool *more* restricted,
  never less, so a hostile server can't relabel a destructive tool read-only. It
  still can't see arguments: a generic `fetch`/`request` tool that mutates via a
  `method` argument classifies read-only unless its server declares otherwise.
- It does not defend against a malicious *operator*, a compromised model
  endpoint, or a hostile MCP server you deliberately connected.

## Operational notes

- **API keys are read from the environment at call time and never written to the
  ledger, logs, or trajectories.** Pass a variable name (`--api-key-env`), never
  the key itself on a command line, where it would land in shell history.
- The `mcp` server speaks JSON-RPC on **stdout**; all logging goes to stderr. Do
  not add printing to stdout in that path.
- `kedge_run` over MCP defaults to `mode="audit"`. Passing `mode="live"` gives
  the model an unguarded shell — an explicit, deliberate opt-in.
- Ledgers contain full prompts, tool arguments, and outputs. Treat
  `kedge.sqlite` (and any `KEDGE_LEDGER_PATH` you set) as sensitive.
- The `kedge serve` control API (which can resolve human-in-the-loop approvals and
  read full trajectories) is **loopback-only by default**. Binding a non-loopback
  address requires a bearer token in `$KEDGE_SERVE_TOKEN`; without one, `serve`
  refuses to start. All endpoints except `/health` require the token when set.

## Supported versions

Pre-1.0. Fixes land on `main`; there are no backported security branches yet.
