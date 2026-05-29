use miden_protocol::account::{AccountId, AccountIdVersion, AccountType};
use miden_protocol::asset::{AssetCallbackFlag, AssetId, FungibleAsset};
use miden_protocol::crypto::rand::{FeltRng, RandomCoin};
use miden_protocol::note::NoteStorage;

use super::*;

// TEST HELPERS
// --------------------------------------------------------------------------------------------

fn dummy_faucet_id(byte: u8) -> AccountId {
    let mut bytes = [0; 15];
    bytes[0] = byte;
    AccountId::dummy(bytes, AccountIdVersion::Version1, AccountType::Public)
}

fn dummy_creator_id() -> AccountId {
    AccountId::dummy([1; 15], AccountIdVersion::Version1, AccountType::Public)
}

fn dummy_consumer_id() -> AccountId {
    AccountId::dummy([2; 15], AccountIdVersion::Version1, AccountType::Public)
}

fn build_pswap_note(
    offered_asset: FungibleAsset,
    requested_asset: FungibleAsset,
    creator_id: AccountId,
) -> (PswapNote, Note) {
    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(creator_id)
        .build();
    let pswap = PswapNote::builder()
        .sender(creator_id)
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(offered_asset)
        .build()
        .unwrap();
    let note: Note = pswap.clone().into();
    (pswap, note)
}

// TESTS
// --------------------------------------------------------------------------------------------

#[test]
fn pswap_note_creation_and_script() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 1000).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 500).unwrap();

    let (pswap, note) = build_pswap_note(offered_asset, requested_asset, creator_id);

    assert_eq!(pswap.sender(), creator_id);
    assert_eq!(pswap.note_type(), NoteType::Public);

    let script = PswapNote::script();
    assert!(Word::from(script.root()) != Word::default(), "Script root should not be zero");
    assert_eq!(note.metadata().sender(), creator_id);
    assert_eq!(note.metadata().note_type(), NoteType::Public);
    assert_eq!(note.assets().num_assets(), 1);
    assert_eq!(note.recipient().script().root(), script.root());
    assert_eq!(
        note.recipient().storage().num_items(),
        PswapNoteStorage::NUM_STORAGE_ITEMS as u16,
    );
}

#[test]
fn pswap_note_builder() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 1000).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 500).unwrap();

    let (pswap, note) = build_pswap_note(offered_asset, requested_asset, creator_id);

    assert_eq!(pswap.sender(), creator_id);
    assert_eq!(pswap.note_type(), NoteType::Public);
    assert_eq!(note.metadata().sender(), creator_id);
    assert_eq!(note.metadata().note_type(), NoteType::Public);
    assert_eq!(note.assets().num_assets(), 1);
    assert_eq!(
        note.recipient().storage().num_items(),
        PswapNoteStorage::NUM_STORAGE_ITEMS as u16,
    );
}

#[test]
fn pswap_tag() {
    let mut offered_faucet_bytes = [0; 15];
    offered_faucet_bytes[0] = 0xcd;
    offered_faucet_bytes[1] = 0xb1;

    let mut requested_faucet_bytes = [0; 15];
    requested_faucet_bytes[0] = 0xab;
    requested_faucet_bytes[1] = 0xec;

    let offered_asset = FungibleAsset::new(
        AccountId::dummy(offered_faucet_bytes, AccountIdVersion::Version1, AccountType::Public),
        100,
    )
    .unwrap();
    let requested_asset = FungibleAsset::new(
        AccountId::dummy(
            requested_faucet_bytes,
            AccountIdVersion::Version1,
            AccountType::Public,
        ),
        200,
    )
    .unwrap();

    let tag = PswapNote::create_tag(NoteType::Public, &offered_asset, &requested_asset);
    let tag_u32 = u32::from(tag);

    // Verify note_type bits (top 2 bits should be 10 for Public)
    let note_type_bits = tag_u32 >> 30;
    assert_eq!(note_type_bits, NoteType::Public as u32);
}

#[test]
fn calculate_output_amount() {
    assert_eq!(PswapNote::calculate_output_amount(100, 100, 50).unwrap(), 50); // Equal ratio
    assert_eq!(PswapNote::calculate_output_amount(200, 100, 50).unwrap(), 100); // 2:1 ratio
    assert_eq!(PswapNote::calculate_output_amount(100, 200, 50).unwrap(), 25); // 1:2 ratio

    // Non-integer ratio (100/73)
    let result = PswapNote::calculate_output_amount(100, 73, 7).unwrap();
    assert!(result > 0, "Should produce non-zero output");
}

