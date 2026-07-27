//! URL slugs. Text munging, where off-by-one and empty input both bite.

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // leading dashes are suppressed
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') { out.pop(); }
    out
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_string(); }
    s.chars().take(max).collect()
}

pub fn is_valid(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn dedupe_dashes(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c == '-' && out.ends_with('-') { continue; }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_joins_with_dashes() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Rust  is   Great"), "rust-is-great");
    }

    #[test]
    fn slugify_trims_both_ends() {
        assert_eq!(slugify("  Hi!  "), "hi");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn truncate_is_a_character_count_not_a_byte_count() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn validity_rejects_empty_and_uppercase() {
        assert!(is_valid("a-1"));
        assert!(!is_valid(""));
        assert!(!is_valid("Abc"));
        assert!(!is_valid("a_b"));
    }

    #[test]
    fn dedupe_collapses_runs_of_dashes() {
        assert_eq!(dedupe_dashes("a--b---c"), "a-b-c");
        assert_eq!(dedupe_dashes("-a-"), "-a-");
    }
}
