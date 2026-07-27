# ADR-0001 — Forge is a layer inside kedge, not a third repository

**Date:** 2026-07-25 · **Status:** accepted

## Context

The originating proposal offered Forge as a new flagship project, then argued
against itself in its own conclusion: *"Do not create a third disconnected agent
repository."* The portfolio currently has kedge (deep, hard to summarize) and
Foreguard (narrow, easy to pitch). A third repo would dilute both without
finishing either.

The reuse case is concrete, not aspirational. Reading the source rather than
assuming: `kedge-ledger` already records and replays trajectories
(`replay(TaskId) -> Trajectory`, `list_runs()`), `kedge-eval` already compares a
baseline to a candidate with thresholds and JUnit output, and `kedge-skill` now
enforces manifests and measures conformance. Forge needs two new crates
(`kedge-bench`, `kedge-forge`) and one extension (`kedge-eval` metrics). A
standalone repo would reimplement the ledger, the replay, and the eval harness —
three things that are already built and tested here.

## Decision

Forge ships as `crates/kedge-bench` and `crates/kedge-forge` inside the kedge
workspace. Foreguard stays a separate product and is **not** absorbed; its
narrow positioning is the thing that makes it sellable, and it consumes
`kedge-core` as a published dependency rather than living in the tree.

The three-part story becomes: **kedge executes and records. Forge learns what a
task actually needed. Foreguard stops anything from exceeding it.**

## Consequences

Kedge's "does too much to summarize in thirty seconds" problem gets worse before
it gets better — the workspace goes from 17 crates to 19. Mitigation is
documentation, not architecture: the README leads with the trust arc, and the
crate list is a detail below it.

Reverting is cheap. Both new crates are leaves in the dependency graph; nothing
existing depends on them, so deleting them is a `git rm` and two lines out of
`Cargo.toml`.
