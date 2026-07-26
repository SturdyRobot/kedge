# BRIEF — Kedge Forge

**Stage:** 1 (FRAME) · **Written:** 2026-07-25 · **Status:** active

Forge is a **layer inside kedge**, not a third repository. See
[ADR-0001](adr/0001-forge-inside-kedge.md).

---

## The thesis, stated so it can be wrong

> A skill learned from a successful agent run can be given **strictly less
> authority** than the general agent that produced it, and reusing it therefore
> **shrinks the blast radius of a prompt injection while improving task
> metrics**.

Both halves are measurable, and the security half is measurable **without any
LLM in the loop** — which is why it, not the learning, is the part we lead with.

The usual version of this claim ("agents that learn from experience get better")
is unfalsifiable as stated and self-graded in practice. The version above fails
loudly if either half is false.

---

## Who is the buyer, and what do they do today instead?

Today, an agent that repairs a test runs with **the union of every permission any
of its tasks might need** — because permissions are configured per *agent*, not
per *task*. There is no mechanism to say "for this job, you only need these three
files." Everyone doing agentic work in a real repo has this problem and solves it
by either trusting the agent completely or reviewing every call by hand.

Forge's buyer is whoever cannot accept either answer. Concretely that is the same
buyer as Foreguard, one step further along: they have accepted per-call preview
and now want per-task authority.

**Honest caveat.** This is a *portfolio and research* project first. The
commercial pull is unproven and Forge should not be positioned as a product
until the numbers in §Kill criteria exist. Plumbline is the standing reminder of
what happens when a well-built B2B middleware idea meets a market that is 12–18
months early.

---

## The single hardest technical problem

**Generalization.** Turning "this run wrote `/repo/src/lib.rs`" into a reusable
skill requires deciding that the *next* run may write `${workspace}/src/**`.
That widening is the entire product, and it is a security decision: too narrow
and the skill is a single-use recording, too wide and the manifest grants back
exactly the authority it was supposed to remove.

`kedge-skill` deliberately refuses to make that call today —
`Conformance::minimized()` emits literal subjects only. Forge is the attempt to
automate it *with evidence*, and the evidence requirement is what makes it hard.

---

## What must be true for this to work

Falsifiable assumptions, in dependency order. Each maps to a risk in
[RISKS.md](RISKS.md).

| # | Assumption | Status |
|---|---|---|
| **A0** | A corpus of recorded successful trajectories exists to learn from. | **FALSE — measured.** [Spike 000](spikes/000-trajectory-corpus.md): 0 runs. |
| **A1** | Successful trajectories in a task family share extractable repeated structure. | Unmeasured — Spike 001, deterministic |
| **A2** | A skill learned from family F solves a *held-out* member of F. | Unmeasured — Spike 002, needs LLM |
| **A3** | A learned skill's authority is strictly smaller than the general agent's, and still sufficient. | Unmeasured — measurable without LLM |
| **A4** | Argument-level capability derivation covers the argument shapes real MCP servers actually use. | Unmeasured — Spike 003 |
| **A5** | Reuse improves task metrics (fewer calls, fewer tokens) rather than just changing them. | Unmeasured, downstream of A2 |

A0 being false is the finding that reordered this whole plan. It does not kill
the project; it says the first slice is the benchmark.

---

## Explicitly out of scope for v1

- **Self-modification of kedge itself.** No evolving prompts, compaction
  policies, or retry strategies. That is the Darwin-machine shape, and it is a
  scope trap.
- **Skill *code* generation.** Forge v1 learns a **manifest and a plan**, not a
  compiled Rust/Python/WASM artifact. Generating executable skills multiplies
  the sandbox requirement by an order of magnitude and is not needed to test the
  thesis.
- **Paper → MCP server** (the "Paper2MCP" idea). Different project.
- **Any claim sourced from a citation we cannot verify.** Several arXiv IDs in
  the originating proposal postdate the assistant's knowledge cutoff and could
  not be confirmed. Nothing in this design cites a paper. If a paper is later
  verified by hand, it can be added with a note saying who verified it.
- **A sandbox.** `kedge-skill` is user-space and argument-level, stated plainly
  in its docs. `kedge-probe` is the kernel layer and is not on this path.

---

## How we will know it failed — kill criteria

Written now, before there is anything to be attached to. Checked at every stage
gate.

| Gate | Kill criterion |
|---|---|
| After Spike 001 | Fewer than **40%** of successful trajectories in a family share a repeated sub-sequence of length ≥3. Below that, there is nothing to distill and Forge reduces to a manifest recorder — which is fine, and is `kedge-skill`, which already ships. **Stop there and say so.** |
| After Spike 002 | A skill learned from family F solves fewer than **50%** of held-out members of F. Below that the skill is a recording, not a capability. |
| After S3 | Learned-skill authority is **not** measurably smaller than the general agent's. The security half of the thesis is the load-bearing half; if it is flat, the project has no distinctive claim. |
| Any time | The only way to make a number look good is to grade it on a benchmark we wrote *and* tuned against. If we cannot state an oracle-independence argument (WORKFLOW principle 8), the number does not ship. |

---

## The riskiest assumption, in one sentence

**A0 was: "there are trajectories to learn from" — and it is measurably false,
so the benchmark that generates them is slice one, not slice seven.**

With A0 addressed, the riskiest becomes **A2 (generalization)**, which is where
the LLM and the real cost enter.
