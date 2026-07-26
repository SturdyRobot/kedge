# RISKS — Kedge Forge

Living register. Re-read weekly per `WORKFLOW.md` §6. A risk that got worse gets
a spike; a risk that is now zero gets deleted, not archived.

Severity is *impact if true*, not likelihood.

---

## R9 — A scripted corpus cannot answer Spike 001 · **HIGH** · open · *new*

Found while building S1, and it corrects the slice ordering.

Spike 001 asks whether successful trajectories share extractable repeated
structure. `kedge-bench`'s corpus is produced by a `ScriptedReasoner` whose step
shapes are **authored**. Mining it for repeated structure measures
`scripted::plan_for`, not agent behaviour. The answer would be whatever was
written, and it would look like a finding.

This is R6 (the benchmark grades itself) in its sharpest form, and it means
Spike 001 as originally scoped is **not answerable with the corpus S1 produces**.

**Resolution — split the spike:**

- **001a — miner validation.** Does the sub-sequence miner recover structure that
  was deliberately planted? Deterministic, free, valid on a scripted corpus,
  and a genuine prerequisite: an unvalidated miner would make 001b unreadable.
- **001b — the actual question.** Does structure exist in *LLM-generated*
  trajectories? Needs an LLM, so it joins Spike 002 behind the same spend gate.

The kill criterion (<40% coverage) moves to **001b**. 001a has its own: if the
miner cannot recover planted structure, fix the miner before spending anything.

**Mitigation already in place:** `plan_for` varies step shapes per family, and
`scripted::plans_are_not_all_the_same_shape` asserts ≥4 distinct shapes — so the
corpus is at least not degenerate. That is damage control, not validity.

---

## R2 — Trajectories may contain no repeated structure · **HIGH** · open

If every successful run is bespoke, there is nothing to extract and Forge
collapses into "record a manifest," which `kedge-skill` already does.

**Why it is plausible:** Repo-maintenance tasks differ in which file breaks,
which test fails, and which fix applies. The *shape* may repeat (inspect →
test → patch → re-test) while no concrete sub-sequence does. A shape is not
something the deterministic observer can extract.

**Spike:** split into 001a/001b — see **R9**. The deterministic half validates
the miner on the scripted corpus; the question itself needs LLM trajectories and
is gated behind the same spend decision as Spike 002.

**Kill threshold:** <40% of successful trajectories share a repeated
sub-sequence of length ≥3, measured on **001b**.

---

## R3 — A learned skill may not generalize · **HIGH** · open

The core product risk. A skill distilled from tasks 1–15 must solve task 16.

**Why it is plausible:** `minimized()` emits literal paths by design. Widening
them is a judgement call, and the widening that makes the skill reusable is the
same widening that gives back the authority the manifest removed. There may be
no setting that satisfies both.

**Spike:** 002, needs an LLM, gated behind 001. Train/test split over the bench
families; report solve rate on held-out members.

**Kill threshold:** <50% held-out solve rate.

---

## R4 — Capability derivation may not match real servers · **MEDIUM** · open

`kedge-skill` recognizes a fixed table of argument keys (`path`, `command`,
`url`, …). A real MCP server using `target_uri` or `resource` would produce
`Requirement::Indeterminate` and be refused.

**Why it matters both ways:** Fail-safe means this errs toward *refusing valid
work*, not permitting invalid work — so it is a usability risk, not a security
hole. But a manifest layer that refuses half of real traffic will not be used,
and an unused security layer secures nothing.

**Spike:** 003. Score the derivation against real MCP server catalogues. The
Foreguard ecosystem validation already assembled 10 servers / 80 tools for a
comparable exercise; reuse that corpus. Report the indeterminate rate.

**Note:** the Foreguard corpus captured tool *names* and declared hints. It may
not have captured argument *schemas*, in which case this spike needs its own
collection pass. Do not assume it is free.

---

## R5 — Minimized manifests may be too tight to transfer · **MEDIUM** · open

A manifest of literal paths from repo A is meaningless in repo B. Generalization
must at minimum re-root paths against `${workspace}`, and the re-rooting has to
survive a repo with a different layout.

**Related:** this is R3 seen from the manifest side rather than the plan side.
They may resolve together or independently; keep both until one is closed.

---

## R6 — The benchmark grades itself · **MEDIUM** · open

We write the tasks, the reference solver, and the metrics. WORKFLOW principle 8:
an oracle derived from the thing under test can only confirm it.

**Mitigation, required before any number is published:**
- Tasks derived from *real* failing-test scenarios (git history, real crates),
  not invented ones.
- The scripted reference solver and the learned skill must be written by
  different passes and never share a helper.
- At least one metric (Reachable Authority) is computed from the *manifest and
  the filesystem*, not from anything the solver produced.

**Closes when:** the oracle-independence argument is written down and survives
an adversarial review pass that is specifically asked to attack it.

---

## R7 — Not a sandbox; TOCTOU · **LOW** · accepted, documented

`kedge-skill` is user-space and argument-level. A tool that ignores its own
arguments is invisible to it, and symlinks resolve at check time only.

**Status:** accepted and stated in `crates/kedge-skill/README.md` and the crate
docs. `kedge-probe` is the kernel-level layer and is explicitly out of scope for
Forge v1. Recorded here so it is never quietly reclassified as solved.

---

## R8 — Scope creep into self-modification · **MEDIUM** · open

The originating proposal contains a "Bounded Darwin Machine" variant, and the
gravitational pull from "learn a skill" to "evolve the harness" is strong.

**Mitigation:** it is in BRIEF §out-of-scope. Any slice that mutates kedge's own
prompts, compaction policy, or retry logic is out of the plan and needs a new
BRIEF.

---

---

## Closed risks

### R1 — No trajectory corpus · **CRITICAL** · **CLOSED 2026-07-26**

Was: `kedge.sqlite` had 0 runs, 0 steps, 0 events
([Spike 000](spikes/000-trajectory-corpus.md)), so every Forge component had no
input and every before/after number was unmeasurable in both directions.

**Closed by:** `kedge-bench` (S1). `cargo run -p kedge-bench` now writes
**20 runs / 110 steps** across 4 families, in ~11s, at $0, with an identical
report fingerprint (`835a1d21d0ee4848`) across consecutive invocations.

**What the slice cost, and what it bought:** two real bugs, both of which would
have silently produced a wrong corpus rather than an obviously broken one.

1. **A no-op breakage.** `clamp_upper`'s `v > hi` → `v >= hi` is behaviourally
   identical; the "broken" fixture passed. A task that is already solved reports
   as *solved*, which looks like success. Caught by a negative control before
   any code was written.
2. **Shared build artifacts.** One `CARGO_TARGET_DIR` across the suite —
   measured at 0.10s/task against 0.15s isolated — made every fixture copy
   resolve to the same cargo artifact, because they all identify as
   `slug 0.0.0`. A pristine copy executed a previously-broken copy's binary and
   reported `FAILED`. Wrong in both directions. The first fix (key on task id)
   was still wrong: concurrent tests using the same id collided again. Keyed on
   workspace path now.

Both are permanent tests: `checks::every_breakage_actually_breaks` and
`checks::a_broken_task_does_not_contaminate_the_next_task_on_the_same_fixture`.
