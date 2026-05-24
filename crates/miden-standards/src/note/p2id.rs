use alloc::vec::Vec;

use miden_protocol::account::AccountId;
use miden_protocol::assembly::Path;
use miden_protocol::asset::Asset;
use miden_protocol::crypto::rand::FeltRng;
use miden_protocol::errors::NoteError;
use miden_protocol::note::{
    Note,
    NoteAssets,
    NoteAttachment,
    NoteAttachments,
    NoteRecipient,
    NoteScript,
    NoteScriptRoot,
    NoteStorage,
    NoteTag,
    NoteType,
    PartialNoteMetadata,
};
use miden_protocol::utils::sync::LazyLock;
use miden_protocol::{Felt, Word};

use crate::StandardsLib;

// NOTE SCRIPT
// ================================================================================================

/// Path to the P2ID note script procedure in the standards library.
const P2ID_SCRIPT_PATH: &str = "::miden::standards::notes::p2id::main";

// Initialize the P2ID note script only once
static P2ID_SCRIPT: LazyLock<NoteScript> = LazyLock::new(|| {
    let standards_lib = StandardsLib::default();
    let path = Path::new(P2ID_SCRIPT_PATH);
    NoteScript::from_library_reference(standards_lib.as_ref(), path)
        .expect("Standards library contains P2ID note script procedure")
});

/// The root hash of the P2ID note script.
pub static SCRIPT_ROOT: LazyLock<NoteScriptRoot> = LazyLock::new(|| P2ID_SCRIPT.root());

// P2ID NOTE
// ================================================================================================

/// A Pay-to-ID note: transfers `assets` from `sender` to `target`. Only `target` can consume
/// the note and claim its assets.
///
/// Construct via [`Self::builder`]. Convert into a protocol [`Note`] via `Note::from(_)`.
#[derive(Debug, Clone)]
pub struct P2idNote {
    sender: AccountId,
    storage: P2idNoteStorage,
    serial_number: Word,
    note_type: NoteType,
    assets: NoteAssets,
    attachments: NoteAttachments,
}

#[bon::bon]
impl P2idNote {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items of the P2ID note.
    pub const NUM_STORAGE_ITEMS: usize = P2idNoteStorage::NUM_ITEMS;

    // BUILDER
    // --------------------------------------------------------------------------------------------

    /// Builds a new [`P2idNote`].
    ///
    /// Use `.asset()` and `.attachment()` to add items individually, `.target()` to set the
    /// recipient, and `.generate_serial_number()` to draw the serial from an RNG.
    ///
    /// # Errors
    ///
    /// Returns an error if `assets` or `attachments` exceed their protocol limits (see
    /// [`NoteAssets::new`] and [`NoteAttachments::new`]).
    #[builder]
    pub fn new(
        #[builder(field)] assets: Vec<Asset>,
        #[builder(field)] attachments: Vec<NoteAttachment>,
        sender: AccountId,
        #[builder(name = target, with = |target: AccountId| P2idNoteStorage::new(target))]
        storage: P2idNoteStorage,
        serial_number: Word,
        #[builder(default = NoteType::Private)] note_type: NoteType,
    ) -> Result<Self, NoteError> {
        let assets = NoteAssets::new(assets)?;
        let attachments = NoteAttachments::new(attachments)?;

        Ok(Self {
            sender,
            storage,
            serial_number,
            note_type,
            assets,
            attachments,
        })
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the compiled P2ID note script.
    pub fn script() -> NoteScript {
        P2ID_SCRIPT.clone()
    }

    /// Returns the P2ID note script root.
    pub fn script_root() -> NoteScriptRoot {
        P2ID_SCRIPT.root()
    }

    /// Returns the account ID of the note sender.
    pub fn sender(&self) -> AccountId {
        self.sender
    }

    /// Returns the P2ID note's storage.
    pub fn storage(&self) -> &P2idNoteStorage {
        &self.storage
    }

    /// Returns the target account ID.
    pub fn target(&self) -> AccountId {
        self.storage.target()
    }

    /// Returns the serial number of this note.
    pub fn serial_number(&self) -> Word {
        self.serial_number
    }

    /// Returns the note type (public or private).
    pub fn note_type(&self) -> NoteType {
        self.note_type
    }

    /// Returns the assets carried by this note.
    pub fn assets(&self) -> &NoteAssets {
        &self.assets
    }

    /// Returns the attachments carried by this note.
    pub fn attachments(&self) -> &NoteAttachments {
        &self.attachments
    }
}

// BUILDER EXTENSIONS
// ================================================================================================

impl<S: p2id_note_builder::State> P2idNoteBuilder<S> {
    /// Appends an asset to the note's assets.
    pub fn asset(mut self, asset: impl Into<Asset>) -> Self {
        self.assets.push(asset.into());
        self
    }

