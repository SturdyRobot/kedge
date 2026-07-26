# RISKS — Kedge Forge

Living register. Re-read weekly per `WORKFLOW.md` §6. A risk that got worse gets
a spike; a risk that is now zero gets deleted, not archived.

Severity is *impact if true*, not likelihood.

---

## R1 — No trajectory corpus · **CRITICAL** · **CONFIRMED TRUE**

Forge consumes recorded successful executions. There are none.

**Evidence:** [Spike 000](spikes/000-trajectory-corpus.md). `kedge.sqlite` has
0 runs, 0 steps, 0 events. No eval suite, no benchmark, no fixtures.

**Impact:** Every downstream component has no input. Any before/after number is
unmeasurable in both directions.

**Mitigation:** Slice 1 is `kedge-bench` — a fixed suite of repo-maintenance
tasks with a **deterministic scripted solver**, producing a reproducible corpus
at zero API cost. See [ADR-0002](adr/0002-benchmark-before-distiller.md).

**Closes when:** `kedge-bench` writes ≥20 runs to a ledger and the run is
byte-reproducible across two invocations.

---

## R2 — Trajectories may contain no repeated structure · **HIGH** · open

If every successful run is bespoke, there is nothing to extract and Forge
collapses into "record a manifest," which `kedge-skill` already does.

**Why it is plausible:** Repo-maintenance tasks differ in which file breaks,
which test fails, and which fix applies. The *shape* may repeat (inspect →
test → patch → re-test) while no concrete sub-sequence does. A shape is not
something the deterministic observer can extract.

**Spike:** 001, deterministic, no API key. Mine the corpus for maximal repeated
tool sub-sequences across runs in a family; report the distribution of lengths
and coverage.

**Kill threshold:** <40% of successful trajectories share a repeated
sub-sequence of length ≥3.

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

## Closed risks

*(none yet)*
