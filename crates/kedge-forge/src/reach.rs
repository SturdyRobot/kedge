//! **Reachable Authority** — how much a manifest can actually touch.
//!
//! This is the number the whole project rests on, and the obvious way to
//! measure it is wrong. Counting *declared entries* fails in exactly the case
//! that matters:
//!
//! ```toml
//! write = ["**"]                                  # 1 entry, the whole disk
//! write = ["src/a.rs", "src/b.rs", "src/c.rs"]    # 3 entries, three files
//! ```
//!
//! By entry count the first manifest is three times tighter than the second.
//! Anything optimizing that metric would learn to emit `**`, and it would
//! produce a very good-looking chart.
//!
//! So Reachable Authority walks the workspace and counts the files each grant
//! actually permits. Three rules keep it honest:
//!
//! - **[`Reach::escapes_root`] is a flag, never a score.** A manifest granting
//!   `/etc/**` touches zero files *inside* the workspace, which would read as
//!   maximally tight. Escaping is detected structurally and disqualifies a
//!   manifest from being scored as a reduction at all.
//! - **[`Reach::truncated`] denies.** Past `MAX_WALK` the counts are lower
//!   bounds, and an unknown is not an improvement.
//! - **A wildcard grant is never a reduction of a literal one**, even when both
//!   reach the same file count today. `write = ["/repo/**"]` and
//!   `write = ["/repo/a.rs"]` are identical in a directory holding one file, and
//!   diverge the moment a file is added — only one of them gained authority
//!   without anyone editing it.
//!
//! Commands and hosts are **reported here and compared elsewhere**. There is no
//! finite command set to walk, and counting allow-entries gets the direction
//! backwards: `["cargo"]` is one entry permitting every subcommand. The
//! promotion gate compares those by containment, using the manifests this type
//! does not hold.
//!
//! The measurement is **filesystem-dependent** by design — authority *is*
//! contextual — which means a `Reach` is only comparable to another `Reach`
//! computed on the same root at the same commit. Comparing across repos is
//! meaningless and must never be published as one number.

use std::path::{Path, PathBuf};

use kedge_skill::{Capability, Manifest};

/// Cap on entries visited. Past this the counts are lower bounds.
pub const MAX_WALK: usize = 50_000;

/// Directory names never descended into: build output and VCS metadata are not
/// part of a task's authority surface, and `.git` alone can dwarf a repo.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".kedge-target"];

/// What a manifest can reach, measured against a real directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reach {
    /// Files under the root the manifest would permit writing.
    pub writable: usize,
    /// Files under the root the manifest would permit reading.
    pub readable: usize,
    /// Distinct command allow-entries. **Reported, never compared.** There is no
    /// finite command set to enumerate, and counting entries gets the direction
    /// backwards: `["cargo"]` is one entry permitting every subcommand, while
    /// `["cargo check", "cargo test"]` is two permitting strictly less.
    /// Containment is the promotion gate's job — it has both manifests.
    pub commands: usize,
    /// Distinct network allow-entries. Reported, never compared, same reason.
    pub hosts: usize,
    /// Grants containing an unescaped wildcard — filesystem **and network**.
    ///
    /// A point-in-time file count cannot tell `write = ["/repo/a.rs"]` from
    /// `write = ["/repo/**"]` in a directory holding one file — both reach
    /// exactly one. They diverge the moment a file is added, and only one of
    /// them gained authority without anyone editing it. Counting wildcards is
    /// how that unbounded future authority stays visible.
    ///
    /// Network is included for the same reason and was initially missed:
    /// `allow = ["*.evil.com"]` reaches every subdomain that will ever exist,
    /// and the host *count* is 1.
    pub wildcard_grants: usize,
    /// A grant matches somewhere outside the root.
    pub escapes_root: bool,
    /// The walk hit [`MAX_WALK`]; `writable`/`readable` are lower bounds.
    pub truncated: bool,
    /// Files visited, so a reduction can be reported as a fraction.
    pub files_scanned: usize,
}

impl Reach {
    /// Whether `self` is a genuine filesystem reduction of `other`.
    ///
    /// Requires: no more writable files, no more readable files, no more
    /// wildcard grants, at least one of those strictly smaller, no escape, and
    /// no truncation on either side.
    ///
    /// **Named for what it actually covers.** Commands and hosts are *not*
    /// compared here, because `Reach` holds only counts and counting command
    /// entries gets the direction backwards — `["cargo"]` is one entry
    /// permitting every subcommand. The gate compares those by containment,
    /// using the manifests it has and this type does not. An earlier version
    /// silently included them and called a strictly narrower skill a widening.
    pub fn is_filesystem_reduction_of(&self, other: &Reach) -> bool {
        // An unknown is not an improvement.
        if self.truncated || other.truncated {
            return false;
        }
        // A manifest that reaches outside the workspace cannot be compared by
        // in-workspace counts at all.
        if self.escapes_root {
            return false;
        }
        let no_worse = self.writable <= other.writable
            && self.readable <= other.readable
            && self.wildcard_grants <= other.wildcard_grants;
        let somewhere_better = self.writable < other.writable
            || self.readable < other.readable
            || self.wildcard_grants < other.wildcard_grants;
        no_worse && somewhere_better
    }

