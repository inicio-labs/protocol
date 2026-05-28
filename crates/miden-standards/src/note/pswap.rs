use alloc::vec;
use core::num::NonZeroU32;

use miden_protocol::account::AccountId;
use miden_protocol::assembly::Path;
use miden_protocol::asset::{Asset, AssetAmount, AssetCallbackFlag, FungibleAsset};
use miden_protocol::errors::NoteError;
use miden_protocol::note::{
    Note,
    NoteAssets,
    NoteAttachment,
    NoteAttachmentScheme,
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
use miden_protocol::{Felt, ONE, Word, ZERO};

use crate::StandardsLib;
use crate::note::{P2idNoteStorage, StandardNoteAttachment};

// NOTE SCRIPT
// ================================================================================================

/// Path to the PSWAP note script procedure in the standards library.
const PSWAP_SCRIPT_PATH: &str = "::miden::standards::notes::pswap::main";

// Initialize the PSWAP note script only once
static PSWAP_SCRIPT: LazyLock<NoteScript> = LazyLock::new(|| {
    let standards_lib = StandardsLib::default();
    let path = Path::new(PSWAP_SCRIPT_PATH);
    NoteScript::from_library_reference(standards_lib.as_ref(), path)
        .expect("Standards library contains PSWAP note script procedure")
});

// PSWAP NOTE STORAGE
// ================================================================================================

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
    /// consumed directly by the creator — a private note is cheaper in fees and bandwidth
    /// and offers the same information (the fill amount is already recorded in the
    /// executed transaction's output).
    #[builder(default = NoteType::Private)]
    payback_note_type: NoteType,
}

impl PswapNoteStorage {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items for the PSWAP note.
    pub const NUM_STORAGE_ITEMS: usize = 7;

    /// Consumes the storage and returns a PSWAP [`NoteRecipient`] with the provided serial number.
    pub fn into_recipient(self, serial_num: Word) -> NoteRecipient {
        NoteRecipient::new(serial_num, PswapNote::script(), NoteStorage::from(self))
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

    /// Returns the requested token amount.
    pub fn requested_asset_amount(&self) -> AssetAmount {
        self.requested_asset.amount()
    }
}

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
        NoteStorage::new(storage_items)
            .expect("number of storage items should not exceed max storage items")
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

// PSWAP NOTE ATTACHMENT
// ================================================================================================

/// Typed attachment carried by both PSWAP output notes, encoded as
/// `[amount, order_id, depth, 0]` under [`PswapNote::PSWAP_ATTACHMENT_SCHEME`].
///
/// `depth` is [`NonZeroU32`] because attachments are only stamped on payback / remainder notes
/// (depth >= 1); the original PSWAP has no PSWAP-scheme attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PswapNoteAttachment {
    amount: AssetAmount,
    order_id: Felt,
    depth: NonZeroU32,
}

impl PswapNoteAttachment {
    /// Creates a new [`PswapNoteAttachment`]. Infallible: depth is non-zero by type, and
    /// [`AssetAmount`] is pre-validated.
    pub fn new(amount: AssetAmount, order_id: Felt, depth: NonZeroU32) -> Self {
        Self { amount, order_id, depth }
    }

    pub fn amount(&self) -> AssetAmount {
        self.amount
    }

    pub fn order_id(&self) -> Felt {
        self.order_id
    }

    pub fn depth(&self) -> NonZeroU32 {
        self.depth
    }
}

impl From<PswapNoteAttachment> for NoteAttachment {
    fn from(attachment: PswapNoteAttachment) -> Self {
        let word = Word::from([
            Felt::from(attachment.amount),
            attachment.order_id,
            Felt::from(attachment.depth.get()),
            ZERO,
        ]);
        NoteAttachment::with_word(PswapNote::PSWAP_ATTACHMENT_SCHEME, word)
    }
}

