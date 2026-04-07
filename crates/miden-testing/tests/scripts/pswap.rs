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

// TESTS
// ================================================================================================

#[rstest::rstest]
#[case::public(NoteType::Public)]
#[case::private(NoteType::Private)]
#[tokio::test]
async fn pswap_note_full_fill_test(#[case] note_type: NoteType) -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    let usdc_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(150))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(50))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 50)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), 25)?.into()],
    )?;

    let offered_asset = FungibleAsset::new(usdc_faucet.id(), 50)?;
    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25)?;

    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(note_type)
        .offered_asset(offered_asset)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));

    let mut mock_chain = builder.build()?;

    let mut note_args_map = BTreeMap::new();
    note_args_map
        .insert(pswap_note.id(), Word::from([Felt::from(25u32), Felt::from(0u32), ZERO, ZERO]));

    let pswap = PswapNote::try_from(&pswap_note)?;
    let (p2id_note, _remainder) =
        pswap.execute(bob.id(), Some(FungibleAsset::new(eth_faucet.id(), 25)?), None)?;

    let tx_context = mock_chain
        .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .extend_expected_output_notes(vec![RawOutputNote::Full(p2id_note.clone())])
        .build()?;

    let executed_transaction = tx_context.execute().await?;

    // Verify: 1 P2ID note with 25 ETH
    let output_notes = executed_transaction.output_notes();
    assert_eq!(output_notes.num_notes(), 1, "Expected exactly 1 P2ID note");

    let actual_recipient = output_notes.get_note(0).recipient_digest();
    let expected_recipient = p2id_note.recipient().digest();
    assert_eq!(actual_recipient, expected_recipient, "RECIPIENT MISMATCH!");

    let p2id_assets = output_notes.get_note(0).assets();
    assert_eq!(p2id_assets.num_assets(), 1);
    if let Asset::Fungible(f) = p2id_assets.iter().next().unwrap() {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 25);
    } else {
        panic!("Expected fungible asset in P2ID note");
    }

    // Verify Bob's vault delta: +50 USDC, -25 ETH
    let vault_delta = executed_transaction.account_delta().vault();
    let added: Vec<Asset> = vault_delta.added_assets().collect();
    let removed: Vec<Asset> = vault_delta.removed_assets().collect();

    assert_eq!(added.len(), 1);
    assert_eq!(removed.len(), 1);
    if let Asset::Fungible(f) = &added[0] {
        assert_eq!(f.faucet_id(), usdc_faucet.id());
        assert_eq!(f.amount(), 50);
    }
    if let Asset::Fungible(f) = &removed[0] {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 25);
    }

    mock_chain.add_pending_executed_transaction(&executed_transaction)?;
    let _ = mock_chain.prove_next_block();

    Ok(())
}

