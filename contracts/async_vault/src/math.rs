//! Pure settlement arithmetic (no `Env`). Every result rounds in the fund's
//! favour, which protects the holders who stay in the fund.
//!
//! `nav` is the price of one share in cash units, scaled by `10^d` (the
//! oracle's `decimals()`); cash and shares both use 7 decimals, so the
//! 10^7 factors cancel.

/// `10^d` as i128, `None` above 10^38.
pub fn pow10(d: u32) -> Option<i128> {
    10i128.checked_pow(d)
}

/// Shares bought by `cash` at `nav`, and the dust returned to the investor.
///
/// `shares = floor(cash * 10^d / nav)`, `cost = ceil(shares * nav / 10^d)`,
/// `dust = cash - cost`. Returns `None` on overflow or invalid input.
pub fn shares_for_cash(cash: i128, nav: i128, d: u32) -> Option<(i128, i128)> {
    if cash < 0 || nav <= 0 {
        return None;
    }
    let scale = pow10(d)?;
    let shares = cash.checked_mul(scale)? / nav;
    let prod = shares.checked_mul(nav)?;
    let cost = prod / scale + if prod % scale == 0 { 0 } else { 1 };
    let dust = cash.checked_sub(cost)?;
    Some((shares, dust))
}

/// Cash paid for redeeming `shares` at `nav`: `floor(shares * nav / 10^d)`.
pub fn cash_for_shares(shares: i128, nav: i128, d: u32) -> Option<i128> {
    if shares < 0 || nav <= 0 {
        return None;
    }
    let scale = pow10(d)?;
    Some(shares.checked_mul(nav)? / scale)
}

/// `|new - old| * 10_000 > old * max_bps`, i.e. the move exceeds the band.
pub fn move_exceeds_band(old: i128, new: i128, max_bps: u32) -> Option<bool> {
    let diff = new.checked_sub(old)?.checked_abs()?;
    Some(diff.checked_mul(10_000)? > old.checked_mul(max_bps as i128)?)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use proptest::prelude::*;

    const D: u32 = 14;
    const ONE_NAV: i128 = 100_000_000_000_000;

    #[test]
    fn nav_one_is_exact() {
        assert_eq!(shares_for_cash(1_000_000_000_001, ONE_NAV, D), Some((1_000_000_000_001, 0)));
        assert_eq!(cash_for_shares(80_000_000_000, ONE_NAV, D), Some(80_000_000_000));
    }

    #[test]
    fn seed_nav_leaves_dust_and_rounds_down() {
        // 10,000 USDC at 1.00000412 (seed epoch-2 NAV).
        let nav = 100_000_412_000_000;
        let (shares, dust) = shares_for_cash(100_000_000_000, nav, D).unwrap();
        assert_eq!(shares, 99_999_588_001);
        assert!(dust >= 0 && dust <= 2);
        let back = cash_for_shares(shares, nav, D).unwrap();
        assert!(back <= 100_000_000_000);
    }

    #[test]
    fn rejects_bad_input_and_overflow() {
        assert_eq!(shares_for_cash(-1, ONE_NAV, D), None);
        assert_eq!(shares_for_cash(1, 0, D), None);
        assert_eq!(cash_for_shares(1, -5, D), None);
        assert_eq!(shares_for_cash(i128::MAX / 10, ONE_NAV, D), None);
        assert_eq!(pow10(39), None);
    }

    #[test]
    fn band_check() {
        let last = 100_000_412_000_000;
        assert_eq!(move_exceeds_band(last, 102_700_000_000_000, 25), Some(true)); // fat finger 1.0270
        assert_eq!(move_exceeds_band(last, 99_999_870_000_000, 25), Some(false));
        // exactly 25 bps is inside the band
        assert_eq!(move_exceeds_band(ONE_NAV, ONE_NAV + ONE_NAV / 400, 25), Some(false));
        assert_eq!(move_exceeds_band(ONE_NAV, ONE_NAV + ONE_NAV / 400 + 1, 25), Some(true));
    }

    fn navs() -> impl Strategy<Value = i128> {
        // 0.5 .. 2.0 at 14 decimals, plus arbitrary small positive values.
        prop_oneof![50_000_000_000_000i128..200_000_000_000_000, 1i128..10_000]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn shares_never_cost_more_than_cash(cash in 0i128..100_000_000_000_000_000, nav in navs()) {
            let (shares, dust) = shares_for_cash(cash, nav, D).unwrap();
            let scale = pow10(D).unwrap();
            prop_assert!(shares * nav / scale <= cash);
            prop_assert!(dust >= 0);
            // dust < ceil(nav / 10^d) + 1
            let bound = (nav + scale - 1) / scale + 1;
            prop_assert!(dust < bound, "dust {} bound {}", dust, bound);
        }

        #[test]
        fn round_trip_creates_no_value(cash in 0i128..100_000_000_000_000_000, nav in navs()) {
            let (shares, _) = shares_for_cash(cash, nav, D).unwrap();
            prop_assert!(cash_for_shares(shares, nav, D).unwrap() <= cash);
        }

        #[test]
        fn monotonic_in_cash(a in 0i128..100_000_000_000_000_000, b in 0i128..100_000_000_000_000_000, nav in navs()) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(shares_for_cash(lo, nav, D).unwrap().0 <= shares_for_cash(hi, nav, D).unwrap().0);
            prop_assert!(cash_for_shares(lo, nav, D).unwrap() <= cash_for_shares(hi, nav, D).unwrap());
        }
    }
}