/// Parses a [`NoteAttachment`] back into a typed [`PswapNoteAttachment`].
///
/// Validates that:
/// - the scheme is [`PswapNote::PSWAP_ATTACHMENT_SCHEME`];
/// - the content is exactly one word (`num_words == 1`);
/// - the word's `amount` slot is a valid [`AssetAmount`];
/// - the word's `depth` slot fits in a `u32` and is non-zero.
///
/// The trailing slot (`word[3]`) is not asserted to be zero: PSWAP_ATTACHMENT_SCHEME's
/// canonical encoding leaves it for forward compatibility.
impl TryFrom<&NoteAttachment> for PswapNoteAttachment {
    type Error = NoteError;

    fn try_from(attachment: &NoteAttachment) -> Result<Self, Self::Error> {
        if attachment.attachment_scheme() != PswapNote::PSWAP_ATTACHMENT_SCHEME {
            return Err(NoteError::other("attachment scheme is not PSWAP_ATTACHMENT_SCHEME"));
        }
        let words = attachment.content().as_words();
        if words.len() != 1 {
            return Err(NoteError::other("PSWAP attachment must encode exactly one word"));
        }
        let word = words[0];

        let amount = AssetAmount::new(word[0].as_canonical_u64()).map_err(|e| {
            NoteError::other_with_source("PSWAP attachment amount is not a valid asset amount", e)
        })?;

        let depth_raw = word[PswapNote::PARENT_ATTACHMENT_DEPTH_OFFSET].as_canonical_u64();
        let depth_u32 = u32::try_from(depth_raw)
            .map_err(|_| NoteError::other("PSWAP attachment depth does not fit in u32"))?;
        let depth = NonZeroU32::new(depth_u32)
            .ok_or_else(|| NoteError::other("PSWAP attachment depth must be non-zero"))?;

        Ok(Self { amount, order_id: word[1], depth })
    }
}

// PSWAP NOTE
// ================================================================================================

/// A partially-fillable swap note for decentralized asset exchange.
///
/// A PSWAP note allows a creator to offer one fungible asset in exchange for another.
/// Unlike a regular SWAP note, consumers may fill it partially — the unfilled portion
/// is re-created as a remainder note with an updated serial number, while the creator
/// receives the filled portion via a payback note.
///
/// The note can be consumed both in local transactions (where the consumer provides
/// fill amounts via note_args) and in network transactions (where note_args default to
/// `[0, 0, 0, 0]`, triggering a full fill). To route a PSWAP note to a network account,
/// set the `attachment` to a [`NetworkAccountTarget`](crate::note::NetworkAccountTarget)
/// via the builder.
#[derive(Debug, Clone, bon::Builder)]
#[builder(finish_fn(vis = "", name = build_internal))]
pub struct PswapNote {
    sender: AccountId,
    storage: PswapNoteStorage,
    serial_number: Word,

    #[builder(default = NoteType::Private)]
    note_type: NoteType,

    offered_asset: FungibleAsset,

    attachment: Option<NoteAttachment>,
}

impl<S: pswap_note_builder::State> PswapNoteBuilder<S>
where
    S: pswap_note_builder::IsComplete,
{
    /// Validates and builds the [`PswapNote`].
    ///
    /// # Errors
    ///
    /// Returns an error if the offered and requested assets have the same faucet ID.
    pub fn build(self) -> Result<PswapNote, NoteError> {
        let note = self.build_internal();

        if note.offered_asset.faucet_id() == note.storage.requested_faucet_id() {
            return Err(NoteError::other(
                "offered and requested assets must have different faucets",
            ));
        }

        Ok(note)
    }
}

impl PswapNote {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items for the PSWAP note.
    pub const NUM_STORAGE_ITEMS: usize = PswapNoteStorage::NUM_STORAGE_ITEMS;

    /// Attachment scheme stamped on both PSWAP output notes (the payback P2ID and the
    /// remainder PSWAP).
    pub const PSWAP_ATTACHMENT_SCHEME: NoteAttachmentScheme =
        StandardNoteAttachment::PswapAttachment.attachment_scheme();

