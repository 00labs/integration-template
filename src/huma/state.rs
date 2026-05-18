use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::hash;
use solana_pubkey::Pubkey;
use spl_token::solana_program::program_pack::Pack;
use spl_token::state::{Account as TokenAccount, Mint};

use crate::huma::constants::{
    DISCRIMINATOR_LEN, HUNDRED_PERCENT_BPS, MAX_ASSETS_STALENESS_SECS, SECONDS_IN_A_YEAR,
    SECONDS_PER_DAY,
};
use crate::trading_venue::error::TradingVenueError;

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Default)]
pub struct HumaConfig {
    pub _id: Pubkey,
    pub _bump: u8,
    pub _owner: Pubkey,
    pub _treasury: Pubkey,
    pub _sentinel: Pubkey,
    pub _protocol_fee_bps: u16,
    pub paused: bool,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct LPConfig {
    pub liquidity_cap: u128,
    pub min_deposit_amount: u64,
    pub _min_redemption_shares: u64,
    pub max_total_redemption_shares_per_window: u128,
    pub max_instant_withdrawal_shares_per_window: u128,
    pub _reserved: [u8; 144],
}

impl Default for LPConfig {
    fn default() -> Self {
        Self {
            liquidity_cap: 0,
            min_deposit_amount: 0,
            _min_redemption_shares: 0,
            max_total_redemption_shares_per_window: 0,
            max_instant_withdrawal_shares_per_window: 0,
            _reserved: [0u8; 144],
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct InstantWithdrawalFeeConfig {
    pub liquid_asset_ratio_lt_bps: u16,
    pub fee_bps: u16,
    pub _reserved: [u8; 160],
}

impl Default for InstantWithdrawalFeeConfig {
    fn default() -> Self {
        Self {
            liquid_asset_ratio_lt_bps: 0,
            fee_bps: 0,
            _reserved: [0u8; 160],
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct InstantWithdrawalConfig {
    pub instant_withdrawal_reserve_limit: u64,
    pub instant_withdrawal_fee_configs: Vec<InstantWithdrawalFeeConfig>,
    pub liquidity_source: Option<Pubkey>,
    pub _reserved: [u8; 127],
}

impl Default for InstantWithdrawalConfig {
    fn default() -> Self {
        Self {
            instant_withdrawal_reserve_limit: 0,
            instant_withdrawal_fee_configs: Vec::new(),
            liquidity_source: None,
            _reserved: [0u8; 127],
        }
    }
}

impl InstantWithdrawalConfig {
    /// Mirrors the on-chain `InstantWithdrawalConfig::compute_instant_withdrawal_fee`.
    /// Returns the total progressive instant-withdrawal fee in token base units, or
    /// `None` when the withdrawal is unsupported (no bracket matches the trajectory,
    /// the ending bracket has a 100% fee, or the withdrawal would drain the pool).
    pub fn compute_instant_withdrawal_fee(
        &self,
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
        let total_assets_before_u128 = total_assets_before as u128;
        let total_assets_after_u128 = total_assets_after as u128;

        let i_end = self.instant_withdrawal_fee_configs.iter().position(|c| {
            liquid_scaled_after <= c.liquid_asset_ratio_lt_bps as u128 * total_assets_after_u128
        })?;
        if self.instant_withdrawal_fee_configs[i_end].fee_bps as u64 >= HUNDRED_PERCENT_BPS {
            return None;
        }
        let i_start = self.instant_withdrawal_fee_configs.iter().position(|c| {
            liquid_scaled_before <= c.liquid_asset_ratio_lt_bps as u128 * total_assets_before_u128
        })?;

        let mut total_fee: u64 = 0;
        let mut withdrawal_amount_enter: u64 = 0;
        for i in (i_end..=i_start).rev() {
            let config = &self.instant_withdrawal_fee_configs[i];
            let withdrawal_amount_exit = if i == i_end {
                withdrawal_amount
            } else {
                let exit_boundary_bps =
                    self.instant_withdrawal_fee_configs[i - 1].liquid_asset_ratio_lt_bps;
                withdrawal_amount_at_boundary(
                    exit_boundary_bps,
                    total_assets_before,
                    liquid_assets_before,
                )
            };
            let slice = withdrawal_amount_exit - withdrawal_amount_enter;
            let slice_fee = (slice as u128 * config.fee_bps as u128).div_ceil(hundred_pct) as u64;
            total_fee += slice_fee;
            withdrawal_amount_enter = withdrawal_amount_exit;
        }
        Some(total_fee)
    }
}

fn withdrawal_amount_at_boundary(
    boundary_bps: u16,
    total_assets_before: u64,
    liquid_assets_before: u64,
) -> u64 {
    let boundary = boundary_bps as u128;
    let hundred_pct = HUNDRED_PERCENT_BPS as u128;
    ((liquid_assets_before as u128 * hundred_pct - boundary * total_assets_before as u128)
        / (hundred_pct - boundary)) as u64
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PoolConfig {
    pub _bump: u8,
    pub huma_config: Pubkey,
    pub _pool_owner: Pubkey,
    pub pool_owner_treasury: Pubkey,
    pub underlying_mint: Pubkey,
    pub pool_authority_bump: u8,
    pub _pool_id: Pubkey,
    pub _pool_name: String,
    pub lp_config: LPConfig,
    pub instant_withdrawal_config: InstantWithdrawalConfig,
    pub _reserved: [u8; 160],
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            _bump: 0,
            huma_config: Pubkey::default(),
            _pool_owner: Pubkey::default(),
            pool_owner_treasury: Pubkey::default(),
            underlying_mint: Pubkey::default(),
            pool_authority_bump: 0,
            _pool_id: Pubkey::default(),
            _pool_name: String::new(),
            lp_config: LPConfig::default(),
            instant_withdrawal_config: InstantWithdrawalConfig::default(),
            _reserved: [0u8; 160],
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Default, PartialEq)]
pub enum DeploymentStrategyType {
    #[default]
    Manual,
    JupLend,
    KaminoLend,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct DeploymentConfig {
    pub _bump: u8,
    pub strategy_type: DeploymentStrategyType,
    pub target_key: Pubkey,
    pub _reserved: [u8; 160],
}

impl Default for DeploymentConfig {
    fn default() -> Self {
        Self {
            _bump: 0,
            strategy_type: DeploymentStrategyType::default(),
            target_key: Pubkey::default(),
            _reserved: [0u8; 160],
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Default, PartialEq)]
pub enum PoolStatus {
    #[default]
    Off,
    On,
    PreClosure,
    Closed,
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct ModeState {
    pub assets: u128,
    pub _losses: u128,
    pub _cumulative_yields: u128,
    pub assets_refreshed_at: u64,
    pub _reserved: [u8; 160],
}

impl ModeState {
    pub fn refreshed_assets(&self, periodic_apy_bps: f64, current_ts: u64) -> u64 {
        let cached_assets = self.assets as u64;
        if periodic_apy_bps <= 0.0 || self.assets_refreshed_at == 0 {
            return cached_assets;
        }
        let seconds_elapsed = current_ts.saturating_sub(self.assets_refreshed_at);
        if seconds_elapsed == 0 {
            return cached_assets;
        }
        let yield_rate_per_second =
            periodic_apy_bps / (SECONDS_IN_A_YEAR as f64 * HUNDRED_PERCENT_BPS as f64);
        (cached_assets as f64 * (1.0_f64 + yield_rate_per_second).powi(seconds_elapsed as i32))
            as u64
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct RedemptionGating {
    pub total_shares_requested: u128,
    pub window_starts_at: u64,
    pub _reserved: [u8; 80],
}

impl Default for RedemptionGating {
    fn default() -> Self {
        Self {
            total_shares_requested: 0,
            window_starts_at: 0,
            _reserved: [0u8; 80],
        }
    }
}

impl RedemptionGating {
    pub fn would_exceed(&self, shares: u64, current_ts: u64, max_shares: u64) -> bool {
        let start_of_today = current_ts / SECONDS_PER_DAY * SECONDS_PER_DAY;
        let used = if start_of_today > self.window_starts_at {
            0
        } else {
            self.total_shares_requested as u64
        };
        used.saturating_add(shares) > max_shares
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct Redemption {
    pub _next_request_id: u128,
    pub _last_request_id: u128,
    pub global_redemption_gating: RedemptionGating,
    pub instant_withdrawal_gating: RedemptionGating,
    pub _reserved: [u8; 136],
}

impl Default for Redemption {
    fn default() -> Self {
        Self {
            _next_request_id: 0,
            _last_request_id: 0,
            global_redemption_gating: RedemptionGating::default(),
            instant_withdrawal_gating: RedemptionGating::default(),
            _reserved: [0u8; 136],
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PoolStats {
    pub _cumulative_amount_deployed: u128,
    pub _cumulative_amount_paid_back: u128,
    pub _reserved: [u8; 160],
}

impl Default for PoolStats {
    fn default() -> Self {
        Self {
            _cumulative_amount_deployed: 0,
            _cumulative_amount_paid_back: 0,
            _reserved: [0u8; 160],
        }
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct PoolState {
    pub _bump: u8,
    pub status: PoolStatus,
    pub disbursement_reserve: u128,
    pub mode_states: Vec<ModeState>,
    pub mode_config_keys: Vec<Pubkey>,
    pub redemption: Redemption,
    pub _pool_stats: PoolStats,
    pub liquid_assets_deployed: u64,
    pub _reserved: [u8; 152],
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            _bump: 0,
            status: PoolStatus::default(),
            disbursement_reserve: 0,
            mode_states: Vec::new(),
            mode_config_keys: Vec::new(),
            redemption: Redemption::default(),
            _pool_stats: PoolStats::default(),
            liquid_assets_deployed: 0,
            _reserved: [0u8; 152],
        }
    }
}

impl PoolState {
    pub fn is_pool_on(&self) -> bool {
        self.status == PoolStatus::On
    }

    pub fn mode_index_for(&self, mode_config_key: &Pubkey) -> Option<usize> {
        self.mode_config_keys
            .iter()
            .position(|k| k == mode_config_key)
    }

    pub fn all_mode_assets_fresh(&self, current_ts: u64) -> bool {
        self.mode_states
            .iter()
            .all(|s| current_ts.saturating_sub(s.assets_refreshed_at) <= MAX_ASSETS_STALENESS_SECS)
    }

    pub fn mode_state(&self, mode_index: usize) -> &ModeState {
        &self.mode_states[mode_index]
    }

    pub fn refreshed_total_assets(&self, mode_index: usize, refreshed_mode_assets: u64) -> u64 {
        let delta =
            refreshed_mode_assets.saturating_sub(self.mode_states[mode_index].assets as u64);
        self.get_pool_total_assets().saturating_add(delta)
    }

    pub fn get_available_balance(&self, underlying_balance: u64, reserve_limit: u64) -> u64 {
        underlying_balance
            .saturating_sub(self.disbursement_reserve as u64)
            .saturating_sub(reserve_limit)
    }

    fn get_pool_total_assets(&self) -> u64 {
        self.mode_states.iter().map(|s| s.assets).sum::<u128>() as u64
    }
}

#[derive(BorshDeserialize, BorshSerialize, Clone, Debug)]
pub struct ModeConfig {
    pub _bump: u8,
    pub _mint_bump: u8,
    pub _id: Pubkey,
    pub _name: String,
    pub _target_apy_bps: u16,
    pub periodic_apy_bps: f64,
    pub _reserved: [u8; 160],
}

impl Default for ModeConfig {
    fn default() -> Self {
        Self {
            _bump: 0,
            _mint_bump: 0,
            _id: Pubkey::default(),
            _name: String::new(),
            _target_apy_bps: 0,
            periodic_apy_bps: 0.0,
            _reserved: [0u8; 160],
        }
    }
}

// ── Deserialization helpers ──────────────────────────────────────────────────

pub fn decode_anchor_account<T: BorshDeserialize>(
    account_name: &str,
    data: &[u8],
) -> Result<T, TradingVenueError> {
    if data.len() < DISCRIMINATOR_LEN {
        return Err(TradingVenueError::DeserializationFailed(
            format!("{account_name} data too short").into(),
        ));
    }
    let expected = anchor_account_discriminator(account_name);
    if data[..DISCRIMINATOR_LEN] != expected {
        return Err(TradingVenueError::DeserializationFailed(
            format!("invalid discriminator for {account_name}").into(),
        ));
    }
    T::deserialize(&mut &data[DISCRIMINATOR_LEN..]).map_err(|e| {
        TradingVenueError::DeserializationFailed(format!("{account_name}: {e}").into())
    })
}

pub fn read_token_account_amount(data: &[u8]) -> Result<u64, TradingVenueError> {
    TokenAccount::unpack_unchecked(data)
        .map(|a| a.amount)
        .map_err(|e| TradingVenueError::DeserializationFailed(e.to_string().into()))
}

pub fn read_mint_supply(data: &[u8]) -> Result<u64, TradingVenueError> {
    Mint::unpack_unchecked(data)
        .map(|m| m.supply)
        .map_err(|e| TradingVenueError::DeserializationFailed(e.to_string().into()))
}

pub fn anchor_account_discriminator(name: &str) -> [u8; DISCRIMINATOR_LEN] {
    anchor_discriminator("account:", name)
}

pub fn anchor_instruction_discriminator(name: &str) -> [u8; DISCRIMINATOR_LEN] {
    anchor_discriminator("global:", name)
}

fn anchor_discriminator(prefix: &str, name: &str) -> [u8; DISCRIMINATOR_LEN] {
    let hash = hash::hashv(&[prefix.as_bytes(), name.as_bytes()]);
    let mut out = [0u8; DISCRIMINATOR_LEN];
    out.copy_from_slice(&hash.as_ref()[..DISCRIMINATOR_LEN]);
    out
}
