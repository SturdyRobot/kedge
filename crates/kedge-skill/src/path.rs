//! Turning a path *as an agent wrote it* into the one string an allow-list can
//! safely be matched against.
//!
//! This is where allow-lists usually break. A manifest granting `/repo/**` is
//! worthless if the agent can pass `/repo/../../etc/passwd`, or a relative
//! `../../etc/passwd`, or reach an existing symlink that points outside the
//! workspace. Matching the raw string is a bypass; matching the *resolved*
//! string is the boundary.
//!
//! Two passes, in this order:
//!
//! 1. **Lexical** — join relative paths onto the base, drop `.`, and pop on
//!    `..`. A `..` that would escape the root is rejected outright rather than
//!    clamped, because clamping silently turns an escape attempt into a
//!    plausible in-bounds path.
//! 2. **Symlink** — canonicalize the longest ancestor that actually exists, then
//!    re-append the remainder. Canonicalizing the whole path is not an option:
//!    the target of a write usually does not exist yet.
//!
//! ## Known limit, stated plainly
//!
//! Pass 2 only defeats symlinks that exist *at check time*. A skill that can
//! write inside its granted tree can create a symlink there and then write
//! "through" it on a later call — the second write resolves to the real target
//! and is caught, but only because the link exists by then. There is no TOCTOU
//! guarantee here: this is a user-space check, not a kernel one. `kedge-probe`
//! is the layer that closes that gap.

use std::path::{Component, Path, PathBuf};

/// Resolve `raw` against `base` into an absolute path suitable for matching.
///
/// Returns `None` when the path escapes the filesystem root via `..`, which is
/// always a deny — there is no legitimate reason for a tool argument to do it.
pub fn resolve(base: &Path, raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }

    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        base.join(raw)
    };

    let lexical = lexical_normalize(&joined)?;
    Some(resolve_symlinks(&lexical))
}

/// Drop `.`, apply `..`, keep everything else. `None` if `..` escapes the root.
fn lexical_normalize(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth: usize = 0;

    for comp in p.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    // Above the root. Reject rather than clamp.
                    return None;
                }
                out.pop();
                depth -= 1;
            }
            Component::Normal(seg) => {
                out.push(seg);
                depth += 1;
            }
        }
    }

    Some(out)
}

/// Canonicalize the longest existing ancestor, then re-append the rest.
fn resolve_symlinks(p: &Path) -> PathBuf {
    for ancestor in p.ancestors() {
        let Ok(real) = ancestor.canonicalize() else {
            continue;
        };
        // `ancestors()` yields `p` first, so `strip_prefix` cannot fail here.
        return match p.strip_prefix(ancestor) {
            // The whole path existed and canonicalized. Joining an empty
            // remainder would append a trailing separator and break matching.
            Ok(rest) if rest.as_os_str().is_empty() => real,
            Ok(rest) => real.join(rest),
            Err(_) => real,
        };
    }
    p.to_path_buf()
}

