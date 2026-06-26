use crate::huma::constants::HUNDRED_PERCENT_BPS;
use crate::huma::state::InstantWithdrawalConfig;

/// Marginal price `d(shares)/d(assets)` for a deposit, in shares per underlying
/// atom (raw, unscaled). The deposit curve `shares = assets * mode_supply /
/// mode_assets` is linear, so the marginal price equals the spot price and is
/// constant across sizes — trivially non-increasing. An empty mode mints 1:1.
///
/// `mode_supply > 0` implies `mode_assets > 0` for any solvent pool, so this
/// does not divide by zero in practice.
pub fn deposit_price(mode_assets: u64, mode_supply: u64) -> f64 {
    if mode_supply == 0 {
        1.0
    } else {
        mode_supply as f64 / mode_assets as f64
    }
}

/// Marginal price `d(out)/d(shares)` for an instant withdrawal of `shares`, in
/// underlying atoms per share (raw, unscaled). This is the net rate after the
/// progressive fee: the gross rate `mode_assets / mode_supply` scaled by
/// `1 - marginal_fee`, where the marginal fee is the bracket the liquid-asset
/// ratio lands in *after* the withdrawal — the rate charged on the last
/// (marginal) atom, i.e. the slope of the fee curve at this size.
///
/// As `shares` grows the liquid ratio falls into lower (higher-fee) brackets, so
/// the price is non-increasing and the output curve is concave, matching the
/// invariants Titan's pricing tests check. `shares == 0` yields the spot price.
/// Mirrors [`underlying_for_instant_withdraw`]'s prelude so the price uses the
/// exact same `withdrawal_amount` and `liquid_assets_before`. Returns `None`
/// when no bracket matches or the ending bracket is a 100% fee.
#[allow(clippy::too_many_arguments)]
pub fn instant_withdraw_price(
    shares: u64,
    mode_assets: u64,
    total_assets: u64,
    mode_supply: u64,
    pool_available_balance: u64,
    liquid_assets_deployed: u64,
    config: &InstantWithdrawalConfig,
) -> Option<f64> {
    if mode_supply == 0 {
        return None;
    }
    let withdrawal_amount = ((shares as u128 * mode_assets as u128) / mode_supply as u128) as u64;
    let liquid_assets_before =
        (liquid_assets_deployed.saturating_add(pool_available_balance)).min(total_assets);
    let marginal_fee_bps =
        config.marginal_fee_bps(total_assets, liquid_assets_before, withdrawal_amount)?;

    let gross_rate = mode_assets as f64 / mode_supply as f64;
    let keep = 1.0 - marginal_fee_bps as f64 / HUNDRED_PERCENT_BPS as f64;
    Some(gross_rate * keep)
}

/// Computes shares minted for a given deposit. Returns `None` if the pool is
/// in a state that cannot quote (insolvent, or rounding to zero shares).
pub fn shares_for_deposit(assets: u64, mode_assets: u64, mode_supply: u64) -> Option<u64> {
    if mode_supply == 0 {
        return Some(assets);
    }
    if mode_assets == 0 {
        // Insolvent: supply exists but assets are zero.
        return None;
    }
    let shares = ((assets as u128 * mode_supply as u128) / mode_assets as u128) as u64;
    if shares == 0 { None } else { Some(shares) }
}