#[test]
fn pswap_note_storage_try_from() {
    let creator_id = dummy_creator_id();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 500).unwrap();

    let storage_items = vec![
        Felt::from(requested_asset.callbacks().as_u8()),
        requested_asset.faucet_id().suffix(),
        requested_asset.faucet_id().prefix().as_felt(),
        Felt::from(requested_asset.amount()),
        Felt::from(NoteType::Private.as_u8()), // payback_note_type
        creator_id.prefix().as_felt(),
        creator_id.suffix(),
    ];

    let parsed = PswapNoteStorage::try_from(storage_items.as_slice()).unwrap();
    assert_eq!(parsed.creator_account_id(), creator_id);
    assert_eq!(parsed.requested_asset_amount(), AssetAmount::new(500).unwrap());
}

#[test]
fn pswap_note_storage_roundtrip() {
    let creator_id = dummy_creator_id();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 500).unwrap();

    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(creator_id)
        .build();

    let note_storage = NoteStorage::from(storage.clone());
    let parsed = PswapNoteStorage::try_from(note_storage.items()).unwrap();

    assert_eq!(parsed.creator_account_id(), creator_id);
    assert_eq!(parsed.requested_asset_amount(), AssetAmount::new(500).unwrap());
}

/// Consumer supplies both an account fill and a note fill, and the sum is below
/// the requested amount → `execute` must combine them into a single payback note
/// carrying account_fill+note_fill of the requested asset and emit a remainder
/// pswap note for the unfilled portion.
#[test]
fn pswap_execute_combined_account_fill_and_note_fill_partial_fill() {
    let creator_id = dummy_creator_id();
    let consumer_id = dummy_consumer_id();
    let offered_faucet = dummy_faucet_id(0xaa);
    let requested_faucet = dummy_faucet_id(0xbb);

    // Offer 100 offered, request 50 requested → 2:1 ratio.
    let offered_asset = FungibleAsset::new(offered_faucet, 100).unwrap();
    let requested_asset = FungibleAsset::new(requested_faucet, 50).unwrap();
    let (pswap, _) = build_pswap_note(offered_asset, requested_asset, creator_id);

    // Account fill = 10, note fill = 20 → total fill = 30 (< 50, so partial).
    let account_fill = FungibleAsset::new(requested_faucet, 10).unwrap();
    let note_fill = FungibleAsset::new(requested_faucet, 20).unwrap();

    let (payback, remainder) =
        pswap.execute(consumer_id, Some(account_fill), Some(note_fill)).unwrap();

    // Payback note must carry the combined 30 of requested asset.
    assert_eq!(payback.assets().num_assets(), 1);
    let payback_asset = payback.assets().iter().next().unwrap();
    let Asset::Fungible(fa) = payback_asset else {
        panic!("expected fungible payback asset");
    };
    assert_eq!(fa.faucet_id(), requested_faucet);
    assert_eq!(fa.amount().as_u64(), 30);

    // Remainder must exist with the unfilled 50 - 30 = 20 of requested, and the
    // offered amount reduced proportionally (100 - 30*2 = 40).
    let remainder = remainder.expect("partial fill should produce remainder");
    assert_eq!(remainder.storage().requested_asset_amount(), AssetAmount::new(20).unwrap());
    assert_eq!(remainder.offered_asset().amount().as_u64(), 40);
    assert_eq!(remainder.storage().creator_account_id(), creator_id);
}

/// Consumer supplies both an account fill and a note fill, and the sum exactly
/// matches the requested amount → `execute` must produce a single payback note for
/// the full amount and no remainder.
#[test]
fn pswap_execute_combined_account_fill_and_note_fill_full_fill() {
    let creator_id = dummy_creator_id();
    let consumer_id = dummy_consumer_id();
    let offered_faucet = dummy_faucet_id(0xaa);
    let requested_faucet = dummy_faucet_id(0xbb);

    let offered_asset = FungibleAsset::new(offered_faucet, 100).unwrap();
    let requested_asset = FungibleAsset::new(requested_faucet, 50).unwrap();
    let (pswap, _) = build_pswap_note(offered_asset, requested_asset, creator_id);

    // Account fill = 30, note fill = 20 → total fill = 50 (exactly requested).
    let account_fill = FungibleAsset::new(requested_faucet, 30).unwrap();
    let note_fill = FungibleAsset::new(requested_faucet, 20).unwrap();

    let (payback, remainder) =
        pswap.execute(consumer_id, Some(account_fill), Some(note_fill)).unwrap();

    // Payback note must carry the full 50 of requested asset.
    assert_eq!(payback.assets().num_assets(), 1);
    let payback_asset = payback.assets().iter().next().unwrap();
    let Asset::Fungible(fa) = payback_asset else {
        panic!("expected fungible payback asset");
    };
    assert_eq!(fa.faucet_id(), requested_faucet);
    assert_eq!(fa.amount().as_u64(), 50);

    // Full fill → no remainder note.
    assert!(remainder.is_none(), "full fill must not produce a remainder");
}

