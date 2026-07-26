//! The task suite: 20 repair tasks across 4 families.
//!
//! A family is one fixture crate — the unit a skill is learned per. Five
//! independent breakages per fixture, each targeting a different function, so a
//! skill learned from four of them has a fifth to be tested against.
//!
//! Every breakage below is a *hypothesis* that it changes observable behaviour.
//! [`crate::checks::every_breakage_actually_breaks`] adjudicates. Several of the
//! obvious-looking candidates written while building this file turned out to be
//! no-ops — `>` to `>=` in a clamp, dropping a `#` comment check on a line with
//! no `=`, `skip(1)` over a line that was filtered anyway. All were caught by
//! that test rather than by inspection, which is the reason it exists.

use std::time::Duration;

use crate::{Acceptance, BenchSuite, BenchTask, Breakage};

const LIB: &str = "src/lib.rs";

const fn splice(find: &'static str, replace: &'static str) -> Breakage {
    Breakage::Splice {
        file: LIB,
        find,
        replace,
    }
}

const fn task(
    id: &'static str,
    family: &'static str,
    fixture: &'static str,
    goal: &'static str,
    breakage: Breakage,
) -> BenchTask {
    BenchTask {
        id,
        family,
        fixture,
        goal,
        breakage,
        acceptance: Acceptance::cargo_test(),
    }
}