/// Computes the underlying received for an instant withdrawal of `shares`,
/// applying the progressive (tax-bracket-style) fee across whatever brackets
/// the liquid-asset-ratio trajectory traverses. Returns `None` when the request
/// cannot be served (insufficient supply, no fee tier matches the trajectory,
/// the ending tier is disabled, or output rounds to zero).
pub fn underlying_for_instant_withdraw(
    shares: u64,
    mode_assets: u64,
    total_assets: u64,
    mode_supply: u64,
    pool_available_balance: u64,
    liquid_assets_deployed: u64,
    config: &InstantWithdrawalConfig,
) -> Option<u64> {
    if mode_supply == 0 {
        return None;
    }
    let withdrawal_amount = ((shares as u128 * mode_assets as u128) / mode_supply as u128) as u64;
    let liquid_assets_before =
        (liquid_assets_deployed.saturating_add(pool_available_balance)).min(total_assets);
    let fee = config.compute_instant_withdrawal_fee(
        total_assets,
        liquid_assets_before,
        withdrawal_amount,
    )?;
    let out = withdrawal_amount.saturating_sub(fee);
    if out == 0 { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huma::state::InstantWithdrawalFeeConfig;

    fn fee_config(tiers: Vec<(u16, u16)>) -> InstantWithdrawalConfig {
        InstantWithdrawalConfig {
            instant_withdrawal_reserve_limit: 0,
            instant_withdrawal_fee_configs: tiers
                .into_iter()
                .map(|(ratio_lt, fee)| InstantWithdrawalFeeConfig {
                    liquid_asset_ratio_lt_bps: ratio_lt,
                    fee_bps: fee,
                    _reserved: [0u8; 160],
                })
                .collect(),
            liquidity_source: None,
            _reserved: [0u8; 127],
        }
    }

    #[test]
    fn deposit_empty_pool_one_to_one() {
        assert_eq!(shares_for_deposit(1_000, 0, 0), Some(1_000));
    }

    #[test]
    fn deposit_proportional() {
        // 2_000 assets, 1_000 supply, deposit 1_000 → 500 shares
        assert_eq!(shares_for_deposit(1_000, 2_000, 1_000), Some(500));
    }

    #[test]
    fn deposit_insolvent_returns_none() {
        assert_eq!(shares_for_deposit(1_000, 0, 500), None);
    }

    #[test]
    fn deposit_rounds_to_zero_returns_none() {
        assert_eq!(shares_for_deposit(1, 1_000_000, 1_000), None);
    }

    #[test]
    fn withdraw_no_fee_proportional() {
        let cfg = fee_config(vec![(10_000, 0)]);
        // 500 shares, 1_000 supply, 2_000 mode assets → 1_000 underlying
        let out = underlying_for_instant_withdraw(500, 2_000, 2_000, 1_000, 2_000, 0, &cfg);
        assert_eq!(out, Some(1_000));
    }

    #[test]
    fn withdraw_zero_supply_returns_none() {
        let cfg = fee_config(vec![(10_000, 0)]);
        assert_eq!(
            underlying_for_instant_withdraw(500, 2_000, 2_000, 0, 2_000, 0, &cfg),
            None
        );
    }

    #[test]
    fn withdraw_no_matching_tier_returns_none() {
        // ratio_after = 100% (10_000 bps); only tier covers <5_000 bps
        let cfg = fee_config(vec![(5_000, 100)]);
        assert_eq!(
            underlying_for_instant_withdraw(800, 1_000, 1_000, 1_000, 1_000, 0, &cfg),
            None
        );
    }

    #[test]
    fn deposit_price_is_constant_spot_rate() {
        // Marginal shares per underlying atom = supply / mode_assets, constant.
        assert_eq!(deposit_price(2_000, 1_000), 0.5);
        assert_eq!(deposit_price(1_000, 1_000), 1.0);
        // An empty mode mints 1:1.
        assert_eq!(deposit_price(0, 0), 1.0);
    }

    #[test]
    fn instant_withdraw_price_positive_non_increasing_and_brackets_chord() {
        // Progressive fee: as the liquid-asset ratio falls the withdrawal lands
        // in lower-ratio, higher-fee brackets. Tiers are ordered ascending by
        // ratio threshold (bps); fee in bps.
        let cfg = fee_config(vec![(2_500, 500), (5_000, 200), (10_000, 50)]);

        // R = mode_assets / mode_supply = 1, so withdrawal_amount == shares.
        // 60% of the pool is liquid initially (deployed = 0).
        let mode_assets = 1_000_000u64;
        let mode_supply = 1_000_000u64;
        let total_assets = 1_000_000u64;
        let pool_available_balance = 600_000u64;
        let liquid_deployed = 0u64;

        let out = |shares: u64| {
            underlying_for_instant_withdraw(
                shares,
                mode_assets,
                total_assets,
                mode_supply,
                pool_available_balance,
                liquid_deployed,
                &cfg,
            )
            .expect("output")
        };
        let price = |shares: u64| {
            instant_withdraw_price(
                shares,
                mode_assets,
                total_assets,
                mode_supply,
                pool_available_balance,
                liquid_deployed,
                &cfg,
            )
            .expect("price")
        };

        // Spot price (shares == 0) exists, positive, and below the gross rate.
        let spot = price(0);
        assert!(spot > 0.0 && spot <= 1.0, "spot {spot}");

        // Sweep across several brackets: positive and non-increasing. (Starts
        // above the dust floor where the div_ceil fee would consume the whole
        // tiny output — the real `bounds()` excludes that region.)
        let samples = [10_000u64, 50_000, 150_000, 250_000, 350_000, 450_000, 520_000];
        let mut prev = f64::INFINITY;
        for &s in &samples {
            let p = price(s);
            assert!(p > 0.0, "price must be positive at {s}, got {p}");
            assert!(p <= prev + 1e-9, "price rose: {prev} -> {p} at {s}");
            prev = p;
        }

        // Mean value theorem: the realized chord is bracketed by the endpoint
        // prices, with a couple atoms of slack for fee rounding (div_ceil).
        for w in samples.windows(2) {
            let (a, b) = (w[0], w[1]);
            let chord = (out(b) as f64 - out(a) as f64) / (b - a) as f64;
            let (pa, pb) = (price(a), price(b)); // pa >= pb
            let atol = 2.0 / (b - a) as f64;
            assert!(
                chord <= pa + atol && chord >= pb - atol,
                "chord {chord} not in [{pb}, {pa}] on [{a}, {b}]"
            );
        }
    }
}
