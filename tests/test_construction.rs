#[cfg(test)]
mod test_construction {
    //! Integration test ensuring that a Huma venue:
    //! - can be constructed from a `ModeConfig` account,
    //! - can load its required state via the AccountsCache,
    //! - returns valid token info (the underlying mint and the mode mint),
    //! - supports quoting for both swap directions
    //!   (deposit: underlying → mode_mint, instant withdraw: mode_mint → underlying),
    //! - and exposes sane quoting boundaries.
    //!
    //! Any AMM implementer integrating with Titan should ensure their venue
    //! passes this style of test, as it verifies the critical invariants that
    //! Titan relies on for routing.

    use std::{env, str::FromStr};

    use rstest::rstest;
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_pubkey::Pubkey;
    use titan_integration_template::account_caching::rpc_cache::RpcClientCache;
    use titan_integration_template::huma::HumaVenue;
    use titan_integration_template::trading_venue::{FromAccount, TradingVenue};
    use titan_integration_template::trading_venue::{QuoteRequest, SwapType};

    use assert_no_alloc::*;

    #[cfg(debug_assertions)] // required when disable_release is set (default)
    #[global_allocator]
    static A: AllocDisabler = AllocDisabler;

    /// Initialize logging for test output.
    ///
    /// Having logging enabled is extremely helpful when debugging state-loading
    /// issues or boundary failures during venue development.
    fn init_test_logger() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    /// Ensure that the Huma venue can:
    /// - Build from a raw on-chain `ModeConfig` account,
    /// - Perform a state update using the caching layer,
    /// - Report valid token metadata (underlying + mode mint),
    /// - Calculate valid quoting boundaries,
    /// - Return nonzero, liquidity-supported quotes at both boundary edges
    ///   for both deposit and instant-withdraw directions.
    ///
    /// The keyed account is a `ModeConfig` PDA; `HumaVenue::from_account`
    /// pairs it with the hardcoded `POOL_CONFIG_KEY` constant.
    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")]
    async fn test_construction(#[case] mode_config_key: String) {
        init_test_logger();

        //
        // Prepare inputs
        //
        let mode_config_key = Pubkey::from_str(&mode_config_key).expect("Invalid test pubkey");

        let rpc_url =
            env::var("SOLANA_RPC_URL").expect("SOLANA_RPC_URL must be set for integration tests");
        let rpc = RpcClient::new(rpc_url);

        //
        // Fetch the ModeConfig account and construct the venue
        //
        let mode_config_account = rpc
            .get_account(&mode_config_key)
            .await
            .expect("Failed to fetch ModeConfig account");

        let mut venue = HumaVenue::from_account(&mode_config_key, &mode_config_account)
            .expect("Failed to construct venue from account");

        //
        // Load on-chain state using the caching layer
        //
        let cache = RpcClientCache::new(rpc);
        venue
            .update_state(&cache)
            .await
            .expect("Venue state update failed");

        //
        // Validate token metadata
        //
        let token_info = venue.get_token_info();
        log::info!("Loaded token info: {:#?}", token_info);
        assert!(!token_info.is_empty());

        // A Huma venue exposes exactly 2 mints: [underlying, mode_mint].
        assert_eq!(token_info.len(), 2);

        //
        // For each direction (underlying → mode, mode → underlying)
        // validate quoting boundaries and quote correctness.
        //
        for (input_idx, output_idx) in [(0, 1), (1, 0)] {
            log::info!("Checking bounds for pair ({}, {})", input_idx, output_idx);

            let (lower_bound, upper_bound) =
                assert_no_alloc(|| venue.bounds(input_idx, output_idx))
                    .expect("Boundary search failed");

            assert!(
                lower_bound < upper_bound,
                "Lower bound must be strictly less than upper bound"
            );

            let input_mint = token_info[input_idx as usize].pubkey;
            let output_mint = token_info[output_idx as usize].pubkey;

            let lb_result = assert_no_alloc(|| {
                venue.quote(QuoteRequest {
                    input_mint,
                    output_mint,
                    amount: lower_bound,
                    swap_type: SwapType::ExactIn,
                })
            })
            .expect("Lower-bound quote failed");

            log::info!("Lower-bound quote: {:#?}", lb_result);

            assert!(
                !lb_result.not_enough_liquidity,
                "Lower bound indicates insufficient liquidity"
            );
            assert!(
                lb_result.expected_output > 0,
                "Lower bound produced zero output"
            );

            let ub_result = assert_no_alloc(|| {
                venue.quote(QuoteRequest {
                    input_mint,
                    output_mint,
                    amount: upper_bound,
                    swap_type: SwapType::ExactIn,
                })
            })
            .expect("Upper-bound quote failed");

            log::info!("Upper-bound quote: {:#?}", ub_result);

            assert!(
                !ub_result.not_enough_liquidity,
                "Upper bound indicates insufficient liquidity"
            );
            assert!(
                ub_result.expected_output > 0,
                "Upper bound produced zero output"
            );
        }
    }
}
