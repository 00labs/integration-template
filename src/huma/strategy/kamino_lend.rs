use klend_interface::state::Reserve;
use solana_account::ReadableAccount;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::constants::{KLEND_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID, SYSVAR_INSTRUCTIONS_ID};
use crate::huma::pda::derive_ata;
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
    /// Underlying tokens currently held by the reserve liquidity supply vault
    /// and immediately withdrawable. Read from `Reserve.liquidity.total_available_amount`.
    /// TODO: clamp instant-withdraw quotes against this when wired into the venue.
    #[allow(dead_code)]
    available_liquidity: u64,
}

impl KaminoLendStrategy {
    pub fn new(reserve: Pubkey) -> Self {
        Self {
            reserve,
            lending_market: Pubkey::default(),
            reserve_collateral_mint: derive_reserve_collateral_mint(&reserve),
            reserve_liquidity_supply: derive_reserve_liquidity_supply(&reserve),
            available_liquidity: 0,
        }
    }

    pub fn required_pubkeys_for_update(&self) -> Vec<Pubkey> {
        vec![self.reserve]
    }

    pub async fn update(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let reserve_account = cache
            .get_account(&self.reserve)
            .await?
            .ok_or(TradingVenueError::NoAccountFound(self.reserve.into()))?;
        let body = reserve_account
            .data()
            .get(DISCRIMINATOR_LEN..)
            .unwrap_or(&[]);
        let reserve: &Reserve = bytemuck::try_from_bytes(body).map_err(|e| {
            TradingVenueError::DeserializationFailed(format!("KLend reserve: {e}").into())
        })?;

        self.lending_market = reserve.lending_market;
        self.available_liquidity = reserve.liquidity.total_available_amount;
        Ok(())
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
        pool_authority_key: &Pubkey,
        _underlying_token_program: &Pubkey,
    ) -> Vec<AccountMeta> {
        let lending_market_authority = derive_lending_market_authority(&self.lending_market);
        let user_source_collateral = derive_ata(
            pool_authority_key,
            &SPL_TOKEN_PROGRAM_ID,
            &self.reserve_collateral_mint,
        );

        vec![
            AccountMeta::new(self.reserve, false),
            AccountMeta::new_readonly(self.lending_market, false),
            AccountMeta::new_readonly(lending_market_authority, false),
            AccountMeta::new(self.reserve_collateral_mint, false),
            AccountMeta::new(self.reserve_liquidity_supply, false),
            AccountMeta::new(user_source_collateral, false),
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
            KLEND_PROGRAM_ID,
        ]
    }
}
