# SPEC — Kedge Forge

**Stage:** 3 · **Written:** 2026-07-25 · Gate: every seam has a typed signature.

Read [BRIEF.md](BRIEF.md) first. This document is a contract, not a narrative.

---

## 1. The pipeline

```
kedge-bench   NEW    fixed task suite + scripted solver
      │              ↓ runs through the real ReActEngine
kedge-ledger  EXISTS SQLite trajectories                        ← the corpus
      │
kedge-forge   NEW    observe:  Trajectory → ObservedAuthority   (deterministic)
      │              reach:    Manifest   → Reach               (deterministic)
      │              registry: skills, lineage, promotion       (SQLite)
      │              gate:     candidate  → GateVerdict         (deny-by-default)
      │
kedge-skill   DONE   Manifest enforcement + Conformance
      │
kedge-eval    EXISTS baseline vs candidate  (+ new improvement metrics)
```

Two crates are new. Everything else exists and is reused rather than
reimplemented — which is the point of building Forge inside kedge
([ADR-0001](adr/0001-forge-inside-kedge.md)).

**Already built, verified by reading the source, not assumed:**

| Capability | Where | Note |
|---|---|---|
| Trajectory recording | `kedge_ledger::Ledger::record_step` via `LedgerObserver` | live journaling |
| Trajectory replay | `Ledger::replay(TaskId) -> Trajectory` | the corpus reader |
| Run enumeration | `Ledger::list_runs() -> Vec<RunSummary>` | |
| Baseline/candidate compare | `kedge_eval::evaluate(&EvalSuite, &RunProfile, &RunProfile) -> EvalReport` | see gap in §6 |
| Profile extraction | `RunProfile::from_trajectory` | steps, tool sequence, tokens, answer |
| CI output | `EvalReport::to_junit`, `::exit_code` | |
| Manifest enforcement | `kedge_skill::SkillGuard` | 43 tests |
| Authority measurement | `kedge_skill::Conformance` | exercised vs declared |

---

## 2. `kedge-bench` — the corpus generator

Exists because R1 says there is no corpus. Its job is **not** to grade Forge; it
is to *feed* it. Grading is a later, separate use of the same suite.

### Domain model

```rust
/// One reproducible repair task.
pub struct BenchTask {
    pub id: String,              // "rust-repair-001", stable, referenced by evals
    pub family: String,          // "rust-test-repair" — the generalization unit
    pub fixture: PathBuf,        // template workspace, copied per run
    pub breakage: Breakage,      // applied to the copy to create the failure
    pub acceptance: Acceptance,  // the independent oracle
}

/// How the fixture is broken. Deterministic and reversible.
pub enum Breakage {
    /// Replace an exact byte range in a file.
    Splice { path: PathBuf, find: String, replace: String },
    /// Delete a file outright.
    Remove { path: PathBuf },
}

/// How we know the task is solved. Never consults the solver's own output.
pub enum Acceptance {
    /// The command exits 0 in the workspace.
    CommandSucceeds { command: String, timeout: Duration },
    /// The file exists and its content satisfies a predicate.
    FileRestored { path: PathBuf, must_contain: String },
}

pub struct BenchSuite {
    pub name: String,
    pub tasks: Vec<BenchTask>,
}
```

### The scripted solver

The reference solver is a `kedge_core::Reasoner`, **not** a bespoke trait. That
choice is load-bearing: it means the corpus is produced by the real
`ReActEngine`, journalled by the real `LedgerObserver`, and is indistinguishable
in shape from a trajectory an LLM would have produced.

```rust
/// Emits a fixed action sequence per task id. No LLM, no network, no cost.
pub struct ScriptedReasoner {
    plans: HashMap<String, Vec<Action>>,
}

#[async_trait]
impl kedge_core::Reasoner for ScriptedReasoner {
    async fn next_action(&self, task: &Task, traj: &Trajectory)
        -> kedge_core::Result<Decision>;
}
```

### Runner

