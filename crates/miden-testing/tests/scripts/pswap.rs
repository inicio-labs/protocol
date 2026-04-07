use std::collections::BTreeMap;

use miden_protocol::account::auth::AuthScheme;
use miden_protocol::account::{Account, AccountStorageMode};
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::crypto::rand::{FeltRng, RandomCoin};
use miden_protocol::note::{Note, NoteAssets, NoteMetadata, NoteRecipient, NoteStorage, NoteType};
use miden_protocol::transaction::RawOutputNote;
use miden_protocol::{Felt, ONE, Word, ZERO};
use miden_standards::account::wallets::BasicWallet;
use miden_standards::note::{PswapNote, PswapNoteStorage};
use miden_testing::{Auth, MockChain};
use rand::{Rng, SeedableRng};

// CONSTANTS
// ================================================================================================

const BASIC_AUTH: Auth = Auth::BasicAuth {
    auth_scheme: AuthScheme::Falcon512Poseidon2,
};

// HELPERS
// ================================================================================================

/// Asserts that `asset` is a [`FungibleAsset`] with the expected faucet and amount.
///
/// Panics with a descriptive message including `context` on mismatch.
#[track_caller]
fn assert_fungible_asset(
    asset: &Asset,
    expected_faucet: miden_protocol::account::AccountId,
    expected_amount: u64,
    context: &str,
) {
    match asset {
        Asset::Fungible(f) => {
            assert_eq!(f.faucet_id(), expected_faucet, "{context}: faucet id mismatch");
            assert_eq!(
                f.amount(),
                expected_amount,
                "{context}: amount mismatch (expected {expected_amount}, got {})",
                f.amount()
            );
        },
        _ => panic!("{context}: expected fungible asset"),
    }
}

/// Asserts the vault delta contains exactly the expected added/removed fungible assets.
///
/// Each entry is `(faucet_id, amount)`. Order does not matter.
#[track_caller]
fn assert_vault_delta(
    vault_delta: &miden_protocol::account::delta::AccountVaultDelta,
    expected_added: &[(miden_protocol::account::AccountId, u64)],
    expected_removed: &[(miden_protocol::account::AccountId, u64)],
    context: &str,
) {
    let added: Vec<Asset> = vault_delta.added_assets().collect();
    let removed: Vec<Asset> = vault_delta.removed_assets().collect();

    assert_eq!(
        added.len(),
        expected_added.len(),
        "{context}: added assets count mismatch (expected {}, got {})",
        expected_added.len(),
        added.len()
    );
    assert_eq!(
        removed.len(),
        expected_removed.len(),
        "{context}: removed assets count mismatch (expected {}, got {})",
        expected_removed.len(),
        removed.len()
    );

    for (faucet_id, amount) in expected_added {
        let found = added.iter().any(|a| {
            matches!(a, Asset::Fungible(f) if f.faucet_id() == *faucet_id && f.amount() == *amount)
        });
        assert!(
            found,
            "{context}: expected added asset (faucet={faucet_id}, amount={amount}) not found"
        );
    }
    for (faucet_id, amount) in expected_removed {
        let found = removed.iter().any(|a| {
            matches!(a, Asset::Fungible(f) if f.faucet_id() == *faucet_id && f.amount() == *amount)
        });
        assert!(
            found,
            "{context}: expected removed asset (faucet={faucet_id}, amount={amount}) not found"
        );
    }
}

/// Builds a note-args [`Word`] from input and inflight amounts.
fn build_note_args(input_amount: u64, inflight_amount: u64) -> Word {
    Word::from([
        Felt::try_from(input_amount).unwrap(),
        Felt::try_from(inflight_amount).unwrap(),
        ZERO,
        ZERO,
    ])
}

// TESTS - FILL (full, partial, network)
// ================================================================================================

/// Parameters for the unified fill test.
#[derive(Debug, Clone)]
struct FillTestCase {
    /// How much of the requested asset the consumer provides.
    fill_amount: u64,
    note_type: NoteType,
    /// When true, the consumer is a network account (no note_args).
    use_network_account: bool,
}