    /// Appends an attachment to the note's attachments.
    pub fn attachment(mut self, attachment: impl Into<NoteAttachment>) -> Self {
        self.attachments.push(attachment.into());
        self
    }
}

impl<S: p2id_note_builder::State> P2idNoteBuilder<S>
where
    S::SerialNumber: p2id_note_builder::IsUnset,
{
    /// Draws a serial number from `rng` and sets it on the builder.
    pub fn generate_serial_number(
        self,
        rng: &mut impl FeltRng,
    ) -> P2idNoteBuilder<p2id_note_builder::SetSerialNumber<S>> {
        self.serial_number(rng.draw_word())
    }
}

// CONVERSIONS
// ================================================================================================

/// Converts a [`P2idNote`] into the protocol [`Note`].
impl From<P2idNote> for Note {
    fn from(note: P2idNote) -> Self {
        let recipient = note.storage.into_recipient(note.serial_number);
        let tag = NoteTag::with_account_target(note.storage.target());
        let metadata = PartialNoteMetadata::new(note.sender, note.note_type).with_tag(tag);
        Note::with_attachments(note.assets, metadata, recipient, note.attachments)
    }
}

/// Parses a protocol [`Note`] back into a [`P2idNote`]. Fails if the note's script root is not
/// the P2ID script root.
impl TryFrom<&Note> for P2idNote {
    type Error = NoteError;

    fn try_from(note: &Note) -> Result<Self, Self::Error> {
        if note.recipient().script().root() != P2idNote::script_root() {
            return Err(NoteError::other("note script root does not match P2ID script root"));
        }

        let storage = P2idNoteStorage::try_from(note.recipient().storage().items())?;

        Ok(Self {
            sender: note.metadata().sender(),
            storage,
            serial_number: note.recipient().serial_num(),
            note_type: note.metadata().note_type(),
            assets: note.assets().clone(),
            attachments: note.attachments().clone(),
        })
    }
}

// P2ID NOTE STORAGE
// ================================================================================================

/// Canonical storage representation for a P2ID note.
///
/// Contains the identifier of the target account that is authorized
/// to consume the note. Only the account matching this ID can execute
/// the note and claim its assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct P2idNoteStorage {
    target: AccountId,
}

impl P2idNoteStorage {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items of the P2ID note.
    pub const NUM_ITEMS: usize = 2;

    /// Creates new P2ID note storage targeting the given account.
    pub fn new(target: AccountId) -> Self {
        Self { target }
    }

    /// Consumes the storage and returns a P2ID [`NoteRecipient`] with the provided serial number.
    ///
    /// Notes created with this recipient will be P2ID notes consumable by the specified target
    /// account stored in this [`P2idNoteStorage`].
    pub fn into_recipient(self, serial_num: Word) -> NoteRecipient {
        NoteRecipient::new(serial_num, P2idNote::script(), NoteStorage::from(self))
    }

    /// Returns the target account ID.
    pub fn target(&self) -> AccountId {
        self.target
    }
}

impl From<P2idNoteStorage> for NoteStorage {
    fn from(storage: P2idNoteStorage) -> Self {
        // Storage layout:
        // [ account_id_suffix, account_id_prefix ]
        NoteStorage::new(vec![storage.target.suffix(), storage.target.prefix().as_felt()])
            .expect("number of storage items should not exceed max storage items")
    }
}

impl TryFrom<&[Felt]> for P2idNoteStorage {
    type Error = NoteError;

