//! Path globs, compiled to anchored regexes.
//!
//! Deliberately *not* a general glob library. A capability manifest is a security
//! boundary, so the grammar is small enough to state completely:
//!
//! | Pattern | Matches                                        |
//! |---------|------------------------------------------------|
//! | `**`    | any sequence of characters, including `/`      |
//! | `*`     | any sequence of characters, **except** `/`     |
//! | `?`     | exactly one character, except `/`              |
//! | *other* | itself, literally (regex-escaped)              |
//!
//! Patterns are anchored at both ends: `/repo/**` matches `/repo/src/main.rs`
//! but not `/repository/x` and not `/repo` itself.
//!
//! We compile to [`regex`] rather than hand-rolling a matcher on purpose — a
//! subtly wrong matcher in an allow-list is a silent bypass, and the regex
//! engine is far better tested than anything written here would be.

use regex::Regex;

/// A compiled path pattern.
#[derive(Debug, Clone)]
pub struct Glob {
    source: String,
    re: Regex,
}

impl Glob {
    /// Compile a glob. Fails only if the escaped output is somehow not a valid
    /// regex, which the escaping should make impossible — it is surfaced rather
    /// than unwrapped so a manifest can never silently compile to "matches all".
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        Ok(Glob {
            source: pattern.to_string(),
            re: Regex::new(&translate(pattern))?,
        })
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.re.is_match(path)
    }

    /// The pattern as written, for reporting.
    pub fn as_str(&self) -> &str {
        &self.source
    }
}

/// Translate a glob to an anchored regex.
fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2 + 4);
    out.push('^');

    let bytes: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            '*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == '*' {
                    out.push_str(".*");
                    i += 2;
                } else {
                    out.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            c => {
                // Escape everything else. `regex::escape` on a single char is
                // exact and leaves no metacharacter unescaped.
                out.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }

    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, path: &str) -> bool {
        Glob::new(pattern).unwrap().is_match(path)
    }

    #[test]
    fn double_star_crosses_separators_and_single_star_does_not() {
        assert!(m("/repo/**", "/repo/src/main.rs"));
        assert!(m("/repo/*", "/repo/main.rs"));
        assert!(!m("/repo/*", "/repo/src/main.rs"));
        assert!(m("/repo/**/*.rs", "/repo/a/b/c.rs"));
    }

    #[test]
    fn patterns_are_anchored_at_both_ends() {
        // The bug that makes an allow-list useless: a prefix match.
        assert!(!m("/repo/**", "/repository/secret"));
        assert!(!m("/repo/**", "/tmp/repo/x"));
        assert!(!m("/repo/*.rs", "/repo/main.rs.bak"));
        // A directory pattern does not grant the directory entry itself.
        assert!(!m("/repo/**", "/repo"));
    }

    #[test]
    fn metacharacters_in_a_pattern_are_literal() {
        // Without escaping, `.` would match any character and `a.txt` would
        // grant `axtxt`. The `+` and `(` cases would fail to compile at all.
        assert!(m("/repo/a.txt", "/repo/a.txt"));
        assert!(!m("/repo/a.txt", "/repo/axtxt"));
        assert!(m("/repo/a+b(c).txt", "/repo/a+b(c).txt"));
        assert!(!m("/repo/a+b(c).txt", "/repo/aab(c).txt"));
    }

    #[test]
    fn a_bare_double_star_grants_everything_and_says_so() {
        // Legal, but it is the loosest possible grant. The conformance report
        // is what makes this visible rather than accidental.
        assert!(m("**", "/etc/passwd"));
        assert!(m("**", "/repo/src/main.rs"));
    }

    #[test]
    fn an_empty_pattern_grants_nothing_but_the_empty_path() {
        assert!(!m("", "/anything"));
        assert!(m("", ""));
    }
}
