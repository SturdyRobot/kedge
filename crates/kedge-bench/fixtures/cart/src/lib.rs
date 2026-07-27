//! Order totals in integer cents. No floats: money bugs hide in rounding.

pub fn subtotal(prices: &[i64], qty: &[i64]) -> i64 {
    prices.iter().zip(qty).map(|(p, q)| p * q).sum()
}

/// `bps` is basis points: 250 = 2.5%. Rounds half up.
pub fn discount(amount: i64, bps: i64) -> i64 {
    (amount * bps + 5_000) / 10_000
}

pub fn tax(amount: i64, bps: i64) -> i64 {
    (amount * bps + 5_000) / 10_000
}

pub fn total(prices: &[i64], qty: &[i64], discount_bps: i64, tax_bps: i64) -> i64 {
    let sub = subtotal(prices, qty);
    let after = sub - discount(sub, discount_bps);
    after + tax(after, tax_bps)
}

pub fn free_shipping(total_cents: i64) -> bool { total_cents >= 5_000 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtotal_multiplies_each_line() {
        assert_eq!(subtotal(&[100, 250], &[2, 1]), 450);
        assert_eq!(subtotal(&[], &[]), 0);
    }

    #[test]
    fn discount_rounds_half_up() {
        assert_eq!(discount(1000, 250), 25);
        assert_eq!(discount(999, 250), 25);
        assert_eq!(discount(1000, 0), 0);
    }

    #[test]
    fn tax_is_applied_after_the_discount() {
        // 1000 - 10% = 900, +10% of 900 = 990. Taxing before the discount
        // would give 1100 - 110 = 990 by luck, so use asymmetric rates.
        assert_eq!(total(&[1000], &[1], 1_000, 2_000), 1_080);
    }

    #[test]
    fn total_composes_the_parts() {
        assert_eq!(total(&[100, 250], &[2, 1], 0, 0), 450);
    }

    #[test]
    fn free_shipping_starts_exactly_at_the_threshold() {
        assert!(free_shipping(5_000));
        assert!(free_shipping(5_001));
        assert!(!free_shipping(4_999));
    }
}
