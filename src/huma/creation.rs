//! Venue-creation parsing for Huma — the creation-parser integration layer.
//!
//! Titan tracks new pools live by feeding the decompiled instructions of
//! confirmed transactions through [`parse_pool_creations`]; each returned
//! [`PoolCreation::pool`] is then built into a venue via
//! [`HumaVenue::from_account`](crate::huma::HumaVenue). See
//! [`crate::trading_venue::venue_creation`] for the contract and
//! `tests/huma_creation.rs` for the fixture test.

use crate::huma::constants::PROGRAM_ID;
use crate::trading_venue::protocol::PoolProtocol;
use crate::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};

/// Anchor discriminator for the permissionless program's `add_mode` instruction
/// (`sha256("global:add_mode")[..8]`).
const ADD_MODE_DISCRIMINATOR: [u8; 8] = [136, 187, 228, 193, 168, 107, 113, 144];

// Account positions within the `add_mode` instruction, in the declaration order
// of the program's `AddMode` account context.
const UNDERLYING_MINT_INDEX: usize = 4;
const MODE_CONFIG_INDEX: usize = 5;
const MODE_MINT_INDEX: usize = 6;

/// Detect every Huma pool/mode created by a confirmed transaction.
///
/// [`HumaVenue::from_account`](crate::huma::HumaVenue) is keyed by **mode**
/// (`ModeConfig`), so a single create-pool transaction is expected to yield one
/// [`PoolCreation`] per mode, with `pool` set to that mode's `ModeConfig`
/// address and `mints` the underlying + mode mints.
pub fn parse_pool_creations(instructions: &[ParsedInstruction]) -> Vec<PoolCreation> {
    instructions
        .iter()
        .filter(|ix| ix.program_id == PROGRAM_ID)
        .filter(|ix| ix.data.get(..8) == Some(&ADD_MODE_DISCRIMINATOR[..]))
        .filter_map(|ix| {
            // Defensive: a real `add_mode` always carries these accounts, but
            // never index past a malformed instruction.
            let mode_config = *ix.accounts.get(MODE_CONFIG_INDEX)?;
            let underlying_mint = *ix.accounts.get(UNDERLYING_MINT_INDEX)?;
            let mode_mint = *ix.accounts.get(MODE_MINT_INDEX)?;
            Some(PoolCreation {
                protocol: PoolProtocol::Huma,
                // A venue is keyed by mode, so `pool` is the new ModeConfig —
                // exactly what `HumaVenue::from_account` consumes.
                pool: mode_config,
                mints: vec![underlying_mint, mode_mint],
            })
        })
        .collect()
}
