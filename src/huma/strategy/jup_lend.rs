use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, JUP_LENDING_PROGRAM_ID, JUP_LIQUIDITY_PROGRAM_ID,
    JUP_LRRM_PROGRAM_ID, SYSTEM_PROGRAM_ID,
};
use crate::huma::pda;
use crate::trading_venue::error::TradingVenueError;

fn derive_f_token_mint(liquidity_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"f_token_mint", liquidity_mint.as_ref()],
        &JUP_LENDING_PROGRAM_ID,
    )
    .0
}

fn derive_lending_admin() -> Pubkey {
    Pubkey::find_program_address(&[b"lending_admin"], &JUP_LENDING_PROGRAM_ID).0
}

fn derive_supply_token_reserves_liquidity(liquidity_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"reserve", liquidity_mint.as_ref()],
        &JUP_LIQUIDITY_PROGRAM_ID,
    )
    .0
}

fn derive_lending_supply_position_on_liquidity(
    liquidity_mint: &Pubkey,
    lending_pda: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"user_supply_position",
            liquidity_mint.as_ref(),
            lending_pda.as_ref(),
        ],
        &JUP_LIQUIDITY_PROGRAM_ID,
    )
    .0
}

fn derive_rate_model(liquidity_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"rate_model", liquidity_mint.as_ref()],
        &JUP_LIQUIDITY_PROGRAM_ID,
    )
    .0
}

fn derive_liquidity() -> Pubkey {
    Pubkey::find_program_address(&[b"liquidity"], &JUP_LIQUIDITY_PROGRAM_ID).0
}

fn derive_rewards_rate_model(liquidity_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"lending_rewards_rate_model", liquidity_mint.as_ref()],
        &JUP_LRRM_PROGRAM_ID,
    )
    .0
}

#[derive(Clone)]
pub struct JupLendStrategy {
    lending: Pubkey,
    liquidity_mint: Pubkey,
}

impl JupLendStrategy {
    pub fn new(lending: Pubkey, liquidity_mint: Pubkey) -> Self {
        Self {
            lending,
            liquidity_mint,
        }
    }

    pub fn required_pubkeys_for_update(&self) -> Vec<Pubkey> {
        Vec::new()
    }

    pub async fn update(&mut self, _cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        Ok(())
    }

    pub fn instant_withdraw_remaining_accounts(
        &self,
        pool_authority_key: &Pubkey,
        underlying_token_program: &Pubkey,
    ) -> Vec<AccountMeta> {
        let liquidity_mint = &self.liquidity_mint;

        let f_token_mint = derive_f_token_mint(liquidity_mint);
        let owner_token_account =
            pda::derive_ata(pool_authority_key, underlying_token_program, &f_token_mint);
        let lending_admin = derive_lending_admin();
        let supply_token_reserves_liquidity =
            derive_supply_token_reserves_liquidity(liquidity_mint);
        let lending_supply_position_on_liquidity =
            derive_lending_supply_position_on_liquidity(liquidity_mint, &self.lending);
        let rate_model = derive_rate_model(liquidity_mint);
        let liquidity = derive_liquidity();
        let vault = pda::derive_ata(&liquidity, underlying_token_program, liquidity_mint);
        let rewards_rate_model = derive_rewards_rate_model(liquidity_mint);

        vec![
            AccountMeta::new(owner_token_account, false),
            AccountMeta::new_readonly(lending_admin, false),
            AccountMeta::new(self.lending, false),
            AccountMeta::new(f_token_mint, false),
            AccountMeta::new(supply_token_reserves_liquidity, false),
            AccountMeta::new(lending_supply_position_on_liquidity, false),
            AccountMeta::new_readonly(rate_model, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(liquidity, false),
            AccountMeta::new(JUP_LIQUIDITY_PROGRAM_ID, false),
            AccountMeta::new_readonly(rewards_rate_model, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(JUP_LENDING_PROGRAM_ID, false),
        ]
    }

    /// JupLend doesn't expose a typed reader for its reserve liquidity, so we
    /// treat the available amount as unbounded and let the on-chain CPI fail
    /// loudly if the protocol is illiquid.
    pub fn available_liquidity_for_withdrawal(&self) -> u64 {
        u64::MAX
    }

    pub fn lookup_table_keys(&self) -> Vec<Pubkey> {
        vec![
            self.lending,
            JUP_LENDING_PROGRAM_ID,
            JUP_LIQUIDITY_PROGRAM_ID,
            JUP_LRRM_PROGRAM_ID,
        ]
    }
}
