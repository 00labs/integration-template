//! `HumaVenue` — Titan integration for one (pool, mode) pair on the Huma
//! permissionless liquidity pool.
//!
//! Each instance represents one tradable pair: `underlying ↔ mode_mint`.
//! Deposits flow `underlying → mode_mint`, instant withdrawals flow the other
//! way and CPI into the configured strategy when the pool's liquid reserve is
//! insufficient.
//!
//! Construction: [`HumaVenue::new`] takes the (pool, mode) keys plus the
//! decoded `ModeConfig`. The [`FromAccount`] impl decodes the keyed
//! `ModeConfig` and pairs it with the hardcoded
//! [`crate::huma::constants::POOL_CONFIG_KEY`].

use async_trait::async_trait;
use solana_account::{Account, ReadableAccount};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sysvar::clock::{self, Clock};

use crate::account_caching::AccountsCache;
use crate::huma::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, HUMA_PROGRAM_ID, HUNDRED_PERCENT_BPS, JUP_LENDING_PROGRAM_ID,
    JUP_LIQUIDITY_PROGRAM_ID, JUP_LRRM_PROGRAM_ID, KLEND_PROGRAM_ID, POOL_CONFIG_KEY,
    SPL_TOKEN_PROGRAM_ID, STRATEGY_CONFIG_KEY, STRATEGY_PROGRAM_ID, SYSTEM_PROGRAM_ID,
    VAULT_PROGRAM_ID,
};
use crate::huma::deployment::Deployment;
use crate::huma::instruction::HumaInstruction;
use crate::huma::state::{DeploymentConfig, HumaConfig, ModeConfig, PoolConfig, PoolState};
use crate::huma::strategy::{self, StrategyConfig, StrategyModeState, StrategyState};
use crate::huma::{math, pda, state};
use crate::trading_venue::error::TradingVenueError;
use crate::trading_venue::protocol::PoolProtocol;
use crate::trading_venue::token_info::TokenInfo;
use crate::trading_venue::{
    AddressLookupTableTrait, FromAccount, QuoteRequest, QuoteResult, SwapType, TradingVenue,
};

#[derive(Clone)]
pub struct HumaVenue {
    // Statically known after construction.
    mode_config_key: Pubkey,
    pool_config_key: Pubkey,
    pool_state_key: Pubkey,
    pool_authority_key: Pubkey,
    mode_mint_key: Pubkey,

    // Decoded at construction; refreshed by `update_state`.
    mode_config: ModeConfig,

    // Populated atomically by `update_state` on success.
    state: Option<InitializedState>,
}

/// Everything `update_state` discovers and decodes. Held as an `Option` on
/// `HumaVenue` so accessors only need a single `ok_or(NotInitialized)` check.
#[derive(Clone)]
struct InitializedState {
    pool_config: PoolConfig,
    pool_state: PoolState,
    huma_config: HumaConfig,

    deployment_config_key: Pubkey,
    deployment_state_key: Pubkey,
    pool_underlying_token_key: Pubkey,
    pool_owner_treasury_underlying_token_key: Pubkey,

    underlying_token_program: Pubkey,
    mode_token_program: Pubkey,

    mode_index: usize,
    mode_supply: u64,
    pool_underlying_balance: u64,
    current_ts: u64,

    deployment: Deployment,
    token_info: [TokenInfo; 2],

    /// Present iff the mode has been cut over, i.e. `mode_config.is_migrated()`.
    /// Pre-cutover this stays `None` and every path below behaves exactly as before.
    migrated: Option<MigratedState>,
}

/// The Strategy-layer half of a migrated mode: what prices it, and the accounts the
/// vault forwards to the strategy on the CPI.
#[derive(Clone)]
struct MigratedState {
    config: StrategyConfig,
    state: StrategyState,
    /// Index of this mode's note mint in the strategy's registry.
    mode_index: usize,
    note_mint: Pubkey,
    note_supply: u64,
    note_token_program: Pubkey,
    strategy_state_key: Pubkey,
    strategy_authority_key: Pubkey,
    strategy_underlying_token_key: Pubkey,
    strategy_treasury_underlying_token_key: Pubkey,
    pool_authority_note_token_key: Pubkey,
    strategy_underlying_balance: u64,
    /// The strategy's own liquidity source, when it has one configured, and the venue
    /// behind it — the same abstraction the pool uses pre-cutover, but reading the
    /// position the *strategy authority* owns.
    deployment_config_key: Option<Pubkey>,
    deployment_state_key: Option<Pubkey>,
    deployment: Option<Deployment>,
}

impl MigratedState {
    fn mode(&self) -> &StrategyModeState {
        self.state.mode(self.mode_index)
    }

    fn projected_mode_assets(&self, current_ts: u64) -> u64 {
        self.mode()
            .projected_assets(self.config.apy_bps(self.mode_index), current_ts)
    }
}

#[derive(Copy, Clone)]
enum SwapDirection {
    Deposit,
    InstantWithdraw,
}

impl HumaVenue {
    /// Canonical constructor. Takes the keys for the (pool, mode) pair plus
    /// the already-decoded `ModeConfig` for that mode.
    pub fn new(pool_config_key: Pubkey, mode_config_key: Pubkey, mode_config: ModeConfig) -> Self {
        let pool_state_key = pda::derive_pool_state(&pool_config_key);
        let mode_mint_key = pda::derive_mode_mint(&pool_config_key, &mode_config_key);

        Self {
            mode_config_key,
            pool_config_key,
            pool_state_key,
            // Set in `update_state` from `pool_config.pool_authority_bump`.
            // The stored bump is canonical (Anchor populates it from
            // `find_program_address` in `create_pool`); using it here mirrors
            // the on-chain `bump = pool_config.pool_authority_bump` constraint
            // and skips an extra `find_program_address` call.
            pool_authority_key: Pubkey::default(),
            mode_mint_key,
            mode_config,
            state: None,
        }
    }

