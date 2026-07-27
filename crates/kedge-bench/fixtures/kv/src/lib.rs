//! A tiny `key=value` config parser.

pub fn parse_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') { return None; }
    let idx = line.find('=')?;
    Some((line[..idx].trim().to_string(), line[idx + 1..].trim().to_string()))
}

pub fn parse_all(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(parse_line).collect()
}

pub fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub fn count_comments(text: &str) -> usize {
    text.lines().filter(|l| l.trim().starts_with('#')).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_pair_parses_and_is_trimmed() {
        assert_eq!(parse_line("  host = localhost  "), Some(("host".into(), "localhost".into())));
    }

    #[test]
    fn comments_and_blanks_are_skipped() {
        assert_eq!(parse_line("# nope"), None);
        assert_eq!(parse_line("   "), None);
        assert_eq!(parse_line("no-equals-here"), None);
    }

    #[test]
    fn a_value_may_itself_contain_equals() {
        assert_eq!(parse_line("q=a=b"), Some(("q".into(), "a=b".into())));
    }

    #[test]
    fn parse_all_keeps_only_real_pairs() {
        let pairs = parse_all("# c\nhost=a\n\nport=1\n");
        assert_eq!(pairs.len(), 2);
        assert_eq!(get(&pairs, "host"), Some("a"));
        assert_eq!(get(&pairs, "port"), Some("1"));
        assert_eq!(get(&pairs, "missing"), None);
    }

    #[test]
    fn comments_are_counted_even_when_indented() {
        assert_eq!(count_comments("# a\n  # b\nx=1\n"), 2);
    }
}