#[tokio::test]
async fn pswap_note_partial_fill_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    let usdc_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(150))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(50))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 50)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), 20)?.into()],
    )?;

    let offered_asset = FungibleAsset::new(usdc_faucet.id(), 50)?;
    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25)?;

    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(offered_asset)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));

    let mut mock_chain = builder.build()?;

    let mut note_args_map = BTreeMap::new();
    note_args_map
        .insert(pswap_note.id(), Word::from([Felt::from(20u32), Felt::from(0u32), ZERO, ZERO]));

    let pswap = PswapNote::try_from(&pswap_note)?;
    let (p2id_note, remainder_pswap) =
        pswap.execute(bob.id(), Some(FungibleAsset::new(eth_faucet.id(), 20)?), None)?;
    let remainder_note =
        Note::from(remainder_pswap.expect("partial fill should produce remainder"));

    let tx_context = mock_chain
        .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .extend_expected_output_notes(vec![
            RawOutputNote::Full(p2id_note),
            RawOutputNote::Full(remainder_note),
        ])
        .build()?;

    let executed_transaction = tx_context.execute().await?;

    // Verify: 2 output notes (P2ID + remainder)
    let output_notes = executed_transaction.output_notes();
    assert_eq!(output_notes.num_notes(), 2);

    // P2ID note: 20 ETH
    if let Asset::Fungible(f) = output_notes.get_note(0).assets().iter().next().unwrap() {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 20);
    }

    // SWAPp remainder: 10 USDC
    if let Asset::Fungible(f) = output_notes.get_note(1).assets().iter().next().unwrap() {
        assert_eq!(f.faucet_id(), usdc_faucet.id());
        assert_eq!(f.amount(), 10);
    }

    // Bob's vault: +40 USDC, -20 ETH
    let vault_delta = executed_transaction.account_delta().vault();
    let added: Vec<Asset> = vault_delta.added_assets().collect();
    let removed: Vec<Asset> = vault_delta.removed_assets().collect();
    assert_eq!(added.len(), 1);
    assert_eq!(removed.len(), 1);
    if let Asset::Fungible(f) = &added[0] {
        assert_eq!(f.faucet_id(), usdc_faucet.id());
        assert_eq!(f.amount(), 40);
    }
    if let Asset::Fungible(f) = &removed[0] {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 20);
    }

    mock_chain.add_pending_executed_transaction(&executed_transaction)?;
    let _ = mock_chain.prove_next_block();

    Ok(())
}

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
    note_args_map.insert(
        alice_pswap_note.id(),
        Word::from([Felt::from(0u32), Felt::try_from(alice_inflight).unwrap(), ZERO, ZERO]),
    );
    note_args_map.insert(
        bob_pswap_note.id(),
        Word::from([Felt::from(0u32), Felt::try_from(bob_inflight).unwrap(), ZERO, ZERO]),
    );

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
        "Expected {} output notes",
        expected_count
    );

    // Charlie's vault should be unchanged (pure inflight)
    let vault_delta = executed_transaction.account_delta().vault();
    assert_eq!(vault_delta.added_assets().count(), 0, "Charlie should receive nothing in vault");
    assert_eq!(vault_delta.removed_assets().count(), 0, "Charlie should not lose anything");
    Ok(())
}

#[tokio::test]
async fn pswap_note_creator_reclaim_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    let usdc_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(50))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(25))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 50)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25)?;
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 50)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));

    let mock_chain = builder.build()?;

    let tx_context = mock_chain.build_tx_context(alice.id(), &[pswap_note.id()], &[])?.build()?;

    let executed_transaction = tx_context.execute().await?;

    // Verify: 0 output notes, Alice gets 50 USDC back
    let output_notes = executed_transaction.output_notes();
    assert_eq!(output_notes.num_notes(), 0, "Expected 0 output notes for reclaim");

    let account_delta = executed_transaction.account_delta();
    let vault_delta = account_delta.vault();
    let added_assets: Vec<Asset> = vault_delta.added_assets().collect();

    assert_eq!(added_assets.len(), 1, "Alice should receive 1 asset back");
    let usdc_reclaimed = match added_assets[0] {
        Asset::Fungible(f) => f,
        _ => panic!("Expected fungible USDC asset"),
    };
    assert_eq!(usdc_reclaimed.faucet_id(), usdc_faucet.id());
    assert_eq!(usdc_reclaimed.amount(), 50);

    Ok(())
}

#[tokio::test]
async fn pswap_note_invalid_input_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    let usdc_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(50))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(30))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 50)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), 30)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25)?;
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 50)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));
    let mock_chain = builder.build()?;

    // Try to fill with 30 ETH when only 25 is requested - should fail
    let mut note_args_map = BTreeMap::new();
    note_args_map
        .insert(pswap_note.id(), Word::from([Felt::from(30u32), Felt::from(0u32), ZERO, ZERO]));

    let tx_context = mock_chain
        .build_tx_context(bob.id(), &[pswap_note.id()], &[])?
        .extend_note_args(note_args_map)
        .build()?;

    let result = tx_context.execute().await;
    assert!(
        result.is_err(),
        "Transaction should fail when input_amount > requested_asset_total"
    );

    Ok(())
}

