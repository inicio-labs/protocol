use alloc::vec;

use miden_protocol::account::AccountId;
use miden_protocol::asset::{AssetAmount, AssetCallbackFlag, AssetId, FungibleAsset};
use miden_protocol::errors::NoteError;
use miden_protocol::note::{NoteRecipient, NoteStorage, NoteTag, NoteType};
use miden_protocol::{Felt, MAX_NOTE_STORAGE_ITEMS, Word};

use super::PswapNote;

/// Canonical storage representation for a PSWAP note.
///
/// Maps to the 7-element [`NoteStorage`] layout consumed by the on-chain MASM script:
///
/// | Slot | Field |
/// |---------|-------|
/// | `[0]` | Requested asset enable_callbacks flag |
/// | `[1]` | Requested asset faucet ID suffix |
/// | `[2]` | Requested asset faucet ID prefix |
/// | `[3]` | Requested asset amount |
/// | `[4]` | Payback note type (0 = private, 1 = public) |
/// | `[5-6]` | Creator account ID (prefix, suffix) |
///
/// Slots `[1, 2]` together form a 2-element [`AssetId`] view of the faucet (see
/// [`Self::requested_asset_id`]).
///
/// The payback note tag is derived at runtime from the creator account ID
/// (via `note_tag::create_account_target` in MASM) rather than stored.
///
/// The PSWAP note's own tag is not stored: it lives in the note's metadata and
/// is lifted from there by the on-chain script when a remainder note is created
/// (the asset pair is unchanged, so the tag carries over unchanged).
#[derive(Debug, Clone, PartialEq, Eq, bon::Builder)]
pub struct PswapNoteStorage {
    requested_asset: FungibleAsset,

    creator_account_id: AccountId,

    /// Note type of the payback note produced when the pswap is filled. Defaults to
    /// [`NoteType::Private`] because the payback carries the fill asset and is typically
    /// consumed directly by the creator.
    #[builder(default = NoteType::Private)]
    payback_note_type: NoteType,
}

impl PswapNoteStorage {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items for the PSWAP note.
    pub const NUM_STORAGE_ITEMS: usize = 7;

    /// Consumes the storage and returns a PSWAP [`NoteRecipient`] with the provided serial number.
    ///
    /// # Errors
    ///
    /// Propagates the error from [`PswapNote::script`] (build-time invariant).
    pub fn into_recipient(self, serial_num: Word) -> Result<NoteRecipient, NoteError> {
        Ok(NoteRecipient::new(serial_num, PswapNote::script()?, NoteStorage::from(self)))
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns a reference to the requested [`FungibleAsset`].
    pub fn requested_asset(&self) -> &FungibleAsset {
        &self.requested_asset
    }

    /// Returns the payback note routing tag, derived from the creator's account ID.
    pub fn payback_note_tag(&self) -> NoteTag {
        NoteTag::with_account_target(self.creator_account_id)
    }

    /// Returns the account ID of the note creator.
    pub fn creator_account_id(&self) -> AccountId {
        self.creator_account_id
    }

    /// Returns the [`NoteType`] used when creating the payback note.
    pub fn payback_note_type(&self) -> NoteType {
        self.payback_note_type
    }

    /// Returns the faucet ID of the requested asset.
    pub fn requested_faucet_id(&self) -> AccountId {
        self.requested_asset.faucet_id()
    }

    /// Returns the requested faucet's two-felt identity as an [`AssetId`].
    pub fn requested_asset_id(&self) -> AssetId {
        AssetId::new(
            self.requested_asset.faucet_id().suffix(),
            self.requested_asset.faucet_id().prefix().as_felt(),
        )
    }

    /// Returns the requested token amount.
    pub fn requested_asset_amount(&self) -> AssetAmount {
        self.requested_asset.amount()
    }
}

/// Compile-time proof that the PSWAP storage layout (7 items) always fits within the protocol's
/// per-note storage cap, so the `NoteStorage::new` call below is unreachable on its error path.
const _: () = assert!(PswapNoteStorage::NUM_STORAGE_ITEMS <= MAX_NOTE_STORAGE_ITEMS);

/// Serializes [`PswapNoteStorage`] into a 7-element [`NoteStorage`].
impl From<PswapNoteStorage> for NoteStorage {
    fn from(storage: PswapNoteStorage) -> Self {
        let storage_items = vec![
            // Requested asset (individual felts) [0-3]
            Felt::from(storage.requested_asset.callbacks().as_u8()),
            storage.requested_asset.faucet_id().suffix(),
            storage.requested_asset.faucet_id().prefix().as_felt(),
            Felt::from(storage.requested_asset.amount()),
            // Payback note type [4]
            Felt::from(storage.payback_note_type.as_u8()),
            // Creator ID [5-6]
            storage.creator_account_id.prefix().as_felt(),
            storage.creator_account_id.suffix(),
        ];
        // SAFETY: `NoteStorage::new` only fails when its input exceeds
        // `MAX_NOTE_STORAGE_ITEMS`. We always pass exactly `NUM_STORAGE_ITEMS = 7` items, and
        // the `const _: () = assert!` above proves at compile time that
        // `NUM_STORAGE_ITEMS <= MAX_NOTE_STORAGE_ITEMS`, so this branch is unreachable.
        NoteStorage::new(storage_items).unwrap_or_else(|_| unreachable!())
    }
}

/// Deserializes [`PswapNoteStorage`] from a slice of exactly 7 [`Felt`]s.
impl TryFrom<&[Felt]> for PswapNoteStorage {
    type Error = NoteError;

    fn try_from(note_storage: &[Felt]) -> Result<Self, Self::Error> {
        if note_storage.len() != Self::NUM_STORAGE_ITEMS {
            return Err(NoteError::InvalidNoteStorageLength {
                expected: Self::NUM_STORAGE_ITEMS,
                actual: note_storage.len(),
            });
        }

        // Reconstruct requested asset from individual felts:
        // [0] = enable_callbacks, [1] = faucet_id_suffix, [2] = faucet_id_prefix, [3] = amount
        let callbacks = AssetCallbackFlag::try_from(
            u8::try_from(note_storage[0].as_canonical_u64())
                .map_err(|_| NoteError::other("enable_callbacks exceeds u8"))?,
        )
        .map_err(|e| NoteError::other_with_source("failed to parse asset callback flag", e))?;

        let faucet_id = AccountId::try_from_elements(note_storage[1], note_storage[2])
            .map_err(|e| NoteError::other_with_source("failed to parse requested faucet ID", e))?;

        let amount = note_storage[3].as_canonical_u64();
        let requested_asset = FungibleAsset::new(faucet_id, amount)
            .map_err(|e| NoteError::other_with_source("failed to create requested asset", e))?
            .with_callbacks(callbacks);

        // [4] = payback_note_type
        let payback_note_type = NoteType::try_from(
            u8::try_from(note_storage[4].as_canonical_u64())
                .map_err(|_| NoteError::other("payback_note_type exceeds u8"))?,
        )
        .map_err(|e| NoteError::other_with_source("failed to parse payback note type", e))?;

        // [5-6] = creator account ID (prefix, suffix)
        let creator_account_id = AccountId::try_from_elements(note_storage[6], note_storage[5])
            .map_err(|e| NoteError::other_with_source("failed to parse creator account ID", e))?;

        Ok(Self {
            requested_asset,
            creator_account_id,
            payback_note_type,
        })
    }
}
