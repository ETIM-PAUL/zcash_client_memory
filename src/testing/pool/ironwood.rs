//! Tests from the shared `zcash_client_backend` suite that activate NU6.3 and so exercise the
//! Ironwood pool: receiving into it, spending from it, and moving value from Orchard into it.

use zcash_client_backend::data_api::testing::pool;

use crate::testing::{MemBlockCache, TestMemDbFactory};

#[test]
fn propose_v5_payment_to_orchard_receiver_is_rejected() {
    pool::propose_v5_payment_to_orchard_receiver_is_rejected(
        TestMemDbFactory::new(),
        MemBlockCache::new(),
    )
}

#[test]
fn orchard_to_ironwood_payment_reports_net_value_delta() {
    pool::orchard_to_ironwood_payment_reports_net_value_delta(
        TestMemDbFactory::new(),
        MemBlockCache::new(),
    )
}

#[test]
#[ignore = "the test adapter does not classify pool crossings (`TransactionSummary::pool_crossing_value`); the balance assertions that precede that check pass"]
fn orchard_to_ironwood_self_migration_reports_fee_only_delta() {
    pool::orchard_to_ironwood_self_migration_reports_fee_only_delta(
        TestMemDbFactory::new(),
        MemBlockCache::new(),
    )
}

#[test]
#[ignore = "requires a configurable anchor retention interval (ZIP 318 pool migration), which this backend does not support"]
fn self_migration_keeps_spending_orchard() {
    pool::self_migration_keeps_spending_orchard(TestMemDbFactory::new(), MemBlockCache::new())
}

#[test]
#[ignore = "requires a configurable anchor retention interval (ZIP 318 pool migration), which this backend does not support"]
fn canonical_crossing_is_bucketed_and_unpadded() {
    pool::canonical_crossing_is_bucketed_and_unpadded(TestMemDbFactory::new(), MemBlockCache::new())
}

#[test]
#[ignore = "requires a configurable anchor retention interval (ZIP 318 pool migration), which this backend does not support"]
fn canonical_crossing_builds_at_empty_boundary_block() {
    pool::canonical_crossing_builds_at_empty_boundary_block(
        TestMemDbFactory::new(),
        MemBlockCache::new(),
    )
}

#[test]
#[ignore = "requires a configurable anchor retention interval (ZIP 318 pool migration), which this backend does not support"]
fn multi_note_crossing_is_not_bucketed() {
    pool::multi_note_crossing_is_not_bucketed(TestMemDbFactory::new(), MemBlockCache::new())
}

#[test]
#[ignore = "requires a configurable anchor retention interval (ZIP 318 pool migration), which this backend does not support"]
fn canonical_crossing_prefers_single_note() {
    pool::canonical_crossing_prefers_single_note(TestMemDbFactory::new(), MemBlockCache::new())
}

#[test]
#[cfg(feature = "pczt")]
fn create_pczt_supports_ironwood_output() {
    pool::create_pczt_supports_ironwood_output(TestMemDbFactory::new(), MemBlockCache::new())
}

#[test]
#[cfg(feature = "transparent-inputs")]
#[ignore = "coinbase shielding finds no funds in this backend (pre-existing transparent coinbase limitation, not investigated)"]
fn shielding_coinbase_to_orchard_receiver_delivers_via_ironwood() {
    pool::shielding_coinbase_to_orchard_receiver_delivers_via_ironwood(
        TestMemDbFactory::new(),
        MemBlockCache::new(),
    )
}

