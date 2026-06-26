use anchor_lang::{prelude::*, solana_program::instruction::Instruction};

/// Huma permissionless program (production). Must match `HumaVenue::program_id`
/// / `constants::PROGRAM_ID` in the off-chain crate.
pub const PROGRAM_ID: Pubkey = pubkey!("HumaXepHnjaRCpjYTokxY4UtaJcmx41prQ8cxGmFC5fn");

// Anchor 8-byte discriminators: sha256("global:<name>")[..8].
const DEPOSIT_DISCRIMINATOR: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];
const INSTANT_WITHDRAW_DISCRIMINATOR: [u8; 8] = [171, 49, 145, 176, 48, 101, 112, 162];

/// `deposit`'s lock-up commitment sentinel for "no commitment" (a Borsh
/// `String`), matching `HumaInstruction::Deposit` in the off-chain crate.
const NO_COMMITMENT: &[u8] = b"NO_COMMITMENT";

/// Build the Huma swap CPI for one route leg.
///
/// `is_deposit` selects the direction; `amount_in` is the leg's input amount
/// (underlying assets for a deposit, mode shares for an instant withdrawal),
/// computed on-chain by the router from the available balance and leg weight.
/// The account metas are forwarded unchanged — they were assembled off-chain by
/// `swap_route::build_swap_leg` from `HumaVenue::generate_swap_instruction`, so
/// the data built here and that account order are the two halves of the same
/// instruction and must stay in lockstep.
pub fn swap(
    is_deposit: bool,
    amount_in: u64,
    account_metas: &[AccountMeta],
) -> Result<Vec<Instruction>> {
    let data = if is_deposit {
        // deposit(assets, commitment = "NO_COMMITMENT", commitment_auto_renewal = false)
        let mut data = Vec::with_capacity(34);
        data.extend_from_slice(&DEPOSIT_DISCRIMINATOR);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&(NO_COMMITMENT.len() as u32).to_le_bytes());
        data.extend_from_slice(NO_COMMITMENT);
        data.push(0); // commitment_auto_renewal = false
        data
    } else {
        // instant_withdraw(shares, max_fee). `max_fee = u64::MAX` is the loosest
        // bound (the quote already priced the fee in), matching the off-chain
        // `generate_swap_instruction`.
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&INSTANT_WITHDRAW_DISCRIMINATOR);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&u64::MAX.to_le_bytes());
        data
    };

    Ok(vec![Instruction {
        program_id: PROGRAM_ID,
        accounts: account_metas.to_vec(),
        data,
    }])
}