#[rstest::rstest]
#[case::full_fill_public(FillTestCase {
    fill_amount: 25,
    note_type: NoteType::Public,
    use_network_account: false,
})]
#[case::full_fill_private(FillTestCase {
    fill_amount: 25,
    note_type: NoteType::Private,
    use_network_account: false,
})]
#[case::partial_fill_public(FillTestCase {
    fill_amount: 20,
    note_type: NoteType::Public,
    use_network_account: false,
})]
#[case::network_full_fill(FillTestCase {
    fill_amount: 25,
    note_type: NoteType::Public,
    use_network_account: true,
})]
#[tokio::test]
async fn pswap_fill_test(#[case] tc: FillTestCase) -> anyhow::Result<()> {
    let offered_amount: u64 = 50;
    let requested_amount: u64 = 25;

    let mut builder = MockChain::builder();

    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(offered_amount + 100))?;
    let eth_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(tc.fill_amount))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), offered_amount)?.into()],
    )?;

    let offered_asset = FungibleAsset::new(usdc_faucet.id(), offered_amount)?;
    let requested_asset = FungibleAsset::new(eth_faucet.id(), requested_amount)?;

    // Build consumer account - network or local
    let bob = if tc.use_network_account {
        let seed: [u8; 32] = builder.rng_mut().draw_word().into();
        builder.add_account_from_builder(
            BASIC_AUTH,
            Account::builder(seed)
                .storage_mode(AccountStorageMode::Network)
                .with_component(BasicWallet)
                .with_assets([FungibleAsset::new(eth_faucet.id(), tc.fill_amount)?.into()]),
            miden_testing::AccountState::Exists,
        )?
    } else {
        builder.add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(eth_faucet.id(), tc.fill_amount)?.into()],
        )?
    };

    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(tc.note_type)
        .offered_asset(offered_asset)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));

    let mut mock_chain = builder.build()?;

    let pswap = PswapNote::try_from(&pswap_note)?;
    let is_full_fill = tc.fill_amount == requested_amount;

    // Build transaction context
    let tx_context = if tc.use_network_account {
        // Network accounts don't pass note_args; the script defaults to full fill.
        let p2id_note = pswap.execute_full_fill_network(bob.id())?;
        mock_chain
            .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
            .extend_expected_output_notes(vec![RawOutputNote::Full(p2id_note)])
            .build()?
    } else {
        let fill_asset = FungibleAsset::new(eth_faucet.id(), tc.fill_amount)?;
        let (p2id_note, remainder_pswap) = pswap.execute(bob.id(), Some(fill_asset), None)?;

        let mut expected_notes = vec![RawOutputNote::Full(p2id_note)];
        if let Some(remainder) = remainder_pswap {
            expected_notes.push(RawOutputNote::Full(Note::from(remainder)));
        }

        let mut note_args_map = BTreeMap::new();
        note_args_map.insert(pswap_note.id(), build_note_args(tc.fill_amount, 0));

        mock_chain
            .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
            .extend_note_args(note_args_map)
            .extend_expected_output_notes(expected_notes)
            .build()?
    };

    let executed_transaction = tx_context.execute().await?;

    // --- Verify output notes ---
    let output_notes = executed_transaction.output_notes();
    let expected_note_count = if is_full_fill { 1 } else { 2 };
    assert_eq!(output_notes.num_notes(), expected_note_count, "output note count mismatch");

    // P2ID note: carries the fill amount of the requested asset
    let p2id_assets = output_notes.get_note(0).assets();
    assert_eq!(p2id_assets.num_assets(), 1, "P2ID note should have exactly 1 asset");
    assert_fungible_asset(
        &p2id_assets.iter().next().unwrap(),
        eth_faucet.id(),
        tc.fill_amount,
        "P2ID note",
    );

    // Remainder note (partial fill only): carries leftover offered asset
    if !is_full_fill {
        let offered_out = pswap.calculate_offered_for_requested(tc.fill_amount);
        let expected_remainder = offered_amount - offered_out;
        let remainder_assets = output_notes.get_note(1).assets();
        assert_fungible_asset(
            &remainder_assets.iter().next().unwrap(),
            usdc_faucet.id(),
            expected_remainder,
            "remainder note",
        );
    }

    // --- Verify consumer vault delta ---
    let offered_out = pswap.calculate_offered_for_requested(tc.fill_amount);
    assert_vault_delta(
        executed_transaction.account_delta().vault(),
        &[(usdc_faucet.id(), offered_out)],
        &[(eth_faucet.id(), tc.fill_amount)],
        "consumer vault delta",
    );

    mock_chain.add_pending_executed_transaction(&executed_transaction)?;
    let _ = mock_chain.prove_next_block();

    Ok(())
}

// TESTS - COW (coincidence of wants) inflight
// ================================================================================================

#[derive(Debug, Clone)]
struct CowTestCase {
    note_a_offered: u64,
    note_a_requested: u64,
    note_b_offered: u64,
    note_b_requested: u64,
}

