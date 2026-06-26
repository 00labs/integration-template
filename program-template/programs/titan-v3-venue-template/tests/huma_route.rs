//! Huma's swap-route test — the same end-to-end suite the example passes, run
//! against `HumaVenue`: quote off-chain, execute `swap_route_v3` in LiteSVM, and
//! assert the simulated output matches the quote. SKIPs cleanly until
//! `SOLANA_RPC_URL` is set and the route program is built (`make build-program`).
//!
//! `run_swap_route` exercises BOTH directions. Instant-withdraw needs the
//! TitanPDA registered as a Huma lender; the shared harness provisions that up
//! front via `RouteVenue::presim_instructions` (a `create_lender_accounts_v2`
//! paid by the test payer —
//! the lender need not sign), so both directions run without manual setup.

mod common;

use common::{RouteConfig, run_swap_route};
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::huma::HumaVenue;
use titan_integration_template::huma::constants::{
    HUMA_PROGRAM_ID, JUP_LENDING_PROGRAM_ID, JUP_LIQUIDITY_PROGRAM_ID, JUP_LRRM_PROGRAM_ID,
    KLEND_PROGRAM_ID, PROGRAM_ID,
};

/// Production `ModeConfig` (USDC mode) the route is built from.
fn pool() -> Pubkey {
    pubkey!("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")
}

/// Programs the Huma swap CPI reaches: the permissionless program (called
/// directly), the core Huma program (CPI'd by it), and the strategy programs
/// the instant-withdraw path invokes.
fn venue_programs() -> Vec<Pubkey> {
    vec![
        PROGRAM_ID,
        HUMA_PROGRAM_ID,
        JUP_LENDING_PROGRAM_ID,
        JUP_LIQUIDITY_PROGRAM_ID,
        JUP_LRRM_PROGRAM_ID,
        KLEND_PROGRAM_ID,
    ]
}

#[tokio::test]
async fn swap_route_both_directions() {
    run_swap_route::<HumaVenue>(RouteConfig {
        pool: pool(),
        venue_programs: venue_programs(),
    })
    .await;
}