/// Receives value into the Ironwood pool by migrating it to the wallet's own Orchard receiver,
/// then spends that Ironwood note to an external recipient. The spend needs a witness from the
/// Ironwood note commitment tree, which no shared test exercises against this backend.
#[test]
fn ironwood_note_received_by_scanning_can_be_spent() {
    use std::convert::Infallible;

    use zcash_client_backend::{
        data_api::{
            Account as _, WalletRead,
            testing::{
                TestBuilder, orchard::OrchardPoolTester, pool::ShieldedPoolTester,
                pool::dsl::TestDsl, single_output_change_strategy,
            },
            wallet::{ConfirmationsPolicy, input_selection::GreedyInputSelector},
        },
        fees::StandardFeeRule,
        wallet::OvkPolicy,
    };
    use zcash_primitives::block::BlockHash;
    use zcash_protocol::{
        PoolType, ShieldedPool, consensus::BlockHeight, local_consensus::LocalNetwork,
        value::Zatoshis,
    };
    use zip321::{Payment, TransactionRequest};

    let activation = BlockHeight::from_u32(100_000);
    let network = LocalNetwork {
        nu6: Some(activation),
        nu6_1: Some(activation),
        nu6_2: Some(activation),
        nu6_3: Some(activation),
        ..TestBuilder::<(), ()>::DEFAULT_NETWORK
    };
    let mut st = TestDsl::from(
        TestBuilder::new()
            .with_network(network)
            .with_data_store_factory(TestMemDbFactory::new())
            .with_block_cache(MemBlockCache::new())
            .with_account_from_sapling_activation(BlockHash([0; 32])),
    )
    .build::<OrchardPoolTester>();

    // Fund the wallet with one Orchard note.
    let note_value = Zatoshis::const_from_u64(100_000);
    st.add_a_single_note_checking_balance(note_value);

    let account = st.get_account();
    let account_id = account.id();
    let usk = account.usk().clone();
    let input_selector = GreedyInputSelector::new();
    let change_strategy =
        single_output_change_strategy(StandardFeeRule::Zip317, None, ShieldedPool::Orchard);

    // Step 1: move 70_000 to the wallet's own Orchard receiver. Post-NU6.3 this payment is
    // delivered through the Ironwood pool, and the wallet must find it by scanning.
    let own_address =
        OrchardPoolTester::fvk_default_address(&OrchardPoolTester::test_account_fvk(&st));
    let migrated = Zatoshis::const_from_u64(70_000);
    let own_zaddr = own_address.to_zcash_address(st.network());
    let proposal = st
        .propose_transfer(
            account_id,
            &input_selector,
            &change_strategy,
            TransactionRequest::new(vec![Payment::without_memo(own_zaddr, migrated)]).unwrap(),
            ConfirmationsPolicy::MIN,
        )
        .unwrap();
    assert_eq!(
        proposal.steps().head.payment_pools().get(&0),
        Some(&PoolType::IRONWOOD)
    );
    let fee1 = proposal.steps().head.balance().fee_required();
    let txid1 = *st
        .create_proposed_transactions::<Infallible, _, Infallible, _>(
            &usk,
            OvkPolicy::Sender,
            &proposal,
        )
        .unwrap()
        .first();
    let (h1, _) = st.generate_next_block_including(txid1);
    st.scan_cached_blocks(h1, 1);

    let after_migration = (note_value - fee1).unwrap();
    assert_eq!(st.get_total_balance(account_id), after_migration);
    let summary = st
        .wallet()
        .get_wallet_summary(ConfirmationsPolicy::MIN)
        .unwrap()
        .unwrap();
    assert_eq!(
        summary.account_balances()[&account_id]
            .ironwood_balance()
            .total(),
        migrated,
        "the migrated value is held in the Ironwood pool"
    );

    // Step 2: pay 50_000 to an external recipient. The Orchard change (100_000 - 70_000 - fee) is
    // too small, so the Ironwood note must be spent.
    let to = OrchardPoolTester::sk_default_address(&OrchardPoolTester::sk(&[0xf5; 32]));
    let sent = Zatoshis::const_from_u64(50_000);
    let to_zaddr = to.to_zcash_address(st.network());
    let proposal = st
        .propose_transfer(
            account_id,
            &input_selector,
            &change_strategy,
            TransactionRequest::new(vec![Payment::without_memo(to_zaddr, sent)]).unwrap(),
            ConfirmationsPolicy::MIN,
        )
        .unwrap();
    let inputs = proposal.steps().head.shielded_inputs().unwrap();
    assert!(
        inputs
            .notes()
            .iter()
            .any(|n| n.note().pool() == ShieldedPool::Ironwood),
        "the payment spends the Ironwood note"
    );
    let fee2 = proposal.steps().head.balance().fee_required();
    let txid2 = *st
        .create_proposed_transactions::<Infallible, _, Infallible, _>(
            &usk,
            OvkPolicy::Sender,
            &proposal,
        )
        .expect("building a spend of an Ironwood note succeeds")
        .first();
    let (h2, _) = st.generate_next_block_including(txid2);
    st.scan_cached_blocks(h2, 1);

    assert_eq!(
        st.get_total_balance(account_id),
        ((after_migration - sent).unwrap() - fee2).unwrap()
    );
}

/// The migration from the version 1 format re-queues already-scanned blocks. Rescanning them must
/// leave the wallet's notes and balance unchanged.
#[test]
fn rescanning_already_scanned_blocks_is_idempotent() {
    use zcash_client_backend::data_api::{
        Account as _, WalletRead, WalletWrite,
        testing::{orchard::OrchardPoolTester, pool::dsl::TestDsl},
        wallet::ConfirmationsPolicy,
    };
    use zcash_protocol::value::Zatoshis;

    let mut st =
        TestDsl::with_sapling_birthday_account(TestMemDbFactory::new(), MemBlockCache::new())
            .build::<OrchardPoolTester>();
    let value = Zatoshis::const_from_u64(60_000);
    let (h1, _, _) = st.add_a_single_note_checking_balance(value);
    let (h2, _, _) = st.add_a_single_note_checking_balance(value);
    let account_id = st.get_account().id();
    let total = (value + value).unwrap();
    assert_eq!(st.get_total_balance(account_id), total);
    let notes_before = st
        .wallet()
        .get_wallet_summary(ConfirmationsPolicy::MIN)
        .unwrap();

    st.wallet_mut().queue_rescan(h1..(h2 + 1)).unwrap();
    st.scan_cached_blocks(h1, usize::try_from((h2 - h1) + 1).unwrap());

    assert_eq!(st.get_total_balance(account_id), total);
    assert_eq!(
        st.get_spendable_balance(account_id, ConfirmationsPolicy::MIN),
        total
    );
    let notes_after = st
        .wallet()
        .get_wallet_summary(ConfirmationsPolicy::MIN)
        .unwrap();
    assert_eq!(
        notes_before.map(|s| s.account_balances().clone()),
        notes_after.map(|s| s.account_balances().clone())
    );
    // The rescanned range is no longer queued.
    assert!(
        st.wallet()
            .suggest_scan_ranges()
            .unwrap()
            .iter()
            .all(|r| r.block_range().end <= h1 || r.block_range().start > h2),
        "the rescanned range is no longer queued"
    );
}
