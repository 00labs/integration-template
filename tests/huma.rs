//! Huma's venue test suite — the shared assertions from `tests/common`, run
//! against `HumaVenue`. Mirrors `tests/example.rs` (the Raydium reference) so
//! both venues are held to exactly the same bar.
//!
//! Like the example suite, the tests SKIP when `SOLANA_RPC_URL` (and, for the
//! simulations, dumped program binaries) are absent.

mod common;

use common::SuiteConfig;
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::huma::HumaVenue;
use titan_integration_template::huma::constants::{
    HUMA_PROGRAM_ID, JUP_LENDING_PROGRAM_ID, JUP_LIQUIDITY_PROGRAM_ID, JUP_LRRM_PROGRAM_ID,
    KLEND_PROGRAM_ID, PROGRAM_ID,
};

// Installs the allocation guard that powers the construction test's
// `assert_no_alloc` checks. The Makefile runs that test under `release-debug`
// so the guard is active; speed tests run under true `--release`.
#[cfg(debug_assertions)]
#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// A `ModeConfig` account on the Huma pool; `HumaVenue::from_account` pairs it
/// with the hardcoded `POOL_CONFIG_KEY`.
fn pool() -> Pubkey {
    pubkey!("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")
}

/// The Huma permissionless + core programs plus the runtime programs the
/// instant-withdraw CPI touches (jup lending/liquidity/lrrm + klend). Each must
/// be dumped to `programs/<id>.so` (see `make dump-programs`).
fn programs() -> Vec<Pubkey> {
    vec![
        PROGRAM_ID,
        HUMA_PROGRAM_ID,
        JUP_LENDING_PROGRAM_ID,
        JUP_LIQUIDITY_PROGRAM_ID,
        JUP_LRRM_PROGRAM_ID,
        KLEND_PROGRAM_ID,
    ]
}

fn config() -> SuiteConfig {
    SuiteConfig {
        pool: pool(),
        programs: programs(),
    }
}

#[tokio::test]
async fn construction() {
    common::construction::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn zero_input_spot_price() {
    common::zero_input_spot_price::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn bound_simulation() {
    common::bound_simulation::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn random_samples() {
    common::random_samples::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn monotone() {
    common::monotone::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn quoting_speed() {
    common::quoting_speed::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn price_monotone() {
    common::price_monotone::<HumaVenue>(&config()).await;
}

#[tokio::test]
async fn mean_value_theorem() {
    common::mean_value_theorem::<HumaVenue>(&config()).await;
}