#[rstest::rstest]
#[case::exact_1to2(CowTestCase { note_a_offered: 50, note_a_requested: 25, note_b_offered: 25, note_b_requested: 50 })]
#[case::exact_2to1(CowTestCase { note_a_offered: 100, note_a_requested: 50, note_b_offered: 50, note_b_requested: 100 })]
#[case::partial_btc_eth(CowTestCase { note_a_offered: 100_000_000, note_a_requested: 3_500_000_000, note_b_offered: 1_750_000_000, note_b_requested: 50_000_000 })]
#[tokio::test]
async fn pswap_cow_inflight_test(#[case] tc: CowTestCase) -> anyhow::Result<()> {
    let mut builder = MockChain::builder();
    let max_supply = 10_000_000_000u64;
    let faucet_a = builder.add_existing_basic_faucet(
        BASIC_AUTH,
        "ASSETA",
        max_supply,
        Some(tc.note_a_offered),
    )?;
    let faucet_b = builder.add_existing_basic_faucet(
        BASIC_AUTH,
        "ASSETB",
        max_supply,
        Some(tc.note_b_offered),
    )?;
    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(faucet_a.id(), tc.note_a_offered)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(faucet_b.id(), tc.note_b_offered)?.into()],
    )?;
    let charlie = builder.add_existing_wallet_with_assets(BASIC_AUTH, [])?;

    let mut rng = RandomCoin::new(Word::default());

    // Alice's note: offers asset A, requests asset B
    let alice_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(faucet_b.id(), tc.note_a_requested)?)
        .creator_account_id(alice.id())
        .build();
    let alice_pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(alice_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(faucet_a.id(), tc.note_a_offered)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(alice_pswap_note.clone()));

    // Bob's note: offers asset B, requests asset A
    let bob_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(faucet_a.id(), tc.note_b_requested)?)
        .creator_account_id(bob.id())
        .build();
    let bob_pswap_note: Note = PswapNote::builder()
        .sender(bob.id())
        .storage(bob_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(faucet_b.id(), tc.note_b_offered)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(bob_pswap_note.clone()));
    let mock_chain = builder.build()?;

    let alice_pswap = PswapNote::try_from(&alice_pswap_note)?;
    let bob_pswap = PswapNote::try_from(&bob_pswap_note)?;

    // For Note A: inflight = all of Bob's offered (asset B)
    let alice_inflight = tc.note_b_offered;
    // For Note B: inflight = offered_out from Note A for alice_inflight (asset A)
    let alice_offered_out = alice_pswap.calculate_offered_for_requested(alice_inflight);
    let bob_inflight = alice_offered_out;

    let mut note_args_map = BTreeMap::new();
    note_args_map.insert(alice_pswap_note.id(), build_note_args(0, alice_inflight));
    note_args_map.insert(bob_pswap_note.id(), build_note_args(0, bob_inflight));

    let (alice_p2id_note, alice_remainder) = alice_pswap.execute(
        charlie.id(),
        None,
        Some(FungibleAsset::new(faucet_b.id(), alice_inflight)?),
    )?;
    let (bob_p2id_note, bob_remainder) = bob_pswap.execute(
        charlie.id(),
        None,
        Some(FungibleAsset::new(faucet_a.id(), bob_inflight)?),
    )?;

    let mut expected_notes =
        vec![RawOutputNote::Full(alice_p2id_note), RawOutputNote::Full(bob_p2id_note)];
    let has_alice_remainder = alice_remainder.is_some();
    let has_bob_remainder = bob_remainder.is_some();
    if let Some(r) = alice_remainder {
        expected_notes.push(RawOutputNote::Full(Note::from(r)));
    }
    if let Some(r) = bob_remainder {
        expected_notes.push(RawOutputNote::Full(Note::from(r)));
    }

    let tx_context = mock_chain
        .build_tx_context(charlie.id(), &[alice_pswap_note.id(), bob_pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .extend_expected_output_notes(expected_notes)
        .build()?;
    let executed_transaction = tx_context.execute().await?;

    let output_notes = executed_transaction.output_notes();
    let expected_count = 2 + (has_alice_remainder as usize) + (has_bob_remainder as usize);
    assert_eq!(
        output_notes.num_notes(),
        expected_count,
        "output note count mismatch (expected {expected_count})"
    );

    // Charlie's vault should be unchanged (pure inflight)
    assert_vault_delta(
        executed_transaction.account_delta().vault(),
        &[],
        &[],
        "charlie vault (pure inflight, should be empty)",
    );
    Ok(())
}

// TESTS - CREATOR RECLAIM
// ================================================================================================

#[tokio::test]
async fn pswap_creator_reclaim_test() -> anyhow::Result<()> {
    let offered_amount: u64 = 50;
    let requested_amount: u64 = 25;

    let mut builder = MockChain::builder();

    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(offered_amount))?;
    let eth_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(requested_amount))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), offered_amount)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), requested_amount)?;
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), offered_amount)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));

    let mock_chain = builder.build()?;

    let tx_context = mock_chain.build_tx_context(alice.id(), &[pswap_note.id()], &[])?.build()?;

    let executed_transaction = tx_context.execute().await?;

    // Creator reclaim: 0 output notes, Alice gets her offered asset back
    assert_eq!(
        executed_transaction.output_notes().num_notes(),
        0,
        "reclaim should produce 0 output notes"
    );

    assert_vault_delta(
        executed_transaction.account_delta().vault(),
        &[(usdc_faucet.id(), offered_amount)],
        &[],
        "alice vault after reclaim",
    );

    Ok(())
}

// TESTS - OVERFILL REJECTED
// ================================================================================================

#[tokio::test]
async fn pswap_overfill_rejected_test() -> anyhow::Result<()> {
    let offered_amount: u64 = 50;
    let requested_amount: u64 = 25;
    let overfill_amount: u64 = 30; // exceeds requested

    let mut builder = MockChain::builder();

    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(offered_amount))?;
    let eth_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(overfill_amount))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), offered_amount)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), overfill_amount)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), requested_amount)?;
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), offered_amount)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));
    let mock_chain = builder.build()?;

    // Try to fill with more than requested - should fail
    let mut note_args_map = BTreeMap::new();
    note_args_map.insert(pswap_note.id(), build_note_args(overfill_amount, 0));

    let tx_context = mock_chain
        .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .build()?;

    let result = tx_context.execute().await;
    assert!(
        result.is_err(),
        "transaction should fail when fill amount ({overfill_amount}) > requested ({requested_amount})"
    );

    Ok(())
}

// TESTS - ROUNDTRIP (builder -> try_from -> execute)
// ================================================================================================

