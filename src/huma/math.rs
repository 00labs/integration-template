use crate::huma::constants::HUNDRED_PERCENT_BPS;
use crate::huma::state::InstantWithdrawalConfig;

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
/// applying the post-withdrawal-tier fee. Returns `None` when the request
/// cannot be served (insufficient supply, no fee tier matches, fee at 100%,
/// or output rounds to zero).
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
    let total_assets_after = total_assets.saturating_sub(withdrawal_amount);
    let liquid_assets_after = liquid_assets_before.saturating_sub(withdrawal_amount);
    let fee_bps = config.fee_bps_for(total_assets_after, liquid_assets_after)?;
    let fee = ((withdrawal_amount as u128 * fee_bps as u128).div_ceil(HUNDRED_PERCENT_BPS as u128))
        as u64;
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
}
