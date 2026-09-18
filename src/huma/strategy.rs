use borsh::{BorshDeserialize, BorshSerialize};
use solana_pubkey::Pubkey;

use crate::huma::constants::{HUNDRED_PERCENT_BPS, SECONDS_IN_A_YEAR, SECONDS_PER_DAY};
use crate::huma::state::{DeploymentStrategyType, PoolStatus};

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct Fee {
    pub accrued: u64,
    pub last_accrued_at: u64,
    pub _reserved: [u8; 64],
}

impl Fee {
    /// The accrual projected to `timestamp` at `bps`, holding the basis flat — the
    /// strategy's simple-interest `Fee::accrued_at`.
    pub fn accrued_at(&self, assets: u64, bps: u16, timestamp: u64) -> u64 {
        if self.last_accrued_at == 0 || timestamp <= self.last_accrued_at {
            return self.accrued;
        }
        let elapsed = timestamp - self.last_accrued_at;
        self.accrued
            + (assets as u128 * elapsed as u128 * bps as u128
                / (SECONDS_IN_A_YEAR as u128 * HUNDRED_PERCENT_BPS as u128)) as u64
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PerformanceFee {
    pub _declared: u64,
    pub _withdrawn: u64,
    pub _loss: u64,
    pub _last_declared_at: u64,
    pub _reserved: [u8; 64],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PerformanceFeeConfig {
    pub _max_performance_fee_bps: u16,
    pub _min_remaining_performance_fee_bps: u16,
    pub _reserved: [u8; 64],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct RateLimiter {
    pub used: u64,
    pub window_starts_at: u64,
    pub _reserved: [u8; 64],
}

impl RateLimiter {
    /// Whether `amount` would breach `max_per_window`, accounting for the window
    /// having rolled over to a new UTC day.
    pub fn would_exceed(&self, amount: u64, current_ts: u64, max_per_window: u64) -> bool {
        let start_of_today = current_ts / SECONDS_PER_DAY * SECONDS_PER_DAY;
        let used = if start_of_today > self.window_starts_at {
            0
        } else {
            self.used
        };
        used.saturating_add(amount) > max_per_window
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PoolCoreConfig {
    pub _name: String,
    pub underlying_mint: Pubkey,
    pub _huma_config: Pubkey,
    pub _owner: Pubkey,
    pub treasury: Pubkey,
    pub _risk_manager: Pubkey,
    pub huma_fee_bps: u16,
    pub operating_expense_bps: u16,
    pub _performance_fee_config: PerformanceFeeConfig,
    pub liquidity_cap: u64,
    pub _reserved: [u8; 256],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PoolCoreState {
    pub status: PoolStatus,
    pub _performance_fee: PerformanceFee,
    pub _reserved: [u8; 128],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct ManualDeploymentConfig {
    pub _daily_limit: u64,
    pub _per_wallet_limit: u64,
    pub _reserved: [u8; 64],
}

/// One mode's configuration in the strategy's registry, paired by index with
/// [`StrategyModeState`].
#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct StrategyModeConfig {
    pub mint: Pubkey,
    pub apy_bps: u16,
    pub _reserved: [u8; 64],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct StrategyModeState {
    pub assets: u64,
    pub _loss: u64,
    pub _cumulative_yield: u64,
    pub _notes_pending_redemption: u64,
    pub assets_refreshed_at: u64,
    pub huma_fee: Fee,
    pub operating_expense: Fee,
    pub _reserved: [u8; 128],
}

impl StrategyModeState {
    /// The strategy's `calculate_projected_assets`. The same curve as the pool's
    /// [`crate::huma::state::ModeState::refreshed_assets`], which stores the per-second
    /// rate `(1 + apy)^(1/year) - 1` and raises it to the elapsed seconds — but that one
    /// takes the stored rate where this takes `apy_bps`, and reaches it through `powi`
    /// rather than `powf`, so they are neither interchangeable nor bit-identical.
    pub fn projected_assets(&self, apy_bps: u16, current_ts: u64) -> u64 {
        if apy_bps == 0 || self.assets_refreshed_at == 0 {
            return self.assets;
        }
        let elapsed = current_ts.saturating_sub(self.assets_refreshed_at);
        if elapsed == 0 {
            return self.assets;
        }
        let exponent = elapsed as f64 / SECONDS_IN_A_YEAR as f64;
        (self.assets as f64
            * (1.0_f64 + apy_bps as f64 / HUNDRED_PERCENT_BPS as f64).powf(exponent)) as u64
    }

    /// What the two senior fees hold back, projected to `current_ts`.
    pub fn reserved_fees_at(
        &self,
        huma_fee_bps: u16,
        operating_expense_bps: u16,
        current_ts: u64,
    ) -> u64 {
        self.huma_fee
            .accrued_at(self.assets, huma_fee_bps, current_ts)
            .saturating_add(self.operating_expense.accrued_at(
                self.assets,
                operating_expense_bps,
                current_ts,
            ))
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct ModePoolConfig {
    pub core: PoolCoreConfig,
    pub mode_configs: Vec<StrategyModeConfig>,
    pub _client_authority: Pubkey,
    pub max_total_redemption_notes_per_window: u64,
    pub _manual_deployment: ManualDeploymentConfig,
    pub _min_initial_deposit: u64,
    pub _reserved: [u8; 128],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct ModePoolState {
    pub core: PoolCoreState,
    pub modes: Vec<StrategyModeState>,
    pub redemption_rate_limit: RateLimiter,
    pub _manual_deployment_rate_limit: RateLimiter,
    pub _reserved: [u8; 128],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct InstantWithdrawalFeeConfig {
    pub liquid_asset_ratio_lt_bps: u16,
    pub fee_bps: u16,
    pub _reserved: [u8; 64],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct InstantWithdrawalConfig {
    pub instant_withdrawal_reserve_limit: u64,
    pub instant_withdrawal_fee_configs: Vec<InstantWithdrawalFeeConfig>,
    pub max_total_instant_withdrawal_notes_per_window: u64,
    pub liquidity_source: Option<Pubkey>,
    pub _reserved: [u8; 128],
}

/// The Strategy layer's own deployment target. Same two fields as the vault's, but a
/// different reserved tail, so it decodes through its own mirror.
#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct DeploymentConfig {
    pub _bump: u8,
    pub strategy_type: DeploymentStrategyType,
    pub target_key: Pubkey,
    pub _reserved: [u8; 64],
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct StrategyConfig {
    pub _bump: u8,
    pub authority_bump: u8,
    pub _id: Pubkey,
    pub pool: ModePoolConfig,
    pub instant_withdrawal_config: InstantWithdrawalConfig,
    pub _reserved: [u8; 256],
}

impl StrategyConfig {
    pub fn mode_index_for(&self, note_mint: &Pubkey) -> Option<usize> {
        self.pool
            .mode_configs
            .iter()
            .position(|tier| &tier.mint == note_mint)
    }

    pub fn apy_bps(&self, mode_index: usize) -> u16 {
        self.pool.mode_configs[mode_index].apy_bps
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct StrategyState {
    pub _bump: u8,
    pub pool: ModePoolState,
    pub instant_withdrawal_rate_limit: RateLimiter,
    pub liquid_assets_deployed: u64,
    pub _reserved: [u8; 256],
}

impl StrategyState {
    pub fn is_on(&self) -> bool {
        self.pool.core.status == PoolStatus::On
    }

    pub fn mode(&self, mode_index: usize) -> &StrategyModeState {
        &self.pool.modes[mode_index]
    }

    /// Every mode's assets projected to `current_ts`, which is what the instant
    /// withdrawal's liquid-asset ratio is measured against.
    pub fn projected_total_assets(&self, config: &StrategyConfig, current_ts: u64) -> u64 {
        self.pool
            .modes
            .iter()
            .enumerate()
            .map(|(index, mode)| mode.projected_assets(config.apy_bps(index), current_ts))
            .fold(0u64, |acc, assets| acc.saturating_add(assets))
    }

    /// The strategy's cash less what the two senior fees hold back across every mode.
    pub fn available_balance(
        &self,
        underlying_balance: u64,
        config: &StrategyConfig,
        current_ts: u64,
    ) -> u64 {
        let reserved = self
            .pool
            .modes
            .iter()
            .map(|mode| {
                mode.reserved_fees_at(
                    config.pool.core.huma_fee_bps,
                    config.pool.core.operating_expense_bps,
                    current_ts,
                )
            })
            .fold(0u64, |acc, reserved| acc.saturating_add(reserved));
        underlying_balance
            .saturating_sub(
                config
                    .instant_withdrawal_config
                    .instant_withdrawal_reserve_limit,
            )
            .saturating_sub(reserved)
    }
}

/// The strategy's progressive instant-withdrawal fee: the same bracket walk the pool
/// runs in [`crate::huma::state::InstantWithdrawalConfig::compute_instant_withdrawal_fee`],
/// over the strategy's own tiers. Kept separate rather than shared because the two
/// configs are distinct account layouts and the pre-cutover path must stay untouched.
pub fn progressive_fee(
    tiers: &[InstantWithdrawalFeeConfig],
    total_assets_before: u64,
    liquid_assets_before: u64,
    withdrawal_amount: u64,
) -> Option<u64> {
    if withdrawal_amount == 0 {
        return Some(0);
    }
    let liquid_assets_after = liquid_assets_before.saturating_sub(withdrawal_amount);
    let total_assets_after = total_assets_before.saturating_sub(withdrawal_amount);
    if total_assets_after == 0 {
        return None;
    }

    let hundred_pct = HUNDRED_PERCENT_BPS as u128;
    let liquid_scaled_before = liquid_assets_before as u128 * hundred_pct;
    let liquid_scaled_after = liquid_assets_after as u128 * hundred_pct;
    let total_before = total_assets_before as u128;
    let total_after = total_assets_after as u128;

    let i_end = tiers.iter().position(|tier| {
        liquid_scaled_after <= tier.liquid_asset_ratio_lt_bps as u128 * total_after
    })?;
    if tiers[i_end].fee_bps as u64 >= HUNDRED_PERCENT_BPS {
        return None;
    }
    let i_start = tiers.iter().position(|tier| {
        liquid_scaled_before <= tier.liquid_asset_ratio_lt_bps as u128 * total_before
    })?;

    let mut total_fee: u64 = 0;
    let mut enter: u64 = 0;
    for index in (i_end..=i_start).rev() {
        let exit = if index == i_end {
            withdrawal_amount
        } else {
            let boundary = tiers[index - 1].liquid_asset_ratio_lt_bps as u128;
            ((liquid_assets_before as u128 * hundred_pct - boundary * total_before)
                / (hundred_pct - boundary)) as u64
        };
        let slice = exit.saturating_sub(enter);
        total_fee += (slice as u128 * tiers[index].fee_bps as u128).div_ceil(hundred_pct) as u64;
        enter = exit;
    }
    Some(total_fee)
}

/// The marginal rate at the end of the trajectory, for the quote's price field.
pub fn marginal_fee_bps(
    tiers: &[InstantWithdrawalFeeConfig],
    total_assets_before: u64,
    liquid_assets_before: u64,
    withdrawal_amount: u64,
) -> Option<u16> {
    let liquid_after = liquid_assets_before.saturating_sub(withdrawal_amount);
    let total_after = total_assets_before.saturating_sub(withdrawal_amount);
    if total_after == 0 {
        return None;
    }
    let hundred_pct = HUNDRED_PERCENT_BPS as u128;
    let liquid_scaled_after = liquid_after as u128 * hundred_pct;
    let i_end = tiers.iter().position(|tier| {
        liquid_scaled_after <= tier.liquid_asset_ratio_lt_bps as u128 * total_after as u128
    })?;
    let fee_bps = tiers[i_end].fee_bps;
    if fee_bps as u64 >= HUNDRED_PERCENT_BPS {
        return None;
    }
    Some(fee_bps)
}

/// What `assets` mints against a mode holding `projected_assets` against `note_supply`
/// notes. `None` when the note count would not fit a `u64`, as the strategy rejects.
pub fn convert_to_notes(assets: u64, projected_assets: u64, note_supply: u64) -> Option<u64> {
    if note_supply == 0 {
        return Some(assets);
    }
    if projected_assets == 0 {
        return Some(0);
    }
    let notes = assets as u128 * note_supply as u128 / projected_assets as u128;
    (notes <= u64::MAX as u128).then_some(notes as u64)
}

/// What `notes` redeem for, gross of the instant-withdrawal fee.
pub fn convert_to_assets(notes: u64, projected_assets: u64, note_supply: u64) -> u64 {
    if note_supply == 0 {
        return notes;
    }
    (notes as u128 * projected_assets as u128 / note_supply as u128) as u64
}

/// The pool's `notes_to_mode_tokens`: either side empty bootstraps the ratio at 1:1.
pub fn notes_to_mode_tokens(notes: u64, mode_token_supply: u64, note_supply: u64) -> u64 {
    if mode_token_supply == 0 || note_supply == 0 {
        return notes;
    }
    (notes as u128 * mode_token_supply as u128 / note_supply as u128) as u64
}

/// The pool's `mode_tokens_to_notes`, the inverse used when a withdrawal converts a
/// holder's mode tokens into the notes backing them.
pub fn mode_tokens_to_notes(mode_tokens: u64, note_supply: u64, mode_token_supply: u64) -> u64 {
    if mode_token_supply == 0 {
        return 0;
    }
    (mode_tokens as u128 * note_supply as u128 / mode_token_supply as u128) as u64
}