/// Verifies that PswapNote builder -> Note -> try_from roundtrips correctly,
/// and that execute produces correct output for both full and partial fills.
#[test]
fn pswap_roundtrip_test() {
    let offered_amount: u64 = 50;
    let requested_amount: u64 = 25;
    let partial_fill_amount: u64 = 10;

    let mut builder = MockChain::builder();
    let usdc_faucet = builder
        .add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(offered_amount + 100))
        .unwrap();
    let eth_faucet = builder
        .add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(requested_amount))
        .unwrap();
    let alice = builder
        .add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(usdc_faucet.id(), offered_amount).unwrap().into()],
        )
        .unwrap();
    let bob = builder
        .add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(eth_faucet.id(), requested_amount).unwrap().into()],
        )
        .unwrap();

    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), requested_amount).unwrap();
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), offered_amount).unwrap())
        .build()
        .unwrap()
        .into();

    // --- Roundtrip: Note -> PswapNote ---
    let pswap = PswapNote::try_from(&pswap_note).unwrap();

    assert_eq!(pswap.sender(), alice.id(), "sender mismatch after roundtrip");
    assert_eq!(pswap.note_type(), NoteType::Public, "note type mismatch after roundtrip");
    assert_eq!(
        pswap.storage().requested_asset_amount(),
        requested_amount,
        "requested amount mismatch after roundtrip"
    );
    assert_eq!(pswap.storage().swap_count(), 0, "swap count should be 0 for fresh note");
    assert_eq!(
        pswap.storage().creator_account_id(),
        alice.id(),
        "creator id mismatch after roundtrip"
    );

    // --- Storage roundtrip: NoteStorage items -> PswapNoteStorage ---
    let note_storage = pswap_note.recipient().storage();
    let parsed = PswapNoteStorage::try_from(note_storage.items()).unwrap();

    assert_eq!(
        parsed.creator_account_id(),
        alice.id(),
        "storage roundtrip: creator id mismatch"
    );
    assert_eq!(parsed.swap_count(), 0, "storage roundtrip: swap count mismatch");
    assert_eq!(
        parsed.requested_asset_amount(),
        requested_amount,
        "storage roundtrip: requested amount mismatch"
    );

    // --- Full fill: should produce P2ID note, no remainder ---
    let full_fill_asset = FungibleAsset::new(eth_faucet.id(), requested_amount).unwrap();
    let (p2id_note, remainder) = pswap.execute(bob.id(), Some(full_fill_asset), None).unwrap();
    assert!(remainder.is_none(), "full fill should not produce remainder");

    assert_eq!(p2id_note.metadata().sender(), bob.id(), "P2ID sender should be consumer");
    assert_eq!(
        p2id_note.metadata().note_type(),
        NoteType::Public,
        "P2ID note type should match original"
    );
    assert_eq!(p2id_note.assets().num_assets(), 1, "P2ID should have 1 asset");
    assert_fungible_asset(
        &p2id_note.assets().iter().next().unwrap(),
        eth_faucet.id(),
        requested_amount,
        "P2ID note (full fill)",
    );

    // --- Partial fill: should produce P2ID note + remainder ---
    let partial_fill_asset = FungibleAsset::new(eth_faucet.id(), partial_fill_amount).unwrap();
    let (p2id_partial, remainder_partial) =
        pswap.execute(bob.id(), Some(partial_fill_asset), None).unwrap();
    let remainder_pswap = remainder_partial.expect("partial fill should produce remainder");

    assert_eq!(p2id_partial.assets().num_assets(), 1);
    assert_fungible_asset(
        &p2id_partial.assets().iter().next().unwrap(),
        eth_faucet.id(),
        partial_fill_amount,
        "P2ID note (partial fill)",
    );

    // Verify remainder properties
    assert_eq!(remainder_pswap.storage().swap_count(), 1, "remainder swap count should be 1");
    assert_eq!(
        remainder_pswap.storage().creator_account_id(),
        alice.id(),
        "remainder creator should be original creator"
    );
    let expected_remaining_requested = requested_amount - partial_fill_amount;
    assert_eq!(
        remainder_pswap.storage().requested_asset_amount(),
        expected_remaining_requested,
        "remaining requested amount mismatch"
    );
}

// TESTS - COW ARBITRAGE (maker earns spread)
// ================================================================================================