    /// Offset of the `depth` field within the [`Self::PSWAP_ATTACHMENT_SCHEME`] word.
    const PARENT_ATTACHMENT_DEPTH_OFFSET: usize = 2;

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the compiled PSWAP note script.
    pub fn script() -> NoteScript {
        PSWAP_SCRIPT.clone()
    }

    /// Returns the root hash of the PSWAP note script.
    pub fn script_root() -> NoteScriptRoot {
        PSWAP_SCRIPT.root()
    }

    /// Builds the `NOTE_ARGS` word that the PSWAP script expects when a consumer wants to fill
    /// part of the swap: `[account_fill, note_fill, 0, 0]`.
    ///
    /// - `account_fill` is the portion of the requested asset the consumer pays out of their own
    ///   vault.
    /// - `note_fill` is the portion sourced from another note in the same transaction (cross-swap
    ///   / net-zero flow).
    ///
    /// Both values are in the requested asset's base units. In a network transaction the kernel
    /// defaults `NOTE_ARGS` to `[0, 0, 0, 0]` and the script falls back to a full fill, so this
    /// helper is only needed for local transactions where the consumer chooses the fill split.
    ///
    /// Infallible: [`AssetAmount`] is bounded by `2^63 - 2^31`, which fits in a [`Felt`].
    pub fn create_args(account_fill: AssetAmount, note_fill: AssetAmount) -> Word {
        Word::from([Felt::from(account_fill), Felt::from(note_fill), ZERO, ZERO])
    }

    /// Returns the account ID of the note sender.
    pub fn sender(&self) -> AccountId {
        self.sender
    }

    /// Returns a reference to the PSWAP note storage.
    pub fn storage(&self) -> &PswapNoteStorage {
        &self.storage
    }

    /// Returns the serial number of this note.
    pub fn serial_number(&self) -> Word {
        self.serial_number
    }

    /// Returns the note type (public or private).
    pub fn note_type(&self) -> NoteType {
        self.note_type
    }

    /// Returns a reference to the offered [`FungibleAsset`].
    pub fn offered_asset(&self) -> &FungibleAsset {
        &self.offered_asset
    }

    /// Returns a reference to the note attachments.
    ///
    /// For notes targeting a network account, this may contain a
    /// [`NetworkAccountTarget`](crate::note::NetworkAccountTarget) with scheme = 2. For a
    /// remainder PSWAP this contains the [`Self::PSWAP_ATTACHMENT_SCHEME`] word
    /// `[amt_payout, order_id, depth, 0]`. For an original PSWAP (no prior fill),
    /// this is typically empty.
    pub fn attachments(&self) -> Option<&NoteAttachment> {
        self.attachment.as_ref()
    }

    /// Returns the order_id of this lineage, equal to `serial_number()[1]`.
    pub fn order_id(&self) -> Felt {
        self.serial_number[1]
    }

    /// Returns the depth carried in this note's [`Self::PSWAP_ATTACHMENT_SCHEME`] attachment,
    /// or 0 if the note has no such attachment (i.e., it is the original PSWAP, not a
    /// remainder produced by an earlier fill).
    ///
    /// The next round's `current_depth` is computed as `parent_depth() + 1`, matching the
    /// on-chain `get_current_depth` MASM procedure. Use [`Self::next_depth`] for the typed
    /// [`NonZeroU32`] form. A malformed `PSWAP_ATTACHMENT_SCHEME` attachment (depth out of
    /// `u32` range, depth == 0, etc.) is treated as if no attachment is present, so discovery
    /// degrades to "looks like an original" rather than corrupting downstream arithmetic.
    pub fn parent_depth(&self) -> u32 {
        self.attachment
            .as_ref()
            .and_then(|att| PswapNoteAttachment::try_from(att).ok())
            .map(|att| att.depth().get())
            .unwrap_or(0)
    }

