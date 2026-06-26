//! Venue-creation parsing for Huma — the creation-parser integration layer.
//!
//! Titan tracks new pools live by feeding the decompiled instructions of
//! confirmed transactions through [`parse_pool_creations`]; each returned
//! [`PoolCreation::pool`] is then built into a venue via
//! [`HumaVenue::from_account`](crate::huma::HumaVenue). See
//! [`crate::trading_venue::venue_creation`] for the contract and
//! `tests/huma_creation.rs` for the fixture test.

use crate::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};

/// Detect every Huma pool/mode created by a confirmed transaction.
///
/// [`HumaVenue::from_account`](crate::huma::HumaVenue) is keyed by **mode**
/// (`ModeConfig`), so a single create-pool transaction is expected to yield one
/// [`PoolCreation`] per mode, with `pool` set to that mode's `ModeConfig`
/// address and `mints` the underlying + mode mints.
pub fn parse_pool_creations(instructions: &[ParsedInstruction]) -> Vec<PoolCreation> {
    // FILL_IN (P1): match Huma's program id + create-pool discriminator and read
    // the new ModeConfig + token mints out of the instruction accounts. Needs
    // the create-pool ix discriminator + account layout from the Huma program.
    let _ = instructions;
    Vec::new()
}
