#!/usr/bin/env bash
# Publish the workspace to crates.io, in dependency order, without lying about
# what it did.
#
#   scripts/publish.sh              # dry run: plan only, publishes nothing
#   scripts/publish.sh --execute    # actually publish
#
# Dry run is the default because a crates.io publish is permanent. A version can
# be yanked, never replaced.
#
# ── What the previous version of this script got wrong ────────────────────────
#
# It kept a hand-written ORDER=() array, which had drifted to miss five
# publishable crates, and its "already published?" check asked whether the crate
# *existed* on crates.io. Every crate here has existed since 0.1.0, so on a
# version bump the check matched everything and the script skipped the entire
# workspace and reported success. A publish script that silently publishes
# nothing is worse than no script.
#
# Both facts now come from `cargo metadata`, so the list cannot drift from the
# workspace, and the check is per *version*.
#
# ── The blocker it never noticed ──────────────────────────────────────────────
#
# `kedge` depends on `kedge-skill` and `kedge-forge`, both `publish = false`.
# cargo refuses to publish a crate whose dependencies are not on the registry,
# so `cargo publish -p kedge` cannot succeed no matter how many times it is
# retried. That is why 0.4.0 is not on crates.io. The script now says so up
# front instead of failing twenty attempts deep.

set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

EXECUTE=0
[[ "${1:-}" == "--execute" ]] && EXECUTE=1

UA="kedge-publish (noel@nlj.dev)"
log() { printf '[%s] %s\n' "$(date -u '+%H:%M:%S')" "$*"; }

# ── plan: order, versions, and blockers, all from the workspace ───────────────
# The planner is a separate file so a crash in it cannot be mistaken for an
# empty plan. An earlier version embedded it, the quoting broke, python exited
# non-zero, and this script cheerfully reported "nothing to publish".
if ! PLAN=$(cargo metadata --no-deps --format-version 1 | python3 scripts/publish_plan.py); then
  log "the publish planner failed; refusing to guess what to publish"
  exit 1
fi
if [[ -z "$PLAN" ]]; then
  log "the planner produced an empty plan, which should be impossible; stopping"
  exit 1
fi

BLOCKED=$(grep -c '^BLOCKED' <<<"$PLAN" || true)
if (( BLOCKED > 0 )); then
  log "cannot publish this workspace:"
  grep '^BLOCKED' <<<"$PLAN" | while IFS=$'\t' read -r _ crate version deps; do
    log "  $crate $version depends on unpublishable: $deps"
  done
  log ""
  log "cargo will not publish a crate whose dependencies are not on crates.io."
  log "Either set publish = true on those crates and release them first, or"
  log "ship this version as binaries only (see .github/workflows/release.yml)."
  exit 1
fi

# ── is this exact version already up? ────────────────────────────────────────
version_live() { # crate, version
  curl -s --max-time 15 -H "User-Agent: $UA" \
    "https://crates.io/api/v1/crates/$1/$2" 2>/dev/null | grep -q '"version"'
}

TODO=()
while IFS=$'\t' read -r kind crate version; do
  [[ "$kind" == "PUBLISH" ]] || continue
  if version_live "$crate" "$version"; then
    log "$crate $version already on crates.io — skipping"
  else
    TODO+=("$crate=$version")
  fi
done <<<"$PLAN"

if (( ${#TODO[@]} == 0 )); then
  log "nothing to do: every crate is already published at its current version"
  exit 0
fi

log "${#TODO[@]} crate(s) to publish, in order:"
for e in "${TODO[@]}"; do log "  ${e/=/ }"; done

if (( ! EXECUTE )); then
  log ""
  log "dry run. Re-run with --execute to publish for real."
  exit 0
fi

# ── publish, waiting out the rate limit ──────────────────────────────────────
for e in "${TODO[@]}"; do
  crate="${e%%=*}"; version="${e##*=}"
  published=0
  # crates.io rate-limits new versions by time, so retrying is the whole game.
  for attempt in $(seq 1 20); do
    out=$(cargo publish -p "$crate" 2>&1)
    if grep -qE '^ *(Published|Uploaded)' <<<"$out" || version_live "$crate" "$version"; then
      log "PUBLISHED $crate $version"
      published=1
      break
    fi
    if grep -q '429 Too Many Requests' <<<"$out"; then
      log "$crate rate-limited (attempt $attempt) — sleeping 120s"
      sleep 120
      continue
    fi
    log "FAILED $crate — not a rate limit:"
    tail -6 <<<"$out" | sed 's/^/    /'
    break
  done
  # Stop on a real failure: everything after this depends on it.
  if (( ! published )); then
    log "stopping — later crates depend on $crate"
    exit 1
  fi
  sleep 15 # let the index catch up before a dependent verifies against it
done

log "done."
