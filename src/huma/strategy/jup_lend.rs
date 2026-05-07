use borsh::{BorshDeserialize, BorshSerialize};
use solana_account::{Account, ReadableAccount};
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, JUP_LENDING_PROGRAM_ID, JUP_LIQUIDITY_PROGRAM_ID,
    JUP_LRRM_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID, SYSTEM_PROGRAM_ID,
};
use crate::huma::state;
use crate::trading_venue::error::TradingVenueError;

/// Jupiter's exchange-price scaling factor (1e12) — converts between supply
/// shares and the underlying amount.
const EXCHANGE_PRICES_PRECISION: u128 = 1_000_000_000_000;

/// Layout of `liquidity::TokenReserve` (Jupiter Lending) — kept in sync with
/// `jup-lend-sdk` IDL `idls/liquidity.json`.
#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Default)]
struct TokenReserve {
    pub _mint: Pubkey,
    pub _vault: Pubkey,
    pub _borrow_rate: u16,
    pub _fee_on_interest: u16,
    pub _last_utilization: u16,
    pub _last_update_timestamp: u64,
    pub supply_exchange_price: u64,
    pub _borrow_exchange_price: u64,
    pub _max_utilization: u16,
    pub total_supply_with_interest: u64,
    pub total_supply_interest_free: u64,
    pub total_borrow_with_interest: u64,
    pub total_borrow_interest_free: u64,
    pub _total_claim_amount: u64,
    pub _interacting_protocol: Pubkey,
    pub _interacting_timestamp: u64,
    pub _interacting_balance: u64,
}

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
    reserve_key: Pubkey,
    /// PDA for our pool's `UserSupplyPosition` — required by Jupiter's
    /// instant_withdraw CPI but not used for the liquidity calculation.
    position_key: Pubkey,
    f_token_mint: Pubkey,
    /// Pool authority's f-token ATA — what Jupiter burns when we instant-withdraw.
    /// Defaults to SPL Token program; Token-2022 underlyings would need a
    /// different program here.
    owner_f_token_ata: Pubkey,
    /// Maximum underlying we can pull from JupLend on demand:
    /// `min(reserve_vault_liquid, our_f_token_redemption_value)`.
    available_liquidity: u64,
}

impl JupLendStrategy {
    pub fn new(lending: Pubkey, liquidity_mint: Pubkey, pool_authority: Pubkey) -> Self {
        let reserve_key = derive_supply_token_reserves_liquidity(&liquidity_mint);
        let position_key = derive_lending_supply_position_on_liquidity(&liquidity_mint, &lending);
        let f_token_mint = derive_f_token_mint(&liquidity_mint);
        let owner_f_token_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &pool_authority,
                &f_token_mint,
                &SPL_TOKEN_PROGRAM_ID,
            );
        Self {
            lending,
            liquidity_mint,
            reserve_key,
            position_key,
            f_token_mint,
            owner_f_token_ata,
            available_liquidity: 0,
        }
    }

    pub fn required_pubkeys_for_update(&self) -> Vec<Pubkey> {
        vec![self.reserve_key, self.owner_f_token_ata]
    }

    pub async fn update(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let [reserve_account, f_token_account]: [Option<Account>; 2] = cache
            .get_accounts(&[self.reserve_key, self.owner_f_token_ata])
            .await?
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;
        let reserve_account =
            reserve_account.ok_or(TradingVenueError::NoAccountFound(self.reserve_key.into()))?;
        let f_token_account = f_token_account.ok_or(TradingVenueError::NoAccountFound(
            self.owner_f_token_ata.into(),
        ))?;

        let reserve: TokenReserve =
            state::decode_anchor_account("TokenReserve", reserve_account.data())?;
        let f_token_balance = state::read_token_account_amount(f_token_account.data())?;
        // f-tokens are the share-bearing receipt for supplied liquidity. The
        // underlying amount we can redeem = balance × supply_exchange_price / 1e12.
        // This is what the on-chain CPI burns from us during instant_withdraw.
        let redemption_value = ((f_token_balance as u128) * (reserve.supply_exchange_price as u128)
            / EXCHANGE_PRICES_PRECISION) as u64;

        // Protocol-wide liquid: total supply minus total borrow.
        let total_supply = reserve
            .total_supply_with_interest
            .saturating_add(reserve.total_supply_interest_free);
        let total_borrow = reserve
            .total_borrow_with_interest
            .saturating_add(reserve.total_borrow_interest_free);
        let vault_liquid = total_supply.saturating_sub(total_borrow);

        self.available_liquidity = vault_liquid.min(redemption_value);
        Ok(())
    }

    pub fn instant_withdraw_remaining_accounts(
        &self,
        _pool_authority_key: &Pubkey,
        underlying_token_program: &Pubkey,
    ) -> Vec<AccountMeta> {
        let liquidity_mint = &self.liquidity_mint;
        let lending_admin = derive_lending_admin();
        let rate_model = derive_rate_model(liquidity_mint);
        let liquidity = derive_liquidity();
        let vault = spl_associated_token_account::get_associated_token_address_with_program_id(
            &liquidity,
            liquidity_mint,
            underlying_token_program,
        );
        let rewards_rate_model = derive_rewards_rate_model(liquidity_mint);

        vec![
            AccountMeta::new(self.owner_f_token_ata, false),
            AccountMeta::new_readonly(lending_admin, false),
            AccountMeta::new(self.lending, false),
            AccountMeta::new(self.f_token_mint, false),
            AccountMeta::new(self.reserve_key, false),
            AccountMeta::new(self.position_key, false),
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

    pub fn available_liquidity_for_withdrawal(&self) -> u64 {
        self.available_liquidity
    }

    pub fn lookup_table_keys(&self) -> Vec<Pubkey> {
        vec![
            self.lending,
            self.reserve_key,
            self.position_key,
            JUP_LENDING_PROGRAM_ID,
            JUP_LIQUIDITY_PROGRAM_ID,
            JUP_LRRM_PROGRAM_ID,
        ]
    }
}