    /// Returns the depth that the next-round payback / remainder should carry, equal to
    /// `parent_depth() + 1`. Always non-zero by construction.
    ///
    /// # Errors
    ///
    /// Returns an error if `parent_depth()` is [`u32::MAX`].
    pub fn next_depth(&self) -> Result<NonZeroU32, NoteError> {
        self.parent_depth()
            .checked_add(1)
            .and_then(NonZeroU32::new)
            .ok_or_else(|| NoteError::other("PSWAP depth overflow"))
    }

    // INSTANCE METHODS
    // --------------------------------------------------------------------------------------------

    /// Executes the swap as a full fill, producing only the payback note (no remainder).
    ///
    /// Equivalent to calling [`Self::execute`] with `account_fill_asset` set to the full
    /// requested amount and `note_fill_asset = None`. It also matches the on-chain
    /// behavior when a note is consumed without explicit `note_args` (e.g. in a network
    /// transaction, where the kernel defaults `note_args` to `[0, 0, 0, 0]` and the MASM
    /// script falls back to a full fill).
    pub fn execute_full_fill(&self, consumer_account_id: AccountId) -> Result<Note, NoteError> {
        let requested_faucet_id = self.storage.requested_faucet_id();
        let total_requested_amount = self.storage.requested_asset_amount();

        let fill_asset =
            FungibleAsset::new(requested_faucet_id, total_requested_amount.as_u64())
                .map_err(|e| NoteError::other_with_source("failed to create full fill asset", e))?
                .with_callbacks(self.storage.requested_asset().callbacks());

        self.create_payback_note(consumer_account_id, fill_asset, total_requested_amount.as_u64())
    }

    /// Executes the swap, producing the output notes for a given fill.
    ///
    /// `account_fill_asset` is debited from the consumer's vault; `note_fill_asset` arrives
    /// from another note in the same transaction (cross-swap). At least one must be
    /// provided.
    ///
    /// Returns `(payback_note, Option<remainder_pswap_note>)`. The remainder is
    /// `None` when the fill equals the total requested amount (full fill).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Both assets are `None`.
    /// - The fill amount is zero.
    /// - The fill amount exceeds the total requested amount.
    pub fn execute(
        &self,
        consumer_account_id: AccountId,
        account_fill_asset: Option<FungibleAsset>,
        note_fill_asset: Option<FungibleAsset>,
    ) -> Result<(Note, Option<PswapNote>), NoteError> {
        // Combine account fill and note fill into a single payback asset.
        let payback_asset = match (account_fill_asset, note_fill_asset) {
            (Some(account_fill), Some(note_fill)) => account_fill.add(note_fill).map_err(|e| {
                NoteError::other_with_source(
                    "failed to combine account fill and note fill assets",
                    e,
                )
            })?,
            (Some(asset), None) | (None, Some(asset)) => asset,
            (None, None) => {
                return Err(NoteError::other(
                    "at least one of account_fill_asset or note_fill_asset must be provided",
                ));
            },
        };
        let fill_amount = payback_asset.amount().as_u64();

        let total_offered_amount = self.offered_asset.amount().as_u64();
        let requested_faucet_id = self.storage.requested_faucet_id();
        let total_requested_amount = self.storage.requested_asset_amount().as_u64();

        // Validate fill amount
        if fill_amount == 0 {
            return Err(NoteError::other("Fill amount must be greater than 0"));
        }
        if fill_amount > total_requested_amount {
            return Err(NoteError::other(alloc::format!(
                "Fill amount {} exceeds requested amount {}",
                fill_amount,
                total_requested_amount
            )));
        }

        // Calculate payout amounts separately for account fill and note fill, matching the
        // MASM which calls calculate_tokens_offered_for_requested twice. This is necessary
        // because the account fill portion goes to the consumer's vault while the total
        // determines the remainder note's offered amount.
        let account_fill_amount = account_fill_asset.as_ref().map_or(0, |a| a.amount().as_u64());
        let note_fill_amount = note_fill_asset.as_ref().map_or(0, |a| a.amount().as_u64());
        let payout_for_account_fill = Self::calculate_output_amount(
            total_offered_amount,
            total_requested_amount,
            account_fill_amount,
        )?;
        let payout_for_note_fill = Self::calculate_output_amount(
            total_offered_amount,
            total_requested_amount,
            note_fill_amount,
        )?;
        let offered_amount_for_fill = payout_for_account_fill + payout_for_note_fill;

        let payback_note =
            self.create_payback_note(consumer_account_id, payback_asset, fill_amount)?;

        // Create remainder note if partial fill
        let remainder = if fill_amount < total_requested_amount {
            let remaining_offered = total_offered_amount - offered_amount_for_fill;
            let remaining_requested = total_requested_amount - fill_amount;

            let remaining_offered_asset =
                FungibleAsset::new(self.offered_asset.faucet_id(), remaining_offered)
                    .map_err(|e| {
                        NoteError::other_with_source("failed to create remainder asset", e)
                    })?
                    .with_callbacks(self.offered_asset.callbacks());

            let remaining_requested_asset =
                FungibleAsset::new(requested_faucet_id, remaining_requested)
                    .map_err(|e| {
                        NoteError::other_with_source(
                            "failed to create remaining requested asset",
                            e,
                        )
                    })?
                    .with_callbacks(self.storage.requested_asset().callbacks());

            Some(self.create_remainder_pswap_note(
                consumer_account_id,
                remaining_offered_asset,
                remaining_requested_asset,
                offered_amount_for_fill,
            )?)
        } else {
            None
        };

        Ok((payback_note, remainder))
    }

