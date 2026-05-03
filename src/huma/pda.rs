use solana_pubkey::Pubkey;

use crate::huma::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, DEPLOYMENT_STATE_SEED, LENDER_STATE_SEED, MODE_MINT_SEED,
    POOL_AUTHORITY_SEED, POOL_STATE_SEED, PROGRAM_ID,
};

pub fn derive_pool_state(pool_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[POOL_STATE_SEED, pool_config_key.as_ref()], &PROGRAM_ID).0
}

pub fn derive_pool_authority(pool_config_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POOL_AUTHORITY_SEED, pool_config_key.as_ref()],
        &PROGRAM_ID,
    )
    .0
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
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}
