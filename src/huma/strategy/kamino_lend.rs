use klend_interface::state::{self, Obligation, Reserve};
use solana_account::{Account, ReadableAccount};
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::constants::{
    KFARMS_PROGRAM_ID, KLEND_FRACTION_BITS, KLEND_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID,
    SYSVAR_INSTRUCTIONS_ID,
};
use crate::trading_venue::error::TradingVenueError;

fn derive_lending_market_authority(lending_market: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"lma", lending_market.as_ref()], &KLEND_PROGRAM_ID).0
}

/// Vanilla obligation PDA: tag=0, id=0, both seed-account slots zero. Matches the
/// `setup_deployment_target` flow on the permissionless program, which is the
/// only obligation Huma creates per (pool_authority, lending_market).
fn derive_obligation(pool_authority: &Pubkey, lending_market: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            &[0u8],
            &[0u8],
            pool_authority.as_ref(),
            lending_market.as_ref(),
            Pubkey::default().as_ref(),
            Pubkey::default().as_ref(),
        ],
        &KLEND_PROGRAM_ID,
    )
    .0
}

fn derive_obligation_farm_user_state(farm_state: &Pubkey, obligation: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"user", farm_state.as_ref(), obligation.as_ref()],
        &KFARMS_PROGRAM_ID,
    )
    .0
}

#[derive(Clone)]
pub struct KaminoLendStrategy {
    reserve: Pubkey,
    pool_authority: Pubkey,
    // Populated by `update` from the reserve account's `lending_market` field.
    lending_market: Pubkey,
    // PDA derived from `pool_authority` + `lending_market`. Set on first update.
    obligation: Pubkey,
    // The next three are read directly from the reserve account, not derived as
    // PDAs — older Kamino reserves use keypair-based token accounts, not the
    // canonical `reserve_coll_supply` / `reserve_liq_supply` PDA convention.
    reserve_collateral_mint: Pubkey,
    reserve_collateral_supply: Pubkey,
    reserve_liquidity_supply: Pubkey,
    // From `reserve.farm_collateral`. `Some` iff a collateral farm is attached;
    // when present, V2 deposit/withdraw CPIs require a 2-account farm trailer.
    farm_state: Option<Pubkey>,
    obligation_farm_user_state: Option<Pubkey>,
    /// Maximum underlying we can pull from Kamino on demand:
    /// `min(reserve.available_liquidity, deposited_collateral × exchange_rate)`.
    /// Captures both "the protocol has free liquidity" and "the pool's obligation
    /// holds enough kTokens to redeem for that underlying."
    available_liquidity: u64,
}

impl KaminoLendStrategy {
    pub fn new(reserve: Pubkey, pool_authority: Pubkey) -> Self {
        Self {
            reserve,
            pool_authority,
            lending_market: Pubkey::default(),
            obligation: Pubkey::default(),
            reserve_collateral_mint: Pubkey::default(),
            reserve_collateral_supply: Pubkey::default(),
            reserve_liquidity_supply: Pubkey::default(),
            farm_state: None,
            obligation_farm_user_state: None,
            available_liquidity: 0,
        }
    }

    pub fn required_pubkeys_for_update(&self) -> Vec<Pubkey> {
        // `obligation` is unknown until the first update has read `reserve.lending_market`.
        // Once known, exposing it here lets the orchestrator pre-fetch both accounts in
        // a single batch on subsequent updates.
        let mut keys = vec![self.reserve];
        if self.obligation != Pubkey::default() {
            keys.push(self.obligation);
        }
        keys
    }