    /// Returns how many offered tokens a consumer receives for `fill_amount` of the
    /// requested asset, based on this note's current offered/requested ratio.
    ///
    /// # Errors
    ///
    /// Returns an error if the calculated payout is not a valid asset amount.
    pub fn calculate_offered_for_requested(&self, fill_amount: u64) -> Result<u64, NoteError> {
        let total_requested = self.storage.requested_asset_amount().as_u64();
        let total_offered = self.offered_asset.amount().as_u64();

        Self::calculate_output_amount(total_offered, total_requested, fill_amount)
    }

    // LINEAGE DISCOVERY
    // --------------------------------------------------------------------------------------------

    /// Reconstructs the depth-`d` payback P2ID [`Note`], so the creator can consume it as an
    /// unauthenticated input note.
    ///
    /// `consumer_account_id` must be the account that consumed the parent PSWAP in round
    /// `depth`: the MASM stamps it as the payback's metadata sender, which feeds into
    /// [`Note::details_commitment`].
    ///
    /// # Errors
    ///
    /// Returns an error if the fill amount is not a valid asset amount.
    pub fn payback_note(
        &self,
        consumer_account_id: AccountId,
        attachment: &PswapNoteAttachment,
    ) -> Result<Note, NoteError> {
        let depth = attachment.depth().get();
        let parent_depth = Felt::from(depth - 1);
        let p2id_serial = Word::from([
            self.serial_number[0] + ONE,
            self.serial_number[1],
            self.serial_number[2],
            self.serial_number[3] + parent_depth,
        ]);

        let recipient =
            P2idNoteStorage::new(self.storage.creator_account_id).into_recipient(p2id_serial);

        let fill_asset =
            FungibleAsset::new(self.storage.requested_faucet_id(), u64::from(attachment.amount()))
                .map_err(|e| NoteError::other_with_source("invalid fill amount", e))?
                .with_callbacks(self.storage.requested_asset().callbacks());
        let assets = NoteAssets::new(vec![fill_asset.into()])?;

        let metadata =
            PartialNoteMetadata::new(consumer_account_id, self.storage.payback_note_type)
                .with_tag(self.storage.payback_note_tag());

        Ok(Note::with_attachments(
            assets,
            metadata,
            recipient,
            NoteAttachments::from(NoteAttachment::from(*attachment)),
        ))
    }