    fn state(&self) -> Result<&InitializedState, TradingVenueError> {
        self.state
            .as_ref()
            .ok_or(TradingVenueError::NotInitialized("venue".into()))
    }

    pub fn pool_config_key(&self) -> Pubkey {
        self.pool_config_key
    }

    pub fn pool_state_key(&self) -> Pubkey {
        self.pool_state_key
    }

    pub fn mode_mint_key(&self) -> Pubkey {
        self.mode_mint_key
    }

    pub fn pool_authority_key(&self) -> Result<Pubkey, TradingVenueError> {
        self.state()?;
        Ok(self.pool_authority_key)
    }

    pub fn huma_config_key(&self) -> Result<Pubkey, TradingVenueError> {
        Ok(self.state()?.pool_config.huma_config)
    }

    fn is_active(&self) -> bool {
        self.state
            .as_ref()
            .map(|venue_state| {
                !venue_state.huma_config.paused
                    && venue_state.pool_state.is_pool_on()
                    // A migrated mode is served through the strategy, so a strategy that
                    // is off, in pre-closure or closed takes the venue with it.
                    && venue_state.migrated.as_ref().map(|migrated| migrated.state.is_on()).unwrap_or(true)
            })
            .unwrap_or(false)
    }

    fn quote_deposit(&self, request: &QuoteRequest) -> Result<QuoteResult, TradingVenueError> {
        let venue_state = self.state()?;
        if let Some(migrated) = venue_state.migrated.as_ref() {
            return self.quote_deposit_migrated(request, venue_state, migrated);
        }

        // Refresh mode assets with accrued yield first: both the marginal price
        // and the liquidity cap depend on them, and this matches the on-chain
        // order of operations (`refresh_assets_for_mode` runs before the
        // `LiquidityCapExceeded` check).
        let mode_assets = venue_state
            .pool_state
            .mode_state(venue_state.mode_index)
            .refreshed_assets(self.mode_config.periodic_apy_bps, venue_state.current_ts);

        // The deposit curve `shares = assets * supply / mode_assets` is linear,
        // so the marginal price is constant and equals the spot price at 0.
        let price = math::deposit_price(mode_assets, venue_state.mode_supply);

        let assets = request.amount;
        if assets == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
                price,
            });
        }
        if assets < venue_state.pool_config.lp_config.min_deposit_amount {
            return Err(TradingVenueError::AmmMethodError(
                "deposit below minimum".into(),
            ));
        }

        let total_assets = venue_state
            .pool_state
            .refreshed_total_assets(venue_state.mode_index, mode_assets);
        let available_cap = venue_state
            .pool_config
            .lp_config
            .liquidity_cap
            .saturating_sub(total_assets as u128) as u64;

        // Liquidity cap is the one case that fits `not_enough_liquidity`:
        // we can serve up to `available_cap` and let the router compose
        // the rest from elsewhere.
        if available_cap == 0 {
            return Err(TradingVenueError::AmmMethodError(
                "liquidity cap reached".into(),
            ));
        }
        let assets_to_serve = assets.min(available_cap);
        let partial = assets_to_serve < assets;

        let shares =
            math::shares_for_deposit(assets_to_serve, mode_assets, venue_state.mode_supply).ok_or(
                TradingVenueError::AmmMethodError("zero shares minted".into()),
            )?;

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: assets_to_serve,
            expected_output: shares,
            not_enough_liquidity: partial,
            price,
        })
    }

    fn quote_instant_withdraw(
        &self,
        request: &QuoteRequest,
    ) -> Result<QuoteResult, TradingVenueError> {
        let venue_state = self.state()?;
        if let Some(migrated) = venue_state.migrated.as_ref() {
            return self.quote_instant_withdraw_migrated(request, venue_state, migrated);
        }
        let config = &venue_state.pool_config.instant_withdrawal_config;

        // Valuation inputs the price (and the served amount) depend on. Refresh
        // mode assets with accrued yield so this matches the on-chain order.
        let mode_assets = venue_state
            .pool_state
            .mode_state(venue_state.mode_index)
            .refreshed_assets(self.mode_config.periodic_apy_bps, venue_state.current_ts);
        let total_assets = venue_state
            .pool_state
            .refreshed_total_assets(venue_state.mode_index, mode_assets);
        let reserve_limit = config.instant_withdrawal_reserve_limit;
        let pool_available_balance = venue_state
            .pool_state
            .get_available_balance(venue_state.pool_underlying_balance, reserve_limit);

        // Marginal price for `n` shares (raw underlying atoms per share), net of
        // the progressive fee; `n == 0` is the spot price. Used for both the
        // zero-input quote and the price reported at the served size.
        let price_for = |n: u64| {
            math::instant_withdraw_price(
                n,
                mode_assets,
                total_assets,
                venue_state.mode_supply,
                pool_available_balance,
                venue_state.pool_state.liquid_assets_deployed,
                config,
            )
            .ok_or(TradingVenueError::AmmMethodError(
                "instant withdrawal not available".into(),
            ))
        };

        let shares = request.amount;
        if shares == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
                price: price_for(0)?,
            });
        }

        if !venue_state
            .pool_state
            .all_mode_assets_fresh(venue_state.current_ts)
        {
            return Err(TradingVenueError::AmmMethodError(
                "mode assets stale".into(),
            ));
        }

        let lp = &venue_state.pool_config.lp_config;
        if venue_state
            .pool_state
            .redemption
            .instant_withdrawal_gating
            .would_exceed(
                shares,
                venue_state.current_ts,
                lp.max_instant_withdrawal_shares_per_window as u64,
            )
            || venue_state
                .pool_state
                .redemption
                .global_redemption_gating
                .would_exceed(
                    shares,
                    venue_state.current_ts,
                    lp.max_total_redemption_shares_per_window as u64,
                )
        {
            return Err(TradingVenueError::AmmMethodError(
                "instant withdrawal window limit reached".into(),
            ));
        }

        // Clamp the request to what the pool + strategy can actually pay out.
        // The on-chain ix pulls `withdrawal_amount - pool_available_balance`
        // from the strategy when the pool's own balance is short; the strategy
        // already bounds its withdrawable amount by both protocol-side
        // liquidity and the pool's own redemption value (k-tokens for Kamino,
        // f-tokens for JupLend). Also cap by `mode_assets` — we can never
        // redeem more underlying than this mode is backed by, and exceeding it
        // would imply burning more shares than exist (`max_shares > mode_supply`).
        let max_servable_underlying = pool_available_balance
            .saturating_add(venue_state.deployment.available_liquidity_for_withdrawal())
            .min(mode_assets);
        let max_shares = if mode_assets == 0 {
            0
        } else {
            (max_servable_underlying as u128 * venue_state.mode_supply as u128
                / mode_assets as u128) as u64
        };
        let shares_to_serve = shares.min(max_shares);
        let partial = shares_to_serve < shares;

        if shares_to_serve == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: true,
                price: price_for(0)?,
            });
        }

        let out = math::underlying_for_instant_withdraw(
            shares_to_serve,
            mode_assets,
            total_assets,
            venue_state.mode_supply,
            pool_available_balance,
            venue_state.pool_state.liquid_assets_deployed,
            config,
        )
        .ok_or(TradingVenueError::AmmMethodError(
            "instant withdrawal not available".into(),
        ))?;

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: shares_to_serve,
            expected_output: out,
            not_enough_liquidity: partial,
            price: price_for(shares_to_serve)?,
        })
    }

    /// Reads the Strategy layer for a migrated mode.
    async fn fetch_migrated_state(
        cache: &dyn AccountsCache,
        note_mint: Pubkey,
        pool_config: &PoolConfig,
        pool_authority_key: Pubkey,
        underlying_token_program: Pubkey,
    ) -> Result<MigratedState, TradingVenueError> {
        let strategy_state_key = pda::derive_strategy_state(&STRATEGY_CONFIG_KEY);
        let strategy_authority_key = pda::derive_strategy_authority(&STRATEGY_CONFIG_KEY);
        let strategy_underlying_token_key =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &strategy_authority_key,
                &pool_config.underlying_mint,
                &underlying_token_program,
            );

        let [
            strategy_config_account,
            strategy_state_account,
            note_mint_account,
            strategy_underlying_account,
        ]: [Option<Account>; 4] = cache
            .get_accounts(&[
                STRATEGY_CONFIG_KEY,
                strategy_state_key,
                note_mint,
                strategy_underlying_token_key,
            ])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;

        let strategy_config_account = strategy_config_account.ok_or(
            TradingVenueError::NoAccountFound(STRATEGY_CONFIG_KEY.into()),
        )?;
        let strategy_state_account = strategy_state_account
            .ok_or(TradingVenueError::NoAccountFound(strategy_state_key.into()))?;
        let note_mint_account =
            note_mint_account.ok_or(TradingVenueError::NoAccountFound(note_mint.into()))?;
        let strategy_underlying_account = strategy_underlying_account.ok_or(
            TradingVenueError::NoAccountFound(strategy_underlying_token_key.into()),
        )?;

        let config: StrategyConfig =
            state::decode_anchor_account("StrategyConfig", strategy_config_account.data())?;
        let strategy_state: StrategyState =
            state::decode_anchor_account("StrategyState", strategy_state_account.data())?;
        let mode_index = config.mode_index_for(&note_mint).ok_or_else(|| {
            TradingVenueError::MissingState("note mint not in the strategy's registry".into())
        })?;

        let note_token_program = *note_mint_account.owner();
        let pool_authority_note_token_key =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &pool_authority_key,
                &note_mint,
                &note_token_program,
            );
        let strategy_treasury_underlying_token_key =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &config.pool.core.treasury,
                &pool_config.underlying_mint,
                &underlying_token_program,
            );
        let deployment_config_key = config.instant_withdrawal_config.liquidity_source;
        let deployment_state_key = deployment_config_key
            .as_ref()
            .map(pda::derive_strategy_deployment_state);

        // The strategy deploys its own liquidity, so a withdrawal it cannot cover from
        // cash pulls from *its* venue, against the position its authority owns.
        let mut deployment = None;
        if let Some(key) = deployment_config_key {
            let account = cache
                .get_accounts(&[key])
                .await?
                .pop()
                .flatten()
                .ok_or(TradingVenueError::NoAccountFound(key.into()))?;
            let deployment_config: strategy::DeploymentConfig =
                state::decode_anchor_account("DeploymentConfig", account.data())?;
            let mut venue = Deployment::new(
                &deployment_config.strategy_type,
                deployment_config.target_key,
                pool_config.underlying_mint,
                strategy_authority_key,
            )?;
            venue.update(cache).await?;
            deployment = Some(venue);
        }

        Ok(MigratedState {
            config,
            state: strategy_state,
            mode_index,
            note_mint,
            note_supply: state::read_mint_supply(note_mint_account.data())?,
            note_token_program,
            strategy_state_key,
            strategy_authority_key,
            strategy_underlying_token_key,
            strategy_treasury_underlying_token_key,
            pool_authority_note_token_key,
            strategy_underlying_balance: state::read_token_account_amount(
                strategy_underlying_account.data(),
            )?,
            deployment_config_key,
            deployment_state_key,
            deployment,
        })
    }

    /// The eight optional Strategy-layer slots `deposit` takes, in declaration order.
    fn deposit_strategy_metas(&self) -> Vec<AccountMeta> {
        match self
            .state()
            .ok()
            .and_then(|venue_state| venue_state.migrated.as_ref())
        {
            None => vec![AccountMeta::new_readonly(VAULT_PROGRAM_ID, false); 8],
            Some(migrated) => vec![
                AccountMeta::new_readonly(STRATEGY_CONFIG_KEY, false),
                AccountMeta::new(migrated.strategy_state_key, false),
                AccountMeta::new(migrated.note_mint, false),
                AccountMeta::new_readonly(migrated.strategy_authority_key, false),
                AccountMeta::new(migrated.pool_authority_note_token_key, false),
                AccountMeta::new(migrated.strategy_underlying_token_key, false),
                AccountMeta::new_readonly(STRATEGY_PROGRAM_ID, false),
                AccountMeta::new_readonly(migrated.note_token_program, false),
            ],
        }
    }

    /// The eleven optional Strategy-layer slots `instant_withdraw` takes.
    fn instant_withdraw_strategy_metas(&self) -> Vec<AccountMeta> {
        match self
            .state()
            .ok()
            .and_then(|venue_state| venue_state.migrated.as_ref())
        {
            None => vec![AccountMeta::new_readonly(VAULT_PROGRAM_ID, false); 11],
            Some(migrated) => {
                let none = AccountMeta::new_readonly(VAULT_PROGRAM_ID, false);
                vec![
                    AccountMeta::new_readonly(STRATEGY_CONFIG_KEY, false),
                    AccountMeta::new(migrated.strategy_state_key, false),
                    AccountMeta::new(migrated.note_mint, false),
                    AccountMeta::new(migrated.pool_authority_note_token_key, false),
                    AccountMeta::new_readonly(migrated.strategy_authority_key, false),
                    AccountMeta::new(migrated.strategy_underlying_token_key, false),
                    AccountMeta::new(migrated.strategy_treasury_underlying_token_key, false),
                    migrated
                        .deployment_config_key
                        .map(|k| AccountMeta::new_readonly(k, false))
                        .unwrap_or_else(|| none.clone()),
                    migrated
                        .deployment_state_key
                        .map(|k| AccountMeta::new(k, false))
                        .unwrap_or_else(|| none.clone()),
                    AccountMeta::new_readonly(STRATEGY_PROGRAM_ID, false),
                    AccountMeta::new_readonly(migrated.note_token_program, false),
                ]
            }
        }
    }

    /// A migrated mode's deposit prices.
    fn quote_deposit_migrated(
        &self,
        request: &QuoteRequest,
        venue_state: &InitializedState,
        migrated: &MigratedState,
    ) -> Result<QuoteResult, TradingVenueError> {
        let mode_assets = migrated.projected_mode_assets(venue_state.current_ts);
        let price = math::deposit_price(mode_assets, venue_state.mode_supply);

        let assets = request.amount;
        if assets == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
                price,
            });
        }
        // The minimum is still the vault's.
        if assets < venue_state.pool_config.lp_config.min_deposit_amount {
            return Err(TradingVenueError::AmmMethodError(
                "deposit below minimum".into(),
            ));
        }

        let total_assets = migrated
            .state
            .projected_total_assets(&migrated.config, venue_state.current_ts);
        let available_cap = migrated
            .config
            .pool
            .core
            .liquidity_cap
            .saturating_sub(total_assets);
        if available_cap == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: true,
                price,
            });
        }
        let assets_to_serve = assets.min(available_cap);

        let notes = strategy::convert_to_notes(assets_to_serve, mode_assets, migrated.note_supply)
            .ok_or_else(|| TradingVenueError::AmmMethodError("note supply overflow".into()))?;
        let shares =
            strategy::notes_to_mode_tokens(notes, venue_state.mode_supply, migrated.note_supply);
        if shares == 0 {
            return Err(TradingVenueError::AmmMethodError(
                "deposit rounds to zero shares".into(),
            ));
        }

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: assets_to_serve,
            expected_output: shares,
            not_enough_liquidity: assets_to_serve < assets,
            price,
        })
    }

    /// Price a migrated mode's instant withdrawal.
    fn quote_instant_withdraw_migrated(
        &self,
        request: &QuoteRequest,
        venue_state: &InitializedState,
        migrated: &MigratedState,
    ) -> Result<QuoteResult, TradingVenueError> {
        let tiers = &migrated
            .config
            .instant_withdrawal_config
            .instant_withdrawal_fee_configs;
        let mode_assets = migrated.projected_mode_assets(venue_state.current_ts);
        let total_assets = migrated
            .state
            .projected_total_assets(&migrated.config, venue_state.current_ts);
        let available = migrated.state.available_balance(
            migrated.strategy_underlying_balance,
            &migrated.config,
            venue_state.current_ts,
        );
        let liquid_assets = migrated
            .state
            .liquid_assets_deployed
            .saturating_add(available)
            .min(total_assets);

        // Underlying per `n` mode tokens, net of the fee the schedule charges there.
        let price_for = |n: u64| -> Result<f64, TradingVenueError> {
            let notes =
                strategy::mode_tokens_to_notes(n, migrated.note_supply, venue_state.mode_supply);
            let gross = strategy::convert_to_assets(notes, mode_assets, migrated.note_supply);
            let fee_bps = strategy::marginal_fee_bps(tiers, total_assets, liquid_assets, gross)
                .ok_or_else(|| {
                    TradingVenueError::AmmMethodError("instant withdrawal not available".into())
                })?;
            let spot = if venue_state.mode_supply == 0 {
                0.0
            } else {
                mode_assets as f64 / venue_state.mode_supply as f64
            };
            Ok(spot * (1.0 - fee_bps as f64 / HUNDRED_PERCENT_BPS as f64))
        };

        let shares = request.amount;
        if shares == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
                price: price_for(0)?,
            });
        }

        let notes =
            strategy::mode_tokens_to_notes(shares, migrated.note_supply, venue_state.mode_supply);
        if migrated.state.instant_withdrawal_rate_limit.would_exceed(
            notes,
            venue_state.current_ts,
            migrated
                .config
                .instant_withdrawal_config
                .max_total_instant_withdrawal_notes_per_window,
        ) || migrated.state.pool.redemption_rate_limit.would_exceed(
            notes,
            venue_state.current_ts,
            migrated.config.pool.max_total_redemption_notes_per_window,
        ) {
            return Err(TradingVenueError::AmmMethodError(
                "instant withdrawal window limit reached".into(),
            ));
        }

        let max_servable = available
            .saturating_add(
                migrated
                    .deployment
                    .as_ref()
                    .map(Deployment::available_liquidity_for_withdrawal)
                    .unwrap_or(0),
            )
            .min(mode_assets);
        let max_shares = if mode_assets == 0 {
            0
        } else {
            (max_servable as u128 * venue_state.mode_supply as u128 / mode_assets as u128) as u64
        };
        let shares_to_serve = shares.min(max_shares);
        if shares_to_serve == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: true,
                price: price_for(0)?,
            });
        }

        let notes_to_serve = strategy::mode_tokens_to_notes(
            shares_to_serve,
            migrated.note_supply,
            venue_state.mode_supply,
        );
        let gross = strategy::convert_to_assets(notes_to_serve, mode_assets, migrated.note_supply);
        let fee = strategy::progressive_fee(tiers, total_assets, liquid_assets, gross).ok_or_else(
            || TradingVenueError::AmmMethodError("instant withdrawal not available".into()),
        )?;

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: shares_to_serve,
            expected_output: gross.saturating_sub(fee),
            not_enough_liquidity: shares_to_serve < shares,
            price: price_for(shares_to_serve)?,
        })
    }

    fn deposit_account_metas(&self, user: Pubkey) -> Result<Vec<AccountMeta>, TradingVenueError> {
        let venue_state = self.state()?;
        let depositor_underlying =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &venue_state.pool_config.underlying_mint,
                &venue_state.underlying_token_program,
            );
        let depositor_mode =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &self.mode_mint_key,
                &venue_state.mode_token_program,
            );

        let mut metas = vec![
            AccountMeta::new_readonly(user, true),
            AccountMeta::new_readonly(venue_state.pool_config.huma_config, false),
            AccountMeta::new_readonly(self.pool_config_key, false),
            AccountMeta::new(self.pool_state_key, false),
            AccountMeta::new_readonly(self.mode_config_key, false),
            AccountMeta::new(self.mode_mint_key, false),
            AccountMeta::new_readonly(self.pool_authority_key, false),
            AccountMeta::new_readonly(venue_state.pool_config.underlying_mint, false),
            AccountMeta::new(venue_state.pool_underlying_token_key, false),
            AccountMeta::new(depositor_underlying, false),
            AccountMeta::new(depositor_mode, false),
        ];
        // The optional Strategy-layer slots sit *before* the two token programs, so this
        // is a splice rather than an append.
        metas.extend(self.deposit_strategy_metas());
        metas.extend([
            AccountMeta::new_readonly(venue_state.underlying_token_program, false),
            AccountMeta::new_readonly(venue_state.mode_token_program, false),
        ]);
        Ok(metas)
    }

    fn instant_withdraw_account_metas(
        &self,
        user: Pubkey,
    ) -> Result<Vec<AccountMeta>, TradingVenueError> {
        let venue_state = self.state()?;
        let lender_state_key = pda::derive_lender_state(&self.mode_config_key, &user);
        let lender_underlying =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &venue_state.pool_config.underlying_mint,
                &venue_state.underlying_token_program,
            );
        let lender_mode =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &self.mode_mint_key,
                &venue_state.mode_token_program,
            );

        let mut metas = vec![
            AccountMeta::new_readonly(user, true),
            AccountMeta::new_readonly(venue_state.pool_config.huma_config, false),
            AccountMeta::new_readonly(self.pool_config_key, false),
            AccountMeta::new(self.pool_state_key, false),
            AccountMeta::new_readonly(self.mode_config_key, false),
            AccountMeta::new(self.mode_mint_key, false),
            AccountMeta::new(lender_state_key, false),
            AccountMeta::new_readonly(venue_state.pool_config.underlying_mint, false),
            AccountMeta::new(self.pool_authority_key, false),
            AccountMeta::new(venue_state.pool_underlying_token_key, false),
            AccountMeta::new(lender_underlying, false),
            AccountMeta::new(lender_mode, false),
            AccountMeta::new_readonly(venue_state.underlying_token_program, false),
            AccountMeta::new_readonly(venue_state.mode_token_program, false),
        ];
        metas.extend(self.instant_withdraw_strategy_metas());
        // The vault forwards its remaining accounts to the strategy's own withdrawal, so
        // post-cutover these describe the strategy authority's position, not the pool's.
        match venue_state.migrated.as_ref() {
            Some(migrated) => {
                if let Some(deployment) = migrated.deployment.as_ref() {
                    metas.extend(deployment.instant_withdraw_remaining_accounts(
                        &migrated.strategy_authority_key,
                        &venue_state.underlying_token_program,
                    ));
                }
            }
            // Pre-cutover the pool serves the withdrawal itself: its treasury takes the fee and
            // its liquidity-source pair leads the venue accounts it pulls through. Never send
            // any of these on the migrated arm above: the vault forwards this list whole, so the
            // strategy would read them as its own venue accounts.
            None => {
                metas.push(AccountMeta::new(
                    venue_state.pool_owner_treasury_underlying_token_key,
                    false,
                ));
                metas.push(AccountMeta::new_readonly(
                    venue_state.deployment_config_key,
                    false,
                ));
                metas.push(AccountMeta::new(venue_state.deployment_state_key, false));
                metas.extend(venue_state.deployment.instant_withdraw_remaining_accounts(
                    &self.pool_authority_key,
                    &venue_state.underlying_token_program,
                ));
            }
        }
        Ok(metas)
    }

    /// Build a `create_lender_accounts_v2` instruction registering `lender` as a
    /// Huma lender on this mode, paid by `payer`.
    ///
    /// A lender's `lender_state` (and mode-token ATA) must exist before its first
    /// instant withdrawal. The program treats `lender` as an unchecked, non-signer
    /// address, so any `payer` can provision any lender — including the router's
    /// TitanPDA, which can't sign a standalone transaction. Used by the simulation
    /// harness to provision the swap signer before exercising the instant-withdraw
    /// direction.
    pub fn create_lender_accounts_ix(
        &self,
        payer: Pubkey,
        lender: Pubkey,
    ) -> Result<Instruction, TradingVenueError> {
        let venue_state = self.state()?;
        let lender_state = pda::derive_lender_state(&self.mode_config_key, &lender);
        let lender_mode_token =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &lender,
                &self.mode_mint_key,
                &venue_state.mode_token_program,
            );

        Ok(Instruction {
            program_id: VAULT_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(payer, true),
                AccountMeta::new_readonly(lender, false),
                AccountMeta::new_readonly(venue_state.pool_config.huma_config, false),
                AccountMeta::new_readonly(self.pool_config_key, false),
                AccountMeta::new_readonly(self.pool_state_key, false),
                AccountMeta::new_readonly(self.mode_config_key, false),
                AccountMeta::new_readonly(self.mode_mint_key, false),
                AccountMeta::new(lender_state, false),
                AccountMeta::new(lender_mode_token, false),
                AccountMeta::new_readonly(venue_state.mode_token_program, false),
                AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data: HumaInstruction::CreateLenderAccountsV2.pack(),
        })
    }

    fn swap_direction(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
    ) -> Result<SwapDirection, TradingVenueError> {
        let underlying = self.state()?.pool_config.underlying_mint;
        if input_mint == underlying && output_mint == self.mode_mint_key {
            Ok(SwapDirection::Deposit)
        } else if input_mint == self.mode_mint_key && output_mint == underlying {
            Ok(SwapDirection::InstantWithdraw)
        } else {
            Err(TradingVenueError::InvalidMint(input_mint.into()))
        }
    }
}

