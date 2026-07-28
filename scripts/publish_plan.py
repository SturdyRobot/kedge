#!/usr/bin/env python3
"""Work out what publish.sh should publish, and in what order.

Reads `cargo metadata` on stdin, writes tab-separated lines on stdout:

    BLOCKED\tcrate\tversion\tunpublishable-deps
    PUBLISH\tcrate\tversion
    CYCLE\tcrates

Kept in its own file rather than embedded in the shell script. It started
embedded, the quoting mangled it, python exited non-zero, and the shell read an
empty plan as "nothing to publish" and exited 0. A publish tool that reports
success when its planner crashed is the exact failure the tool exists to
prevent, so the two halves are now separately runnable and separately testable.
"""

import json
import sys


def main() -> int:
    meta = json.load(sys.stdin)
    pkgs = {p["name"]: p for p in meta["packages"]}

    # `publish = false` in Cargo.toml arrives as an empty list.
    nopub = {n for n, p in pkgs.items() if p.get("publish") == []}
    pubable = [n for n in pkgs if n not in nopub]

    # cargo will not publish a crate whose dependencies are absent from the
    # registry, so a publishable crate depending on an unpublishable one can
    # never go out. Say so before anything is attempted.
    #
    # Dev-dependencies do not count. cargo strips a path-only dev-dependency
    # when packaging, because it is not needed to build the library, so
    # kedge-forge can dev-depend on the unpublishable kedge-bench and still go
    # out. The first version of this check ignored `kind` and refused to publish
    # the workspace over exactly that, which is the same false-blocker class it
    # was written to catch.
    def blocking_deps(name: str) -> list[str]:
        return sorted(
            {
                d["name"]
                for d in pkgs[name]["dependencies"]
                if d["name"] in nopub and d.get("kind") in (None, "build")
            }
        )

    for name in sorted(pubable):
        blockers = blocking_deps(name)
        if blockers:
            print(f"BLOCKED\t{name}\t{pkgs[name]['version']}\t{','.join(blockers)}")

    # Dependency order: a crate ships only once everything it needs is up.
    shipped: set[str] = set()
    while len(shipped) < len(pubable):
        ready = sorted(
            n
            for n in pubable
            if n not in shipped
            and all(
                d["name"] not in pubable
                or d["name"] in shipped
                # A dev-dependency is stripped at publish time, so it does not
                # have to be on the registry first. Including it here would
                # deadlock any pair of crates that dev-depend on each other.
                or d.get("kind") == "dev"
                for d in pkgs[n]["dependencies"]
            )
        )
        if not ready:
            # cargo would have rejected a real cycle; do not spin on one here.
            print("CYCLE\t" + ",".join(sorted(set(pubable) - shipped)))
            return 2
        for name in ready:
            shipped.add(name)
            print(f"PUBLISH\t{name}\t{pkgs[name]['version']}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
