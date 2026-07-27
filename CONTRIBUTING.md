# Contributing to Kedge

Thanks for your interest. Kedge is a solo-maintained, security-sensitive project
(it executes tool calls on behalf of language models), so contributions are held
to a high bar — but small, well-tested changes are very welcome.

## Prerequisites

- A recent stable Rust toolchain (see `rust-toolchain.toml`).
- `rustfmt` and `clippy` components: `rustup component add rustfmt clippy`.

## Before you open a PR

CI runs all of the following with warnings denied — run them locally first:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

Wire up the pre-push hook once per clone and the first of those runs itself:

```bash
git config core.hooksPath .githooks
```

It only checks formatting. That is the gate that is instant to run and the one
that has actually broken `main` here, while clippy and the test suite take long
enough that a hook running them gets bypassed within a day. Use
`git push --no-verify` when you mean it.

If your change touches `kedge-core`, keep it **wasm-clean** (no native-only
dependencies) — the browser demo compiles it to `wasm32-unknown-unknown`:

```bash
rustup target add wasm32-unknown-unknown
cargo build -p kedge-core --target wasm32-unknown-unknown
```

## Expectations

- **Every behavior change ships with a test.** Security-relevant changes ship with
  a regression test that encodes the exact case being fixed.
- **Fail safe.** When in doubt, the default must be the more restrictive one
  (intercept, deny, confine) — see `SECURITY.md` for the threat model.
- **Keep the audit trail honest.** Don't add silent `let _ = ...` drops on
  security-relevant journal writes; surface failures.
- **No secrets in the tree.** API keys are read from the environment by *name*
  (`--api-key-env`), never written to config, code, logs, or the ledger.
- Match the surrounding style: doc-comment the *why*, not just the *what*.

## Reporting a vulnerability

Please do **not** open a public issue for anything exploitable — see
[`SECURITY.md`](SECURITY.md) for private disclosure.
