# SLICES — Kedge Forge

**Stage:** 4 · Ordered by **risk**, then dependency, then value. Never by ease.

Each slice is shippable alone, revertible alone, ≤ 2 days, and has an acceptance
criterion written now. Each runs the four-pass build loop (contract → implement
→ adversarial review → pressure test) and must clear the gate in
`WORKFLOW.md` §2.

Spikes are interleaved, not appended. A spike that trips its kill criterion
stops the plan there — that is the point of writing them down.

---

- [x] **S0 — `kedge-skill`: manifests + conformance** *(shipped)*

  Deny-by-default capability manifests, `SkillGuard` enforcement,
  `Conformance` with over-permission detection and `minimized()`.

  **Acceptance:** met. 43 tests, clippy clean, workspace green. A refused call
  is proven not to reach the executor via a recording executor, not inferred
  from a return value.
  Branch `feat/kedge-skill-manifests`, commit `7b33a48`.

---

- [ ] **S1 — `kedge-bench`: the corpus generator** · *risk R1 (CRITICAL)*

  20 repo-maintenance tasks across ≥3 families, a `ScriptedReasoner`
  (`kedge_core::Reasoner`, no LLM), and a runner that drives the real
  `ReActEngine` with the real `LedgerObserver`.

  **Why first:** [Spike 000](spikes/000-trajectory-corpus.md) measured the
  ledger at **0 runs**. Every later slice consumes trajectories. This is the
  only slice that produces them.

  **Acceptance:**
  1. `cargo run -p kedge-bench` writes **≥20 runs** to a ledger.
  2. Two consecutive runs produce a **byte-identical** `BenchReport` (excluding
     `elapsed_ms`).
  3. Suite completes in **< 30 s** with **$0** API cost.
  4. Solve rate is reported and is **not** 100% — a suite the reference solver
     aces completely has no headroom to measure improvement against.

  **Watch for:** tasks invented to be solvable rather than derived from real
  failures. R6 (the benchmark grades itself) starts accruing here, and the
  cheapest mitigation is to source breakages from real git history now rather
  than retrofit credibility later.

---

- [ ] **Spike 001 — repeated structure** · *risk R2 (HIGH)* · deterministic, no key

  Mine the S1 corpus for maximal repeated tool sub-sequences within each family.
  Report the distribution of lengths and the fraction of trajectories covered.

  **Output:** `docs/spikes/001-repeated-structure.md` with a table and a verdict.

  > **KILL: < 40% of successful trajectories share a repeated sub-sequence of
  > length ≥ 3.** Below that there is nothing to distill, and the honest
  > conclusion is that Forge reduces to a manifest recorder — which is
  > `kedge-skill`, which already ships. Stop and publish that finding.

---

- [ ] **S2 — `kedge-forge observe`: trajectory → manifest** · *risk R5* · deterministic

  `observe(&Trajectory, base) -> ObservedAuthority`, reusing
  `kedge_skill::required` so observation and enforcement cannot diverge.
  Surfaces indeterminate calls rather than dropping them.

  **Acceptance:**
  1. **Round-trip invariant:** for every run in the S1 corpus, replaying it
     under `observe(run).manifest()` yields **0 violations and 0 unused
     entries**. Property test over the whole corpus, not a sample.
  2. A trajectory containing an indeterminate call produces a manifest marked
     `complete = false` and is rejected by the gate in S5.
  3. Emitted manifests re-parse. An unparseable manifest is a hard error.

---

- [ ] **S3 — Reachable Authority + eval metrics** · *risk R3 (the thesis)*

  `reach(&Manifest, root) -> Reach`, plus `SolveRate`, `ToolCallReduction`,
  `AuthorityDelta` added to `kedge_eval::MetricKind`.

  **Why here:** this is the first slice that can produce the security number the
  whole BRIEF rests on, and it needs **no LLM**. If the security half of the
  thesis is flat, we learn it now for the price of a filesystem walk instead of
  after building a distiller.

  **Acceptance:**
  1. A real number on the S1 corpus: writable-file count under the general
     agent's manifest vs. under each learned skill's.
  2. `is_reduction_of` returns `false` on a truncated walk. An unknown is not an
     improvement.
  3. A manifest granting anything outside `root` sets `escapes_root` and is
     never scored as a reduction.
  4. `StepCountParity` and the new `ToolCallReduction` disagree on the same
     input, demonstrating the §6 gap was real and is now covered.

  > **KILL: learned-skill authority is not measurably smaller than the general
  > agent's.** The security half is the load-bearing half. Flat here means the
  > project has no distinctive claim, and the honest move is to say so.