    fn try_from(note_storage: &[Felt]) -> Result<Self, Self::Error> {
        if note_storage.len() != P2idNote::NUM_STORAGE_ITEMS {
            return Err(NoteError::InvalidNoteStorageLength {
                expected: P2idNote::NUM_STORAGE_ITEMS,
                actual: note_storage.len(),
            });
        }

        let target = AccountId::try_from_elements(note_storage[0], note_storage[1])
            .map_err(|err| NoteError::other_with_source("failed to create account id", err))?;

        Ok(Self { target })
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use miden_protocol::Felt;
    use miden_protocol::account::{AccountId, AccountIdVersion, AccountType};
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::errors::NoteError;

    use super::*;

    // STORAGE TESTS
    // --------------------------------------------------------------------------------------------

    #[test]
    fn try_from_valid_storage_succeeds() {
        let target = AccountId::dummy([1u8; 15], AccountIdVersion::Version1, AccountType::Private);

        let storage = vec![target.suffix(), target.prefix().as_felt()];

        let parsed =
            P2idNoteStorage::try_from(storage.as_slice()).expect("storage should be valid");

        assert_eq!(parsed.target(), target);
    }

    #[test]
    fn try_from_invalid_length_returns_error() {
        let storage = vec![Felt::ZERO];

        let err = P2idNoteStorage::try_from(storage.as_slice())
            .expect_err("should fail due to invalid length");

        assert!(matches!(
            err,
            NoteError::InvalidNoteStorageLength {
                expected: P2idNote::NUM_STORAGE_ITEMS,
                actual: 1
            }
        ));
    }

    #[test]
    fn try_from_invalid_storage_contents_returns_error() {
        let storage = vec![Felt::new_unchecked(999_u64), Felt::new_unchecked(888_u64)];

        let err = P2idNoteStorage::try_from(storage.as_slice())
            .expect_err("should fail due to invalid account id encoding");

        assert!(matches!(err, NoteError::Other { source: Some(_), .. }));
    }

    // BUILDER TESTS
    // --------------------------------------------------------------------------------------------

    fn dummy_sender() -> AccountId {
        AccountId::dummy([1u8; 15], AccountIdVersion::Version1, AccountType::Private)
    }

    fn dummy_target() -> AccountId {
        AccountId::dummy([2u8; 15], AccountIdVersion::Version1, AccountType::Private)
    }

    fn dummy_faucet_a() -> AccountId {
        AccountId::dummy([3u8; 15], AccountIdVersion::Version1, AccountType::Public)
    }

    fn dummy_faucet_b() -> AccountId {
        AccountId::dummy([4u8; 15], AccountIdVersion::Version1, AccountType::Public)
    }

    /// Minimal builder — only required fields, defaults for the rest.
    #[test]
    fn builder_minimal() {
        let note = P2idNote::builder()
            .sender(dummy_sender())
            .target(dummy_target())
            .serial_number(Word::default())
            .build()
            .unwrap();

        assert_eq!(note.sender(), dummy_sender());
        assert_eq!(note.target(), dummy_target());
        assert_eq!(note.note_type(), NoteType::Private); // default
        assert_eq!(note.assets().num_assets(), 0);
        assert_eq!(note.attachments().num_attachments(), 0);
    }

    /// `.asset()` accumulates across multiple calls (one entry per distinct faucet).
    #[test]
    fn builder_asset_accumulates() {
        let a = FungibleAsset::new(dummy_faucet_a(), 100).unwrap();
        let b = FungibleAsset::new(dummy_faucet_b(), 200).unwrap();

        let note = P2idNote::builder()
            .sender(dummy_sender())
            .target(dummy_target())
            .serial_number(Word::default())
            .asset(a)
            .asset(b)
            .build()
            .unwrap();

        assert_eq!(note.assets().num_assets(), 2);
    }

    /// Two assets from the same faucet are rejected by `NoteAssets::new`.
    #[test]
    fn builder_rejects_duplicate_faucet() {
        let a = FungibleAsset::new(dummy_faucet_a(), 100).unwrap();
        let b = FungibleAsset::new(dummy_faucet_a(), 200).unwrap();

        let err = P2idNote::builder()
            .sender(dummy_sender())
            .target(dummy_target())
            .serial_number(Word::default())
            .asset(a)
            .asset(b)
            .build()
            .expect_err("duplicate faucet should be rejected");

        assert!(matches!(err, NoteError::DuplicateFungibleAsset(_)));
    }

    /// `.generate_serial_number(rng)` draws from RNG and sets the field.
    #[test]
    fn builder_generate_serial_number() {
        let mut rng = RandomCoin::new(Word::default());
        let note = P2idNote::builder()
            .sender(dummy_sender())
            .target(dummy_target())
            .generate_serial_number(&mut rng)
            .build()
            .unwrap();

        assert_ne!(note.serial_number(), Word::default());
    }

    /// `SCRIPT_ROOT` static must yield the same value as `script_root()`.
    #[test]
    fn script_root_static_matches_accessor() {
        assert_eq!(*SCRIPT_ROOT, P2idNote::script_root());
    }

    /// `From<P2idNote> for Note` is infallible; round-trips through `TryFrom<&Note>`.
    #[test]
    fn from_p2id_roundtrips_via_try_from() {
        let original = P2idNote::builder()
            .sender(dummy_sender())
            .target(dummy_target())
            .serial_number(Word::default())
            .asset(FungibleAsset::new(dummy_faucet_a(), 42).unwrap())
            .note_type(NoteType::Public)
            .build()
            .unwrap();

        let note: Note = original.clone().into();
        let parsed = P2idNote::try_from(&note).expect("roundtrip should succeed");

        assert_eq!(parsed.sender(), original.sender());
        assert_eq!(parsed.target(), original.target());
        assert_eq!(parsed.serial_number(), original.serial_number());
        assert_eq!(parsed.note_type(), original.note_type());
        assert_eq!(parsed.assets().num_assets(), original.assets().num_assets());
    }
}