impl FromAccount for HumaVenue {
    /// Constructs a venue from a `ModeConfig` keyed account. The pool is fixed
    /// to [`POOL_CONFIG_KEY`]; the keyed pubkey identifies the mode within it.
    /// Validates the account decodes as a `ModeConfig` to fail fast on the
    /// wrong account type.
    fn from_account(pubkey: &Pubkey, account: &Account) -> Result<Self, TradingVenueError>
    where
        Self: Sized,
    {
        let mode_config: ModeConfig = state::decode_anchor_account("ModeConfig", account.data())?;
        Ok(HumaVenue::new(POOL_CONFIG_KEY, *pubkey, mode_config))
    }
}

#[async_trait]
impl TradingVenue for HumaVenue {
    fn initialized(&self) -> bool {
        self.state.is_some()
    }

    fn program_id(&self) -> Pubkey {
        VAULT_PROGRAM_ID
    }

    fn program_dependencies(&self) -> Vec<Pubkey> {
        let mut programs = vec![
            VAULT_PROGRAM_ID,
            HUMA_PROGRAM_ID,
            JUP_LENDING_PROGRAM_ID,
            JUP_LIQUIDITY_PROGRAM_ID,
            JUP_LRRM_PROGRAM_ID,
            KLEND_PROGRAM_ID,
        ];
        // A migrated mode's swap CPIs into the Strategy layer, so the simulation needs
        // its binary too.
        if self
            .state
            .as_ref()
            .is_some_and(|venue_state| venue_state.migrated.is_some())
        {
            programs.push(STRATEGY_PROGRAM_ID);
        }
        programs
    }