/// Test that PswapNote builder + try_from + execute roundtrips correctly
#[test]
fn compare_pswap_create_output_notes_vs_test_helper() {
    let mut builder = MockChain::builder();
    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(150)).unwrap();
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(50)).unwrap();
    let alice = builder
        .add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(usdc_faucet.id(), 50).unwrap().into()],
        )
        .unwrap();
    let bob = builder
        .add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(eth_faucet.id(), 25).unwrap().into()],
        )
        .unwrap();

    // Create swap note using PswapNote builder
    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25).unwrap();
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 50).unwrap())
        .build()
        .unwrap()
        .into();

    // Roundtrip: try_from -> execute -> verify outputs
    let pswap = PswapNote::try_from(&pswap_note).unwrap();

    // Verify roundtripped PswapNote preserves key fields
    assert_eq!(pswap.sender(), alice.id(), "Sender mismatch after roundtrip");
    assert_eq!(pswap.note_type(), NoteType::Public, "Note type mismatch after roundtrip");
    assert_eq!(pswap.storage().requested_asset_amount(), 25, "Requested amount mismatch");
    assert_eq!(pswap.storage().swap_count(), 0, "Swap count should be 0");
    assert_eq!(pswap.storage().creator_account_id(), alice.id(), "Creator ID mismatch");

    // Full fill: should produce P2ID note, no remainder
    let (p2id_note, remainder) = pswap
        .execute(bob.id(), Some(FungibleAsset::new(eth_faucet.id(), 25).unwrap()), None)
        .unwrap();
    assert!(remainder.is_none(), "Full fill should not produce remainder");

    // Verify P2ID note properties
    assert_eq!(p2id_note.metadata().sender(), bob.id(), "P2ID sender should be consumer");
    assert_eq!(p2id_note.metadata().note_type(), NoteType::Public, "P2ID note type mismatch");
    assert_eq!(p2id_note.assets().num_assets(), 1, "P2ID should have 1 asset");
    if let Asset::Fungible(f) = p2id_note.assets().iter().next().unwrap() {
        assert_eq!(f.faucet_id(), eth_faucet.id(), "P2ID asset faucet mismatch");
        assert_eq!(f.amount(), 25, "P2ID asset amount mismatch");
    } else {
        panic!("Expected fungible asset in P2ID note");
    }

    // Partial fill: should produce P2ID note + remainder
    let (p2id_partial, remainder_partial) = pswap
        .execute(bob.id(), Some(FungibleAsset::new(eth_faucet.id(), 10).unwrap()), None)
        .unwrap();
    let remainder_pswap = remainder_partial.expect("Partial fill should produce remainder");

    assert_eq!(p2id_partial.assets().num_assets(), 1);
    if let Asset::Fungible(f) = p2id_partial.assets().iter().next().unwrap() {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 10);
    }

    // Verify remainder properties
    assert_eq!(remainder_pswap.storage().swap_count(), 1, "Remainder swap count should be 1");
    assert_eq!(
        remainder_pswap.storage().creator_account_id(),
        alice.id(),
        "Remainder creator should be Alice"
    );
    let remaining_requested = remainder_pswap.storage().requested_asset_amount();
    assert_eq!(remaining_requested, 15, "Remaining requested should be 15");
}

/// Test that PswapNote::parse_inputs roundtrips correctly
#[test]
fn pswap_parse_inputs_roundtrip() {
    let mut builder = MockChain::builder();
    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(150)).unwrap();
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(50)).unwrap();
    let alice = builder
        .add_existing_wallet_with_assets(
            BASIC_AUTH,
            [FungibleAsset::new(usdc_faucet.id(), 50).unwrap().into()],
        )
        .unwrap();

    let mut rng = RandomCoin::new(Word::default());
    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25).unwrap();
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 50).unwrap())
        .build()
        .unwrap()
        .into();

    let storage = pswap_note.recipient().storage();
    let items = storage.items();

    let parsed = PswapNoteStorage::try_from(items).unwrap();

    assert_eq!(parsed.creator_account_id(), alice.id(), "Creator ID roundtrip failed!");
    assert_eq!(parsed.swap_count(), 0, "Swap count should be 0");

    // Verify requested amount from value word
    assert_eq!(parsed.requested_asset_amount(), 25, "Requested amount should be 25");
}