/// The canonical suite. Order is fixed; reports join on `id`.
pub fn suite() -> BenchSuite {
    BenchSuite {
        name: "repair-v1",
        tasks: vec![
            // ── numeric-bounds (fixture: clamp) ──
            task(
                "clamp-001",
                "numeric-bounds",
                "clamp",
                "`clamp_upper` returns the wrong branch. Fix it so the test suite passes.",
                splice("if v > hi { hi } else { v }", "if v > hi { v } else { hi }"),
            ),
            task(
                "clamp-002",
                "numeric-bounds",
                "clamp",
                "`clamp_lower` compares in the wrong direction. Fix it so the test suite passes.",
                splice("if v < lo { lo } else { v }", "if v > lo { lo } else { v }"),
            ),
            task(
                "clamp-003",
                "numeric-bounds",
                "clamp",
                "`in_range` excludes its upper endpoint. Fix it so the test suite passes.",
                splice("v >= lo && v <= hi", "v >= lo && v < hi"),
            ),
            task(
                "clamp-004",
                "numeric-bounds",
                "clamp",
                "`clamp` passes its bounds to the wrong helpers. Fix it so the test suite passes.",
                splice(
                    "clamp_lower(clamp_upper(v, hi), lo)",
                    "clamp_lower(clamp_upper(v, lo), hi)",
                ),
            ),
            task(
                "clamp-005",
                "numeric-bounds",
                "clamp",
                "`span` is off by one — it should be inclusive. Fix it so the test suite passes.",
                splice("hi - lo + 1", "hi - lo"),
            ),
            // ── config-parse (fixture: kv) ──
            task(
                "kv-001",
                "config-parse",
                "kv",
                "Parsed keys keep their surrounding whitespace. Fix it so the test suite passes.",
                splice("line[..idx].trim().to_string()", "line[..idx].to_string()"),
            ),
            task(
                "kv-002",
                "config-parse",
                "kv",
                "Indented comments are not counted. Fix it so the test suite passes.",
                splice(
                    "l.trim().starts_with('#')",
                    "l.starts_with('#')",
                ),
            ),
            task(
                "kv-003",
                "config-parse",
                "kv",
                "Parsed values include the separator. Fix it so the test suite passes.",
                splice("line[idx + 1..].trim()", "line[idx..].trim()"),
            ),
            task(
                "kv-004",
                "config-parse",
                "kv",
                "`parse_all` drops leading lines. Fix it so the test suite passes.",
                splice(
                    "text.lines().filter_map(parse_line)",
                    "text.lines().skip(2).filter_map(parse_line)",
                ),
            ),
            task(
                "kv-005",
                "config-parse",
                "kv",
                "`get` returns the key instead of the value. Fix it so the test suite passes.",
                splice("map(|(_, v)| v.as_str())", "map(|(k, _)| k.as_str())"),
            ),
            // ── money-arithmetic (fixture: cart) ──
            task(
                "cart-001",
                "money-arithmetic",
                "cart",
                "`subtotal` adds where it should multiply. Fix it so the test suite passes.",
                splice("map(|(p, q)| p * q)", "map(|(p, q)| p + q)"),
            ),
            task(
                "cart-002",
                "money-arithmetic",
                "cart",
                "`discount` truncates instead of rounding half up. Fix it so the test suite passes.",
                splice(
                    "pub fn discount(amount: i64, bps: i64) -> i64 {\n    (amount * bps + 5_000) / 10_000",
                    "pub fn discount(amount: i64, bps: i64) -> i64 {\n    (amount * bps) / 10_000",
                ),
            ),
            task(
                "cart-003",
                "money-arithmetic",
                "cart",
                "`tax` rounds the wrong way. Fix it so the test suite passes.",
                splice(
                    "pub fn tax(amount: i64, bps: i64) -> i64 {\n    (amount * bps + 5_000) / 10_000",
                    "pub fn tax(amount: i64, bps: i64) -> i64 {\n    (amount * bps - 5_000) / 10_000",
                ),
            ),
            task(
                "cart-004",
                "money-arithmetic",
                "cart",
                "Tax is computed before the discount instead of after. Fix it so the test suite passes.",
                splice("after + tax(after, tax_bps)", "after + tax(sub, tax_bps)"),
            ),
            task(
                "cart-005",
                "money-arithmetic",
                "cart",
                "Free shipping misses the exact threshold. Fix it so the test suite passes.",
                splice("total_cents >= 5_000", "total_cents > 5_000"),
            ),
            // ── text-normalize (fixture: slug) ──
            task(
                "slug-001",
                "text-normalize",
                "slug",
                "Slugs gain a leading dash. Fix it so the test suite passes.",
                splice(
                    "let mut last_dash = true; // leading dashes are suppressed",
                    "let mut last_dash = false; // leading dashes are suppressed",
                ),
            ),
            task(
                "slug-002",
                "text-normalize",
                "slug",
                "`truncate` drops one character too many. Fix it so the test suite passes.",
                splice("s.chars().take(max).collect()", "s.chars().take(max - 1).collect()"),
            ),
            task(
                "slug-003",
                "text-normalize",
                "slug",
                "`is_valid` accepts the empty string. Fix it so the test suite passes.",
                splice(
                    "!s.is_empty() && s.chars().all(",
                    "s.chars().all(",
                ),
            ),
            task(
                "slug-004",
                "text-normalize",
                "slug",
                "`is_valid` allows the wrong separator. Fix it so the test suite passes.",
                splice("|| c == '-')", "|| c == '_')"),
            ),
            task(
                "slug-005",
                "text-normalize",
                "slug",
                "`dedupe_dashes` removes every dash, not just repeats. Fix it so the test suite passes.",
                splice(
                    "if c == '-' && out.ends_with('-') { continue; }",
                    "if c == '-' { continue; }",
                ),
            ),
        ],
    }
}

/// Budget for the whole suite, used by the timing check.
pub const SUITE_BUDGET: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suite_has_the_shape_the_slice_promised() {
        let s = suite();
        assert_eq!(s.tasks.len(), 20, "S1 acceptance requires >= 20 tasks");
        assert_eq!(
            s.families().len(),
            4,
            "S1 acceptance requires >= 3 families"
        );
        // Five per family, so a skill can be learned from four and tested on a
        // held-out fifth in Spike 002.
        for family in s.families() {
            let n = s.tasks.iter().filter(|t| t.family == family).count();
            assert_eq!(n, 5, "family `{family}` has {n} tasks, expected 5");
        }
    }
}
