//! Huma's venue-creation parsing test.
//!
//! Self-contained fixture (no RPC) for Huma's pool-creation instruction,
//! mirroring `tests/venue_creation.rs`. Currently a stub — filling it in needs
//! Huma's create-pool discriminator + account layout from the program.

use solana_pubkey::{Pubkey, pubkey};

use titan_integration_template::huma::{HUMA_PERMISSIONLESS_PROGRAM_ID, parse_pool_creations};
use titan_integration_template::trading_venue::protocol::PoolProtocol;
use titan_integration_template::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};

// FILL_IN: the new ModeConfig (pool) address created by the fixture instruction.
const POOL: Pubkey = pubkey!("11111111111111111111111111111111");
// FILL_IN: the underlying mint of the new pool.
const UNDERLYING_MINT: Pubkey = pubkey!("11111111111111111111111111111111");
// FILL_IN: the mode (LP) mint of the new pool.
const MODE_MINT: Pubkey = pubkey!("11111111111111111111111111111111");

fn require_fixture_constants_replaced() {
    if POOL == Pubkey::default() {
        todo!("replace POOL with a real ModeConfig created by the fixture instruction")
    }
    if UNDERLYING_MINT == Pubkey::default() || MODE_MINT == Pubkey::default() {
        todo!("replace UNDERLYING_MINT and MODE_MINT with the new pool's real mints")
    }
}

fn huma_pool_creation() -> ParsedInstruction {
    // FILL_IN: build Huma's real pool-creation instruction fixture — program id,
    // discriminator, account order, and data layout the parser expects, with the
    // new ModeConfig and mint accounts at their real instruction positions.
    todo!("build Huma pool-creation instruction fixture")
}

fn unrelated_instruction() -> ParsedInstruction {
    ParsedInstruction {
        program_id: HUMA_PERMISSIONLESS_PROGRAM_ID,
        accounts: vec![],
        data: vec![],
    }
}

#[test]
fn parses_huma_pool_creation() {
    require_fixture_constants_replaced();
    let creations = parse_pool_creations(&[huma_pool_creation()]);

    assert_eq!(
        creations,
        vec![PoolCreation {
            protocol: PoolProtocol::Huma,
            pool: POOL,
            mints: vec![UNDERLYING_MINT, MODE_MINT],
        }],
    );
}

#[test]
fn ignores_transactions_without_a_creation() {
    let creations = parse_pool_creations(&[unrelated_instruction()]);
    assert!(
        creations.is_empty(),
        "a transaction without a pool creation creates no pools, got {creations:?}"
    );
}
