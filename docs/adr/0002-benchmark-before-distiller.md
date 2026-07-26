# ADR-0002 — The benchmark is slice one, and its solver is scripted

**Date:** 2026-07-25 · **Status:** accepted
**Supersedes:** the milestone ordering in the originating Forge proposal

## Context

The proposal's milestone put "convert a recorded kedge trajectory into a skill
candidate" at issue 2 and "build one public benchmark with 20–30 tasks" at issue
7. [Spike 000](../spikes/000-trajectory-corpus.md) measured the ledger that
issue 2 would read from: **0 runs, 0 steps, 0 events.** No eval suite, no
fixtures, no benchmark anywhere in the repo.

So the ordering is inverted. The benchmark is not how Forge gets *graded* — it
is how Forge gets *fed*. Issue 2 has no input until issue 7 exists, and every
before/after number is unmeasurable in both directions until then. `WORKFLOW.md`
principle 1 and stage 2 both say this directly: a plan with the core-risk
measurement in the last phase has the plan inverted.

## Decision

`kedge-bench` is slice one. Its reference solver is a **deterministic
`ScriptedReasoner`**, not an LLM.

Scripted is the load-bearing half of that decision. It makes the corpus free
($0/run), reproducible (byte-identical reports, so a drifting corpus cannot
silently invalidate downstream numbers), and fast enough to run in CI on every
push. Spike 001 — does repeated structure exist at all — then runs with no API
key, which means the cheapest test of the project's premise is not gated behind
a credential or a budget.

The solver implements `kedge_core::Reasoner` rather than a bespoke trait, so the
corpus is produced by the real `ReActEngine` and journalled by the real
`LedgerObserver`. A trajectory in the corpus is shaped exactly like one an LLM
would have produced. A bespoke corpus format would have been faster to write and
would have made every downstream result untransferable.

## Consequences

A scripted solver cannot fail in the interesting ways an LLM does — it never
flails, never retries wrongly, never takes a creative wrong path. So the corpus
is *cleaner than reality*, and any distiller trained only on it will be
optimistic. This is a known limitation, recorded as R6, and the mitigation is
that Spike 002 must use a real LLM against held-out tasks before any
generalization claim is published.

The LLM enters at slice 6, not slice 1. That is a deliberate cost deferral: four
slices of deterministic, testable work ship before a single token is bought.
