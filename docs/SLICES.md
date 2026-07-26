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

- [x] **S1 — `kedge-bench`: the corpus generator** *(shipped 2026-07-26)* · *risk R1 — **CLOSED***

  20 repair tasks across 4 families, a `ScriptedReasoner` (`kedge_core::Reasoner`,
  no LLM), and a runner driving the real `ReActEngine` with the real
  `LedgerObserver`.

  **Acceptance:**
  1. ✔ **20 runs / 110 steps** written to a ledger (was 0/0/0).
  2. ✔ Identical report fingerprint `835a1d21d0ee4848` across consecutive runs,
     despite differing wall-clock. `TaskId`s are derived from the task name, not
     `Uuid::new_v4()`, so the ledger is reproducible too.
  3. ✔ ~11 s including the integrity pass, against a 30 s budget. $0.
  4. ✘→**criterion corrected.** Solve rate is **100%**, not the "not 100%" the
     criterion demanded.

  **On criterion 4.** It was miscalibrated, and the honest fix is to correct it
  rather than to hobble tasks until the suite fails. The scripted solver computes
  the exact inverse of each breakage — it is an oracle-solver by construction, so
  100% is the *expected* result and what it demonstrates is that all 20 tasks are
  solvable and the harness works end to end. Headroom is a property the **LLM
  baseline** needs, and that measurement belongs to Spike 002.

  What 100% must not be is unfalsifiable. So the runner carries a positive
  control: `a_solver_that_does_nothing_but_claim_success_scores_zero` runs a
  reasoner that immediately returns `Finish { "fixed it" }` and asserts it scores
  **0/3** — the verdict comes from the fixture's own tests, never from what the
  agent reported.

  **Two bugs found, both silent-wrongness rather than obvious breakage.** Written
  up in `RISKS.md` under closed-R1: a no-op breakage (`v > hi` → `v >= hi` is
  behaviourally identical, so the "broken" fixture passed), and shared cargo
  build artifacts across fixture copies that all identify as `slug 0.0.0` (a
  pristine copy ran a broken copy's binary). Both now have permanent tests.

  17 tests, clippy clean, workspace green.

---

- [ ] **Spike 001a — miner validation** · deterministic, free

  Does the sub-sequence miner recover structure that was deliberately planted in
  the scripted corpus? A prerequisite, not the question itself.

  > **KILL: the miner cannot recover planted structure.** Fix the miner before
  > spending anything on 001b — an unvalidated miner makes 001b unreadable.

---

- [ ] **Spike 001b — repeated structure** · *risk R2/R9* · **needs an LLM**

  **Rescoped during S1.** The original spike was to mine S1's corpus for repeated
  sub-sequences with no API key. That is invalid: the corpus is produced by a
  `ScriptedReasoner` whose step shapes are authored, so mining it measures
  `scripted::plan_for` rather than agent behaviour — R6 in its sharpest form.

  The question needs **LLM-generated** trajectories, so it joins Spike 002 behind
  the same spend gate.

  > **KILL: <40% of successful trajectories share a repeated sub-sequence of
  > length ≥3.** Below that there is nothing to distill, and the honest
  > conclusion is that Forge reduces to a manifest recorder — which is
  > `kedge-skill`, which already ships. Stop and publish that.

---

- [x] **S2 — `kedge-forge observe`: trajectory → manifest** *(shipped 2026-07-26)* · *risk R5*

  `observe(&Trajectory, base) -> ObservedAuthority`, reusing
  `kedge_skill::required` so observation and enforcement cannot diverge.

  **Acceptance:**
  1. ✔ **Round-trip invariant over all 20 corpus runs**, not a sample. The
     corpus is generated in-process, so the test cannot pass against a stale
     artifact. Every run: `verified exact`.
  2. ✔ An indeterminate call is surfaced in `unobservable` and blocks
     `is_complete()`, which S5's gate will read.
  3. ✔ `compiled()` returns `Err` rather than storing an unparseable manifest.

  **The design decision worth recording: the observer verifies its own output.**
  An observation can be perfectly correct and still be *unmanifestable*. A
  trajectory that ran `cargo test && curl …` exercised a real capability that no
  manifest can ever grant, because `kedge-skill` denies any command carrying a
  shell metacharacter and always will. Emitting a manifest for that run yields a
  file that rejects the very trajectory it came from.

  So `observe_verified` replays the trajectory through a real `SkillGuard` built
  from the manifest it just emitted, and returns `Verification::Failed` rather
  than handing back something authoritative-looking and wrong. The round-trip is
  a property of the API, not something the tests happen to check.

  It is not tautological: derivation and enforcement share a code path, so
  re-deriving proves little. What it proves is that the **rendering** step
  survived — that a manifest *can* be written granting what was observed.

  **A refactor this forced.** `Conformance::minimized` and the observer both
  emit manifests. Two emitters would eventually disagree, and the disagreement
  would look like a finding — so both now delegate to a single
  `kedge_skill::manifest::render`.

  **One correction.** A test asserted that an unnameable effect passes
  verification (the theory being it is invisible to both sides). It does not —
  the guard refuses it for the same reason the observer cannot name it, so the
  two signals agree. `is_complete()` still requires both, so neither silently
  becomes load-bearing alone.

  Observed output, `cart-002` — reads two files, writes one, two commands, no
  invented globs:

  ```toml
  [capabilities.filesystem]
  read  = ["${workspace}/Cargo.toml", "${workspace}/src/lib.rs"]
  write = ["${workspace}/src/lib.rs"]

  [capabilities.process]
  allow = ["cargo check -q", "cargo test -q"]
  ```

  9 tests (7 unit + 2 acceptance), clippy clean, workspace green.

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
2. **Spike 001a** — validate the miner while it is still free to do so.
3. **S2, S3** — the security thesis, measurable with no LLM and no spend.
4. **S4, S5** — the governance skeleton; completes a shippable deterministic release.
5. **Spikes 001b + 002** — the expensive risks, deliberately last, gated on the
   cheap ones passing first. S1 moved 001b into this group: it turns out the
   premise cannot be tested for free after all.
6. **S6, S7** — only reachable if the spikes survive.

The originating proposal ordered these roughly 1 → 5 → 3 → 4 → 2 → 6 → 7, with
the benchmark seventh. That puts the corpus that everything consumes after the
things that consume it, and the cheap deterministic measurements after the
expensive probabilistic one.