```rust
pub struct BenchOutcome {
    pub task_id: String,
    pub run: TaskId,             // ledger key — the join back to the corpus
    pub solved: bool,            // per Acceptance, evaluated on the filesystem
    pub steps: u32,
    pub elapsed_ms: u64,
}

pub struct BenchReport {
    pub suite: String,
    pub outcomes: Vec<BenchOutcome>,
}

impl BenchReport {
    pub fn solve_rate(&self) -> f64;
    pub fn to_json(&self) -> String;
}

/// Copy fixture → apply breakage → run engine → evaluate acceptance → record.
pub async fn run_suite(
    suite: &BenchSuite,
    reasoner: Arc<dyn Reasoner>,
    tools: Arc<dyn ToolExecutor>,
    ledger: &Ledger,
    scratch: &Path,
) -> Result<BenchReport, BenchError>;
```

### Non-functionals

| Property | Budget | Why |
|---|---|---|
| Full suite wall-clock, scripted solver | **< 30 s** | it runs in CI on every push |
| Reproducibility | **byte-identical** `BenchReport` across two runs | it is the corpus; a drifting corpus invalidates every downstream number |
| API cost, scripted solver | **$0** | Spike 001 must be runnable without a key |

Reproducibility is achieved by: fixed task ordering, fixture copied fresh per
run, no wall-clock or RNG in the scripted plans, and `elapsed_ms` excluded from
the reproducibility hash (it is timing, not behaviour).

---

## 3. `kedge-forge` — observe

Deterministic. No LLM. Reuses `kedge_skill::required` so the derivation that
*observes* authority is byte-for-byte the derivation that *enforces* it — a
second implementation could disagree with the guard, and a manifest that the
guard then rejects is worse than no manifest.

```rust
/// What a recorded trajectory actually exercised.
pub struct ObservedAuthority {
    pub task: TaskId,
    pub exercised: BTreeMap<Capability, usize>,
    /// Calls whose effect could not be named. These are why a manifest may be
    /// incomplete, and they must surface rather than be dropped.
    pub indeterminate: Vec<(String, String)>,   // (tool, reason)
}

pub fn observe(traj: &Trajectory, base: &Path) -> ObservedAuthority;

impl ObservedAuthority {
    /// Literal-subject manifest. Same output shape as
    /// `Conformance::minimized`, from a stored run rather than a live one.
    pub fn manifest(&self, name: &str, version: &str) -> String;
}
```

**Invariant (contract test):** for any trajectory `T`, replaying `T` under
`observe(T).manifest()` yields a `Conformance` with **0 violations and 0 unused
entries**. If that fails, the observer and the guard disagree, and the observer
is wrong by definition.

**Invariant:** `indeterminate` non-empty ⇒ the emitted manifest is marked
incomplete and is **not eligible for promotion**. Silently emitting a manifest
that omits an unnameable effect would produce a skill that appears
least-privilege and is not.

---

## 4. `kedge-forge` — Reachable Authority

The headline metric, and the one that carries the security half of the thesis.
See [ADR-0003](adr/0003-reachable-authority-metric.md).

Counting *declared entries* is worthless: `write = ["**"]` is one entry and
grants the disk. Reachable Authority counts what the manifest can actually
touch.

```rust
pub struct Reach {
    /// Files under `root` the manifest would permit writing.
    pub writable: usize,
    pub readable: usize,
    /// Distinct allow-entries, as a secondary signal only.
    pub commands: usize,
    pub hosts: usize,
    /// True if any grant matches a path outside `root`. A manifest that escapes
    /// the workspace cannot be compared by in-root counts alone, so this is a
    /// hard flag, never folded into a score.
    pub escapes_root: bool,
    /// Set when the walk hit `MAX_WALK`; counts are lower bounds.
    pub truncated: bool,
}

pub fn reach(manifest: &Manifest, root: &Path) -> Result<Reach, ForgeError>;

impl Reach {
    /// Strictly-smaller in every dimension, with no escape and no truncation.
    /// Truncated walks return `false` — an unknown cannot be an improvement.
    pub fn is_reduction_of(&self, other: &Reach) -> bool;
}
```

Walk excludes `.git`, respects `MAX_WALK = 50_000` entries, follows no symlinks.

**The demo number this produces:** *the general agent could write 1,247 files;
the learned skill can write 3.* Deterministic, no LLM, reproducible on any
checkout.

---

## 5. `kedge-forge` — registry and promotion gate