    /// Writable files as a fraction of everything scanned, for reports.
    pub fn writable_fraction(&self) -> f64 {
        if self.files_scanned == 0 {
            return 0.0;
        }
        self.writable as f64 / self.files_scanned as f64
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} writable, {} readable of {} file(s); {} command(s), {} host(s)",
            self.writable, self.readable, self.files_scanned, self.commands, self.hosts
        );
        if self.escapes_root {
            s.push_str("  ⚠ ESCAPES ROOT");
        }
        if self.truncated {
            s.push_str("  ⚠ TRUNCATED (counts are lower bounds)");
        }
        s
    }
}

/// Measure what `manifest` can reach under `root`.
pub fn reach(manifest: &Manifest, root: &Path) -> std::io::Result<Reach> {
    // Both sides must move together. `Manifest` canonicalizes the literal head
    // of each pattern when it compiles, so on macOS a grant written against
    // `/var/folders/…` is stored as `/private/var/folders/…`. Walking the
    // uncanonicalized root produced paths that matched nothing at all and
    // reported a wide-open manifest as reaching **zero** files — which would
    // have read as perfect least privilege.
    let root = &root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut r = Reach {
        writable: 0,
        readable: 0,
        commands: 0,
        hosts: 0,
        escapes_root: escapes_root(manifest, root),
        truncated: false,
        files_scanned: 0,
        wildcard_grants: manifest
            .declared()
            .iter()
            .filter(|(kind, pattern)| {
                (kind.starts_with("filesystem.") || *kind == "network") && has_wildcard(pattern)
            })
            .count(),
    };

    for (kind, _) in manifest.declared() {
        match kind {
            "process" => r.commands += 1,
            "network" => r.hosts += 1,
            _ => {}
        }
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if r.files_scanned >= MAX_WALK {
            r.truncated = true;
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // An unreadable directory is not an authority signal; skip it rather
            // than failing the whole measurement.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // `symlink_metadata` so a link is never followed out of the tree and
            // never double-counts its target.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                let skip = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| SKIP_DIRS.contains(&n));
                if !skip {
                    stack.push(path);
                }
                continue;
            }

            r.files_scanned += 1;
            if r.files_scanned > MAX_WALK {
                r.truncated = true;
                break;
            }
            if manifest.permits(&Capability::FsRead(path.clone())) {
                r.readable += 1;
            }
            if manifest.permits(&Capability::FsWrite(path)) {
                r.writable += 1;
            }
        }
    }

    Ok(r)
}

/// Whether any filesystem grant can match outside `root`.
///
/// Structural, not probe-based: take each pattern's literal head — everything
/// before the first glob character, trimmed back to a path separator — and check
/// it is inside the root. A bare `**` has an empty head and escapes; so does
/// `/etc/**`. Anything the analysis cannot place is treated as escaping, because
/// the fail-safe answer to "can this reach outside?" is yes.
fn escapes_root(manifest: &Manifest, root: &Path) -> bool {
    // Canonicalized for the same reason the walk is: the manifest's patterns
    // already are, so an uncanonicalized root would call every grant an escape.
    let root = &root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    manifest
        .declared()
        .into_iter()
        .filter(|(kind, _)| kind.starts_with("filesystem."))
        .any(|(_, pattern)| !head_is_inside(&pattern, root))
}

/// Whether a pattern contains an **unescaped** wildcard.
///
/// `\*` is a literal asterisk in a filename, not a grant over everything.
fn has_wildcard(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '*' | '?' => return true,
            _ => i += 1,
        }
    }
    false
}

fn head_is_inside(pattern: &str, root: &Path) -> bool {
    let head_end = match pattern.find(['*', '?']) {
        Some(i) => pattern[..i].rfind('/').map(|s| s + 1).unwrap_or(0),
        None => pattern.len(),
    };
    let head = &pattern[..head_end];
    if head.is_empty() {
        return false;
    }
    let head = PathBuf::from(head);
    if !head.is_absolute() {
        // A relative grant resolves against something this function cannot see.
        return false;
    }
    head.starts_with(root)
}

