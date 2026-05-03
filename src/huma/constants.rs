use solana_pubkey::{Pubkey, pubkey};

pub const PROGRAM_ID: Pubkey = pubkey!("HumaXepHnjaRCpjYTokxY4UtaJcmx41prQ8cxGmFC5fn");
pub const HUMA_PROGRAM_ID: Pubkey = pubkey!("EVQ4s1b6N1vmWFDv8PRNc77kufBP8HcrSNWXQAhRsJq9");
pub const JUP_LENDING_PROGRAM_ID: Pubkey = pubkey!("jup3YeL8QhtSx1e253b2FDvsMNC87fDrgQZivbrndc9");
pub const JUP_LIQUIDITY_PROGRAM_ID: Pubkey = pubkey!("jupeiUmn818Jg1ekPURTpr4mFo29p46vygyykFJ3wZC");
pub const JUP_LRRM_PROGRAM_ID: Pubkey = pubkey!("jup7TthsMgcR9Y3L277b8Eo9uboVSmu1utkuXHNUKar");
pub const KLEND_PROGRAM_ID: Pubkey = pubkey!("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");
pub const SPL_TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJe1bsn");
pub const SYSTEM_PROGRAM_ID: Pubkey = pubkey!("11111111111111111111111111111111");
pub const SYSVAR_INSTRUCTIONS_ID: Pubkey = pubkey!("Sysvar1nstructions1111111111111111111111111");
pub const POOL_CONFIG_KEY: Pubkey = pubkey!("28hFhD21Nka3stL27a8zZ4nRLgaDVxRYwJgeEVgeakzS");

pub const POOL_STATE_SEED: &[u8] = b"pool_state";
pub const POOL_AUTHORITY_SEED: &[u8] = b"pool_authority";
pub const MODE_MINT_SEED: &[u8] = b"mode_mint";
pub const DEPLOYMENT_STATE_SEED: &[u8] = b"deployment_state";
pub const LENDER_STATE_SEED: &[u8] = b"lender_state";

pub const DISCRIMINATOR_LEN: usize = 8;

pub const HUNDRED_PERCENT_BPS: u64 = 10_000;
pub const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
pub const SECONDS_IN_A_YEAR: u64 = 365 * SECONDS_PER_DAY;
pub const MAX_ASSETS_STALENESS_SECS: u64 = 5 * SECONDS_PER_DAY;