/// Canonicalize the literal prefix of a *pattern* — everything before the first
/// glob metacharacter — so a manifest written as `/tmp/**` still matches a
/// resolved `/private/tmp/x` on macOS.
///
/// The glob portion is left exactly as written; only the fixed head moves.
pub fn canonicalize_pattern_head(pattern: &str) -> String {
    let head_end = pattern
        .find(['*', '?'])
        .map(|i| pattern[..i].rfind('/').map(|s| s + 1).unwrap_or(0))
        .unwrap_or(pattern.len());

    let (head, tail) = pattern.split_at(head_end);
    if head.is_empty() || !Path::new(head).is_absolute() {
        return pattern.to_string();
    }

    // Trailing `/` is stripped for canonicalize, then restored.
    let trimmed = head.trim_end_matches('/');
    match Path::new(trimmed).canonicalize() {
        Ok(real) => {
            let mut s = real.to_string_lossy().into_owned();
            if head.ends_with('/') && !s.ends_with('/') {
                s.push('/');
            }
            s.push_str(tail);
            s
        }
        Err(_) => pattern.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(base: &str, raw: &str) -> Option<String> {
        resolve(Path::new(base), raw).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn relative_paths_join_onto_the_base() {
        assert_eq!(r("/repo", "src/main.rs").unwrap(), "/repo/src/main.rs");
        assert_eq!(r("/repo", "./src/main.rs").unwrap(), "/repo/src/main.rs");
    }

    #[test]
    fn traversal_is_applied_not_matched_as_text() {
        // The whole point. A manifest granting `/repo/**` must not be satisfied
        // by a string that merely *starts* with `/repo/`.
        //
        // Every path here is deliberately one that does not exist, so this
        // asserts the lexical pass alone. Using something real like `/etc`
        // would make the expected value platform-dependent — on macOS `/etc`
        // is a symlink to `/private/etc`, and pass 2 would resolve it.
        assert_eq!(r("/repo", "src/../../loot/x").unwrap(), "/loot/x");
        assert_eq!(r("/repo", "/repo/../loot/x").unwrap(), "/loot/x");
        assert_eq!(r("/repo", "../loot/x").unwrap(), "/loot/x");
    }

    #[test]
    fn escaping_the_root_is_rejected_not_clamped() {
        // Clamping would turn `/../../..` into `/`, which then matches a `/**`
        // grant. Refusing to produce a path at all is the fail-safe answer.
        assert!(r("/", "../../etc/passwd").is_none());
        assert!(r("/repo", "../../../../../../etc/passwd").is_none());
    }

    #[test]
    fn an_empty_path_is_never_resolvable() {
        assert!(r("/repo", "").is_none());
    }

    #[test]
    fn absolute_paths_ignore_the_base() {
        assert_eq!(r("/repo", "/loot/x").unwrap(), "/loot/x");
    }

    #[test]
    fn a_fully_existing_path_resolves_without_a_trailing_separator() {
        // Regression: when the whole path canonicalizes, the remainder to
        // re-append is empty, and `join("")` used to add a trailing `/` — which
        // then failed to match any glob written the normal way.
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let resolved = resolve(Path::new("/repo"), &dir.to_string_lossy()).unwrap();
        assert_eq!(resolved, dir);
        assert!(!resolved.to_string_lossy().ends_with('/'));
    }

    #[test]
    fn a_pattern_and_a_path_agree_after_both_pass_through_a_symlink() {
        // The two halves have to move together. If only paths were resolved,
        // a manifest written against the symlinked name would stop matching.
        #[cfg(unix)]
        {
            let root = std::env::temp_dir().join("kedge-skill-agree");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("real")).unwrap();
            let link = root.join("link");
            std::os::unix::fs::symlink(root.join("real"), &link).unwrap();

            let pattern = canonicalize_pattern_head(&format!("{}/**", link.to_string_lossy()));
            let resolved = resolve(&root, "link/src/main.rs").unwrap();

            let g = crate::glob::Glob::new(&pattern).unwrap();
            assert!(
                g.is_match(&resolved.to_string_lossy()),
                "pattern {pattern} did not match {resolved:?}"
            );

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn an_existing_symlink_resolves_to_its_target() {
        let dir = std::env::temp_dir().join("kedge-skill-symlink-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inside")).unwrap();
        std::fs::create_dir_all(dir.join("outside")).unwrap();

        #[cfg(unix)]
        {
            let link = dir.join("inside/escape");
            std::os::unix::fs::symlink(dir.join("outside"), &link).unwrap();

            let resolved = resolve(&dir, "inside/escape/loot.txt").unwrap();
            let real_outside = dir.join("outside").canonicalize().unwrap();

            // Resolved through the link: a grant on `inside/**` will not match.
            assert!(resolved.starts_with(&real_outside), "got {resolved:?}");
            assert!(resolved.ends_with("loot.txt"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pattern_head_canonicalizes_but_the_glob_survives() {
        let dir = std::env::temp_dir();
        let pattern = format!("{}/kedge-skill-x/**", dir.to_string_lossy());
        let out = canonicalize_pattern_head(&pattern);
        // Whatever the head resolved to, the glob tail is untouched.
        assert!(out.ends_with("/kedge-skill-x/**"), "got {out}");
    }

    #[test]
    fn a_pattern_with_no_glob_is_left_alone_when_it_does_not_exist() {
        let p = "/definitely/not/here/file.txt";
        assert_eq!(canonicalize_pattern_head(p), p);
    }
}
