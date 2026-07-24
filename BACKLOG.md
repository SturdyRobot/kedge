# Kedge — Backlog & Roadmap

Legend: ✅ shipped · 🚀 do next · ⏳ later · 🛑 skip (for now)

---

## ✅ Shipped (on `main`, all tests green)

The core runtime and the whole trust/ops surface are done:

- **ReAct core** — hard token/step/wall-clock budgets, validated state machine,
  byte-for-byte SQLite replay.
- **Shadow-Guard audit** (`kedge run --audit` + `kedge audit`) — zero-risk dry-run
  of mutating tools + forensic ROI/security report.
- **Human-in-the-loop** (`kedge run --hitl`) — pause + approve each mutation.
- **Crash recovery** (`kedge resume`) — continue from the last journaled step.
- **Subagent mesh** (`kedge-mesh`) — bounded, isolated, contained failures.
- **MCP client** (`kedge-mcp`) — stdio **and** streamable HTTP.
- **Regression harness** (`kedge eval`) — JUnit + CI exit codes.
- **AST compaction** + **content-hashed cache** (`kedge-cache`).
- **Guardrails** — `kedge-policy` (user-space) + `kedge-probe` (eBPF LSM, Linux).
- **OpenTelemetry** export (`--features otel`).
- **Python bindings** (`kedge-bridge` → `pip install kedge-rt`) — verified on
  CPython 3.14 (see below).

---

## Language reach — the roadmap

### ✅ 1. Python (`kedge-bridge` / maturin) — SHIPPED
`pip install kedge-rt`. Python is the king of AI — this gets Kedge into 80%+ of
real-world AI pipelines. Exposes AST compaction, content hashing, tool
classification, the policy matcher, and the forensic audit as an **abi3 wheel**
(one wheel, CPython ≥3.9). Verified end-to-end with a real `import kedge_rt`.
*Follow-on:* async agent execution + subagent supervision from Python.

### ✅ 2. WebAssembly (`kedge-web`) — SHIPPED
Live on **nlj.dev** (the 🦀 Kedge icon), running the real `kedge-core`
engine client-side at ~137 KB. Making core wasm-clean meant target-gating
tokio/uuid (dropping `net` to avoid `mio`), swapping `std::time::Instant` for
`web-time::Instant` (std's panics on wasm), and gating the wall-clock timeout.
A CI job now builds core + `kedge-web` for `wasm32` so it can't silently rot.

### ⏳ 3. TypeScript (`napi-rs`) — POST-LAUNCH
`npm install kedge-rt`. TypeScript/Node.js is the second-largest AI ecosystem
(Vercel AI SDK, LangChain.js). Build **after** the Python release is stable and
only if demand shows up.

### 🛑 4. Everyone else (Go, Java, C#, …) — DON'T build native bindings
Kedge already speaks **MCP over stdio and HTTP**. A Go/Java/C# team runs the
`kedge` binary as a background process and sends JSON-RPC/MCP requests — **$0
additional code**. The daemon covers every other language for free.

> **Golden rule:** only write a custom native binding when demand forces it. The
> day a Fortune 500 shows up with a signed contract that says "we buy Kedge today
> if you ship a native Java SDK" is the day you build the Java binding — not
> before. Python + WASM + the CLI/MCP daemon covers ~99% of use cases.

---

## Other parked

### ⏳ Test suite is timing/port-sensitive under load
`kedge-exec`, `kedge-mcp`, `kedge-server` and `kedge-probe`'s integration test
bind ports, spawn subprocesses, and assert on timeouts. Under a loaded machine
(e.g. a parallel `cargo build` running alongside) six targets fail; each passes
in isolation. On a contended CI runner this reads as flaky.

Fix properly rather than by bumping sleeps: inject the clock/timeout instead of
asserting against wall time, and bind port 0 everywhere rather than fixed ports.
Until then, prefer `cargo test --workspace -- --test-threads=…` on constrained
machines.

### ⏳ Ledger writes block the async ReAct loop
`StepObserver::on_step` performs a synchronous SQLite insert from inside the
engine loop. This is currently **deliberate** — see the design note on
`LedgerObserver` — because buffering the write would open a lost-step window on
crash, which is the exact failure the ledger exists to prevent. Mitigated with
WAL + `busy_timeout` + `synchronous=NORMAL`.

Revisit only when a real workload proves it's the bottleneck (many agents sharing
one ledger under `kedge-mesh`). The right answer then is a per-agent ledger or a
*durable* write-ahead queue — not a fire-and-forget channel.

### 🛑 kedge-zk — SP1 zkVM execution proofs
Zero-knowledge proof of policy-compliant execution. Skip until there's a real SP1
host to *generate and verify* a proof on — `sp1-sdk` is a massive dep and can't be
validated in the current environment. Shipping unverified crypto code is a
credibility risk.

---

## Recommended order
1. **WASM demo** (`kedge-web`) — the hiring showpiece. 🚀
2. **TypeScript** (`napi-rs`) — after Python is stable, on demand. ⏳
3. Everything else → the **MCP daemon**, not a binding. 🛑
4. **kedge-zk** — only with an SP1 host. 🛑