#[tokio::test]
async fn pswap_cow_arbitrage_spread_test() -> anyhow::Result<()> {
    let max_supply = 100_000u64;

    // Alice offers 100 USDC, requests 40 ETH (rate: 2.5 USDC/ETH)
    let alice_offered: u64 = 100;
    let alice_requested: u64 = 40;
    // Bob offers 50 ETH, requests 80 USDC (rate: 1.6 USDC/ETH)
    let bob_offered: u64 = 50;
    let bob_requested: u64 = 80;
    // Charlie seeds 80 USDC, fills Bob fully, then fills Alice with 40 ETH from Bob's note
    let charlie_usdc: u64 = 80;

    let mut builder = MockChain::builder();
    let usdc_faucet = builder.add_existing_basic_faucet(
        BASIC_AUTH,
        "USDC",
        max_supply,
        Some(alice_offered + charlie_usdc),
    )?;
    let eth_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", max_supply, Some(bob_offered))?;
    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), alice_offered)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), bob_offered)?.into()],
    )?;
    let charlie = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), charlie_usdc)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());

    // Note A: Alice offers USDC, requests ETH
    let alice_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(eth_faucet.id(), alice_requested)?)
        .creator_account_id(alice.id())
        .build();
    let alice_pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(alice_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), alice_offered)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(alice_pswap_note.clone()));

    // Note B: Bob offers ETH, requests USDC
    let bob_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(usdc_faucet.id(), bob_requested)?)
        .creator_account_id(bob.id())
        .build();
    let bob_pswap_note: Note = PswapNote::builder()
        .sender(bob.id())
        .storage(bob_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(eth_faucet.id(), bob_offered)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(bob_pswap_note.clone()));
    let mock_chain = builder.build()?;

    // Charlie fills both using input_amount (from vault):
    // Note B: input_amount=80 USDC -> full fill -> Charlie gets 50 ETH, P2ID to Bob with 80 USDC
    // Note A: input_amount=40 ETH -> full fill -> Charlie gets 100 USDC, P2ID to Alice with 40 ETH
    let mut note_args_map = BTreeMap::new();
    note_args_map.insert(alice_pswap_note.id(), build_note_args(alice_requested, 0));
    note_args_map.insert(bob_pswap_note.id(), build_note_args(bob_requested, 0));

    let alice_pswap = PswapNote::try_from(&alice_pswap_note)?;
    let (alice_p2id_note, _) = alice_pswap.execute(
        charlie.id(),
        Some(FungibleAsset::new(eth_faucet.id(), alice_requested)?),
        None,
    )?;
    let bob_pswap = PswapNote::try_from(&bob_pswap_note)?;
    let (bob_p2id_note, _) = bob_pswap.execute(
        charlie.id(),
        Some(FungibleAsset::new(usdc_faucet.id(), bob_requested)?),
        None,
    )?;

    let tx_context = mock_chain
        .build_tx_context(charlie.id(), &[bob_pswap_note.id(), alice_pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .extend_expected_output_notes(vec![
            RawOutputNote::Full(alice_p2id_note),
            RawOutputNote::Full(bob_p2id_note),
        ])
        .build()?;
    let executed_transaction = tx_context.execute().await?;

    assert_eq!(
        executed_transaction.output_notes().num_notes(),
        2,
        "expected exactly 2 P2ID notes"
    );

    // Verify Charlie's vault delta (NET changes)
    // Charlie starts with 80 USDC. Receives 100 USDC + 50 ETH, sends 80 USDC + 40 ETH.
    // Net: +20 USDC, +10 ETH.
    let expected_usdc_profit = alice_offered - charlie_usdc; // 100 - 80 = 20
    let expected_eth_profit = bob_offered - alice_requested; // 50 - 40 = 10

    assert_vault_delta(
        executed_transaction.account_delta().vault(),
        &[(usdc_faucet.id(), expected_usdc_profit), (eth_faucet.id(), expected_eth_profit)],
        &[],
        "charlie vault (arbitrage profit)",
    );
    Ok(())
}

// TESTS - MIXED INPUT AND INFLIGHT
// ================================================================================================

#[tokio::test]
async fn pswap_mixed_input_and_inflight_test() -> anyhow::Result<()> {
    let max_supply = 100_000u64;

    let alice_offered: u64 = 100;
    let alice_requested: u64 = 50;
    let bob_offered: u64 = 30;
    let bob_requested: u64 = 60;
    let charlie_eth: u64 = 20;

    let mut builder = MockChain::builder();
    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", max_supply, Some(alice_offered))?;
    let eth_faucet = builder.add_existing_basic_faucet(
        BASIC_AUTH,
        "ETH",
        max_supply,
        Some(bob_offered + charlie_eth),
    )?;
    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), alice_offered)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), bob_offered)?.into()],
    )?;
    let charlie = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), charlie_eth)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());

    // Note A: Alice offers 100 USDC, requests 50 ETH
    let alice_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(eth_faucet.id(), alice_requested)?)
        .creator_account_id(alice.id())
        .build();
    let alice_pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(alice_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), alice_offered)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(alice_pswap_note.clone()));

    // Note B: Bob offers 30 ETH, requests 60 USDC
    let bob_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(usdc_faucet.id(), bob_requested)?)
        .creator_account_id(bob.id())
        .build();
    let bob_pswap_note: Note = PswapNote::builder()
        .sender(bob.id())
        .storage(bob_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(eth_faucet.id(), bob_offered)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(bob_pswap_note.clone()));
    let mock_chain = builder.build()?;

    let alice_pswap = PswapNote::try_from(&alice_pswap_note)?;
    let bob_pswap = PswapNote::try_from(&bob_pswap_note)?;

    // Note A: input_amount=20 ETH (from vault), inflight_amount=30 ETH (from Note B) -> full fill
    // Note B: input_amount=0, inflight_amount=60 USDC (from Note A) -> full fill
    let alice_input = charlie_eth;
    let alice_inflight = bob_offered;
    let bob_inflight = bob_requested;

    let mut note_args_map = BTreeMap::new();
    note_args_map.insert(alice_pswap_note.id(), build_note_args(alice_input, alice_inflight));
    note_args_map.insert(bob_pswap_note.id(), build_note_args(0, bob_inflight));

    let (alice_p2id_note, _) = alice_pswap.execute(
        charlie.id(),
        Some(FungibleAsset::new(eth_faucet.id(), alice_input)?),
        Some(FungibleAsset::new(eth_faucet.id(), alice_inflight)?),
    )?;
    let (bob_p2id_note, _) = bob_pswap.execute(
        charlie.id(),
        None,
        Some(FungibleAsset::new(usdc_faucet.id(), bob_inflight)?),
    )?;

    let tx_context = mock_chain
        .build_tx_context(charlie.id(), &[alice_pswap_note.id(), bob_pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .extend_expected_output_notes(vec![
            RawOutputNote::Full(alice_p2id_note),
            RawOutputNote::Full(bob_p2id_note),
        ])
        .build()?;
    let executed_transaction = tx_context.execute().await?;

    assert_eq!(executed_transaction.output_notes().num_notes(), 2);

    // Charlie's vault: receives offered_out_input from Note A for 20 ETH input
    let expected_usdc_received = alice_pswap.calculate_offered_for_requested(alice_input);
    assert_vault_delta(
        executed_transaction.account_delta().vault(),
        &[(usdc_faucet.id(), expected_usdc_received)],
        &[(eth_faucet.id(), alice_input)],
        "charlie vault (mixed input+inflight)",
    );
    Ok(())
}

// TESTS - FUZZ: PARTIAL FILL CONSERVATION
// ================================================================================================

#[tokio::test]
async fn pswap_fuzz_partial_fill_conservation() -> anyhow::Result<()> {
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(42);

    for iteration in 0..50 {
        let offered_total: u64 = rng.random_range(2..100_000);
        let requested_total: u64 = rng.random_range(2..100_000);
        let fill_amount: u64 = rng.random_range(1..=requested_total);

        let mut builder = MockChain::builder();
        let max_supply = 1_000_000_000u64;
        let usdc_faucet = builder.add_existing_basic_faucet(
            BASIC_AUTH,
            "USDC",
            max_supply,
            Some(offered_total),
        )?;
        let eth_faucet =
            builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", max_supply, Some(fill_amount))?;

        let alice = builder.add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(usdc_faucet.id(), offered_total)?.into()],
        )?;
        let bob = builder.add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(eth_faucet.id(), fill_amount)?.into()],
        )?;

        let mut coin_rng = RandomCoin::new(Word::default());
        let storage = PswapNoteStorage::builder()
            .requested_asset(FungibleAsset::new(eth_faucet.id(), requested_total)?)
            .creator_account_id(alice.id())
            .build();
        let pswap_note: Note = PswapNote::builder()
            .sender(alice.id())
            .storage(storage)
            .serial_number(coin_rng.draw_word())
            .note_type(NoteType::Public)
            .offered_asset(FungibleAsset::new(usdc_faucet.id(), offered_total)?)
            .build()?
            .into();
        builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));
        let mock_chain = builder.build()?;

        let mut note_args_map = BTreeMap::new();
        note_args_map.insert(pswap_note.id(), build_note_args(fill_amount, 0));

        let pswap = PswapNote::try_from(&pswap_note)?;
        let offered_out = pswap.calculate_offered_for_requested(fill_amount);

        // Verify Rust-side invariants
        assert!(offered_out > 0, "iter {iteration}: offered_out must be > 0");
        assert!(
            offered_out <= offered_total,
            "iter {iteration}: offered_out ({offered_out}) > offered_total ({offered_total})"
        );

        let (p2id_note, remainder_pswap) = pswap.execute(
            bob.id(),
            Some(FungibleAsset::new(eth_faucet.id(), fill_amount)?),
            None,
        )?;

        let mut expected_notes = vec![RawOutputNote::Full(p2id_note)];
        let is_partial = fill_amount < requested_total;
        if let Some(remainder) = remainder_pswap {
            expected_notes.push(RawOutputNote::Full(Note::from(remainder)));
        }

        let tx_context = mock_chain
            .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
            .extend_expected_output_notes(expected_notes)
            .extend_note_args(note_args_map)
            .build()?;

        let executed_tx = tx_context.execute().await.map_err(|e| {
            anyhow::anyhow!(
                "iter {iteration} failed: {e} (offered={offered_total}, requested={requested_total}, fill={fill_amount})"
            )
        })?;

        let output_notes = executed_tx.output_notes();
        let expected_count = if is_partial { 2 } else { 1 };
        assert_eq!(
            output_notes.num_notes(),
            expected_count,
            "iter {iteration}: expected {expected_count} notes"
        );

        // Verify Bob's vault
        let added: Vec<Asset> = executed_tx.account_delta().vault().added_assets().collect();
        assert_eq!(added.len(), 1, "iter {iteration}: bob should receive 1 asset");
        assert_fungible_asset(
            &added[0],
            usdc_faucet.id(),
            offered_out,
            &format!("iter {iteration}: bob vault added asset"),
        );

        // Conservation: offered_out + remainder_offered == offered_total
        if is_partial {
            let remainder_asset = output_notes.get_note(1).assets().iter().next().unwrap();
            if let Asset::Fungible(f) = remainder_asset {
                let remainder_offered = f.amount();
                assert_eq!(
                    offered_out + remainder_offered,
                    offered_total,
                    "iter {iteration}: conservation violated                      (offered_out={offered_out} + remainder={remainder_offered} != total={offered_total})"
                );
            }
        }
    }

    Ok(())
}

