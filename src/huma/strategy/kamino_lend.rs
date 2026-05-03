use klend_interface::state::Reserve;
use solana_account::{Account, ReadableAccount};
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::constants::{KLEND_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID, SYSVAR_INSTRUCTIONS_ID};
use crate::huma::pda::derive_ata;
use crate::huma::state::read_token_account_amount;
use crate::trading_venue::error::TradingVenueError;

/// 8-byte Anchor discriminator prefix on the KLend Reserve account.
const DISCRIMINATOR_LEN: usize = 8;

fn derive_lending_market_authority(lending_market: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"lma", lending_market.as_ref()], &KLEND_PROGRAM_ID).0
}

fn derive_reserve_liquidity_supply(reserve: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"reserve_liq_supply", reserve.as_ref()],
        &KLEND_PROGRAM_ID,
    )
    .0
}

fn derive_reserve_collateral_mint(reserve: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"reserve_coll_mint", reserve.as_ref()], &KLEND_PROGRAM_ID).0
}

#[derive(Clone)]
pub struct KaminoLendStrategy {
    reserve: Pubkey,
    lending_market: Pubkey,
    reserve_collateral_mint: Pubkey,
    reserve_liquidity_supply: Pubkey,
    /// Pool authority's k-token ATA — what we redeem to pull underlying back.
    pool_authority_k_token_ata: Pubkey,
    /// Maximum underlying we can pull from Kamino on demand:
    /// `min(reserve.total_available_amount, k_token_balance × exchange_rate)`.
    /// Captures both "the protocol has free liquidity" and "we hold enough
    /// k-tokens to redeem for that underlying."
    available_liquidity: u64,
}

impl KaminoLendStrategy {
    pub fn new(reserve: Pubkey, pool_authority: Pubkey) -> Self {
        let reserve_collateral_mint = derive_reserve_collateral_mint(&reserve);
        let pool_authority_k_token_ata = derive_ata(
            &pool_authority,
            &SPL_TOKEN_PROGRAM_ID,
            &reserve_collateral_mint,
        );
        Self {
            reserve,
            lending_market: Pubkey::default(),
            reserve_collateral_mint,
            reserve_liquidity_supply: derive_reserve_liquidity_supply(&reserve),
            pool_authority_k_token_ata,
            available_liquidity: 0,
        }
    }

    pub fn required_pubkeys_for_update(&self) -> Vec<Pubkey> {
        vec![self.reserve, self.pool_authority_k_token_ata]
    }

    pub async fn update(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let [reserve_account, k_token_account]: [Option<Account>; 2] = cache
            .get_accounts(&[self.reserve, self.pool_authority_k_token_ata])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;
        let reserve_account =
            reserve_account.ok_or(TradingVenueError::NoAccountFound(self.reserve.into()))?;
        let k_token_account = k_token_account.ok_or(TradingVenueError::NoAccountFound(
            self.pool_authority_k_token_ata.into(),
        ))?;
        let k_token_balance = read_token_account_amount(k_token_account.data())?;

        let body = reserve_account
            .data()
            .get(DISCRIMINATOR_LEN..)
            .unwrap_or(&[]);
        let reserve: &Reserve = bytemuck::try_from_bytes(body).map_err(|e| {
            TradingVenueError::DeserializationFailed(format!("KLend reserve: {e}").into())
        })?;

        self.lending_market = reserve.lending_market;

        // Underlying we'd get if we redeemed all our k-tokens at the current
        // exchange rate. exchange_rate = (available + borrowed) / collateral_total_supply.
        let available = reserve.available_liquidity();
        let total_collateral = reserve.collateral_total_supply();
        let redemption_value = if total_collateral == 0 {
            0
        } else {
            let total_supply_underlying = available as u128 + reserve.borrowed_amount();
            ((k_token_balance as u128 * total_supply_underlying) / total_collateral as u128) as u64
        };
        self.available_liquidity = available.min(redemption_value);
        Ok(())
    }

    pub fn available_liquidity_for_withdrawal(&self) -> u64 {
        self.available_liquidity
    }

    /// Returns the 9 remaining accounts for the instant withdrawal CPI into KLend.
    ///
    /// Account order:
    /// 1. `reserve` (writable)
    /// 2. `lending_market`
    /// 3. `lending_market_authority`
    /// 4. `reserve_collateral_mint` (writable)
    /// 5. `reserve_liquidity_supply` (writable)
    /// 6. `user_source_collateral` — pool authority's kToken ATA (writable)
    /// 7. `collateral_token_program` — SPL Token program
    /// 8. `instruction_sysvar_account`
    /// 9. `klend_program`
    pub fn instant_withdraw_remaining_accounts(
        &self,
        _pool_authority_key: &Pubkey,
        _underlying_token_program: &Pubkey,
    ) -> Vec<AccountMeta> {
        let lending_market_authority = derive_lending_market_authority(&self.lending_market);

        vec![
            AccountMeta::new(self.reserve, false),
            AccountMeta::new_readonly(self.lending_market, false),
            AccountMeta::new_readonly(lending_market_authority, false),
            AccountMeta::new(self.reserve_collateral_mint, false),
            AccountMeta::new(self.reserve_liquidity_supply, false),
            AccountMeta::new(self.pool_authority_k_token_ata, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSVAR_INSTRUCTIONS_ID, false),
            AccountMeta::new_readonly(KLEND_PROGRAM_ID, false),
        ]
    }

    pub fn lookup_table_keys(&self) -> Vec<Pubkey> {
        vec![
            self.reserve,
            self.lending_market,
            self.reserve_collateral_mint,
            self.reserve_liquidity_supply,
            self.pool_authority_k_token_ata,
            KLEND_PROGRAM_ID,
        ]
    }
}
