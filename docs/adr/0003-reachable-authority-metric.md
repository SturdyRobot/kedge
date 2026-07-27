# ADR-0003 — Authority is measured as a reachable set, not a declaration count

**Date:** 2026-07-25 · **Status:** accepted

## Context

The security half of the Forge thesis is *"a learned skill has strictly less
authority than the general agent that produced it."* That needs a number, and
the obvious number is wrong.

Counting **declared entries** is worthless in exactly the case that matters:

```toml
write = ["**"]          # 1 entry, grants the entire filesystem
write = ["src/a.rs", "src/b.rs", "src/c.rs"]   # 3 entries, grants 3 files
```

By entry count the first manifest is three times tighter than the second. Any
minimizer optimizing that metric would learn to emit `**`. A metric that
rewards the worst possible manifest is worse than no metric — it actively
misleads, and it would have made a great-looking chart.

## Decision

**Reachable Authority** counts what a manifest can actually touch: walk the
workspace root and count the files each grant permits reading and writing.

```rust
pub struct Reach {
    pub writable: usize, pub readable: usize,
    pub commands: usize, pub hosts: usize,
    pub escapes_root: bool,
    pub truncated: bool,
}
```

Three rules keep it honest:

- **`escapes_root` is a flag, never a score.** A manifest granting `/etc/**`
  scores zero writable files *inside* the workspace, which would read as
  maximally tight. Any grant matching outside the root sets the flag, and a
  flagged manifest is never scored as a reduction.
- **`truncated` denies.** If the walk exceeds `MAX_WALK`, counts are lower
  bounds, and `is_reduction_of` returns `false`. An unknown is not an
  improvement.
- **Reduction must hold in every dimension.** A candidate that halves writable
  files while adding one command has not reduced authority; it has traded. The
  gate denies it and a human decides.

Commands and hosts stay as entry counts, because there is no finite set to
enumerate them against. They are secondary signals, and the spec says so.

## Consequences

The metric is **filesystem-dependent**: the same manifest scores differently on
a repo with 200 files and one with 20,000. That is correct — authority *is*
contextual — but it means a Reach is only comparable against another Reach
computed on the same root at the same commit. Comparisons across repos are
meaningless and must never be published as one number.

The walk costs real time on a large tree. Capped at 50,000 entries, `.git`
excluded, symlinks not followed. Uncapping it would trade a bounded wrong answer
for an unbounded hang.

The upside is that this produces the demo sentence with no LLM in it and no
self-grading: *the general agent could write 1,247 files in this repo; the
learned skill can write 3.* Both halves are recomputable from a clean checkout
by anyone.