    fn market_id(&self) -> Pubkey {
        self.mode_config_key
    }

    fn get_token_info(&self) -> &[TokenInfo] {
        self.state
            .as_ref()
            .map(|venue_state| venue_state.token_info.as_slice())
            .unwrap_or(&[])
    }

    fn protocol(&self) -> PoolProtocol {
        PoolProtocol::Huma
    }

    fn get_required_pubkeys_for_update(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        let venue_state = self.state()?;
        let mut keys = vec![
            self.pool_config_key,
            self.mode_config_key,
            self.pool_state_key,
            self.mode_mint_key,
            venue_state.pool_config.underlying_mint,
            venue_state.pool_underlying_token_key,
            venue_state.deployment_config_key,
            venue_state.pool_config.huma_config,
            clock::ID,
        ];
        keys.extend(venue_state.deployment.required_pubkeys_for_update());
        // Asked for only once the mode has been observed as migrated, so an unmigrated
        // pool never depends on a strategy existing. The cutover therefore costs one
        // refresh cycle: the flag is seen on the update that reads `ModeConfig`, and the
        // strategy accounts arrive on the next one.
        if let Some(migrated) = venue_state.migrated.as_ref() {
            keys.extend([
                STRATEGY_CONFIG_KEY,
                migrated.strategy_state_key,
                migrated.note_mint,
                migrated.strategy_underlying_token_key,
            ]);
            if let Some(deployment) = migrated.deployment.as_ref() {
                keys.extend(deployment.required_pubkeys_for_update());
            }
        }
        Ok(keys)
    }