    /// Reconstructs the depth-`d` remainder PSWAP [`Note`] in this lineage.
    ///
    /// Called on the original PSWAP, this returns the full Note for the remainder produced
    /// in round `depth`. The returned Note matches the created note exactly.
    ///
    /// - `consumer_account_id` — the account that consumed the parent PSWAP in round `depth`, used
    ///   as the remainder's sender.
    /// - `attachment` — the on-chain `[amount, order_id, depth, 0]` attachment for this round,
    ///   where `amount` is the offered-asset units paid out.
    /// - `remaining_offered` / `remaining_requested` — the leftover amounts that survive into this
    ///   remainder. Both are required because the price formula uses floor division, so one isn't
    ///   derivable from the other across rounds in general.
    ///
    /// # Errors
    ///
    /// Returns an error if any amount is not a valid asset amount.
    pub fn remainder_note(
        &self,
        consumer_account_id: AccountId,
        attachment: &PswapNoteAttachment,
        remaining_offered: AssetAmount,
        remaining_requested: AssetAmount,
    ) -> Result<Note, NoteError> {
        let depth = attachment.depth().get();
        let remainder_serial = Word::from([
            self.serial_number[0],
            self.serial_number[1],
            self.serial_number[2],
            self.serial_number[3] + Felt::from(depth),
        ]);

        let requested_asset =
            FungibleAsset::new(self.storage.requested_faucet_id(), u64::from(remaining_requested))
                .map_err(|e| NoteError::other_with_source("invalid remaining_requested amount", e))?
                .with_callbacks(self.storage.requested_asset().callbacks());
        let offered_asset =
            FungibleAsset::new(self.offered_asset.faucet_id(), u64::from(remaining_offered))
                .map_err(|e| NoteError::other_with_source("invalid remaining_offered amount", e))?
                .with_callbacks(self.offered_asset.callbacks());

        let new_storage = PswapNoteStorage::builder()
            .requested_asset(requested_asset)
            .creator_account_id(self.storage.creator_account_id)
            .payback_note_type(self.storage.payback_note_type)
            .build();
        let recipient = new_storage.into_recipient(remainder_serial);

        let assets = NoteAssets::new(vec![offered_asset.into()])?;

        let tag = Self::create_tag(self.note_type, &offered_asset, &requested_asset);
        let metadata = PartialNoteMetadata::new(consumer_account_id, self.note_type).with_tag(tag);

        Ok(Note::with_attachments(
            assets,
            metadata,
            recipient,
            NoteAttachments::from(NoteAttachment::from(*attachment)),
        ))
    }

    // ASSOCIATED FUNCTIONS
    // --------------------------------------------------------------------------------------------

    /// Builds the 32-bit [`NoteTag`] for a PSWAP note.
    ///
    /// ```text
    /// [31..30] note_type          (2 bits)
    /// [29..16] script_root MSBs   (14 bits)
    /// [15..8]  offered faucet ID  (8 bits, top byte of prefix)
    /// [7..0]   requested faucet ID (8 bits, top byte of prefix)
    /// ```
    pub fn create_tag(
        note_type: NoteType,
        offered_asset: &FungibleAsset,
        requested_asset: &FungibleAsset,
    ) -> NoteTag {
        let pswap_root_bytes = Self::script().root().as_bytes();

        // Construct the pswap use case ID from the 14 most significant bits of the script root.
        // This leaves the two most significant bits zero.
        let mut pswap_use_case_id = (pswap_root_bytes[0] as u16) << 6;
        pswap_use_case_id |= (pswap_root_bytes[1] >> 2) as u16;

        // Get bits 0..8 from the faucet IDs of both assets which will form the tag payload.
        let offered_asset_id: u64 = offered_asset.faucet_id().prefix().into();
        let offered_asset_tag = (offered_asset_id >> 56) as u8;

        let requested_asset_id: u64 = requested_asset.faucet_id().prefix().into();
        let requested_asset_tag = (requested_asset_id >> 56) as u8;

        let asset_pair = ((offered_asset_tag as u16) << 8) | (requested_asset_tag as u16);

        let tag = ((note_type as u8 as u32) << 30)
            | ((pswap_use_case_id as u32) << 16)
            | asset_pair as u32;

        NoteTag::new(tag)
    }

