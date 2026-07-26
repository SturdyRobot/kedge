# kedge-skill

**Deny-by-default capability manifests for agent skills.** Declare what a skill
may touch, run it, and get back proof of whether it stayed inside — plus the
tightest manifest that would still have worked.

## Why

`kedge-policy` is a blocklist:

```toml
blocked_tools = ["shell", "delete_file"]
```

Blocklists fail in one direction. Everything you didn't think of is allowed, so
the security of a run depends on the imagination of whoever wrote the list. That
is the same failure mode as trusting a tool because its *name* looks harmless —
the thing `kedge-core`'s classifier is explicitly built not to do.

This crate is the other direction. A skill states its authority up front, and
anything not stated is refused. The two layers compose: keep the blocklist for
coarse run-wide bans, use a manifest to scope one skill.

## The manifest

```toml
[skill]
name    = "rust-test-repair"
version = "0.1.0"

[capabilities.filesystem]
read  = ["${workspace}/**"]
write = ["${workspace}/src/**", "${workspace}/tests/**"]

[capabilities.process]
allow = ["cargo check", "cargo test"]

# network and secrets are omitted, so both are denied.
```

There is no wildcard shorthand and no "allow everything" switch. An absent
section grants nothing, an empty list grants nothing, and an unresolved
`${variable}` is a hard error rather than an empty string — expanding
`${workspace}/**` into `/**` would silently grant the entire disk.

**Write does not imply read.** A skill that reads a file before rewriting it
needs both grants. The manifest is meant to be an exact statement of authority,
and conveniences like implied grants are what make an audit of one untrustworthy.

## Usage

```rust
let vars     = HashMap::from([("workspace".into(), "/repo".into())]);
let manifest = Arc::new(Manifest::from_toml_file("skill.toml", &vars)?);
let guard    = SkillGuard::new(manifest, "/repo", tools);

// ... run the agent with `guard` as its ToolExecutor ...

let c = guard.conformance();
println!("{}", c.report(guard.manifest()));
if !c.conforms() { std::process::exit(1); }
```

```text
kedge-skill — conformance for `rust-test-repair` v0.1.0

  5 call(s), 5 permitted, 0 refused

  ✔ conforms — every call stayed inside the manifest

  ⚠ 2 declared entr(ies) never exercised — over-permission:
      filesystem.write `/repo/tests/**`
      process `cargo check`
```

## The two questions it answers

**Did the skill stay inside its manifest?** Enforcement is a hard gate — a
refused call never reaches the executor. The tests assert that against a
recording executor rather than against the returned error, because a guard that
runs the call and *then* returns an error looks identical from the outside.

**Was the manifest bigger than the skill needed?** This is the half a blocklist
cannot do at all. Least privilege is normally aspirational because nobody knows
the true minimum authority a task requires. Running the task under a generous
manifest and recording what was actually exercised is how you find out;
`Conformance::minimized()` then writes it down:

```toml
# Minimized from an observed run: every entry below was exercised.
[skill]
name    = "rust-test-repair"
version = "0.2.0"

[capabilities.filesystem]
read = [
  "/repo/Cargo.toml",
  "/repo/src/lib.rs",
]
write = [
  "/repo/src/lib.rs",
]

[capabilities.process]
allow = [
  "cargo test",
]
```

Every entry is a literal subject that actually ran — no clustering, no inferred
prefixes. Widening it back into globs is a judgement call with real security
consequences, so it stays a human's to make. The gap between declared and
exercised is the number worth driving down: unused authority is exactly what an
injected instruction gets to spend.

## Where the bodies are buried

An allow-list is only as good as the string it matches against, so the parts
most likely to be wrong get the most attention:

- **Path traversal is applied, not matched as text.** `/repo/../../etc/passwd`
  resolves to `/etc/passwd` before it is checked, so a `/repo/**` grant does not
  cover it. A `..` that would climb above the root is **rejected** rather than
  clamped — clamping turns an escape attempt into a plausible in-bounds path.
- **Symlinks are resolved** by canonicalizing the longest existing ancestor
  (the write target usually doesn't exist yet, so the whole path can't be).
  Manifest patterns get their literal head canonicalized too, so both sides move
  together and a pattern written against a symlinked name keeps matching.
- **Globs are anchored at both ends.** `/repo/**` does not match
  `/repository/secret`. They compile to regex rather than a hand-rolled matcher,
  because a subtly wrong matcher in an allow-list is a silent bypass.
- **Commands match on argv boundaries**, and any shell metacharacter is an
  outright deny. An allow-entry of `cargo test` says nothing about
  `cargo test; rm -rf /`, and a prefix match would happily approve it.
- **URL userinfo can't forge a host.** `https://api.good.com@evil.com/` reaches
  evil.com, so the host is taken after the *last* `@`.
- **An effect that can't be named can't be granted.** A mutating call carrying
  no path, command, URL or credential in its arguments is refused, not waved
  through. Otherwise the allow-list quietly degrades into a blocklist of
  argument names.

## Scope, honestly

This is a **user-space, argument-level** check. It sees the tool calls an agent
makes and the arguments it passes, and nothing else.

- It is **not a sandbox**. A tool that ignores its own arguments, or reaches the
  filesystem by some route the arguments don't describe, is invisible here.
  `kedge-probe` is the kernel-level layer for that.
- Symlinks resolve as they exist at check time, so there is **no TOCTOU
  guarantee**.
- The refusal of unnameable effects will occasionally refuse something
  legitimate. The fix is to make the tool describe itself, not to loosen the
  check.

## License

[Business Source License 1.1](../../LICENSE) — source-available; converts to
Apache-2.0 on the Change Date.