// TESTS - FUZZ: INFLIGHT COW CONSERVATION
// ================================================================================================

#[tokio::test]
async fn pswap_fuzz_inflight_cow_conservation() -> anyhow::Result<()> {
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(123);

    for iteration in 0..20 {
        let note_a_offered: u64 = rng.random_range(10..10_000);
        let note_a_requested: u64 = rng.random_range(10..10_000);
        // Note B offers up to note_a_requested of B (so it can fill Note A partially or fully)
        let note_b_offered: u64 = rng.random_range(1..=note_a_requested);
        let note_b_requested: u64 = rng.random_range(1..10_000);

        let mut builder = MockChain::builder();
        let max_supply = 1_000_000_000u64;
        let faucet_a = builder.add_existing_basic_faucet(
            BASIC_AUTH,
            "ASSETA",
            max_supply,
            Some(note_a_offered),
        )?;
        let faucet_b = builder.add_existing_basic_faucet(
            BASIC_AUTH,
            "ASSETB",
            max_supply,
            Some(note_b_offered),
        )?;
        let alice = builder.add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(faucet_a.id(), note_a_offered)?.into()],
        )?;
        let bob = builder.add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(faucet_b.id(), note_b_offered)?.into()],
        )?;
        let charlie = builder.add_existing_wallet_with_assets(BASIC_AUTH, [])?;

        let mut coin_rng = RandomCoin::new(Word::default());

        let alice_storage = PswapNoteStorage::builder()
            .requested_asset(FungibleAsset::new(faucet_b.id(), note_a_requested)?)
            .creator_account_id(alice.id())
            .build();
        let alice_pswap_note: Note = PswapNote::builder()
            .sender(alice.id())
            .storage(alice_storage)
            .serial_number(coin_rng.draw_word())
            .note_type(NoteType::Public)
            .offered_asset(FungibleAsset::new(faucet_a.id(), note_a_offered)?)
            .build()?
            .into();
        builder.add_output_note(RawOutputNote::Full(alice_pswap_note.clone()));

        let bob_storage = PswapNoteStorage::builder()
            .requested_asset(FungibleAsset::new(faucet_a.id(), note_b_requested)?)
            .creator_account_id(bob.id())
            .build();
        let bob_pswap_note: Note = PswapNote::builder()
            .sender(bob.id())
            .storage(bob_storage)
            .serial_number(coin_rng.draw_word())
            .note_type(NoteType::Public)
            .offered_asset(FungibleAsset::new(faucet_b.id(), note_b_offered)?)
            .build()?
            .into();
        builder.add_output_note(RawOutputNote::Full(bob_pswap_note.clone()));
        let mock_chain = builder.build()?;

        let alice_pswap = PswapNote::try_from(&alice_pswap_note)?;
        let bob_pswap = PswapNote::try_from(&bob_pswap_note)?;

        let alice_inflight = note_b_offered;
        let alice_offered_out = alice_pswap.calculate_offered_for_requested(alice_inflight);
        let bob_inflight = alice_offered_out;

        // Skip if bob_inflight exceeds Note B's requested (would fail validation)
        if bob_inflight > note_b_requested || bob_inflight == 0 {
            continue;
        }

        // For asset conservation in a CoW, we need alice_inflight == bob_offered_out.
        let bob_offered_out = bob_pswap.calculate_offered_for_requested(bob_inflight);
        if bob_offered_out != alice_inflight {
            continue;
        }

        let mut note_args_map = BTreeMap::new();
        note_args_map.insert(alice_pswap_note.id(), build_note_args(0, alice_inflight));
        note_args_map.insert(bob_pswap_note.id(), build_note_args(0, bob_inflight));

        let (alice_p2id, alice_rem) = alice_pswap.execute(
            charlie.id(),
            None,
            Some(FungibleAsset::new(faucet_b.id(), alice_inflight)?),
        )?;
        let (bob_p2id, bob_rem) = bob_pswap.execute(
            charlie.id(),
            None,
            Some(FungibleAsset::new(faucet_a.id(), bob_inflight)?),
        )?;

        let mut expected_notes =
            vec![RawOutputNote::Full(alice_p2id), RawOutputNote::Full(bob_p2id)];
        if let Some(r) = alice_rem {
            expected_notes.push(RawOutputNote::Full(Note::from(r)));
        }
        if let Some(r) = bob_rem {
            expected_notes.push(RawOutputNote::Full(Note::from(r)));
        }

        let tx_context = mock_chain
            .build_tx_context(charlie.id(), &[alice_pswap_note.id(), bob_pswap_note.id()], &[])?
            .extend_note_args(note_args_map)
            .extend_expected_output_notes(expected_notes)
            .build()?;

        let executed_tx = tx_context.execute().await.map_err(|e| {
            anyhow::anyhow!(
                "iter {iteration} failed: {e}                  (a_off={note_a_offered}, a_req={note_a_requested},                   b_off={note_b_offered}, b_req={note_b_requested})"
            )
        })?;

        // Charlie's vault should be unchanged (pure inflight)
        assert_vault_delta(
            executed_tx.account_delta().vault(),
            &[],
            &[],
            &format!("iter {iteration}: charlie vault (pure inflight)"),
        );
    }

    Ok(())
}

