//! Numeric bounds. Every function here is broken by one bench task.

pub fn clamp_upper(v: i32, hi: i32) -> i32 { if v > hi { hi } else { v } }

pub fn clamp_lower(v: i32, lo: i32) -> i32 { if v < lo { lo } else { v } }

pub fn in_range(v: i32, lo: i32, hi: i32) -> bool { v >= lo && v <= hi }

pub fn clamp(v: i32, lo: i32, hi: i32) -> i32 { clamp_lower(clamp_upper(v, hi), lo) }

pub fn span(lo: i32, hi: i32) -> i32 { hi - lo + 1 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_bounds_above_and_at_the_edge() {
        assert_eq!(clamp_upper(9, 5), 5);
        assert_eq!(clamp_upper(5, 5), 5);
        assert_eq!(clamp_upper(1, 5), 1);
    }

    #[test]
    fn lower_bounds_below_and_at_the_edge() {
        assert_eq!(clamp_lower(1, 5), 5);
        assert_eq!(clamp_lower(5, 5), 5);
        assert_eq!(clamp_lower(9, 5), 9);
    }

    #[test]
    fn range_includes_both_endpoints() {
        assert!(in_range(1, 1, 5));
        assert!(in_range(5, 1, 5));
        assert!(in_range(3, 1, 5));
        assert!(!in_range(0, 1, 5));
        assert!(!in_range(6, 1, 5));
    }

    #[test]
    fn clamp_pulls_from_either_side() {
        assert_eq!(clamp(0, 1, 5), 1);
        assert_eq!(clamp(7, 1, 5), 5);
        assert_eq!(clamp(3, 1, 5), 3);
    }

    #[test]
    fn span_is_inclusive() {
        assert_eq!(span(1, 5), 5);
        assert_eq!(span(0, 0), 1);
    }
}
