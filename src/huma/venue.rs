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
    HUMA_PROGRAM_ID, JUP_LENDING_PROGRAM_ID, JUP_LIQUIDITY_PROGRAM_ID, JUP_LRRM_PROGRAM_ID,
    KLEND_PROGRAM_ID, POOL_CONFIG_KEY, PROGRAM_ID, SPL_TOKEN_PROGRAM_ID,
};
use crate::huma::instruction::HumaInstruction;
use crate::huma::state::{DeploymentConfig, HumaConfig, ModeConfig, PoolConfig, PoolState};
use crate::huma::strategy::Strategy;
use crate::huma::{math, pda, state};
use crate::trading_venue::{
    AddressLookupTableTrait, FromAccount, QuoteRequest, QuoteResult, SwapType, TradingVenue,
    error::TradingVenueError, protocol::PoolProtocol, token_info::TokenInfo,
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

    strategy: Strategy,
    token_info: [TokenInfo; 2],
}

#[derive(Copy, Clone)]
enum SwapDirection {
    Deposit,
    InstantWithdraw,
}

/// Builds the trivial 0-in / 0-out quote required by the trait contract for
/// zero-amount requests.
fn zero_quote(request: &QuoteRequest) -> QuoteResult {
    QuoteResult {
        input_mint: request.input_mint,
        output_mint: request.output_mint,
        amount: 0,
        expected_output: 0,
        not_enough_liquidity: false,
    }
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
            .map(|s| !s.huma_config.paused && s.pool_state.is_pool_on())
            .unwrap_or(false)
    }

    fn quote_deposit(&self, request: &QuoteRequest) -> Result<QuoteResult, TradingVenueError> {
        let s = self.state()?;

        let assets = request.amount;
        if assets == 0 {
            return Ok(zero_quote(request));
        }
        if assets < s.pool_config.lp_config.min_deposit_amount {
            return Err(TradingVenueError::AmmMethodError(
                "deposit below minimum".into(),
            ));
        }

        // Refresh mode assets with accrued yield before computing the cap, so
        // our off-chain calculation matches the on-chain order of operations
        // (`refresh_assets_for_mode` runs before `LiquidityCapExceeded` check).
        let mode_assets = s
            .pool_state
            .mode_state(s.mode_index)
            .refreshed_assets(self.mode_config.periodic_apy_bps, s.current_ts);
        let total_assets = s
            .pool_state
            .refreshed_total_assets(s.mode_index, mode_assets);
        let available_cap = s
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

        let shares = math::shares_for_deposit(assets_to_serve, mode_assets, s.mode_supply).ok_or(
            TradingVenueError::AmmMethodError("zero shares minted".into()),
        )?;

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: assets_to_serve,
            expected_output: shares,
            not_enough_liquidity: partial,
        })
    }

    fn quote_instant_withdraw(
        &self,
        request: &QuoteRequest,
    ) -> Result<QuoteResult, TradingVenueError> {
        let s = self.state()?;

        let shares = request.amount;
        if shares == 0 {
            return Ok(zero_quote(request));
        }
        if !s.pool_state.all_mode_assets_fresh(s.current_ts) {
            return Err(TradingVenueError::AmmMethodError(
                "mode assets stale".into(),
            ));
        }

        let lp = &s.pool_config.lp_config;
        if s.pool_state
            .redemption
            .instant_withdrawal_gating
            .would_exceed(
                shares,
                s.current_ts,
                lp.max_instant_withdrawal_shares_per_window as u64,
            )
            || s.pool_state
                .redemption
                .global_redemption_gating
                .would_exceed(
                    shares,
                    s.current_ts,
                    lp.max_total_redemption_shares_per_window as u64,
                )
        {
            return Err(TradingVenueError::AmmMethodError(
                "instant withdrawal window limit reached".into(),
            ));
        }

        let mode_assets = s
            .pool_state
            .mode_state(s.mode_index)
            .refreshed_assets(self.mode_config.periodic_apy_bps, s.current_ts);
        let total_assets = s
            .pool_state
            .refreshed_total_assets(s.mode_index, mode_assets);
        let reserve_limit = s
            .pool_config
            .instant_withdrawal_config
            .instant_withdrawal_reserve_limit;
        let pool_available_balance = s
            .pool_state
            .get_available_balance(s.pool_underlying_balance, reserve_limit);

        // Clamp the request to what the pool + strategy can actually pay out.
        // The on-chain ix pulls `withdrawal_amount - pool_available_balance`
        // from the strategy when the pool's own balance is short; the strategy
        // already bounds its withdrawable amount by both protocol-side
        // liquidity and the pool's own redemption value (k-tokens for Kamino,
        // f-tokens for JupLend). Also cap by `mode_assets` — we can never
        // redeem more underlying than this mode is backed by, and exceeding it
        // would imply burning more shares than exist (`max_shares > mode_supply`).
        let max_servable_underlying = pool_available_balance
            .saturating_add(s.strategy.available_liquidity_for_withdrawal())
            .min(mode_assets);
        let max_shares = if mode_assets == 0 {
            0
        } else {
            (max_servable_underlying as u128 * s.mode_supply as u128 / mode_assets as u128) as u64
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
            });
        }

        let out = math::underlying_for_instant_withdraw(
            shares_to_serve,
            mode_assets,
            total_assets,
            s.mode_supply,
            pool_available_balance,
            s.pool_state.liquid_assets_deployed,
            &s.pool_config.instant_withdrawal_config,
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
        })
    }

    fn deposit_account_metas(&self, user: Pubkey) -> Result<Vec<AccountMeta>, TradingVenueError> {
        let s = self.state()?;
        let depositor_underlying =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &s.pool_config.underlying_mint,
                &s.underlying_token_program,
            );
        let depositor_mode =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &self.mode_mint_key,
                &s.mode_token_program,
            );

        Ok(vec![
            AccountMeta::new_readonly(user, true),
            AccountMeta::new_readonly(s.pool_config.huma_config, false),
            AccountMeta::new_readonly(self.pool_config_key, false),
            AccountMeta::new(self.pool_state_key, false),
            AccountMeta::new_readonly(self.mode_config_key, false),
            AccountMeta::new(self.mode_mint_key, false),
            AccountMeta::new_readonly(self.pool_authority_key, false),
            AccountMeta::new_readonly(s.pool_config.underlying_mint, false),
            AccountMeta::new(s.pool_underlying_token_key, false),
            AccountMeta::new(depositor_underlying, false),
            AccountMeta::new(depositor_mode, false),
            AccountMeta::new_readonly(s.underlying_token_program, false),
            AccountMeta::new_readonly(s.mode_token_program, false),
        ])
    }

    fn instant_withdraw_account_metas(
        &self,
        user: Pubkey,
    ) -> Result<Vec<AccountMeta>, TradingVenueError> {
        let s = self.state()?;
        let lender_state_key = pda::derive_lender_state(&self.mode_config_key, &user);
        let lender_underlying =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &s.pool_config.underlying_mint,
                &s.underlying_token_program,
            );
        let lender_mode =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &user,
                &self.mode_mint_key,
                &s.mode_token_program,
            );

        let mut metas = vec![
            AccountMeta::new_readonly(user, true),
            AccountMeta::new_readonly(s.pool_config.huma_config, false),
            AccountMeta::new_readonly(self.pool_config_key, false),
            AccountMeta::new(self.pool_state_key, false),
            AccountMeta::new_readonly(self.mode_config_key, false),
            AccountMeta::new(self.mode_mint_key, false),
            AccountMeta::new_readonly(s.deployment_config_key, false),
            AccountMeta::new(s.deployment_state_key, false),
            AccountMeta::new(lender_state_key, false),
            AccountMeta::new_readonly(s.pool_config.underlying_mint, false),
            AccountMeta::new(self.pool_authority_key, false),
            AccountMeta::new(s.pool_underlying_token_key, false),
            AccountMeta::new(lender_underlying, false),
            AccountMeta::new(s.pool_owner_treasury_underlying_token_key, false),
            AccountMeta::new(lender_mode, false),
            AccountMeta::new_readonly(s.underlying_token_program, false),
            AccountMeta::new_readonly(s.mode_token_program, false),
        ];
        metas.extend(s.strategy.instant_withdraw_remaining_accounts(
            &self.pool_authority_key,
            &s.underlying_token_program,
        ));
        Ok(metas)
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
        PROGRAM_ID
    }

    fn program_dependencies(&self) -> Vec<Pubkey> {
        vec![
            PROGRAM_ID,
            HUMA_PROGRAM_ID,
            JUP_LENDING_PROGRAM_ID,
            JUP_LIQUIDITY_PROGRAM_ID,
            JUP_LRRM_PROGRAM_ID,
            KLEND_PROGRAM_ID,
        ]
    }

    fn market_id(&self) -> Pubkey {
        self.mode_config_key
    }

    fn get_token_info(&self) -> &[TokenInfo] {
        self.state
            .as_ref()
            .map(|s| s.token_info.as_slice())
            .unwrap_or(&[])
    }

    fn protocol(&self) -> PoolProtocol {
        PoolProtocol::Huma
    }

    fn get_required_pubkeys_for_update(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        let s = self.state()?;
        let mut keys = vec![
            self.pool_config_key,
            self.mode_config_key,
            self.pool_state_key,
            self.mode_mint_key,
            s.pool_config.underlying_mint,
            s.pool_underlying_token_key,
            s.deployment_config_key,
            s.pool_config.huma_config,
            clock::ID,
        ];
        keys.extend(s.strategy.required_pubkeys_for_update());
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
        let mut strategy = Strategy::from_deployment_config(
            &deployment_config,
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

        // Round 3: strategy-specific accounts (cache-fed by the strategy).
        strategy.update(cache).await?;

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
            strategy,
            token_info,
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
                program_id: PROGRAM_ID,
                accounts: self.deposit_account_metas(user)?,
                data: HumaInstruction::Deposit {
                    assets: request.amount,
                }
                .pack(),
            }),
            // max_fee_bps = 10_000 is the loosest bound (we already priced in fees).
            SwapDirection::InstantWithdraw => Ok(Instruction {
                program_id: PROGRAM_ID,
                accounts: self.instant_withdraw_account_metas(user)?,
                data: HumaInstruction::InstantWithdraw {
                    shares: request.amount,
                    max_fee_bps: 10_000,
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
        let s = self.state()?;
        let mut keys = vec![
            PROGRAM_ID,
            HUMA_PROGRAM_ID,
            self.pool_config_key,
            self.mode_config_key,
            self.pool_state_key,
            self.pool_authority_key,
            self.mode_mint_key,
            s.pool_config.underlying_mint,
            s.pool_config.huma_config,
            s.pool_underlying_token_key,
            s.pool_owner_treasury_underlying_token_key,
            s.deployment_config_key,
            s.deployment_state_key,
            s.underlying_token_program,
            s.mode_token_program,
        ];
        keys.extend(s.strategy.lookup_table_keys());
        Ok(keys)
    }
}