/// Test that a PSWAP note can be consumed by a network account (full fill, no note_args).
///
/// Alice (local) creates a PSWAP note offering 50 USDC for 25 ETH. A network account with a
/// BasicWallet consumes it. Since no note_args are provided, the script defaults to a full fill.
#[tokio::test]
async fn pswap_note_network_account_full_fill_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    let usdc_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", 1000, Some(150))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", 1000, Some(50))?;

    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 50)?.into()],
    )?;

    // Create a network account with BasicWallet that holds 25 ETH
    let seed: [u8; 32] = builder.rng_mut().draw_word().into();
    let network_consumer = builder.add_account_from_builder(
        BASIC_AUTH,
        Account::builder(seed)
            .storage_mode(AccountStorageMode::Network)
            .with_component(BasicWallet)
            .with_assets([FungibleAsset::new(eth_faucet.id(), 25)?.into()]),
        miden_testing::AccountState::Exists,
    )?;

    let requested_asset = FungibleAsset::new(eth_faucet.id(), 25)?;

    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(alice.id())
        .build();
    let pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 50)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(pswap_note.clone()));

    let mut mock_chain = builder.build()?;

    // No note_args — simulates a network transaction where args default to [0, 0, 0, 0].
    // The PSWAP script defaults to a full fill when both input and inflight are 0.
    let pswap = PswapNote::try_from(&pswap_note)?;
    let p2id_note = pswap.execute_full_fill_network(network_consumer.id())?;

    let tx_context = mock_chain
        .build_tx_context(network_consumer.id(), &[pswap_note.id()], &[])?
        .extend_expected_output_notes(vec![RawOutputNote::Full(p2id_note.clone())])
        .build()?;

    let executed_transaction = tx_context.execute().await?;

    // Verify: 1 P2ID note with 25 ETH for Alice
    let output_notes = executed_transaction.output_notes();
    assert_eq!(output_notes.num_notes(), 1, "Expected exactly 1 P2ID note");

    let actual_recipient = output_notes.get_note(0).recipient_digest();
    let expected_recipient = p2id_note.recipient().digest();
    assert_eq!(actual_recipient, expected_recipient, "Recipient mismatch");

    let p2id_assets = output_notes.get_note(0).assets();
    assert_eq!(p2id_assets.num_assets(), 1);
    if let Asset::Fungible(f) = p2id_assets.iter().next().unwrap() {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 25);
    } else {
        panic!("Expected fungible asset in P2ID note");
    }

    // Verify network consumer's vault delta: +50 USDC, -25 ETH
    let vault_delta = executed_transaction.account_delta().vault();
    let added: Vec<Asset> = vault_delta.added_assets().collect();
    let removed: Vec<Asset> = vault_delta.removed_assets().collect();

    assert_eq!(added.len(), 1);
    assert_eq!(removed.len(), 1);
    if let Asset::Fungible(f) = &added[0] {
        assert_eq!(f.faucet_id(), usdc_faucet.id());
        assert_eq!(f.amount(), 50);
    }
    if let Asset::Fungible(f) = &removed[0] {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 25);
    }

    mock_chain.add_pending_executed_transaction(&executed_transaction)?;
    let _ = mock_chain.prove_next_block();

    Ok(())
}

