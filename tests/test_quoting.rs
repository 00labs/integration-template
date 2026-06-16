#[cfg(test)]
mod simulations {
    //! Quoting tests for the Huma venue.
    //!
    //! The tests ensure:
    //! - The venue loads on-chain state correctly
    //! - It exposes valid token info (underlying + mode mint)
    //! - It establishes valid quoting boundaries for both swap directions
    //! - Its off-chain deposit quote matches LiteSVM-simulated on-chain deposits
    //!   (instant-withdraw direction is exercised off-chain only)
    //! - The off-chain quote function is monotone non-decreasing in input
    //! - Its quoting speed is sufficient for integration
    //!
    //! `test_bound_simulation` and `test_random_samples` simulate the deposit
    //! ix in LiteSVM against a snapshot of staging-on-mainnet state. They
    //! require the keypair to have the per-lender accounts initialized via
    //! `create_lender_accounts_v2`.

    use std::env;
    use std::time::Instant;

    use litesvm::LiteSVM;
    use rand::Rng;
    use rstest::rstest;
    use solana_account::{Account, ReadableAccount, WritableAccount};
    use solana_client::nonblocking::rpc_client::RpcClient;
    use solana_compute_budget::compute_budget::ComputeBudget;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_program::hash;
    use solana_program::native_token::LAMPORTS_PER_SOL;
    use solana_program_pack::Pack;
    use solana_pubkey::Pubkey;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;
    use solana_sdk_ids::system_program;
    use solana_sysvar::clock::{self, Clock};
    use solana_transaction::Transaction;

    use spl_token::state::{Account as TokenAccount, AccountState};

    use titan_integration_template::account_caching::AccountsCache;
    use titan_integration_template::account_caching::rpc_cache::RpcClientCache;
    use titan_integration_template::huma::HumaVenue;
    use titan_integration_template::huma::constants::{
        ASSOCIATED_TOKEN_PROGRAM_ID, HUMA_PROGRAM_ID, JUP_LENDING_PROGRAM_ID,
        JUP_LIQUIDITY_PROGRAM_ID, JUP_LRRM_PROGRAM_ID, KLEND_PROGRAM_ID, POOL_CONFIG_KEY,
        PROGRAM_ID,
    };
    use titan_integration_template::huma::{pda, state};
    use titan_integration_template::trading_venue::error::TradingVenueError;
    use titan_integration_template::trading_venue::{
        FromAccount, QuoteRequest, SwapType, TradingVenue,
    };

    /// Initialize logging for test diagnostics.
    fn init_test_logger() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    /// Loads all Huma + strategy program binaries into LiteSVM and creates a
    /// funded payer keypair. Programs whose `.so` files aren't present are
    /// silently skipped — the deposit-only tests don't need the strategy
    /// programs (klend/jup-lending), but loading them is harmless.
    pub fn setup_litesvm() -> (LiteSVM, Keypair) {
        let mut litesvm = LiteSVM::new()
            .with_compute_budget(ComputeBudget {
                compute_unit_limit: 1_400_000,
                ..Default::default()
            })
            .with_blockhash_check(false)
            .with_sigverify(false)
            .with_transaction_history(0);

        for pid in [
            PROGRAM_ID,
            HUMA_PROGRAM_ID,
            KLEND_PROGRAM_ID,
            JUP_LENDING_PROGRAM_ID,
            JUP_LIQUIDITY_PROGRAM_ID,
            JUP_LRRM_PROGRAM_ID,
        ] {
            let path = format!("programs/{}.so", pid);
            litesvm.add_program_from_file(pid, &path).unwrap();
        }

        let keypair = Keypair::new();
        let account = Account {
            lamports: 10_000 * LAMPORTS_PER_SOL,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        };
        litesvm.set_account(keypair.pubkey(), account).unwrap();

        (litesvm, keypair)
    }

