# Kedge

[![CI](https://github.com/nlj3/kedge/actions/workflows/ci.yml/badge.svg)](https://github.com/nlj3/kedge/actions/workflows/ci.yml)
[![License: BUSL-1.1](https://img.shields.io/badge/license-BUSL--1.1-orange.svg)](LICENSE)
[![Try it live](https://img.shields.io/badge/demo-run%20it%20in%20your%20browser-5ac8fa)](https://nlj.dev)

> **Run the engine in your browser** → **[nlj.dev](https://nlj.dev)** (open the Kedge icon)
> The real ReAct engine compiled to WebAssembly: deterministic Think → Act → Observe,
> executing entirely client-side. No server, no API key, no network.

A **deterministic, dry-run-first execution substrate for AI agents**, written in Rust.

Kedge sits between an LLM (a frontier API or local Ollama) and a real system, and
gives you the two things that make an autonomous agent trustworthy:

- **Preview, before.** In `--audit` mode, every *mutating* tool call the agent
  makes is **intercepted and journaled instead of executed**, so you see exactly
  what the agent *would* do to your files, APIs, and data before anything happens.
  Read-only tools still run, so the agent reasons on real data.
- **Audit & replay, after.** Every step is journaled to a **replayable SQLite
  ledger**, so you can reconstruct and re-run exactly what happened.

All of it runs under **hard, enforceable budgets** (tokens, steps, wall-clock) in
**one memory-safe static binary**: no Python runtime, no GC pauses, no GIL.

The design goal is *control and accountability*: preview before, audit after, and
never run away. **Kedge is policy, not a sandbox**: it keeps an agent's chosen
actions inside the bounds you set; for hard OS-level containment, run it inside a
container or VM. See [SECURITY.md](SECURITY.md) for the exact threat model.

```
$ kedge run "assess the toolchain"
▶ run 4b7ef49c-…
  goal: assess the toolchain

  [0] 🧠 Establish the toolchain before touching the project.
      → shell {"args":["--version"],"cmd":"cargo"}
      ← cargo 1.95.0 (…)
  [1] 🧠 Confirm the compiler is present too.
      → shell {"args":["--version"],"cmd":"rustc"}
      ← rustc 1.95.0 (…)
  [2] 🧠 Toolchain verified; nothing else to do for this demo goal.
      ⏹ finish: Toolchain verified.

✔ finished: Toolchain verified.
  3 steps · 54 tokens
  replay with: kedge replay 4b7ef49c-… --db kedge.sqlite
```

(That's the offline demo policy. Add `--model` to drive a real LLM; see below.)

## What it does

The two features that carry the product (the **trust arc**):

- **Shadow-Guard: preview mutations before they happen** (`kedge run --audit`, the
  default). Read-only tools execute for real so the agent reasons on real data, but
  every tool classified as *mutating* is **intercepted and journaled instead of
  run**: nothing is written, sent, or called. You get the agent's intended side
  effects before you ever let it act; `kedge audit` then prints a security scorecard
  + a measured token/cost report. *(Classification is fail-safe: anything not
  clearly read-only is treated as mutating.)*
- **Deterministic replay ledger**: every step is journaled to an append-only
  SQLite ledger; `kedge replay <id>` reconstructs the exact trajectory, so a run is
  re-runnable and auditable after the fact.

Enforced around them:

- **Hard budgets**: token, step, and wall-clock ceilings, checked *before* work
  happens; an agent physically can't run away.
- **Three guard modes, one boundary**: dry-run (`--audit`), human-approve
  (`--hitl`: journaled `y/N`, or remote via a webhook + the HTTP API), or block
  outright (`--deny` / `kedge-policy` blocked tools + PII redaction). **Safe by
  default:** `kedge run` shadow-audits unless you opt into `--live`.
- **Crash recovery** (`kedge resume`): continue an interrupted run from its last
  journaled step, through the same guard chain, without re-executing already-
  performed actions.
- **MCP client + server** (`kedge-mcp`): consume external tools over JSON-RPC 2.0
  (stdio + streamable HTTP), and expose Kedge's own tools to any MCP host.
- **Bounded subagent mesh** (`kedge-mesh`): spawn subagents in isolated Tokio
  tasks under hard budgets; a panic/timeout is contained, never touching the parent.
- **HTTP control API** (`kedge serve`): inspect runs and resolve HITL approvals
  remotely (bearer-token auth required off loopback), so a dashboard/Slack/web UI
  can drive Kedge instead of a terminal.

Bundled utilities (handy, not the headline; dedicated tools go deeper in each lane):

- **AST-aware compaction** (`kedge compact`) + a content-hashed cache: shrink a
  source file to a signatures-only skeleton to fit a token budget, and `kedge
  expand` a specific body back on demand.
- **Verification loop** (`kedge verify` / `kedge-exec`): run cargo/go/npm/pytest in
  an isolated subprocess (clean process-group teardown) and parse compiler
  diagnostics into a structured pass/fail the agent can react to.
- **Regression gate** (`kedge eval`): diff a run against a baseline ledger
  (step/tool/token/drift metrics), emit JUnit + a CI exit code; local and
  deterministic, no LLM-judge spend.
- **OpenTelemetry** (`--features otel`), **Python bindings** (`kedge-bridge` →
  `pip install kedge-rt`), and an **experimental, observe-only eBPF/LSM prototype**
  (`kedge-probe`, Linux: it logs, it does *not* enforce; see the crate docs).

## Install

**Prerequisites:** a recent stable [Rust toolchain](https://rustup.rs) and a C
compiler (`cc`/`clang` on macOS/Linux, MSVC on Windows). The Tree-sitter
grammar and bundled SQLite build a little native code. No network is required at
runtime.

Install the `kedge` binary straight from GitHub:

```sh
cargo install --git https://github.com/nlj3/kedge
```

Or clone and build from source:

```sh
git clone https://github.com/nlj3/kedge
cd kedge
cargo install --path .        # installs `kedge` into ~/.cargo/bin
# or just: cargo build --release   → target/release/kedge
```

Then:

```sh
kedge --help
kedge run "assess the toolchain"
```

## Architecture

A Cargo workspace with strict separation of concerns. `kedge-core` is the
dependency root (pure, no I/O); every satellite crate depends on it and converts
its own errors into the core taxonomy at the boundary.

```mermaid
flowchart TD
    CLI["<b>kedge</b> (bin)<br/>clap CLI"]
    Core["<b>kedge-core</b><br/>domain · ReAct engine<br/>budgets · error taxonomy"]
    Compact["<b>kedge-compact</b><br/>Tree-sitter compaction"]
    MCP["<b>kedge-mcp</b><br/>JSON-RPC 2.0 client"]
    Exec["<b>kedge-exec</b><br/>subprocess runner + verify"]
    Ledger["<b>kedge-ledger</b><br/>SQLite journal + replay"]
    LLM["<b>kedge-llm</b><br/>OpenAI-compatible reasoner"]

    CLI --> Compact & MCP & Exec & Ledger & LLM & Core
    Compact --> Core
    MCP --> Core
    Exec --> Core
    Ledger --> Core
    LLM --> Core

    classDef root fill:#1f6feb,stroke:#0b3d91,color:#fff;
    classDef sat fill:#0d1117,stroke:#30363d,color:#c9d1d9;
    class Core root;
    class Compact,MCP,Exec,Ledger,LLM,CLI sat;
```

| Crate | Responsibility |
|-------|----------------|
| **kedge-core** | Domain model, the ReAct engine + validated state machine, hard budget enforcement (atomic + wall-clock), the shared error type. Pure and heavily tested. |
| **kedge-compact** | Tree-sitter token compactor for **Rust, Python, JS, TS, Go**. Keeps code *skeletons* (signatures, doc comments) and elides function bodies to fit a context budget. |
| **kedge-mcp** | A native async **JSON-RPC 2.0** client for MCP over newline-delimited stdio, with concurrent request de-multiplexing. Speaks `initialize` / `tools/list` / `tools/call`. |
| **kedge-exec** | Tokio subprocess runner. Each child leads its own **process group**, so a timeout reaps the whole subtree (`killpg`). Auto-detects and runs the project's verifier (**cargo/go/npm/pytest**) with a cargo **diagnostic interceptor**. |
| **kedge-ledger** | Append-only **SQLite** journal. Records each step live via the engine's observer hook and reconstructs a run's trajectory (`replay`). |
| **kedge-llm** | A `Reasoner` over any **OpenAI-compatible** chat endpoint (OpenAI/Ollama/vLLM/LM Studio) that emits ReAct JSON parsed straight into the engine's `Action` type. |
| **kedge-mesh** | Bounded subagent supervision: spawn children in isolated Tokio tasks under hard token/step/wall-clock bounds; panics/timeouts/runaways are contained and journaled, never touching the parent. |
| **kedge-eval** | Event-sourced regression harness: profiles baseline vs candidate ledgers (step/tool/token/drift metrics), emits JUnit XML + CI exit codes. |
| **kedge-cache** | Content-hashed (`sha256`) cache of deterministic AST compaction, never LLM responses. Unchanged file → cached skeleton, no re-parse. |
| **kedge-policy** | Lightweight user-space guardrails from `kedge-policy.toml` (blocked tools, PII redaction, per-run budgets): a native matcher, not OPA/Rego. |
| **kedge-audit** | Shadow-Guard dry-run interceptor + forensic report (intercepted mutations, measured token/cost). |
| **kedge-probe** | **Experimental, observe-only** eBPF/LSM prototype (Linux): it logs, it does *not* enforce, and it isn't wired into a normal run; portable no-op fallback elsewhere. The eBPF object is a separate, workspace-excluded crate. |
| **kedge** (bin) | `clap` CLI wiring it all together. |

### The ReAct engine

The heart is a strict `Think → Act → Observe` cycle. An explicit `StateMachine`
rejects any transition outside the cycle, and the shared `BudgetTracker` is
**charged before any expensive work is done**, so exhaustion is detected
deterministically, and the engine always returns a full trajectory even when it
stops early.

```mermaid
stateDiagram-v2
    [*] --> Think
    Think --> Act: Decision (charged to budget first)
    Act --> Observe: run tool (MCP or built-in shell)
    Observe --> Think: append step to trajectory + journal
    Think --> [*]: finish
    Observe --> [*]: budget exhausted (tokens / steps / wall-clock)
```

Plugging in a real model is just implementing one trait:

```rust
#[async_trait]
pub trait Reasoner: Send + Sync {
    async fn next_action(&self, task: &Task, trajectory: &Trajectory) -> Result<Decision>;
}
```

`kedge-llm` ships a `ChatReasoner` implementing this against any
OpenAI-compatible endpoint. With no `--model`, the CLI uses a deterministic demo
policy so the whole pipeline runs offline.

## Driving a real model

`--model` points the agent at any OpenAI-compatible chat endpoint (**Ollama,
OpenAI, vLLM, LM Studio, llama.cpp**):

```sh
# Local Ollama (default base URL), built-in shell tool:
kedge run "what version of cargo is installed?" --model llama3.1

# OpenAI (key read from an env var, never the command line):
OPENAI_API_KEY=sk-... \
  kedge run "summarize the crate layout" \
  --model gpt-4o-mini --api-base https://api.openai.com/v1 --api-key-env OPENAI_API_KEY

# Give the agent a real MCP server as its tool source:
kedge run "list the Rust files and read main.rs" \
  --model llama3.1 \
  --mcp "npx -y @modelcontextprotocol/server-filesystem ."
```

The model is prompted to emit a strict ReAct JSON object each turn; its reported
token usage is charged against the budget, and every step is journaled for
replay just like the demo path.

## CLI

```
kedge run <goal>          Drive an agent under budgets, journaling every step
        --model NAME         LLM to drive the agent (omit → offline demo policy)
        --api-base URL       OpenAI-compatible base URL (default: local Ollama)
        --api-key-env VAR    read the API key from this env var
        --mcp "CMD ARGS"     launch an MCP server as the tool source
        --max-tokens N       token ceiling (default 100k)
        --max-steps N        step ceiling (default 12)
        --max-secs N         wall-clock ceiling (default 120)
        --db <path>          SQLite ledger (default kedge.sqlite)
        --config <path>      load defaults from a TOML file
        --json               emit the result as JSON

kedge compact <file> [--lang L] [--json]   AST compaction (rust/python/js/ts/go)
kedge verify [dir]              [--json]   build/test — cargo/go/npm/pytest (auto-detected)
kedge replay <id>               [--json]   reconstruct a past run from the ledger
kedge ledger list               [--json]   list every recorded run
kedge ledger show <id>          [--json]   one run's metadata, stats & full trajectory

kedge run … --audit                        shadow dry-run: read tools run for real,
                                            mutating tools are intercepted + journaled
kedge run … --hitl                         human-in-the-loop: pause + ask (y/N) before
                                            each mutating tool runs (decisions journaled)
kedge serve [--db] [--addr]                HTTP control API: GET /runs, /runs/<id>,
                                            /approvals · POST /approvals/<id>
kedge audit --ledger <db>                  forensic report: intercepted mutations +
        --price-per-1k <USD>                measured token/cost (cost only from your
        --runs-per-day <N>                  explicit price/volume inputs)
kedge resume <id> [--force]                resume a crashed/interrupted run from its
                                            last journaled step (no re-execution)
kedge eval  --suite <s> --candidate <db>   regression-test a run vs a baseline suite
        --output-format <json|junit|pretty>  (exit 1 on regression; JUnit for CI)
```

`Ctrl-C` during a run finalizes the ledger and prints the partial trajectory
(completed steps are journaled as they happen). Every command supports `--json`
for scripting/CI, and piping into `head`/`less` is safe.

### Configuration

`run` reads defaults from `./kedge.toml` (or `--config <path>`). Flags override
the file, which overrides the built-in defaults:

```toml
model      = "llama3.1"
api_base   = "http://localhost:11434/v1"
mcp        = "npx -y @modelcontextprotocol/server-filesystem ."
max_steps  = 20
max_tokens = 200000
db         = "runs.sqlite"
```

## Build & test

```sh
cargo build            # workspace + `kedge` binary
cargo test --workspace # 43 tests (incl. end-to-end CLI tests), all green
```

Requires a Rust toolchain and a C compiler (Tree-sitter grammars and bundled
SQLite build native code). No network is needed to build or to run the demo path.

## AI-Native development

This is a Rust codebase built with an **AI-native workflow**: I drive an AI coding
agent as a pair programmer and own the architecture, the design decisions, and the
final review. AI is a tool in the loop, not the author of record. The value is
that one developer can direct it to ship and *maintain* a system this broad without
the quality bar dropping. Concretely, where it was used:

- **Design & scaffolding.** I set the crate boundaries and the core contracts (the
  `Reasoner` trait, the budget model, the error taxonomy); the agent scaffolds
  implementations against them, and I review, refactor, and reject.
- **Adversarial self-audit.** After each subsystem landed, the agent was tasked to
  *attack its own code*. That surfaced real defects a happy-path pass would miss:
  an output-drain deadlock when a backgrounded child holds the pipe, a non-Unix
  timeout that never actually killed the child, an MCP reader/dispatch race, and
  a `verify` false-positive on non-cargo directories. Every one was fixed **with a
  regression test** so it can't silently return.
- **Test generation.** The suite (unit + end-to-end CLI tests via `assert_cmd`)
  was written agent-first from stated invariants, then pruned by hand.
- **CI as the backstop.** `cargo fmt`, `clippy -D warnings`, and the full test
  suite run on Linux and macOS on every push; the machine-written code has to pass
  the same gate as anything else.

The determinism goals of the tool itself (hard budgets, replay of a recorded run
against a baseline) are also what make an AI-native workflow trustworthy here:
every run is journaled, so a change's effect is verifiable rather than vibes.

## Status

Every subsystem has a real, tested implementation. `kedge run` drives a live
model through real MCP (or built-in) tools under hard budgets, journaling each
step for deterministic replay; `compact` handles five languages; `verify`
auto-detects the project's build/test system; every command has `--json` output
and a `kedge.toml`-configurable, pipe-safe CLI covered by end-to-end tests.
A TUI and richer built-in tools are the remaining niceties.

## Foreguard, extracted from this engine

Kedge's fail-safe, deny-wins tool classifier is also published on its own as
**[Foreguard](https://github.com/nlj3/foreguard)**, a dry-run trust layer that
sits between any MCP host and its tool server: it previews each mutating tool
call, tracks whether untrusted data is driving it, and asks a human before
anything executes. It depends on `kedge-core` rather than forking it, so there
is one source of truth for classification.

Kedge is the substrate; Foreguard is the focused product built on it.

## License

**[Business Source License 1.1](LICENSE)** (BUSL-1.1): the same source-available
model used by HashiCorp, MariaDB, CockroachDB, and Sentry.

- **Free** to read, fork, modify, self-host, and use for any **non-commercial**
  purpose: personal projects, research, evaluation, and internal non-revenue use.
- **Commercial / enterprise use requires a paid license.** If Kedge (or a
  derivative or hosted version) is used in or as part of a for-profit product,
  service, or internal system, contact **noel@nlj.dev** for a
  commercial license.
- On the **Change Date (2030-07-23)** each version converts to the **Apache
  License 2.0** and becomes fully open source.

