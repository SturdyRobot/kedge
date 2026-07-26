# ADR-0004 — No citation ships that has not been verified by hand

**Date:** 2026-07-25 · **Status:** accepted

## Context

The proposal that originated Forge grounded its architecture in eight papers.
Four of the arXiv identifiers — `2603.18000`, `2606.03024`, `2605.11039`,
`2607.01236` — postdate the assistant's knowledge cutoff and could not be
confirmed to exist. Four others (`2502.11705`, `2505.22954`, `2505.03335`,
`2506.06287`) are recognizable.

A research-framed README is a claim about the literature. If a reader checks one
citation and finds nothing, every other number on the page becomes suspect —
including the ones that are real and measured. The cost of a fabricated citation
is not the citation; it is the credibility of the measurements next to it.

## Decision

Nothing in the Forge design cites a paper. The architecture is justified by the
codebase and by measurements taken in this repo.

A citation may be added later under one condition: a human opened the paper,
confirmed the identifier resolves, and confirmed it says what the citation
claims it says. The commit that adds it names who verified it.

## Consequences

The design reads as less academically grounded than the proposal did. That is
the honest state — the ideas were arrived at by reading kedge's source and
measuring its ledger, and presenting them as literature-derived would be a claim
about provenance that is not true.

This is the same standard already applied elsewhere in the portfolio: kedge's
README had "byte-identical deterministic replay" removed when it could not be
substantiated, and Foreguard's taint tracking is documented as "best-effort, not
sound." A verified-only citation rule is that discipline extended from claims
about the software to claims about the literature.