/// Regression for the silent `AssetCallbackFlag` drop: when the PSWAP's requested or
/// offered asset carries `Enabled` callbacks, the on-chain MASM preserves that flag
/// on every output note's asset. The Rust-side `execute`, `payback_note`, and
/// `remainder_note` must do the same — otherwise the reconstructed `Note::details_commitment`
/// diverges from the on-chain leaf and the unauthenticated consume path fails.
#[test]
fn pswap_output_assets_preserve_callback_flag() {
    let creator_id = dummy_creator_id();
    let consumer_id = dummy_consumer_id();
    let offered_faucet = dummy_faucet_id(0xaa);
    let requested_faucet = dummy_faucet_id(0xbb);

    let offered_asset = FungibleAsset::new(offered_faucet, 100)
        .unwrap()
        .with_callbacks(AssetCallbackFlag::Enabled);
    let requested_asset = FungibleAsset::new(requested_faucet, 50)
        .unwrap()
        .with_callbacks(AssetCallbackFlag::Enabled);
    let (pswap, _) = build_pswap_note(offered_asset, requested_asset, creator_id);

    // --- execute() (partial fill) ---
    let account_fill = FungibleAsset::new(requested_faucet, 20)
        .unwrap()
        .with_callbacks(AssetCallbackFlag::Enabled);
    let (payback, remainder) = pswap.execute(consumer_id, Some(account_fill), None).unwrap();

    let Asset::Fungible(fa) = payback.assets().iter().next().unwrap() else {
        panic!("expected fungible payback asset");
    };
    assert_eq!(fa.callbacks(), AssetCallbackFlag::Enabled);

    let remainder = remainder.expect("partial fill should produce remainder");
    assert_eq!(
        remainder.offered_asset().callbacks(),
        AssetCallbackFlag::Enabled,
        "remainder offered asset must inherit callbacks",
    );
    assert_eq!(
        remainder.storage().requested_asset().callbacks(),
        AssetCallbackFlag::Enabled,
        "remainder storage's requested asset must inherit callbacks",
    );

    // --- payback_note() reconstruction ---
    let depth_one = NonZeroU32::new(1).unwrap();
    let payback_attachment =
        PswapNoteAttachment::new(AssetAmount::new(20).unwrap(), pswap.order_id(), depth_one);
    let reconstructed_payback = pswap.payback_note(consumer_id, &payback_attachment).unwrap();
    let Asset::Fungible(fa) = reconstructed_payback.assets().iter().next().unwrap() else {
        panic!("expected fungible payback asset");
    };
    assert_eq!(
        fa.callbacks(),
        AssetCallbackFlag::Enabled,
        "payback_note must preserve requested asset's callback flag",
    );

    // --- remainder_note() reconstruction ---
    let remainder_attachment =
        PswapNoteAttachment::new(AssetAmount::new(40).unwrap(), pswap.order_id(), depth_one);
    let reconstructed_remainder = pswap
        .remainder_note(
            consumer_id,
            &remainder_attachment,
            AssetAmount::new(60).unwrap(),
            AssetAmount::new(30).unwrap(),
        )
        .unwrap();
    let Asset::Fungible(fa) = reconstructed_remainder.assets().iter().next().unwrap() else {
        panic!("expected fungible remainder asset");
    };
    assert_eq!(
        fa.callbacks(),
        AssetCallbackFlag::Enabled,
        "remainder_note must preserve offered asset's callback flag",
    );
}

/// `create_args` is infallible at the type level because `AssetAmount` fits in a `Felt`.
/// `MAX` round-trips through the resulting word.
#[test]
fn create_args_round_trips_max_asset_amount() {
    let args = PswapNote::create_args(AssetAmount::MAX, AssetAmount::ZERO);
    assert_eq!(args[0], Felt::from(AssetAmount::MAX));
    assert_eq!(args[1], ZERO);
    assert_eq!(args[2], ZERO);
    assert_eq!(args[3], ZERO);
}

