//! Anchor instruction encoders for the permissionless program.
//!
//! `HumaInstruction::pack()` produces the 8-byte Anchor discriminator
//! followed by Borsh-serialized arguments. Account metas are assembled
//! separately by the venue.

use borsh::BorshSerialize;

use crate::huma::state::anchor_instruction_discriminator;

/// Sentinel commitment string used by the rest of the Huma protocol when a
/// deposit has no lock-up commitment.
const NO_COMMITMENT: &str = "NO_COMMITMENT";

pub enum HumaInstruction {
    Deposit { assets: u64 },
    InstantWithdraw { shares: u64, max_fee_bps: u16 },
}

#[derive(BorshSerialize)]
struct DepositArgs {
    assets: u64,
    commitment: String,
    commitment_auto_renewal: bool,
}

#[derive(BorshSerialize)]
struct InstantWithdrawArgs {
    shares: u64,
    max_fee_bps: u16,
}

impl HumaInstruction {
    pub fn pack(self) -> Vec<u8> {
        match self {
            HumaInstruction::Deposit { assets } => {
                let mut data = anchor_instruction_discriminator("deposit").to_vec();
                DepositArgs {
                    assets,
                    commitment: NO_COMMITMENT.to_string(),
                    commitment_auto_renewal: false,
                }
                .serialize(&mut data)
                .unwrap();
                data
            }
            HumaInstruction::InstantWithdraw {
                shares,
                max_fee_bps,
            } => {
                let mut data = anchor_instruction_discriminator("instant_withdraw").to_vec();
                InstantWithdrawArgs {
                    shares,
                    max_fee_bps,
                }
                .serialize(&mut data)
                .unwrap();
                data
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deposit_layout() {
        let data = HumaInstruction::Deposit { assets: 1_000 }.pack();
        // 8 disc + 8 u64 + 4 string-len + 13 string ("NO_COMMITMENT") + 1 bool = 34
        assert_eq!(data.len(), 34);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 1_000);
        // Borsh u32 length prefix for "NO_COMMITMENT" == 13
        assert_eq!(u32::from_le_bytes(data[16..20].try_into().unwrap()), 13);
        assert_eq!(&data[20..33], NO_COMMITMENT.as_bytes());
        // bool false
        assert_eq!(data[33], 0);
    }

    #[test]
    fn instant_withdraw_layout() {
        let data = HumaInstruction::InstantWithdraw {
            shares: 500,
            max_fee_bps: 200,
        }
        .pack();
        // 8 disc + 8 u64 + 2 u16 = 18
        assert_eq!(data.len(), 18);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 500);
        assert_eq!(u16::from_le_bytes(data[16..18].try_into().unwrap()), 200);
    }
}