    async fn update_state(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        // Round 1: PoolConfig + ModeConfig. ModeConfig is re-fetched (despite
        // being decoded in `new`) because `periodic_apy_bps` can change.
        let [pool_config_account, mode_config_account]: [Option<Account>; 2] = cache
            .get_accounts(&[self.pool_config_key, self.mode_config_key])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;
        let pool_config_account = pool_config_account.ok_or(TradingVenueError::NoAccountFound(
            self.pool_config_key.into(),
        ))?;
        let mode_config_account = mode_config_account.ok_or(TradingVenueError::NoAccountFound(
            self.mode_config_key.into(),
        ))?;

        let pool_config: PoolConfig =
            state::decode_anchor_account("PoolConfig", pool_config_account.data())?;
        let mode_config: ModeConfig =
            state::decode_anchor_account("ModeConfig", mode_config_account.data())?;

        // The stored bump is canonical (set by Anchor in `create_pool`), but
        // deriving from it mirrors the on-chain ix's `bump =
        // pool_config.pool_authority_bump` constraint and avoids a second
        // `find_program_address` call.
        self.pool_authority_key =
            pda::pool_authority_with_bump(&self.pool_config_key, pool_config.pool_authority_bump)
                .ok_or(TradingVenueError::DeserializationFailed(
                "invalid pool_authority_bump".into(),
            ))?;

        let huma_config_key = pool_config.huma_config;
        let deployment_config_key = pool_config
            .instant_withdrawal_config
            .liquidity_source
            .ok_or(TradingVenueError::MissingState(
                "PoolConfig.liquidity_source".into(),
            ))?;
        let deployment_state_key = pda::derive_deployment_state(&deployment_config_key);
        // Token program is provisional until we read the underlying-mint owner
        // below. ATAs derive from the program, so re-derive after that read.
        let pool_underlying_token_key =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &self.pool_authority_key,
                &pool_config.underlying_mint,
                &SPL_TOKEN_PROGRAM_ID,
            );
        let pool_owner_treasury_underlying_token_key =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &pool_config.pool_owner_treasury,
                &pool_config.underlying_mint,
                &SPL_TOKEN_PROGRAM_ID,
            );

        // Round 2: HumaConfig + DeploymentConfig + everything whose key is
        // derivable from PoolConfig. Only strategy-specific accounts wait
        // for round 3 because they depend on the decoded DeploymentConfig.
        let [
            huma_config_account,
            deployment_config_account,
            pool_state_account,
            mode_mint_account,
            pool_underlying_account,
            underlying_mint_account,
            clock_account,
        ]: [Option<Account>; 7] = cache
            .get_accounts(&[
                huma_config_key,
                deployment_config_key,
                self.pool_state_key,
                self.mode_mint_key,
                pool_underlying_token_key,
                pool_config.underlying_mint,
                clock::ID,
            ])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;
        let huma_config_account =
            huma_config_account.ok_or(TradingVenueError::NoAccountFound(huma_config_key.into()))?;
        let deployment_config_account = deployment_config_account.ok_or(
            TradingVenueError::NoAccountFound(deployment_config_key.into()),
        )?;
        let pool_state_account = pool_state_account.ok_or(TradingVenueError::NoAccountFound(
            self.pool_state_key.into(),
        ))?;
        let mode_mint_account = mode_mint_account
            .ok_or(TradingVenueError::NoAccountFound(self.mode_mint_key.into()))?;
        let pool_underlying_account = pool_underlying_account.ok_or(
            TradingVenueError::NoAccountFound(pool_underlying_token_key.into()),
        )?;
        let underlying_mint_account = underlying_mint_account.ok_or(
            TradingVenueError::NoAccountFound(pool_config.underlying_mint.into()),
        )?;
        let clock_account =
            clock_account.ok_or(TradingVenueError::NoAccountFound(clock::ID.into()))?;

        let huma_config: HumaConfig =
            state::decode_anchor_account("HumaConfig", huma_config_account.data())?;
        let deployment_config: DeploymentConfig =
            state::decode_anchor_account("DeploymentConfig", deployment_config_account.data())?;
        let mut deployment = Deployment::new(
            &deployment_config.strategy_type,
            deployment_config.target_key,
            pool_config.underlying_mint,
            self.pool_authority_key,
        )?;

        let pool_state: PoolState =
            state::decode_anchor_account("PoolState", pool_state_account.data())?;
        let mode_index = pool_state.mode_index_for(&self.mode_config_key).ok_or(
            TradingVenueError::MissingState("mode_config_key not found in pool_state".into()),
        )?;

        let mode_supply = state::read_mint_supply(mode_mint_account.data())?;
        let mode_token_program = *mode_mint_account.owner();
        let underlying_token_program = *underlying_mint_account.owner();
        let pool_underlying_balance =
            state::read_token_account_amount(pool_underlying_account.data())?;

        let clock: Clock = bincode::deserialize(clock_account.data())
            .map_err(|e| TradingVenueError::DeserializationFailed(format!("clock: {e}").into()))?;
        let current_ts = clock.unix_timestamp.max(0) as u64;

        let token_info = [
            TokenInfo::new(
                &pool_config.underlying_mint,
                &underlying_mint_account,
                clock.epoch,
            )?,
            TokenInfo::new(&self.mode_mint_key, &mode_mint_account, clock.epoch)?,
        ];

        // Round 3: deployment-venue accounts (cache-fed by the venue).
        deployment.update(cache).await?;

        // Round 4: the Strategy layer, once the mode has been cut over to it. Skipped
        // entirely while `strategy_note_mint` is unset, so the pre-cutover path costs
        // nothing and never depends on a strategy existing.
        let migrated = if mode_config.is_migrated() {
            Some(
                Self::fetch_migrated_state(
                    cache,
                    mode_config.strategy_note_mint,
                    &pool_config,
                    self.pool_authority_key,
                    underlying_token_program,
                )
                .await?,
            )
        } else {
            None
        };

        // Atomic commit.
        self.mode_config = mode_config;
        self.state = Some(InitializedState {
            pool_config,
            pool_state,
            huma_config,
            deployment_config_key,
            deployment_state_key,
            pool_underlying_token_key,
            pool_owner_treasury_underlying_token_key,
            underlying_token_program,
            mode_token_program,
            mode_index,
            mode_supply,
            pool_underlying_balance,
            current_ts,
            deployment,
            token_info,
            migrated,
        });
        Ok(())
    }

    fn quote(&self, request: QuoteRequest) -> Result<QuoteResult, TradingVenueError> {
        if request.swap_type == SwapType::ExactOut {
            return Err(TradingVenueError::ExactOutNotSupported);
        }
        if !self.is_active() {
            return Err(TradingVenueError::InactivePoolError(
                self.pool_config_key,
                self.protocol(),
            ));
        }

        match self.swap_direction(request.input_mint, request.output_mint)? {
            SwapDirection::Deposit => self.quote_deposit(&request),
            SwapDirection::InstantWithdraw => self.quote_instant_withdraw(&request),
        }
    }

    fn generate_swap_instruction(
        &self,
        request: QuoteRequest,
        user: Pubkey,
    ) -> Result<Instruction, TradingVenueError> {
        if request.swap_type == SwapType::ExactOut {
            return Err(TradingVenueError::ExactOutNotSupported);
        }
        match self.swap_direction(request.input_mint, request.output_mint)? {
            SwapDirection::Deposit => Ok(Instruction {
                program_id: VAULT_PROGRAM_ID,
                accounts: self.deposit_account_metas(user)?,
                data: HumaInstruction::Deposit {
                    assets: request.amount,
                }
                .pack(),
            }),
            // `max_fee` is an absolute slippage cap in underlying atoms; u64::MAX
            // is the loosest bound (accept any fee — we already priced it in).
            SwapDirection::InstantWithdraw => Ok(Instruction {
                program_id: VAULT_PROGRAM_ID,
                accounts: self.instant_withdraw_account_metas(user)?,
                data: HumaInstruction::InstantWithdraw {
                    shares: request.amount,
                    max_fee: u64::MAX,
                }
                .pack(),
            }),
        }
    }
}