// TESTS - FUZZ: CHAINED FILLS CONSERVATION
// ================================================================================================

#[tokio::test]
async fn pswap_fuzz_chained_fills_conservation() -> anyhow::Result<()> {
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(999);

    for chain_idx in 0..20 {
        let initial_offered: u64 = rng.random_range(10..10_000);
        let initial_requested: u64 = rng.random_range(10..10_000);
        let num_fills: usize = rng.random_range(2..=4);

        // Generate fill amounts that sum to less than initial_requested
        let mut fills = Vec::new();
        let mut remaining = initial_requested;
        for i in 0..num_fills {
            if remaining <= 1 {
                break;
            }
            let max_fill = if i == num_fills - 1 {
                remaining - 1 // leave at least 1 for remainder
            } else {
                remaining / 2
            };
            if max_fill == 0 {
                break;
            }
            let fill = rng.random_range(1..=max_fill);
            fills.push(fill);
            remaining -= fill;
        }

        if fills.is_empty() {
            continue;
        }

        let mut current_offered = initial_offered;
        let mut current_requested = initial_requested;
        let mut total_offered_out = 0u64;
        let mut total_fill = 0u64;
        let mut coin_rng = RandomCoin::new(Word::default());
        let mut current_serial = coin_rng.draw_word();

        for (swap_count, fill_amount) in fills.iter().enumerate() {
            let remaining_requested = current_requested - fill_amount;

            let mut builder = MockChain::builder();
            let max_supply = 1_000_000_000u64;
            let usdc_faucet = builder.add_existing_basic_faucet(
                BASIC_AUTH,
                "USDC",
                max_supply,
                Some(current_offered),
            )?;
            let eth_faucet = builder.add_existing_basic_faucet(
                BASIC_AUTH,
                "ETH",
                max_supply,
                Some(*fill_amount),
            )?;

            let alice = builder.add_existing_wallet_with_assets(
                BASIC_AUTH,
                [FungibleAsset::new(usdc_faucet.id(), current_offered)?.into()],
            )?;
            let bob = builder.add_existing_wallet_with_assets(
                BASIC_AUTH,
                [FungibleAsset::new(eth_faucet.id(), *fill_amount)?.into()],
            )?;

            let offered_fungible = FungibleAsset::new(usdc_faucet.id(), current_offered)?;
            let requested_fungible = FungibleAsset::new(eth_faucet.id(), current_requested)?;
            let pswap_tag =
                PswapNote::create_tag(NoteType::Public, &offered_fungible, &requested_fungible);
            let offered_asset = Asset::Fungible(offered_fungible);

            let storage = PswapNoteStorage::builder()
                .requested_asset(requested_fungible)
                .pswap_tag(pswap_tag)
                .swap_count(swap_count as u16)
                .creator_account_id(alice.id())
                .build();
            let note_assets = NoteAssets::new(vec![offered_asset])?;
            let note_storage = NoteStorage::from(storage);
            let recipient = NoteRecipient::new(current_serial, PswapNote::script(), note_storage);
            let metadata = NoteMetadata::new(alice.id(), NoteType::Public).with_tag(pswap_tag);
            let pswap_note = Note::new(note_assets, metadata, recipient);

            builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));
            let mock_chain = builder.build()?;

            let mut note_args_map = BTreeMap::new();
            note_args_map.insert(pswap_note.id(), build_note_args(*fill_amount, 0));

            let pswap = PswapNote::try_from(&pswap_note)?;
            let offered_out = pswap.calculate_offered_for_requested(*fill_amount);
            let (p2id_note, remainder_pswap) = pswap.execute(
                bob.id(),
                Some(FungibleAsset::new(eth_faucet.id(), *fill_amount)?),
                None,
            )?;

            let mut expected_notes = vec![RawOutputNote::Full(p2id_note)];
            if remaining_requested > 0 {
                let remainder =
                    Note::from(remainder_pswap.expect("partial fill should produce remainder"));
                expected_notes.push(RawOutputNote::Full(remainder));
            }

            let tx_context = mock_chain
                .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
                .extend_expected_output_notes(expected_notes)
                .extend_note_args(note_args_map)
                .build()?;

            let executed_tx = tx_context.execute().await.map_err(|e| {
                anyhow::anyhow!(
                    "chain {chain_idx} fill {swap_count} failed: {e}                      (offered={current_offered}, requested={current_requested}, fill={fill_amount})"
                )
            })?;

            let output_notes = executed_tx.output_notes();
            let expected_count = if remaining_requested > 0 { 2 } else { 1 };
            assert_eq!(
                output_notes.num_notes(),
                expected_count,
                "chain {chain_idx} fill {swap_count}: output note count mismatch"
            );

            total_offered_out += offered_out;
            total_fill += fill_amount;
            current_offered -= offered_out;
            current_requested = remaining_requested;
            current_serial = Word::from([
                current_serial[0] + ONE,
                current_serial[1],
                current_serial[2],
                current_serial[3],
            ]);
        }

        // Conservation: total_offered_out + remaining_offered == initial_offered
        assert_eq!(
            total_offered_out + current_offered,
            initial_offered,
            "chain {chain_idx}: offered asset conservation violated              (total_out={total_offered_out} + remaining={current_offered} != initial={initial_offered})"
        );
        assert_eq!(
            total_fill + current_requested,
            initial_requested,
            "chain {chain_idx}: requested asset conservation violated              (total_fill={total_fill} + remaining={current_requested} != initial={initial_requested})"
        );
    }

    Ok(())
}
