use anchor_lang::prelude::*;

pub const MAX_SWAPS: usize = 12;
pub const MAX_MINTS: usize = 12;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Copy, Eq, Debug)]
pub enum Venue {
    RaydiumAmm,
    /// Huma deposit / instant-withdraw leg. `is_deposit` selects which Huma
    /// instruction the adapter builds (underlying → mode shares vs the reverse).
    /// Must stay byte-identical to `swap_route::Venue` (see `venue_parity.rs`).
    Huma { is_deposit: bool },
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Copy, Eq, Debug)]
pub struct SwapSpecInputV2 {
    pub venue: Venue,
    pub from: u8,
    pub to: u8,
    pub weight_nanos: u32,
    pub n_accounts: u8,
}

#[account]
pub struct TitanPda {
    pub bump: u8,
}

impl TitanPda {
    pub const SIZE: usize = 1;
    pub const SEED: &'static [u8] = b"titan_pda";
}