#[async_trait]
impl AddressLookupTableTrait for HumaVenue {
    async fn get_lookup_table_keys(
        &self,
        _accounts_cache: Option<&dyn AccountsCache>,
    ) -> Result<Vec<Pubkey>, TradingVenueError> {
        let venue_state = self.state()?;
        let mut keys = vec![
            VAULT_PROGRAM_ID,
            HUMA_PROGRAM_ID,
            self.pool_config_key,
            self.mode_config_key,
            self.pool_state_key,
            self.pool_authority_key,
            self.mode_mint_key,
            venue_state.pool_config.underlying_mint,
            venue_state.pool_config.huma_config,
            venue_state.pool_underlying_token_key,
            venue_state.pool_owner_treasury_underlying_token_key,
            venue_state.deployment_config_key,
            venue_state.deployment_state_key,
            venue_state.underlying_token_program,
            venue_state.mode_token_program,
        ];
        keys.extend(venue_state.deployment.lookup_table_keys());
        if let Some(migrated) = venue_state.migrated.as_ref() {
            keys.extend([
                STRATEGY_PROGRAM_ID,
                STRATEGY_CONFIG_KEY,
                migrated.strategy_state_key,
                migrated.strategy_authority_key,
                migrated.note_mint,
                migrated.note_token_program,
                migrated.pool_authority_note_token_key,
                migrated.strategy_underlying_token_key,
                migrated.strategy_treasury_underlying_token_key,
            ]);
            keys.extend(migrated.deployment_config_key);
            keys.extend(migrated.deployment_state_key);
            if let Some(deployment) = migrated.deployment.as_ref() {
                keys.extend(deployment.lookup_table_keys());
            }
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huma::state::ModeConfig;

    fn unmigrated_venue() -> HumaVenue {
        HumaVenue::new(POOL_CONFIG_KEY, Pubkey::new_unique(), ModeConfig::default())
    }

    /// `deposit` takes eight optional Strategy-layer accounts and `instant_withdraw`
    /// eleven. The program does not enable Anchor's `allow-missing-optionals`, so the
    /// slots have to be materialized either way — with the vault program's own ID
    /// standing for an absent account.
    #[test]
    fn unmigrated_strategy_slots_are_program_id() {
        let venue = unmigrated_venue();

        let deposit = venue.deposit_strategy_metas();
        assert_eq!(deposit.len(), 8);
        assert!(deposit.iter().all(|m| m.pubkey == VAULT_PROGRAM_ID));
        assert!(deposit.iter().all(|m| !m.is_signer && !m.is_writable));

        let withdraw = venue.instant_withdraw_strategy_metas();
        assert_eq!(withdraw.len(), 11);
        assert!(withdraw.iter().all(|m| m.pubkey == VAULT_PROGRAM_ID));
    }

    #[test]
    fn default_mode_config_is_unmigrated() {
        assert!(!ModeConfig::default().is_migrated());
        let migrated = ModeConfig {
            strategy_note_mint: Pubkey::new_unique(),
            ..ModeConfig::default()
        };
        assert!(migrated.is_migrated());
    }
}