#[tokio::test]
async fn pswap_cow_arbitrage_maker_earns_spread_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();
    let max_supply = 100_000u64;
    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", max_supply, Some(180))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", max_supply, Some(50))?;
    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 100)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), 50)?.into()],
    )?;
    // Charlie starts with 80 USDC to seed the arbitrage
    let charlie = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 80)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());

    // Note A: Alice offers 100 USDC, requests 40 ETH
    let alice_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(eth_faucet.id(), 40)?)
        .creator_account_id(alice.id())
        .build();
    let alice_pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(alice_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 100)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(alice_pswap_note.clone()));

    // Note B: Bob offers 50 ETH, requests 80 USDC
    let bob_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(usdc_faucet.id(), 80)?)
        .creator_account_id(bob.id())
        .build();
    let bob_pswap_note: Note = PswapNote::builder()
        .sender(bob.id())
        .storage(bob_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(eth_faucet.id(), 50)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(bob_pswap_note.clone()));
    let mock_chain = builder.build()?;

    // Charlie fills both using input_amount (from vault):
    // Note B: input_amount=80 USDC -> full fill -> Charlie gets 50 ETH, P2ID to Bob with 80 USDC
    // Note A: input_amount=40 ETH -> full fill -> Charlie gets 100 USDC, P2ID to Alice with 40 ETH
    let mut note_args_map = BTreeMap::new();
    note_args_map.insert(
        alice_pswap_note.id(),
        Word::from([Felt::from(40u32), Felt::from(0u32), ZERO, ZERO]),
    );
    note_args_map.insert(
        bob_pswap_note.id(),
        Word::from([Felt::from(80u32), Felt::from(0u32), ZERO, ZERO]),
    );

    let alice_pswap = PswapNote::try_from(&alice_pswap_note)?;
    let (alice_p2id_note, _) =
        alice_pswap.execute(charlie.id(), Some(FungibleAsset::new(eth_faucet.id(), 40)?), None)?;
    let bob_pswap = PswapNote::try_from(&bob_pswap_note)?;
    let (bob_p2id_note, _) =
        bob_pswap.execute(charlie.id(), Some(FungibleAsset::new(usdc_faucet.id(), 80)?), None)?;

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
        "Expected exactly 2 P2ID notes"
    );

    // Verify Charlie's vault delta (NET changes)
    // Charlie starts with 80 USDC. Receives 100 USDC + 50 ETH, sends 80 USDC + 40 ETH.
    // Net: +20 USDC, +10 ETH. The vault delta reports net changes per asset.
    let vault_delta = executed_transaction.account_delta().vault();
    let added: Vec<Asset> = vault_delta.added_assets().collect();
    let removed: Vec<Asset> = vault_delta.removed_assets().collect();
    assert_eq!(added.len(), 2, "Charlie should have net gains in 2 assets");
    assert_eq!(removed.len(), 0, "Charlie should have no net losses");

    let mut usdc_net = 0u64;
    let mut eth_net = 0u64;
    for asset in &added {
        if let Asset::Fungible(f) = asset {
            if f.faucet_id() == usdc_faucet.id() {
                usdc_net = f.amount();
            } else if f.faucet_id() == eth_faucet.id() {
                eth_net = f.amount();
            }
        }
    }

    assert_eq!(usdc_net, 20, "Net USDC profit should be 20");
    assert_eq!(eth_net, 10, "Net ETH profit should be 10");
    Ok(())
}

