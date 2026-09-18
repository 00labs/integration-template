use solana_pubkey::Pubkey;

use crate::huma::constants::{
    DEPLOYMENT_STATE_SEED, LENDER_STATE_SEED, MODE_MINT_SEED, POOL_AUTHORITY_SEED, POOL_STATE_SEED,
    STRATEGY_PROGRAM_ID, STRATEGY_STATE_SEED, VAULT_PROGRAM_ID,
};

pub fn derive_pool_state(pool_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POOL_STATE_SEED, pool_config_key.as_ref()],
        &VAULT_PROGRAM_ID,
    )
    .0
}

/// Derives the pool authority using the bump stored on `PoolConfig`. The
/// stored bump is canonical (Anchor populates it via `find_program_address`
/// in `create_pool`); this just avoids re-running that derivation and
/// mirrors how the on-chain ix uses `bump = pool_config.pool_authority_bump`.
pub fn pool_authority_with_bump(pool_config_key: &Pubkey, bump: u8) -> Option<Pubkey> {
    Pubkey::create_program_address(
        &[POOL_AUTHORITY_SEED, pool_config_key.as_ref(), &[bump]],
        &VAULT_PROGRAM_ID,
    )
    .ok()
}

pub fn derive_mode_mint(pool_config_key: &Pubkey, mode_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            MODE_MINT_SEED,
            pool_config_key.as_ref(),
            mode_config_key.as_ref(),
        ],
        &VAULT_PROGRAM_ID,
    )
    .0
}

pub fn derive_deployment_state(deployment_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[DEPLOYMENT_STATE_SEED, deployment_config_key.as_ref()],
        &VAULT_PROGRAM_ID,
    )
    .0
}

pub fn derive_lender_state(mode_config_key: &Pubkey, lender: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[LENDER_STATE_SEED, mode_config_key.as_ref(), lender.as_ref()],
        &VAULT_PROGRAM_ID,
    )
    .0
}

pub fn derive_strategy_state(strategy_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[STRATEGY_STATE_SEED, strategy_config_key.as_ref()],
        &STRATEGY_PROGRAM_ID,
    )
    .0
}

/// The strategy's own authority, which owns its underlying token account and is the
/// mint authority on every note mint. Seeded like the pool's, under the strategy program.
pub fn derive_strategy_authority(strategy_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POOL_AUTHORITY_SEED, strategy_config_key.as_ref()],
        &STRATEGY_PROGRAM_ID,
    )
    .0
}

/// The Strategy-layer twin of [`derive_deployment_state`]: same seed, but the
/// deployment books of a migrated mode live under the strategy program.
pub fn derive_strategy_deployment_state(deployment_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[DEPLOYMENT_STATE_SEED, deployment_config_key.as_ref()],
        &STRATEGY_PROGRAM_ID,
    )
    .0
}