    pub async fn update(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        // Round 1: reserve. Needed first because `lending_market` and `farm_collateral`
        // determine which obligation / farm accounts to fetch in round 2.
        let [reserve_account]: [Option<Account>; 1] = cache
            .get_accounts(&[self.reserve])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;
        let reserve_account =
            reserve_account.ok_or(TradingVenueError::NoAccountFound(self.reserve.into()))?;
        let reserve: &Reserve = state::from_account_data(reserve_account.data()).map_err(|e| {
            TradingVenueError::DeserializationFailed(format!("KLend reserve: {e}").into())
        })?;

        self.lending_market = reserve.lending_market;
        self.reserve_collateral_mint = reserve.collateral.mint_pubkey;
        self.reserve_collateral_supply = reserve.collateral.supply_vault;
        self.reserve_liquidity_supply = reserve.liquidity.supply_vault;
        self.obligation = derive_obligation(&self.pool_authority, &self.lending_market);
        if reserve.farm_collateral != Pubkey::default() {
            let farm_state = reserve.farm_collateral;
            self.farm_state = Some(farm_state);
            self.obligation_farm_user_state = Some(derive_obligation_farm_user_state(
                &farm_state,
                &self.obligation,
            ));
        } else {
            self.farm_state = None;
            self.obligation_farm_user_state = None;
        }

        let available = reserve.available_liquidity();
        let total_collateral = reserve.collateral_total_supply();
        // `borrowed_amount()` returns a U68F60 scaled fraction; right-shift by
        // `KLEND_FRACTION_BITS` to recover atoms so it matches `available`'s units.
        let total_supply_underlying =
            available as u128 + (reserve.borrowed_amount() >> KLEND_FRACTION_BITS);

        // Round 2: obligation. Read this reserve's `deposited_amount` (kTokens
        // held by the pool authority's obligation).
        let [obligation_account]: [Option<Account>; 1] = cache
            .get_accounts(&[self.obligation])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;
        let deposited_collateral = match obligation_account {
            Some(account) => {
                let obligation: &Obligation =
                    state::from_account_data(account.data()).map_err(|e| {
                        TradingVenueError::DeserializationFailed(
                            format!("KLend obligation: {e}").into(),
                        )
                    })?;
                obligation
                    .deposits
                    .iter()
                    .find(|d| d.deposit_reserve == self.reserve)
                    .map(|d| d.deposited_amount)
                    .unwrap_or(0)
            }
            None => 0,
        };

        // exchange_rate = total_supply_underlying / collateral_total_supply.
        // redemption_value = deposited × exchange_rate.
        let redemption_value = if total_collateral == 0 {
            0
        } else {
            ((deposited_collateral as u128 * total_supply_underlying) / total_collateral as u128)
                as u64
        };
        self.available_liquidity = available.min(redemption_value);
        Ok(())
    }

    pub fn available_liquidity_for_withdrawal(&self) -> u64 {
        self.available_liquidity
    }

    /// Returns the 11 or 13 remaining accounts for the instant withdrawal CPI
    /// into KLend's `WithdrawObligationCollateralAndRedeemReserveCollateralV2`.
    ///
    /// The 2-account farm trailer is present iff the reserve has a collateral
    /// farm attached (`reserve.farm_collateral != Pubkey::default()`).
    ///
    /// Order:
    /// 1.  `reserve` (writable)
    /// 2.  `lending_market`
    /// 3.  `lending_market_authority`
    /// 4.  `obligation` (writable)
    /// 5.  `reserve_collateral_mint` (writable)
    /// 6.  `reserve_source_collateral` (writable) — Kamino's per-reserve
    ///     collateral supply vault, where the pool's kTokens live.
    /// 7.  `reserve_liquidity_supply` (writable)
    /// 8.  `collateral_token_program`
    /// 9.  `instruction_sysvar_account`
    /// 10. `farms_program`
    /// 11. `obligation_farm_user_state` (writable, **only when farmed**)
    /// 12. `reserve_farm_state` (writable, **only when farmed**)
    /// Last. `klend_program`
    pub fn instant_withdraw_remaining_accounts(
        &self,
        _pool_authority_key: &Pubkey,
        _underlying_token_program: &Pubkey,
    ) -> Vec<AccountMeta> {
        let lending_market_authority = derive_lending_market_authority(&self.lending_market);

        let mut metas = vec![
            AccountMeta::new(self.reserve, false),
            AccountMeta::new_readonly(self.lending_market, false),
            AccountMeta::new_readonly(lending_market_authority, false),
            AccountMeta::new(self.obligation, false),
            AccountMeta::new(self.reserve_collateral_mint, false),
            AccountMeta::new(self.reserve_collateral_supply, false),
            AccountMeta::new(self.reserve_liquidity_supply, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSVAR_INSTRUCTIONS_ID, false),
            AccountMeta::new_readonly(KFARMS_PROGRAM_ID, false),
        ];
        if let (Some(farm_state), Some(user_state)) =
            (self.farm_state, self.obligation_farm_user_state)
        {
            metas.push(AccountMeta::new(user_state, false));
            metas.push(AccountMeta::new(farm_state, false));
        }
        metas.push(AccountMeta::new_readonly(KLEND_PROGRAM_ID, false));
        metas
    }

    pub fn lookup_table_keys(&self) -> Vec<Pubkey> {
        let mut keys = vec![
            self.reserve,
            self.lending_market,
            self.obligation,
            self.reserve_collateral_mint,
            self.reserve_collateral_supply,
            self.reserve_liquidity_supply,
            KLEND_PROGRAM_ID,
            KFARMS_PROGRAM_ID,
        ];
        if let Some(farm_state) = self.farm_state {
            keys.push(farm_state);
        }
        if let Some(user_state) = self.obligation_farm_user_state {
            keys.push(user_state);
        }
        keys
    }
}