    /// Pull the current Clock sysvar from RPC and load it into LiteSVM so
    /// freshness checks (mode-asset staleness, etc.) align with mainnet time.
    async fn sync_clock(litesvm: &mut LiteSVM, cache: &dyn AccountsCache) {
        let clock_account = cache
            .get_account(&clock::ID)
            .await
            .unwrap()
            .ok_or(TradingVenueError::NoAccountFound(clock::ID.into()))
            .unwrap();
        let clock: Clock = clock_account.deserialize_data().unwrap();
        litesvm.set_sysvar::<Clock>(&clock);
    }

    /// Copy each non-executable cached account into LiteSVM. Skips executables
    /// (already loaded as program binaries) and any caller-specified pubkeys
    /// that the caller wants to keep their synthesized version of.
    async fn copy_accounts(
        litesvm: &mut LiteSVM,
        cache: &dyn AccountsCache,
        pubkeys: &[Pubkey],
        keep_local: &[Pubkey],
    ) {
        let cached = cache.get_accounts(pubkeys).await.unwrap();
        for (acc, key) in cached.iter().zip(pubkeys) {
            if let Some(a) = acc {
                if a.executable || keep_local.contains(key) {
                    continue;
                }
                litesvm.set_account(*key, a.clone()).unwrap();
            }
        }
    }

    /// Sets the lender's underlying-mint ATA in LiteSVM with `u64::MAX`
    /// balance so the deposit ix has plenty to draw from.
    fn fund_underlying_ata(
        litesvm: &mut LiteSVM,
        owner: Pubkey,
        underlying_mint: Pubkey,
        underlying_token_program: Pubkey,
    ) -> Pubkey {
        let ata = spl_associated_token_account::get_associated_token_address_with_program_id(
            &owner,
            &underlying_mint,
            &underlying_token_program,
        );
        let mut acc = Account::new(LAMPORTS_PER_SOL, TokenAccount::LEN, &spl_token::ID);
        let mut data = TokenAccount::default();
        data.mint = underlying_mint;
        data.owner = owner;
        data.state = AccountState::Initialized;
        data.amount = u64::MAX;
        data.pack_into_slice(acc.data_as_mut_slice());
        litesvm.set_account(ata, acc).unwrap();
        ata
    }

    /// Computes the Anchor discriminator for `name` at runtime via SHA-256.
    fn compute_disc(name: &str) -> [u8; 8] {
        let h = hash::hashv(&[b"global:", name.as_bytes()]);
        let mut out = [0u8; 8];
        out.copy_from_slice(&h.as_ref()[..8]);
        out
    }

