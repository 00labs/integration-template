mod jup_lend;
mod kamino_lend;

pub use jup_lend::JupLendStrategy;
pub use kamino_lend::KaminoLendStrategy;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::account_caching::AccountsCache;
use crate::huma::state::{DeploymentConfig, DeploymentStrategyType};
use crate::trading_venue::error::TradingVenueError;

#[derive(Clone)]
pub enum Strategy {
    JupLend(JupLendStrategy),
    KaminoLend(KaminoLendStrategy),
}

macro_rules! dispatch {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        match $self {
            Strategy::JupLend(s) => s.$method($($arg),*),
            Strategy::KaminoLend(s) => s.$method($($arg),*),
        }
    };
}

impl Strategy {
    /// Builds a routable strategy from an on-chain `DeploymentConfig`. Manual
    /// strategies have no CPI path for instant withdrawals and are rejected.
    /// `pool_authority` is needed to derive the strategy-specific accounts the
    /// pool holds (e.g. Kamino's k-token ATA).
    pub fn from_deployment_config(
        deployment_config: &DeploymentConfig,
        underlying_mint: Pubkey,
        pool_authority: Pubkey,
    ) -> Result<Self, TradingVenueError> {
        match deployment_config.strategy_type {
            DeploymentStrategyType::JupLend => Ok(Strategy::JupLend(JupLendStrategy::new(
                deployment_config.target_key,
                underlying_mint,
                pool_authority,
            ))),
            DeploymentStrategyType::KaminoLend => Ok(Strategy::KaminoLend(
                KaminoLendStrategy::new(deployment_config.target_key, pool_authority),
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
            Strategy::JupLend(s) => s.update(cache).await,
            Strategy::KaminoLend(s) => s.update(cache).await,
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