/// `PswapNoteAttachment` accessors mirror what was passed to `new`.
#[test]
fn pswap_note_attachment_accessors() {
    let order_id = OrderId::from(Felt::from(42u32));
    let depth = NonZeroU32::new(3).unwrap();
    let attachment = PswapNoteAttachment::new(AssetAmount::new(100).unwrap(), order_id, depth);
    assert_eq!(attachment.amount(), AssetAmount::new(100).unwrap());
    assert_eq!(attachment.order_id(), order_id);
    assert_eq!(attachment.depth(), depth);
}

/// `From<PswapNoteAttachment> for NoteAttachment` encodes the depth via `.get()`.
#[test]
fn pswap_note_attachment_encodes_depth_via_get() {
    let depth = NonZeroU32::new(7).unwrap();
    let attachment = PswapNoteAttachment::new(
        AssetAmount::new(50).unwrap(),
        OrderId::from(Felt::from(9u32)),
        depth,
    );
    let note_att: NoteAttachment = attachment.into();
    let word = note_att.content().as_words()[0];
    assert_eq!(word[2], Felt::from(7u32));
}

/// `parent_depth` returns 0 when the note has no attachment.
#[test]
fn parent_depth_zero_when_no_attachment() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 100).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 50).unwrap();
    let (pswap, _) = build_pswap_note(offered_asset, requested_asset, creator_id);
    assert_eq!(pswap.parent_depth(), 0);
    assert_eq!(pswap.next_depth().unwrap().get(), 1);
}

/// `parent_depth` returns the stored depth when the note carries a PSWAP attachment.
#[test]
fn parent_depth_reads_attachment_depth() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 100).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 50).unwrap();
    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(creator_id)
        .build();
    let order_id = OrderId::from(Felt::from(1u32));
    let attachment = PswapNoteAttachment::new(
        AssetAmount::new(10).unwrap(),
        order_id,
        NonZeroU32::new(4).unwrap(),
    );
    let pswap = PswapNote::builder()
        .sender(creator_id)
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(offered_asset)
        .attachment(attachment.into())
        .build()
        .unwrap();
    assert_eq!(pswap.parent_depth(), 4);
    assert_eq!(pswap.next_depth().unwrap().get(), 5);
}

/// `parent_depth` falls back to 0 when the attachment encodes a depth outside `u32` range
/// (treated as corrupted by external construction; downstream arithmetic is left
/// well-defined).
#[test]
fn parent_depth_zero_on_out_of_range_attachment() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 100).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 50).unwrap();
    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(creator_id)
        .build();
    // Stamp a raw word with a depth exceeding u32::MAX.
    let oversized_depth = Felt::try_from(u64::from(u32::MAX) + 1).unwrap();
    let word = Word::from([Felt::from(1u32), Felt::from(1u32), oversized_depth, ZERO]);
    let raw_attachment =
        NoteAttachment::with_word(PswapNote::PSWAP_ATTACHMENT_SCHEME, word);
    let pswap = PswapNote::builder()
        .sender(creator_id)
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(offered_asset)
        .attachment(raw_attachment)
        .build()
        .unwrap();
    assert_eq!(pswap.parent_depth(), 0);
}

/// `TryFrom<&NoteAttachment>` rejects a wrong scheme.
#[test]
fn try_from_rejects_wrong_scheme() {
    let word = Word::from([Felt::from(1u32), Felt::from(2u32), Felt::from(1u32), ZERO]);
    // Use NetworkAccountTarget (scheme = 2) instead of PSWAP_ATTACHMENT_SCHEME (3).
    let other = NoteAttachment::with_word(StandardNoteAttachment::NetworkAccountTarget.attachment_scheme(), word);
    assert!(PswapNoteAttachment::try_from(&other).is_err());
}

/// `TryFrom<&NoteAttachment>` rejects depth == 0 (the invariant only one path enforces).
#[test]
fn try_from_rejects_zero_depth() {
    let word = Word::from([Felt::from(1u32), Felt::from(2u32), ZERO, ZERO]);
    let att = NoteAttachment::with_word(PswapNote::PSWAP_ATTACHMENT_SCHEME, word);
    assert!(PswapNoteAttachment::try_from(&att).is_err());
}