#[tokio::test]
async fn pswap_mixed_input_and_inflight_fill_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();
    let max_supply = 100_000u64;
    let usdc_faucet =
        builder.add_existing_basic_faucet(BASIC_AUTH, "USDC", max_supply, Some(100))?;
    let eth_faucet = builder.add_existing_basic_faucet(BASIC_AUTH, "ETH", max_supply, Some(50))?;
    let alice = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(usdc_faucet.id(), 100)?.into()],
    )?;
    let bob = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), 30)?.into()],
    )?;
    // Charlie has 20 ETH
    let charlie = builder.add_existing_wallet_with_assets(
        BASIC_AUTH,
        [FungibleAsset::new(eth_faucet.id(), 20)?.into()],
    )?;

    let mut rng = RandomCoin::new(Word::default());

    // Note A: Alice offers 100 USDC, requests 50 ETH
    let alice_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(eth_faucet.id(), 50)?)
        .creator_account_id(alice.id())
        .build();
    let alice_pswap_note: Note = PswapNote::builder()
        .sender(alice.id())
        .storage(alice_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(usdc_faucet.id(), 100)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(alice_pswap_note.clone()));

    // Note B: Bob offers 30 ETH, requests 60 USDC
    let bob_storage = PswapNoteStorage::builder()
        .requested_asset(FungibleAsset::new(usdc_faucet.id(), 60)?)
        .creator_account_id(bob.id())
        .build();
    let bob_pswap_note: Note = PswapNote::builder()
        .sender(bob.id())
        .storage(bob_storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(FungibleAsset::new(eth_faucet.id(), 30)?)
        .build()?
        .into();
    builder.add_output_note(RawOutputNote::Full(bob_pswap_note.clone()));
    let mock_chain = builder.build()?;

    // Note A: input_amount=20 ETH (from vault), inflight_amount=30 ETH (from Note B) -> full fill
    // Note B: input_amount=0, inflight_amount=60 USDC (from Note A) -> full fill
    let mut note_args_map = BTreeMap::new();
    note_args_map.insert(
        alice_pswap_note.id(),
        Word::from([Felt::from(20u32), Felt::from(30u32), ZERO, ZERO]),
    );
    note_args_map.insert(
        bob_pswap_note.id(),
        Word::from([Felt::from(0u32), Felt::from(60u32), ZERO, ZERO]),
    );

    let alice_pswap = PswapNote::try_from(&alice_pswap_note)?;
    let (alice_p2id_note, _) = alice_pswap.execute(
        charlie.id(),
        Some(FungibleAsset::new(eth_faucet.id(), 20)?),
        Some(FungibleAsset::new(eth_faucet.id(), 30)?),
    )?;
    let bob_pswap = PswapNote::try_from(&bob_pswap_note)?;
    let (bob_p2id_note, _) =
        bob_pswap.execute(charlie.id(), None, Some(FungibleAsset::new(usdc_faucet.id(), 60)?))?;

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

    // Charlie's vault: receives offered_out_input from Note A = calculate(100, 50, 20) = 40 USDC
    // Charlie sends 20 ETH from vault to Note A
    let vault_delta = executed_transaction.account_delta().vault();
    let added: Vec<Asset> = vault_delta.added_assets().collect();
    let removed: Vec<Asset> = vault_delta.removed_assets().collect();
    assert_eq!(added.len(), 1, "Charlie should receive 1 asset (USDC)");
    assert_eq!(removed.len(), 1, "Charlie should send 1 asset (ETH)");
    if let Asset::Fungible(f) = &added[0] {
        assert_eq!(f.faucet_id(), usdc_faucet.id());
        assert_eq!(
            f.amount(),
            40,
            "Charlie should receive 40 USDC (offered_out_input for 20 ETH input)"
        );
    }
    if let Asset::Fungible(f) = &removed[0] {
        assert_eq!(f.faucet_id(), eth_faucet.id());
        assert_eq!(f.amount(), 20, "Charlie should send 20 ETH from vault");
    }
    Ok(())
}

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
        note_args_map.insert(
            pswap_note.id(),
            Word::from([Felt::try_from(fill_amount).unwrap(), Felt::from(0u32), ZERO, ZERO]),
        );

        let pswap = PswapNote::try_from(&pswap_note)?;
        let offered_out = pswap.calculate_offered_for_requested(fill_amount);

        // Verify Rust-side invariants
        assert!(offered_out > 0, "Iter {iteration}: offered_out must be > 0");
        assert!(
            offered_out <= offered_total,
            "Iter {iteration}: offered_out ({offered_out}) > offered_total ({offered_total})"
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
                "Iter {iteration} failed: {e} (offered={offered_total}, requested={requested_total}, fill={fill_amount})"
            )
        })?;

        let output_notes = executed_tx.output_notes();
        let expected_count = if is_partial { 2 } else { 1 };
        assert_eq!(
            output_notes.num_notes(),
            expected_count,
            "Iter {iteration}: expected {expected_count} notes"
        );

        // Verify Bob's vault
        let vault_delta = executed_tx.account_delta().vault();
        let added: Vec<Asset> = vault_delta.added_assets().collect();
        assert_eq!(added.len(), 1, "Iter {iteration}");
        if let Asset::Fungible(f) = &added[0] {
            assert_eq!(f.amount(), offered_out, "Iter {iteration}");
        }

        // Conservation: offered_out + remainder_offered == offered_total
        if is_partial {
            if let Asset::Fungible(f) = output_notes.get_note(1).assets().iter().next().unwrap() {
                let remainder_offered = f.amount();
                assert_eq!(
                    offered_out + remainder_offered,
                    offered_total,
                    "Iter {iteration}: conservation violated"
                );
            }
        }
    }

    Ok(())
}