```rust
pub struct SkillId(pub Uuid);

pub struct SkillRecord {
    pub id: SkillId,
    pub name: String,
    pub version: String,
    pub parent: Option<SkillId>,      // lineage
    pub manifest_toml: String,
    pub origin_run: TaskId,           // the trajectory it was learned from
    pub reach: Reach,
    pub complete: bool,               // no indeterminate calls
    pub promoted: bool,
}

pub struct Registry { /* SQLite, WAL, same idiom as kedge-ledger */ }

impl Registry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ForgeError>;
    pub fn insert_candidate(&self, rec: &SkillRecord) -> Result<SkillId, ForgeError>;
    pub fn promote(&self, id: SkillId, verdict: &GateVerdict) -> Result<(), ForgeError>;
    pub fn rollback(&self, id: SkillId, reason: &str) -> Result<(), ForgeError>;
    pub fn lineage(&self, id: SkillId) -> Result<Vec<SkillRecord>, ForgeError>;
    pub fn current(&self, name: &str) -> Result<Option<SkillRecord>, ForgeError>;
}
```

### The gate — deny-by-default, same ethos as everything else

```rust
pub enum GateReason {
    ConformanceViolation { tool: String, reason: String },
    Incomplete { indeterminate: usize },
    AuthorityWidened { dimension: &'static str, from: usize, to: usize },
    EscapesRoot,
    ReachTruncated,
    EvalRegressed { metric: String, detail: String },
    NoBaseline,
}

pub struct GateVerdict {
    pub promote: bool,
    pub reasons: Vec<GateReason>,   // non-empty whenever `promote` is false
}

pub fn gate(
    candidate: &SkillRecord,
    baseline: Option<&SkillRecord>,
    conformance: &Conformance,
    eval: Option<&EvalReport>,
) -> GateVerdict;
```

**Promotion requires all of:**

1. `conformance.conforms()` on the origin trajectory — no violations.
2. `candidate.complete` — no indeterminate calls.
3. `!reach.escapes_root` and `!reach.truncated`.
4. If a baseline exists: `candidate.reach.is_reduction_of(&baseline.reach)`
   **or** equal. Authority may never widen on promotion; a genuinely wider skill
   is a new skill with a new name, reviewed by a human.
5. If an `EvalReport` exists: `eval.passed`.
6. If no baseline exists, promotion is allowed **only** for a first version, and
   the reasons list records `NoBaseline` so the audit trail says the gate was
   thin, not that it was clean.

Every `false` verdict carries at least one reason. A silent denial is a bug.

---

## 6. `kedge-eval` — the named gap

`kedge-eval` exists and is reused, but its four metrics are **regression parity**
metrics — `StepCountParity`, `ToolCallEquivalence`, `TokenDeltaThreshold`,
`OutputDrift`. They answer *"did this change break the run?"*

Forge needs **improvement** metrics: *"is the candidate better?"* Parity is
exactly the wrong shape — a skill that solves the task in 11 calls instead of 18
**fails** `StepCountParity` and `ToolCallEquivalence` today.

Additions required (Slice 3):

```rust
pub enum MetricKind {
    // ... existing four ...
    /// Solve rate over a BenchSuite, candidate ≥ baseline.
    SolveRate,
    /// Tool calls per solved task, candidate ≤ baseline.
    ToolCallReduction,
    /// Reachable Authority, candidate ≤ baseline in every dimension.
    AuthorityDelta,
}
```

This is an addition, not a rewrite: `Thresholds` and the report/JUnit plumbing
are unchanged.

---

## 7. Failure modes

| Dependency | Fails how | Behaviour |
|---|---|---|
| Ledger | missing / corrupt / locked | `ForgeError::Ledger`, no partial registry write |
| Fixture | absent or unreadable | task marked unsolved with a distinct reason, suite continues |
| Acceptance command | times out | unsolved, **not** an error — a hang is a legitimate task failure |
| Filesystem walk | `> MAX_WALK` | `Reach::truncated = true`, gate denies on it |
| Registry | concurrent writers | SQLite WAL; promotion is a single transaction |
| Manifest emitted by observer | fails to re-parse | hard error — an unparseable manifest must never be stored |

Two rules across all of them: an error never leaves the registry holding a
half-promoted skill, and no failure path silently degrades into "permitted."

---

## 8. What is deliberately not specified

- **The distiller** (literal paths → globs). It is Slice 6, behind Spike 002,
  and specifying it now would be designing against an unmeasured assumption.
- **Skill code generation.** Out of scope per BRIEF.
- **Any sandbox.** `kedge-probe` is a different layer.