/// `TryFrom<&NoteAttachment>` rejects a depth value that exceeds `u32::MAX`.
#[test]
fn try_from_rejects_out_of_range_depth() {
    let oversized = Felt::try_from(u64::from(u32::MAX) + 1).unwrap();
    let word = Word::from([Felt::from(1u32), Felt::from(2u32), oversized, ZERO]);
    let att = NoteAttachment::with_word(PswapNote::PSWAP_ATTACHMENT_SCHEME, word);
    assert!(PswapNoteAttachment::try_from(&att).is_err());
}

/// `TryFrom<&NoteAttachment>` rejects an amount that exceeds `AssetAmount::MAX`.
#[test]
fn try_from_rejects_invalid_amount() {
    // 2^63 > AssetAmount::MAX = 2^63 - 2^31.
    let bad_amount = Felt::try_from(1u64 << 63).unwrap();
    let word = Word::from([bad_amount, Felt::from(2u32), Felt::from(1u32), ZERO]);
    let att = NoteAttachment::with_word(PswapNote::PSWAP_ATTACHMENT_SCHEME, word);
    assert!(PswapNoteAttachment::try_from(&att).is_err());
}

/// `TryFrom<&NoteAttachment>` rejects an attachment whose word count is not 1 (covering the
/// MASM `num_words == 1` assert from the Rust side).
#[test]
fn try_from_rejects_wrong_num_words() {
    let words = vec![Word::default(), Word::default()];
    let multi = NoteAttachment::with_words(PswapNote::PSWAP_ATTACHMENT_SCHEME, words).unwrap();
    assert!(PswapNoteAttachment::try_from(&multi).is_err());
}

/// `TryFrom<&NoteAttachment>` round-trips a valid attachment.
#[test]
fn try_from_round_trips_valid_attachment() {
    let original = PswapNoteAttachment::new(
        AssetAmount::new(123).unwrap(),
        OrderId::from(Felt::from(7u32)),
        NonZeroU32::new(5).unwrap(),
    );
    let encoded: NoteAttachment = original.into();
    let decoded = PswapNoteAttachment::try_from(&encoded).unwrap();
    assert_eq!(decoded, original);
}

/// `next_depth` propagates a `NoteError` if the parent depth is at `u32::MAX`.
#[test]
fn next_depth_errors_on_u32_overflow() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 100).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 50).unwrap();
    let mut rng = RandomCoin::new(Word::default());
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(creator_id)
        .build();
    let attachment = PswapNoteAttachment::new(
        AssetAmount::new(1).unwrap(),
        OrderId::from(Felt::from(1u32)),
        NonZeroU32::new(u32::MAX).unwrap(),
    );
    let pswap = PswapNote::builder()
        .sender(creator_id)
        .storage(storage)
        .serial_number(rng.draw_word())
        .note_type(NoteType::Public)
        .offered_asset(offered_asset)
        .attachment(attachment.into())
        .build()
        .unwrap();
    assert!(pswap.next_depth().is_err());
}

/// `OrderId` is a transparent newtype around `Felt`; both From conversions round-trip.
#[test]
fn order_id_round_trips_through_felt() {
    let felt = Felt::from(123u32);
    let oid = OrderId::from(felt);
    assert_eq!(oid.as_felt(), felt);
    assert_eq!(Felt::from(oid), felt);
}

/// `PswapNote::order_id` is `serial_number[1]` wrapped as [`OrderId`].
#[test]
fn pswap_note_order_id_equals_serial_1() {
    let creator_id = dummy_creator_id();
    let offered_asset = FungibleAsset::new(dummy_faucet_id(0xaa), 100).unwrap();
    let requested_asset = FungibleAsset::new(dummy_faucet_id(0xbb), 50).unwrap();
    let (pswap, _) = build_pswap_note(offered_asset, requested_asset, creator_id);
    assert_eq!(Felt::from(pswap.order_id()), pswap.serial_number()[1]);
}

/// `PswapNoteStorage::requested_asset_id` exposes the faucet ID as a 2-felt [`AssetId`].
#[test]
fn requested_asset_id_packs_faucet_id() {
    let creator_id = dummy_creator_id();
    let faucet_id = dummy_faucet_id(0xcc);
    let requested_asset = FungibleAsset::new(faucet_id, 42).unwrap();
    let storage = PswapNoteStorage::builder()
        .requested_asset(requested_asset)
        .creator_account_id(creator_id)
        .build();
    let expected = AssetId::new(faucet_id.suffix(), faucet_id.prefix().as_felt());
    assert_eq!(storage.requested_asset_id(), expected);
}
