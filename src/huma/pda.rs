use solana_pubkey::Pubkey;

use crate::huma::constants::{
    DEPLOYMENT_STATE_SEED, LENDER_STATE_SEED, MODE_MINT_SEED, POOL_AUTHORITY_SEED, POOL_STATE_SEED,
    PROGRAM_ID,
};

pub fn derive_pool_state(pool_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[POOL_STATE_SEED, pool_config_key.as_ref()], &PROGRAM_ID).0
}

/// Derives the pool authority using the bump stored on `PoolConfig`. Required
/// because the on-chain bump may not be the canonical one.
pub fn pool_authority_with_bump(pool_config_key: &Pubkey, bump: u8) -> Option<Pubkey> {
    Pubkey::create_program_address(
        &[POOL_AUTHORITY_SEED, pool_config_key.as_ref(), &[bump]],
        &PROGRAM_ID,
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
        &PROGRAM_ID,
    )
    .0
}

pub fn derive_deployment_state(deployment_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[DEPLOYMENT_STATE_SEED, deployment_config_key.as_ref()],
        &PROGRAM_ID,
    )
    .0
}

pub fn derive_lender_state(mode_config_key: &Pubkey, lender: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[LENDER_STATE_SEED, mode_config_key.as_ref(), lender.as_ref()],
        &PROGRAM_ID,
    )
    .0
}

pub fn derive_ata(wallet: &Pubkey, token_program: &Pubkey, mint: &Pubkey) -> Pubkey {
    spl_associated_token_account::get_associated_token_address_with_program_id(
        wallet,
        mint,
        token_program,
    )
}