    /// Computes `floor((offered_total * fill_amount) / requested_total)` via a
    /// u128 intermediate, mirroring `u64::widening_mul` + `u128::div` on the
    /// MASM side.
    ///
    /// # Errors
    ///
    /// Returns an error if the result does not fit in a valid [`AssetAmount`].
    fn calculate_output_amount(
        offered_total: u64,
        requested_total: u64,
        fill_amount: u64,
    ) -> Result<u64, NoteError> {
        let product = (offered_total as u128) * (fill_amount as u128);
        let quotient = product / (requested_total as u128);
        let amount = u64::try_from(quotient)
            .map_err(|_| NoteError::other("payout quotient does not fit in u64"))?;
        // Validate the result is a valid fungible asset amount.
        AssetAmount::new(amount).map_err(|e| {
            NoteError::other_with_source("payout amount exceeds max fungible asset amount", e)
        })?;
        Ok(amount)
    }


    /// Builds a payback note (P2ID) that delivers the filled assets to the swap creator.
    ///
    /// The note inherits its type (public/private) from this PSWAP note and derives a
    /// deterministic serial number by incrementing the least significant element of the
    /// serial number (`serial[0] + 1`).
    ///
    /// The attachment carries `[fill_amount, order_id, current_depth, 0]` under
    /// [`Self::PSWAP_ATTACHMENT_SCHEME`]. `current_depth` is `parent_depth + 1` — i.e.,
    /// the round number that produced this payback (1-indexed).
    fn create_payback_note(
        &self,
        consumer_account_id: AccountId,
        payback_asset: FungibleAsset,
        fill_amount: u64,
    ) -> Result<Note, NoteError> {
        let payback_note_tag = self.storage.payback_note_tag();
        // Derive P2ID serial: increment least significant element (matching MASM add.1)
        let p2id_serial_num = Word::from([
            self.serial_number[0] + ONE,
            self.serial_number[1],
            self.serial_number[2],
            self.serial_number[3],
        ]);

        // P2ID recipient targets the creator
        let recipient =
            P2idNoteStorage::new(self.storage.creator_account_id).into_recipient(p2id_serial_num);

        let current_depth = self.next_depth()?;
        let fill_amount_typed = AssetAmount::new(fill_amount).map_err(|e| {
            NoteError::other_with_source("fill amount is not a valid asset amount", e)
        })?;
        let attachment: NoteAttachment =
            PswapNoteAttachment::new(fill_amount_typed, self.order_id(), current_depth).into();

        let p2id_assets = NoteAssets::new(vec![payback_asset.into()])?;
        let p2id_metadata =
            PartialNoteMetadata::new(consumer_account_id, self.storage.payback_note_type)
                .with_tag(payback_note_tag);

        Ok(Note::with_attachments(
            p2id_assets,
            p2id_metadata,
            recipient,
            NoteAttachments::from(attachment),
        ))
    }