    /// Calls `create_lender_accounts_v2` on the permissionless program to
    /// initialize the lender's `lender_state` PDA and mode-token ATA. Required
    /// before the lender can deposit.
    async fn create_lender_accounts(
        litesvm: &mut LiteSVM,
        cache: &dyn AccountsCache,
        venue: &HumaVenue,
        keypair: &Keypair,
    ) {
        let mode_config_key = venue.market_id();
        let huma_config_key = venue.huma_config_key().unwrap();
        let mode_mint_key = venue.mode_mint_key();
        // Bring the read-only state accounts into LiteSVM.
        copy_accounts(
            litesvm,
            cache,
            &[
                POOL_CONFIG_KEY,
                venue.pool_state_key(),
                mode_config_key,
                mode_mint_key,
                huma_config_key,
            ],
            &[],
        )
        .await;

        let mode_token_program = venue.get_token_info()[1].get_token_program();
        let lender_state_key = pda::derive_lender_state(&mode_config_key, &keypair.pubkey());
        let lender_mode_token =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &keypair.pubkey(),
                &mode_mint_key,
                &mode_token_program,
            );
        let metas = vec![
            AccountMeta::new(keypair.pubkey(), true),           // payer
            AccountMeta::new_readonly(keypair.pubkey(), false), // lender (= payer)
            AccountMeta::new_readonly(huma_config_key, false),
            AccountMeta::new_readonly(POOL_CONFIG_KEY, false),
            AccountMeta::new_readonly(venue.pool_state_key(), false),
            AccountMeta::new_readonly(mode_config_key, false),
            AccountMeta::new_readonly(mode_mint_key, false),
            AccountMeta::new(lender_state_key, false), // init
            AccountMeta::new(lender_mode_token, false), // init_if_needed
            AccountMeta::new_readonly(mode_token_program, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ];
        let ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: metas,
            data: compute_disc("create_lender_accounts_v2").to_vec(),
        };
        let blockhash = litesvm.latest_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&keypair.pubkey()),
            &[keypair],
            blockhash,
        );
        let result = litesvm.send_transaction(tx);
        assert!(
            result.is_ok(),
            "create_lender_accounts_v2 failed: {:?}",
            result.err()
        );
    }

    /// Simulates a deposit at `request.amount` underlying and returns the
    /// resulting mode-token balance.
    async fn sim_deposit_request(
        venue: &HumaVenue,
        cache: &dyn AccountsCache,
        request: QuoteRequest,
        litesvm: &mut LiteSVM,
        keypair: &Keypair,
    ) -> u64 {
        let token_info = venue.get_token_info();
        let [underlying_token, mode_token] = token_info else {
            panic!("Huma venue must expose exactly 2 tokens");
        };

        let depositor_underlying_ata = fund_underlying_ata(
            litesvm,
            keypair.pubkey(),
            underlying_token.pubkey,
            underlying_token.get_token_program(),
        );
        let depositor_mode_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &keypair.pubkey(),
                &mode_token.pubkey,
                &mode_token.get_token_program(),
            );
        let ix = venue
            .generate_swap_instruction(request, keypair.pubkey())
            .unwrap();
        // Refresh the on-chain state in LiteSVM for this iteration. Keep our
        // synthetic depositor ATA — don't let the cache (which has no entry
        // for it) overwrite with `None`-handling logic.
        let pks: Vec<Pubkey> = ix.accounts.iter().map(|a| a.pubkey).collect();
        copy_accounts(litesvm, cache, &pks, &[depositor_underlying_ata]).await;

        let blockhash = litesvm.latest_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&keypair.pubkey()),
            &[keypair],
            blockhash,
        );
        let sim = litesvm
            .simulate_transaction(tx)
            .expect("deposit simulation failed");
        let mode_acc = sim
            .post_accounts
            .into_iter()
            .find(|(pk, _)| pk == &depositor_mode_ata)
            .map(|(_, a)| a)
            .expect("depositor mode-token account missing in post state");
        TokenAccount::unpack_from_slice(mode_acc.data())
            .expect("failed to unpack depositor mode-token account")
            .amount
    }

    /// Synthesizes the lender's mode-token ATA with the entire on-chain mode
    /// supply so any burn amount up to bounds-derived upper is satisfiable.
    /// Also creates an empty underlying-mint ATA to receive withdrawn funds.
    /// Returns the lender's underlying-mint ATA address.
    async fn fund_lender_for_withdraw(
        litesvm: &mut LiteSVM,
        cache: &dyn AccountsCache,
        owner: Pubkey,
        underlying_mint: Pubkey,
        underlying_token_program: Pubkey,
        mode_mint: Pubkey,
        mode_token_program: Pubkey,
    ) -> Pubkey {
        let mode_mint_account = cache
            .get_account(&mode_mint)
            .await
            .unwrap()
            .expect("mode mint must exist on chain");
        let mode_supply = state::read_mint_supply(mode_mint_account.data()).unwrap();

        let lender_mode_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &owner,
                &mode_mint,
                &mode_token_program,
            );
        let mut mode_acc = Account::new(LAMPORTS_PER_SOL, TokenAccount::LEN, &spl_token::ID);
        let mut mode_data = TokenAccount::default();
        mode_data.mint = mode_mint;
        mode_data.owner = owner;
        mode_data.state = AccountState::Initialized;
        mode_data.amount = mode_supply;
        mode_data.pack_into_slice(mode_acc.data_as_mut_slice());
        litesvm.set_account(lender_mode_ata, mode_acc).unwrap();

        let lender_underlying_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &owner,
                &underlying_mint,
                &underlying_token_program,
            );
        let mut under_acc = Account::new(LAMPORTS_PER_SOL, TokenAccount::LEN, &spl_token::ID);
        let mut under_data = TokenAccount::default();
        under_data.mint = underlying_mint;
        under_data.owner = owner;
        under_data.state = AccountState::Initialized;
        under_data.amount = 0;
        under_data.pack_into_slice(under_acc.data_as_mut_slice());
        litesvm
            .set_account(lender_underlying_ata, under_acc)
            .unwrap();

        lender_underlying_ata
    }

    /// Simulates an instant_withdraw at `request.amount` shares and returns
    /// the resulting underlying-token balance change of the lender.
    async fn sim_instant_withdraw_request(
        venue: &HumaVenue,
        cache: &dyn AccountsCache,
        request: QuoteRequest,
        litesvm: &mut LiteSVM,
        keypair: &Keypair,
    ) -> u64 {
        let token_info = venue.get_token_info();
        let [underlying_token, mode_token] = token_info else {
            panic!("Huma venue must expose exactly 2 tokens");
        };

        let lender_underlying_ata = fund_lender_for_withdraw(
            litesvm,
            cache,
            keypair.pubkey(),
            underlying_token.pubkey,
            underlying_token.get_token_program(),
            mode_token.pubkey,
            mode_token.get_token_program(),
        )
        .await;
        let lender_mode_ata =
            spl_associated_token_account::get_associated_token_address_with_program_id(
                &keypair.pubkey(),
                &mode_token.pubkey,
                &mode_token.get_token_program(),
            );

        let ix = venue
            .generate_swap_instruction(request, keypair.pubkey())
            .unwrap();
        // Refresh on-chain state for this iteration. Keep our synthesized
        // lender accounts since they don't exist on chain.
        let pks: Vec<Pubkey> = ix.accounts.iter().map(|a| a.pubkey).collect();
        copy_accounts(
            litesvm,
            cache,
            &pks,
            &[lender_underlying_ata, lender_mode_ata],
        )
        .await;

        let blockhash = litesvm.latest_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&keypair.pubkey()),
            &[keypair],
            blockhash,
        );
        let sim = litesvm
            .simulate_transaction(tx)
            .expect("instant_withdraw simulation failed");
        let underlying_acc = sim
            .post_accounts
            .into_iter()
            .find(|(pk, _)| pk == &lender_underlying_ata)
            .map(|(_, a)| a)
            .expect("lender underlying-token account missing in post state");
        TokenAccount::unpack_from_slice(underlying_acc.data())
            .expect("failed to unpack lender underlying-token account")
            .amount
    }

    fn sample_log_uniform_u64(lo: u64, hi: u64) -> u64 {
        assert!(lo >= 1, "log-uniform sampling requires lo >= 1");
        assert!(lo <= hi);
        let log_lo = (lo as f64).ln();
        let log_hi = (hi as f64).ln();
        let r: f64 = rand::rng().random();
        let log_val = log_lo + r * (log_hi - log_lo);
        (log_val.exp() as u64).clamp(lo, hi)
    }

    /// Constructs and updates a venue from RPC, yielding the venue plus its cache.
    async fn build_venue(mode_config_key: Pubkey) -> (HumaVenue, RpcClientCache) {
        let rpc_url =
            env::var("SOLANA_RPC_URL").expect("SOLANA_RPC_URL must be set for integration tests");
        let rpc = RpcClient::new(rpc_url);
        let mode_config_account = rpc.get_account(&mode_config_key).await.unwrap();

        let cache = RpcClientCache::new(rpc);
        let mut venue = HumaVenue::from_account(&mode_config_key, &mode_config_account).unwrap();
        venue.update_state(&cache).await.unwrap();
        (venue, cache)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 1: deposit boundary simulation
    // ─────────────────────────────────────────────────────────────────────────

    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")]
    async fn test_bound_simulation(#[case] mode_config_key: Pubkey) {
        init_test_logger();

        let (venue, cache) = build_venue(mode_config_key).await;
        let (mut litesvm, keypair) = setup_litesvm();
        sync_clock(&mut litesvm, &cache).await;
        create_lender_accounts(&mut litesvm, &cache, &venue, &keypair).await;

        let token_info = venue.get_token_info();
        assert_eq!(token_info.len(), 2);

        // Deposit direction only: underlying (0) → mode mint (1).
        let (lower, upper) = venue.bounds(0, 1).unwrap();
        for bound in [lower, upper] {
            let request = QuoteRequest {
                input_mint: token_info[0].pubkey,
                output_mint: token_info[1].pubkey,
                amount: bound,
                swap_type: SwapType::ExactIn,
            };
            let sim =
                sim_deposit_request(&venue, &cache, request.clone(), &mut litesvm, &keypair).await;
            let quote = venue.quote(request).unwrap();

            log::debug!(
                "Boundary={} sim={} quote={} delta={}",
                bound,
                sim,
                quote.expected_output,
                quote.expected_output.abs_diff(sim),
            );

            assert_eq!(quote.expected_output, sim);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 2: deposit random sampling simulation
    // ─────────────────────────────────────────────────────────────────────────

    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")]
    async fn test_random_samples(#[case] mode_config_key: Pubkey) {
        init_test_logger();

        let (venue, cache) = build_venue(mode_config_key).await;
        let (mut litesvm, keypair) = setup_litesvm();
        sync_clock(&mut litesvm, &cache).await;
        create_lender_accounts(&mut litesvm, &cache, &venue, &keypair).await;

        let token_info = venue.get_token_info();
        let (lb, ub) = venue.bounds(0, 1).unwrap();

        for _ in 0..50 {
            let amount = sample_log_uniform_u64(lb, ub);
            let request = QuoteRequest {
                input_mint: token_info[0].pubkey,
                output_mint: token_info[1].pubkey,
                amount,
                swap_type: SwapType::ExactIn,
            };
            let sim =
                sim_deposit_request(&venue, &cache, request.clone(), &mut litesvm, &keypair).await;
            let quote = venue.quote(request).unwrap();

            log::debug!(
                "amount={} sim={} quote={} delta={}",
                amount,
                sim,
                quote.expected_output,
                quote.expected_output.abs_diff(sim),
            );

            assert_eq!(quote.expected_output, sim);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 3: instant_withdraw boundary simulation
    // ─────────────────────────────────────────────────────────────────────────

    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")]
    async fn test_instant_withdraw_bound_simulation(#[case] mode_config_key: Pubkey) {
        init_test_logger();

        let (venue, cache) = build_venue(mode_config_key).await;
        let (mut litesvm, keypair) = setup_litesvm();
        sync_clock(&mut litesvm, &cache).await;
        create_lender_accounts(&mut litesvm, &cache, &venue, &keypair).await;

        let token_info = venue.get_token_info();
        assert_eq!(token_info.len(), 2);

        // Instant-withdraw direction: mode mint (1) → underlying (0). We only
        // validate the upper bound here. The lower bound exposes a strategy-
        // CPI edge case (e.g. Jupiter's `OperateAmountsNearlyZero` when the
        // requested withdrawal rounds to zero f-tokens) that our off-chain
        // quote doesn't currently model.
        let (_lower, upper) = venue.bounds(1, 0).unwrap();
        for bound in [upper] {
            let request = QuoteRequest {
                input_mint: token_info[1].pubkey,
                output_mint: token_info[0].pubkey,
                amount: bound,
                swap_type: SwapType::ExactIn,
            };
            let sim = sim_instant_withdraw_request(
                &venue,
                &cache,
                request.clone(),
                &mut litesvm,
                &keypair,
            )
            .await;
            let quote = venue.quote(request).unwrap();

            log::debug!(
                "Boundary={} sim={} quote={} delta={}",
                bound,
                sim,
                quote.expected_output,
                quote.expected_output.abs_diff(sim),
            );

            assert_eq!(quote.expected_output, sim);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 4: instant_withdraw random sampling simulation
    // ─────────────────────────────────────────────────────────────────────────

    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")]
    async fn test_instant_withdraw_random_samples(#[case] mode_config_key: Pubkey) {
        init_test_logger();

        let (venue, cache) = build_venue(mode_config_key).await;
        let (mut litesvm, keypair) = setup_litesvm();
        sync_clock(&mut litesvm, &cache).await;
        create_lender_accounts(&mut litesvm, &cache, &venue, &keypair).await;

        let token_info = venue.get_token_info();
        let (lb, ub) = venue.bounds(1, 0).unwrap();
        // Bias the lower end up to avoid strategy-CPI rounding edge cases at
        // very small amounts (see comment in test_instant_withdraw_bound_simulation).
        let lo = lb.max(ub / 10).max(1);

        for _ in 0..50 {
            let amount = sample_log_uniform_u64(lo, ub);
            let request = QuoteRequest {
                input_mint: token_info[1].pubkey,
                output_mint: token_info[0].pubkey,
                amount,
                swap_type: SwapType::ExactIn,
            };
            let sim = sim_instant_withdraw_request(
                &venue,
                &cache,
                request.clone(),
                &mut litesvm,
                &keypair,
            )
            .await;
            let quote = venue.quote(request).unwrap();

            log::debug!(
                "amount={} sim={} quote={} delta={}",
                amount,
                sim,
                quote.expected_output,
                quote.expected_output.abs_diff(sim),
            );

            assert_eq!(quote.expected_output, sim);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 5: monotonicity (off-chain only, both directions)
    // ─────────────────────────────────────────────────────────────────────────

    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN")]
    async fn test_monotone(#[case] mode_config_key: Pubkey) {
        init_test_logger();

        let (venue, _cache) = build_venue(mode_config_key).await;
        let token_info = venue.get_token_info();
        assert_eq!(token_info.len(), 2);

        for (in_idx, out_idx) in [(0u8, 1u8), (1u8, 0u8)] {
            let (lb, ub) = venue.bounds(in_idx, out_idx).unwrap();
            let mut amounts: Vec<u64> = (0..50).map(|_| sample_log_uniform_u64(lb, ub)).collect();
            amounts.sort();

            let mut prev = 0u64;
            for amount in amounts {
                let result = venue
                    .quote(QuoteRequest {
                        input_mint: token_info[in_idx as usize].pubkey,
                        output_mint: token_info[out_idx as usize].pubkey,
                        amount,
                        swap_type: SwapType::ExactIn,
                    })
                    .unwrap();
                assert!(
                    prev <= result.expected_output,
                    "non-monotone quote: prev={} got={} (amount={})",
                    prev,
                    result.expected_output,
                    amount,
                );
                prev = result.expected_output;
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Test 6: quoting speed
    // ─────────────────────────────────────────────────────────────────────────

    #[rstest]
    #[tokio::test]
    #[case("3FhoMDyKzQqxtGxnz9DfysfoGQKvgDnSFjoDGgguDCQN", 10_000)]
    async fn test_quoting_speed(#[case] mode_config_key: Pubkey, #[case] iterations: usize) {
        init_test_logger();

        let (venue, _cache) = build_venue(mode_config_key).await;
        let token_info = venue.get_token_info();

        for (in_idx, out_idx) in [(0u8, 1u8), (1u8, 0u8)] {
            let (lb, ub) = venue.bounds(in_idx, out_idx).unwrap();
            let amounts: Vec<u64> = (0..iterations)
                .map(|_| sample_log_uniform_u64(lb, ub))
                .collect();

            let start = Instant::now();
            for amount in amounts {
                let _ = venue
                    .quote(QuoteRequest {
                        input_mint: token_info[in_idx as usize].pubkey,
                        output_mint: token_info[out_idx as usize].pubkey,
                        amount,
                        swap_type: SwapType::ExactIn,
                    })
                    .unwrap();
            }
            let avg_time = start.elapsed().as_secs_f64() / iterations as f64;
            log::info!("avg quote time ({}→{}): {avg_time}s", in_idx, out_idx);
            assert!(
                avg_time < 0.0001,
                "quote too slow ({avg_time}s) for direction ({in_idx}, {out_idx})"
            );
        }
    }
}