/// The manifest a general-purpose agent effectively runs with today: the whole
/// workspace, readable and writable.
///
/// This is the honest baseline for the comparison, because it is what actually
/// happens — permissions are configured per *agent*, not per *task*, so an
/// assistant that might need to edit any file is given the authority to edit
/// every file.
pub fn general_agent_manifest(root: &Path, commands: &[&str]) -> String {
    let root = root.to_string_lossy();
    let mut s = String::from(
        "# The authority a general-purpose agent runs with: the whole workspace.\n\
         [skill]\nname    = \"general-agent\"\nversion = \"0.0.0\"\n\n\
         [capabilities.filesystem]\n",
    );
    s.push_str(&format!(
        "read  = [\"{root}/**\"]\nwrite = [\"{root}/**\"]\n"
    ));
    if !commands.is_empty() {
        s.push_str("\n[capabilities.process]\nallow = [");
        for c in commands {
            s.push_str(&format!("\n  {c:?},"));
        }
        s.push_str("\n]\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for (p, n) in [
            ("Cargo.toml", "c"),
            ("src/lib.rs", "l"),
            ("src/secrets.rs", "s"),
            ("tests/it.rs", "t"),
        ] {
            std::fs::write(root.join(p), n).unwrap();
        }
        // Must be skipped, or the count is dominated by VCS internals.
        std::fs::write(root.join(".git/config"), "x").unwrap();
        d
    }

    fn compile(toml: &str) -> Manifest {
        Manifest::from_toml_str(toml, &HashMap::new()).unwrap()
    }

    fn narrow(root: &Path) -> Manifest {
        compile(&format!(
            "[skill]\nname=\"n\"\nversion=\"0.1.0\"\n\
             [capabilities.filesystem]\nread=[\"{r}/src/lib.rs\"]\nwrite=[\"{r}/src/lib.rs\"]\n",
            r = root.to_string_lossy()
        ))
    }

    fn wide(root: &Path) -> Manifest {
        compile(&general_agent_manifest(root, &[]))
    }

    #[test]
    fn a_wildcard_grant_counts_every_file_not_one_entry() {
        // The whole reason this metric exists. By declared-entry count the wide
        // manifest is *smaller* than the narrow one is by file count.
        let d = tree();
        let w = reach(&wide(d.path()), d.path()).unwrap();
        let n = reach(&narrow(d.path()), d.path()).unwrap();

        assert_eq!(w.files_scanned, 4, "`.git` was not skipped");
        assert_eq!(w.writable, 4);
        assert_eq!(n.writable, 1);
        assert!(n.is_filesystem_reduction_of(&w));
        assert!(!w.is_filesystem_reduction_of(&n));
    }

    #[test]
    fn a_grant_outside_the_root_is_flagged_and_never_scores_as_a_reduction() {
        // Touches zero files inside the workspace, so by count alone it would
        // look maximally tight.
        let d = tree();
        let escaping = compile(
            "[skill]\nname=\"e\"\nversion=\"0.1.0\"\n\
             [capabilities.filesystem]\nread=[\"/etc/**\"]\nwrite=[\"/etc/**\"]\n",
        );
        let r = reach(&escaping, d.path()).unwrap();
        assert!(r.escapes_root);
        assert_eq!(r.writable, 0, "it really does look tight by count");
        assert!(!r.is_filesystem_reduction_of(&reach(&wide(d.path()), d.path()).unwrap()));

        // And a bare `**`, whose head is empty.
        let bare = compile(
            "[skill]\nname=\"b\"\nversion=\"0.1.0\"\n\
             [capabilities.filesystem]\nread=[\"**\"]\n",
        );
        assert!(reach(&bare, d.path()).unwrap().escapes_root);
    }

    #[test]
    fn a_truncated_walk_is_never_an_improvement() {
        let d = tree();
        let mut a = reach(&narrow(d.path()), d.path()).unwrap();
        let b = reach(&wide(d.path()), d.path()).unwrap();
        assert!(a.is_filesystem_reduction_of(&b));

        a.truncated = true;
        assert!(
            !a.is_filesystem_reduction_of(&b),
            "an unknown was scored as better"
        );

        let mut b2 = b;
        b2.truncated = true;
        assert!(!reach(&narrow(d.path()), d.path())
            .unwrap()
            .is_filesystem_reduction_of(&b2));
    }

    #[test]
    fn commands_are_not_compared_here_but_wildcards_are() {
        let base = Reach {
            writable: 100,
            readable: 100,
            commands: 1,
            hosts: 0,
            escapes_root: false,
            truncated: false,
            files_scanned: 100,
            wildcard_grants: 1,
        };
        // Red-team A3: MORE command entries is not more authority, and this
        // type must not claim otherwise. Commands are the gate's business.
        let more_commands = Reach {
            writable: 3,
            readable: 3,
            commands: 2,
            ..base
        };
        assert!(
            more_commands.is_filesystem_reduction_of(&base),
            "command entry count leaked back into the filesystem comparison"
        );

        // Red-team A4: a wildcard grant is unbounded future authority even when
        // it reaches the same number of files today.
        let same_files_but_globbed = Reach {
            writable: 3,
            readable: 3,
            wildcard_grants: 2,
            ..base
        };
        assert!(!same_files_but_globbed.is_filesystem_reduction_of(&base));
    }

    #[test]
    fn an_identical_manifest_is_not_a_reduction_of_itself() {
        let d = tree();
        let r = reach(&narrow(d.path()), d.path()).unwrap();
        assert!(
            !r.is_filesystem_reduction_of(&r),
            "equal must not count as better"
        );
    }

    #[test]
    fn symlinks_are_not_followed_out_of_the_tree() {
        #[cfg(unix)]
        {
            let d = tree();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("loot.txt"), "x").unwrap();
            std::os::unix::fs::symlink(outside.path(), d.path().join("escape")).unwrap();

            let r = reach(&wide(d.path()), d.path()).unwrap();
            // Still the original four; the link's target was not walked.
            assert_eq!(r.files_scanned, 4, "a symlink was followed");
        }
    }
}
