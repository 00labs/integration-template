//! Huma's venue-creation parsing test.
//!
//! Self-contained fixture (no RPC) reproducing a real **production** `add_mode`
//! instruction: program `HumaXep…` (= `PROGRAM_ID`), the Anchor `add_mode`
//! discriminator, and the account ordering of the permissionless program's
//! `AddMode` context. Source transaction:
//! `5SmPcQByiS9QbGdamzdN4H2bTrLRMj5jAPKa1VZUnekvrqzucGZNteHogyvqh5hw8ZQd6T7tpwJQY1uA4h1DSzg8`

use solana_pubkey::{Pubkey, pubkey};

use titan_integration_template::huma::{HUMA_PERMISSIONLESS_PROGRAM_ID, parse_pool_creations};
use titan_integration_template::trading_venue::protocol::PoolProtocol;
use titan_integration_template::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};

// The new mode created by the fixture — the ModeConfig PDA handed to
// `HumaVenue::from_account`.
const MODE_CONFIG: Pubkey = pubkey!("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN");
// Tradable mints of the new mode: underlying (USDC) and the mode (LP) mint.
const UNDERLYING_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const MODE_MINT: Pubkey = pubkey!("59obFNBzyTBGowrkif5uK7ojS58vsuWz3ZCvg6tfZAGw");

/// The real `add_mode` instruction, with accounts in on-chain order. Indices
/// 4 / 5 / 6 are the underlying mint, the new ModeConfig, and the mode mint.
fn huma_pool_creation() -> ParsedInstruction {
    ParsedInstruction {
        program_id: HUMA_PERMISSIONLESS_PROGRAM_ID,
        accounts: vec![
            pubkey!("Huma8ZB251nwuYxDME4EFir1z8wvYw97Yr3hg9g3qKQL"), // 0  pool_owner
            pubkey!("28hFhD21Nka3stL27a8zZ4nRLgaDVxRYwJgeEVgeakzS"), // 1  pool_config
            pubkey!("iFgP2EbzHUZzMjqbjaagJQ8zmn6as3Hw95aVUKm67od"), // 2  pool_state
            pubkey!("9936VFvgRmW1STvdgeyPQaKHDx5DwBtbhZkT3HcdL3QK"), // 3  pool_authority
            UNDERLYING_MINT,                                         // 4  underlying_mint
            MODE_CONFIG,                                             // 5  mode_config (init)
            MODE_MINT,                                               // 6  mode_mint (init)
            pubkey!("BmcaTXkNC4ybJK3d22gKDPjGxd61rzYFiSrdNeamxjS1"), // 7  token_metadata
            pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), // 8  token_program
            pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"), // 9  associated_token_program
            pubkey!("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s"), // 10 mpl_token_metadata_program
            pubkey!("11111111111111111111111111111111"),            // 11 system_program
            pubkey!("Sysvar1nstructions1111111111111111111111111"), // 12 sysvar_instructions_program
            pubkey!("4AFwhfpkqoYkpo4J3DoKE1XE2VYhATasDYpMV1G4euEd"), // 13 (remaining) mode token ATA
        ],
        // Real instruction data: add_mode discriminator (8) + mode_id (32) +
        // mode_name ("Classic") + target_apy_bps + token-metadata args.
        data: vec![
            136, 187, 228, 193, 168, 107, 113, 144, 220, 146, 104, 91, 68, 157, 127, 175, 104,
            165, 240, 151, 100, 44, 59, 253, 181, 196, 171, 178, 29, 17, 157, 247, 52, 101, 235,
            67, 200, 60, 93, 183, 7, 0, 0, 0, 67, 108, 97, 115, 115, 105, 99, 26, 4, 27, 0, 0, 0,
            80, 97, 121, 70, 105, 32, 83, 116, 114, 97, 116, 101, 103, 121, 32, 84, 111, 107, 101,
            110, 32, 45, 32, 85, 83, 68, 67, 8, 0, 0, 0, 80, 83, 84, 45, 85, 83, 68, 67, 34, 0, 0,
            0, 104, 116, 116, 112, 115, 58, 47, 47, 109, 101, 116, 97, 46, 104, 117, 109, 97, 46,
            102, 105, 110, 97, 110, 99, 101, 47, 112, 115, 116, 46, 106, 115, 111, 110,
        ],
    }
}

/// Same program, but not an `add_mode` (no matching discriminator).
fn unrelated_instruction() -> ParsedInstruction {
    ParsedInstruction {
        program_id: HUMA_PERMISSIONLESS_PROGRAM_ID,
        accounts: vec![],
        data: vec![0, 1, 2, 3, 4, 5, 6, 7],
    }
}

#[test]
fn parses_huma_pool_creation() {
    let creations = parse_pool_creations(&[huma_pool_creation()]);

    assert_eq!(
        creations,
        vec![PoolCreation {
            protocol: PoolProtocol::Huma,
            pool: MODE_CONFIG,
            mints: vec![UNDERLYING_MINT, MODE_MINT],
        }],
    );
}

#[test]
fn ignores_transactions_without_a_creation() {
    let creations = parse_pool_creations(&[unrelated_instruction()]);
    assert!(
        creations.is_empty(),
        "a transaction without an add_mode creates no pools, got {creations:?}"
    );
}