    /// Builds a remainder PSWAP note carrying the unfilled portion of the swap.
    ///
    /// The remainder inherits the original creator, tags, and note type, with an updated
    /// serial number (`serial[3] + 1`).
    ///
    /// The attachment carries `[offered_amount_for_fill, order_id, current_depth, 0]` under
    /// [`Self::PSWAP_ATTACHMENT_SCHEME`]. The remainder must carry this attachment so that
    /// when *it* is later consumed as a parent, `get_current_depth` reads the right scheme
    /// and increments depth correctly.
    fn create_remainder_pswap_note(
        &self,
        consumer_account_id: AccountId,
        remaining_offered_asset: FungibleAsset,
        remaining_requested_asset: FungibleAsset,
        offered_amount_for_fill: u64,
    ) -> Result<PswapNote, NoteError> {
        let new_storage = PswapNoteStorage::builder()
            .requested_asset(remaining_requested_asset)
            .creator_account_id(self.storage.creator_account_id)
            .payback_note_type(self.storage.payback_note_type)
            .build();

        // Remainder serial: increment most significant element (matching MASM movup.3 add.1
        // movdn.3)
        let remainder_serial_num = Word::from([
            self.serial_number[0],
            self.serial_number[1],
            self.serial_number[2],
            self.serial_number[3] + ONE,
        ]);

        let current_depth = self.next_depth()?;
        let payout_typed = AssetAmount::new(offered_amount_for_fill).map_err(|e| {
            NoteError::other_with_source("payout amount is not a valid asset amount", e)
        })?;
        let attachment: NoteAttachment =
            PswapNoteAttachment::new(payout_typed, self.order_id(), current_depth).into();

        PswapNote::builder()
            .sender(consumer_account_id)
            .storage(new_storage)
            .serial_number(remainder_serial_num)
            .note_type(self.note_type)
            .offered_asset(remaining_offered_asset)
            .attachment(attachment)
            .build()
    }
}

// CONVERSIONS
// ================================================================================================

/// Converts a [`PswapNote`] into a protocol [`Note`], computing the final PSWAP tag.
impl From<PswapNote> for Note {
    fn from(pswap: PswapNote) -> Self {
        let tag = PswapNote::create_tag(
            pswap.note_type,
            &pswap.offered_asset,
            pswap.storage.requested_asset(),
        );

        let recipient = pswap.storage.into_recipient(pswap.serial_number);

        let assets = NoteAssets::new(vec![pswap.offered_asset.into()])
            .expect("single fungible asset should be valid");

        let metadata = PartialNoteMetadata::new(pswap.sender, pswap.note_type).with_tag(tag);

        let attachments = pswap.attachment.map(NoteAttachments::from).unwrap_or_default();

        Note::with_attachments(assets, metadata, recipient, attachments)
    }
}

/// Parses a protocol [`Note`] back into a [`PswapNote`] by deserializing its storage.
impl TryFrom<&Note> for PswapNote {
    type Error = NoteError;

    fn try_from(note: &Note) -> Result<Self, Self::Error> {
        if note.recipient().script().root() != PswapNote::script_root() {
            return Err(NoteError::other("note script root does not match PSWAP script root"));
        }

        let storage = PswapNoteStorage::try_from(note.recipient().storage().items())?;

        if note.assets().num_assets() != 1 {
            return Err(NoteError::other("PSWAP note must have exactly one asset"));
        }
        let offered_asset = match note.assets().iter().next().unwrap() {
            Asset::Fungible(fa) => *fa,
            Asset::NonFungible(_) => {
                return Err(NoteError::other("PSWAP note asset must be fungible"));
            },
        };

        let attachment = match note.attachments().num_attachments() {
            0 => None,
            1 => {
                Some(note.attachments().get(0).expect("length should have been validated").clone())
            },
            _ => return Err(NoteError::other("pswap note supports only one attachment")),
        };

        PswapNote::builder()
            .sender(note.metadata().sender())
            .storage(storage)
            .serial_number(note.recipient().serial_num())
            .note_type(note.metadata().note_type())
            .offered_asset(offered_asset)
            .maybe_attachment(attachment)
            .build()
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use miden_protocol::account::{AccountId, AccountIdVersion, AccountType};
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::crypto::rand::{FeltRng, RandomCoin};

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
        let order_id = Felt::from(42u32);
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
        let attachment =
            PswapNoteAttachment::new(AssetAmount::new(50).unwrap(), Felt::from(9u32), depth);
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
        let order_id = Felt::from(1u32);
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
            Felt::from(7u32),
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
            Felt::from(1u32),
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
}
