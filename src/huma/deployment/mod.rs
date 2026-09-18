mod jup_lend;
mod kamino_lend;

pub use jup_lend::JupLendStrategy;
pub use kamino_lend::KaminoLendStrategy;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::state::DeploymentStrategyType;
use crate::trading_venue::error::TradingVenueError;

#[derive(Clone)]
pub enum Deployment {
    JupLend(JupLendStrategy),
    KaminoLend(KaminoLendStrategy),
}

macro_rules! dispatch {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            Deployment::JupLend(s) => s.$method($($arg),*),
            Deployment::KaminoLend(s) => s.$method($($arg),*),
        }
    };
}

impl Deployment {
    /// Builds a routable venue for a deployment target. Manual targets have no CPI path
    /// for instant withdrawals and are rejected. `position_owner` derives the accounts
    /// that hold the position (e.g. Kamino's k-token ATA) — the pool authority before a
    /// mode is cut over, the strategy authority after.
    pub fn new(
        strategy_type: &DeploymentStrategyType,
        target_key: Pubkey,
        underlying_mint: Pubkey,
        position_owner: Pubkey,
    ) -> Result<Self, TradingVenueError> {
        match strategy_type {
            DeploymentStrategyType::JupLend => Ok(Deployment::JupLend(JupLendStrategy::new(
                target_key,
                underlying_mint,
                position_owner,
            ))),
            DeploymentStrategyType::KaminoLend => Ok(Deployment::KaminoLend(
                KaminoLendStrategy::new(target_key, position_owner),
            )),
            DeploymentStrategyType::Manual => Err(TradingVenueError::UnsupportedVenue(
                "manual strategy is not routable".into(),
            )),
        }
    }

    pub fn required_pubkeys_for_update(&self) -> Vec<Pubkey> {
        dispatch!(self, required_pubkeys_for_update)
    }

    pub async fn update(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        match self {
            Deployment::JupLend(s) => s.update(cache).await,
            Deployment::KaminoLend(s) => s.update(cache).await,
        }
    }

    pub fn instant_withdraw_remaining_accounts(
        &self,
        pool_authority_key: &Pubkey,
        underlying_token_program: &Pubkey,
    ) -> Vec<AccountMeta> {
        dispatch!(
            self,
            instant_withdraw_remaining_accounts,
            pool_authority_key,
            underlying_token_program
        )
    }

    pub fn available_liquidity_for_withdrawal(&self) -> u64 {
        dispatch!(self, available_liquidity_for_withdrawal)
    }

    pub fn lookup_table_keys(&self) -> Vec<Pubkey> {
        dispatch!(self, lookup_table_keys)
    }
}
