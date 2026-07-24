# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html) from 1.0.0.

## [0.2.0] — 2026-07-24

### Security — hardening pass (~22 findings closed, each with a regression test)

- **Workspace / config trust.** An auto-discovered `./kedge.toml` in the working
  directory is now untrusted: its execution-sensitive fields (`mcp`, `api_base`,
  `api_key_env`, and an out-of-tree `db` path) are stripped with a warning, so a
  freshly-cloned repo can no longer turn `kedge run` into code execution or
  credential exfiltration. Those fields are honored only from a trusted source —
  `--config <path>`, the operator config dir (`$XDG_CONFIG_HOME/kedge` /
  `~/.config/kedge/kedge.toml`), or a CLI flag. Policy files load only from the
  operator dir, never the agent's target `--cwd`.
- **Process / key isolation.** Child processes spawned by tools get a scrubbed
  environment (allowlist only), and the harness removes its own LLM API key from
  its process environment after reading it — closing the `/proc/<ppid>/environ`
  read-back path.
- **Unified guard chain.** The CLI and MCP entry points build their tool executor
  through one `GuardChain`, so no path can drift to a weaker wiring. `kedge run`
  and `kedge resume` now default to shadow-audit (were unguarded); `kedge-policy`
  (blocked tools + PII redaction) is enforced on both paths.
- **Control API auth.** `kedge serve` requires a bearer token (`$KEDGE_SERVE_TOKEN`)
  for any non-loopback bind and refuses to start without one; all endpoints except
  `/health` require the token when set.
- **Fail-safe classification.** Tool classification scans every name token
  (deny-wins), the policy blocklist is case/whitespace-normalized, and MCP
  capability hints (`readOnlyHint`/`destructiveHint`) may only *increase*
  restriction — a hostile server cannot relabel a destructive tool read-only.
- **Journal integrity.** A security-relevant event (an interception or approval)
  that cannot be journaled now fails the run loudly instead of proceeding off the
  record.
- **File / path & resource confinement.** MCP `path`/`db` arguments are confined
  under an allowed root (`$KEDGE_MCP_ROOT`, else the server cwd); compaction reads
  are capped at 8 MiB, restricted to regular files, and depth-bounded; MCP
  tool-result (1 MiB), SSE buffer (8 MiB), and tool-description (2 KiB) sizes are
  capped; duplicate MCP tool names are de-duplicated with a warning; the AST cache
  keys on `(hash, lang, version)`; and an empty-metrics eval suite no longer
  vacuously passes.

### Changed

- Documentation: `kedge-probe` is described accurately as an **experimental,
  observe-only** eBPF prototype (it does not enforce and is not wired into a normal
  run) rather than as kernel-level containment.