#[tokio::test]
async fn pswap_fuzz_inflight_cow_conservation() -> anyhow::Result<()> {
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(123);

    for iteration in 0..20 {
        // Generate two compatible PSWAP notes:
        // Note A: offers asset_a_amount of A, requests asset_b_amount of B
        // Note B: offers some of B, requests some of A
        // We ensure Note B's offered <= Note A's requested so inflight is valid
        let note_a_offered: u64 = rng.random_range(10..10_000);
        let note_a_requested: u64 = rng.random_range(10..10_000);
        // Note B offers up to note_a_requested of B (so it can fill Note A partially or fully)
        let note_b_offered: u64 = rng.random_range(1..=note_a_requested);
        // Note B requests proportional amount of A based on some rate
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
        // This ensures total asset B is conserved: alice_inflight (in Alice's P2ID) +
        // (note_b_offered - bob_offered_out) (in Bob's remainder) == note_b_offered.
        let bob_offered_out = bob_pswap.calculate_offered_for_requested(bob_inflight);
        if bob_offered_out != alice_inflight {
            continue;
        }

        let mut note_args_map = BTreeMap::new();
        note_args_map.insert(
            alice_pswap_note.id(),
            Word::from([Felt::from(0u32), Felt::try_from(alice_inflight).unwrap(), ZERO, ZERO]),
        );
        note_args_map.insert(
            bob_pswap_note.id(),
            Word::from([Felt::from(0u32), Felt::try_from(bob_inflight).unwrap(), ZERO, ZERO]),
        );

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
                "Iter {iteration} failed: {e} (a_off={note_a_offered}, a_req={note_a_requested}, b_off={note_b_offered}, b_req={note_b_requested})"
            )
        })?;

        // Charlie's vault should be unchanged (pure inflight)
        let vault_delta = executed_tx.account_delta().vault();
        assert_eq!(
            vault_delta.added_assets().count(),
            0,
            "Iter {iteration}: Charlie should receive nothing"
        );
        assert_eq!(
            vault_delta.removed_assets().count(),
            0,
            "Iter {iteration}: Charlie should lose nothing"
        );
    }

    Ok(())
}

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
            note_args_map.insert(
                pswap_note.id(),
                Word::from([Felt::try_from(*fill_amount).unwrap(), Felt::from(0u32), ZERO, ZERO]),
            );

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
                    "Chain {chain_idx} fill {swap_count} failed: {e} (offered={current_offered}, requested={current_requested}, fill={fill_amount})"
                )
            })?;

            let output_notes = executed_tx.output_notes();
            let expected_count = if remaining_requested > 0 { 2 } else { 1 };
            assert_eq!(
                output_notes.num_notes(),
                expected_count,
                "Chain {chain_idx} fill {swap_count}"
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
            "Chain {chain_idx}: USDC conservation violated"
        );
        assert_eq!(
            total_fill + current_requested,
            initial_requested,
            "Chain {chain_idx}: ETH conservation violated"
        );
    }

    Ok(())
}