---

- [ ] **S4 — `kedge-forge` registry: versions, lineage, results** · *risk R8*

  SQLite (WAL, same idiom as `kedge-ledger`): skill records, parent links,
  reach, eval results, promotion and rollback history.

  **Acceptance:**
  1. `lineage(id)` returns the full parent chain in order.
  2. A failed promotion leaves **no** partial write — asserted by killing the
     transaction mid-flight, not by inspection.
  3. `rollback` restores the prior `current(name)` and records why.

---

- [ ] **S5 — the promotion gate** · *risk R8*

  `gate(candidate, baseline, conformance, eval) -> GateVerdict`. Deny-by-default:
  six conditions, all required, every denial carrying at least one reason.

  **Acceptance:**
  1. One adversarial test per `GateReason` variant, each with a candidate
     constructed to trip exactly that reason.
  2. A candidate that widens authority in **any** single dimension is denied,
     including when it narrows in every other dimension.
  3. `promote == false` with an empty `reasons` vec is impossible — property
     test, not an example.

  **After S5 the deterministic skeleton is complete.** Forge can observe a
  recorded run, measure its authority, store it with lineage, and refuse to
  promote anything that widens what it can touch — with no LLM anywhere in the
  path. That is a shippable release on its own, and the natural stopping point
  if the spikes go badly.

---

- [ ] **Spike 002 — generalization** · *risk R3 (HIGH)* · **needs an LLM, costs money**

  Train/test split over the S1 families. Learn a skill from members 1..n-1,
  measure solve rate on the held-out member.

  **Output:** `docs/spikes/002-generalization.md`.

  > **KILL: < 50% held-out solve rate.** Below that the skill is a recording,
  > not a capability.

  **Gate before spending:** Spike 001 must have passed, and S3 must have shown
  a real authority reduction. Do not buy tokens to test the second-riskiest
  assumption before the first and third are settled.

---

- [ ] **Spike 003 — real-server argument coverage** · *risk R4 (MEDIUM)*

  Score `kedge_skill::required` against real MCP server argument schemas; report
  the indeterminate rate.

  **Note:** Foreguard's ecosystem validation assembled 10 servers / 80 tools,
  but captured tool *names* and declared hints — it may not have captured
  argument *schemas*. Budget for a fresh collection pass; do not assume this is
  free.

  Independent of 001/002. Can run any time; belongs before any public claim
  that the manifest layer works against real servers.

---

- [ ] **S6 — the distiller: literal paths → globs** · behind Spike 002

  The first slice with an LLM in the loop, and the first that can be wrong in a
  way tests do not catch. Deliberately unspecified in SPEC — designing it before
  Spike 002 would be designing against an unmeasured assumption.

  **Non-negotiable when it is written:** a widening must be justified by
  corroboration across ≥2 independent trajectories, never by a single run — the
  same rule that governs namespace inference in Foreguard, for the same reason.

---

- [ ] **S7 — the adversarial demo**

  Inject a poisoned instruction into a bench fixture. Show the general agent's
  blast radius, then the learned skill's, as file counts from S3.

  **Acceptance:** every number on the page traceable to a command in the repo
  that reproduces it. No number that cannot be regenerated from a clean
  checkout ships.

---

## Ordering rationale

Risk order, not feature order:

1. **S1** — R1 is confirmed true and blocks everything. Nothing else can start.
2. **Spike 001** — cheapest possible test of whether the premise holds at all.
3. **S2, S3** — the security thesis, measurable with no LLM and no spend.
4. **S4, S5** — the governance skeleton; completes a shippable deterministic release.
5. **Spike 002** — the expensive risk, deliberately last among the risks, and
   gated on the cheap ones passing first.
6. **S6, S7** — only reachable if the spikes survive.

The originating proposal ordered these roughly 1 → 5 → 3 → 4 → 2 → 6 → 7, with
the benchmark seventh. That puts the corpus that everything consumes after the
things that consume it, and the cheap deterministic measurements after the
expensive probabilistic one.
