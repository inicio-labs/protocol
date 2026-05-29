use alloc::vec;
use core::num::NonZeroU32;

use miden_protocol::account::AccountId;
use miden_protocol::assembly::Path;
use miden_protocol::asset::{Asset, AssetAmount, FungibleAsset};
use miden_protocol::errors::NoteError;
use miden_protocol::note::{
    Note,
    NoteAssets,
    NoteAttachment,
    NoteAttachmentScheme,
    NoteAttachments,
    NoteScript,
    NoteScriptRoot,
    NoteTag,
    NoteType,
    PartialNoteMetadata,
};
use miden_protocol::utils::sync::LazyLock;
use miden_protocol::{Felt, ONE, Word, ZERO};

use crate::StandardsLib;
use crate::note::{P2idNoteStorage, StandardNoteAttachment};

mod attachment;
mod storage;

#[cfg(test)]
mod tests;

pub use attachment::PswapNoteAttachment;
pub use storage::PswapNoteStorage;

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

// ORDER ID
// ================================================================================================

/// Identifier of a PSWAP order, stable across the entire payback / remainder lineage.
///
/// Equal to `serial_number[1]` of the originating PSWAP and stamped verbatim into every
/// downstream payback P2ID and remainder PSWAP's `PswapAttachment` (slot `[1]`).
///
/// `OrderId` is a transparent newtype around [`Felt`]; conversions are provided so callers can
/// move between the typed and raw forms when interfacing with the MASM bridge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct OrderId(Felt);

impl OrderId {
    /// Wraps a raw [`Felt`] as an [`OrderId`].
    pub const fn new(value: Felt) -> Self {
        Self(value)
    }

    /// Returns the underlying [`Felt`].
    pub const fn as_felt(&self) -> Felt {
        self.0
    }
}

impl From<Felt> for OrderId {
    fn from(value: Felt) -> Self {
        Self(value)
    }
}

impl From<OrderId> for Felt {
    fn from(order_id: OrderId) -> Self {
        order_id.0
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

        // Reject zero-amount assets: an offered amount of 0 means the note pays out nothing on
        // any fill (useless), and a requested amount of 0 would divide by zero in
        // `calculate_output_amount`. Catching both here makes the type unconditionally safe
        // for all callers of `execute` / `calculate_offered_for_requested`, including the
        // `TryFrom<&Note>` reconstruction path which funnels through this builder.
        if note.offered_asset.amount() == AssetAmount::ZERO
            || note.storage.requested_asset_amount() == AssetAmount::ZERO
        {
            return Err(NoteError::other("PSWAP offered and requested amounts must be non-zero"));
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
    pub(super) const PARENT_ATTACHMENT_DEPTH_OFFSET: usize = 2;

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
    /// - `note_fill` is the portion sourced from another note in the same transaction (cross-swap /
    ///   net-zero flow).
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

    /// Returns the [`OrderId`] of this lineage, equal to `serial_number()[1]`.
    pub fn order_id(&self) -> OrderId {
        OrderId::new(self.serial_number[1])
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

        let fill_asset = FungibleAsset::new(requested_faucet_id, total_requested_amount.as_u64())
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
    /// - Either fill asset's faucet does not match the requested faucet.
    /// - The fill amount is zero.
    /// - The fill amount exceeds the total requested amount.
    pub fn execute(
        &self,
        consumer_account_id: AccountId,
        account_fill_asset: Option<FungibleAsset>,
        note_fill_asset: Option<FungibleAsset>,
    ) -> Result<(Note, Option<PswapNote>), NoteError> {
        // Reject fill assets that aren't of the requested faucet. `FungibleAsset::add` catches
        // mismatched faucets only when both fill sources are present; the single-source arms
        // below bypass `add`, so a wrong-faucet asset would otherwise mint a payback note whose
        // asset disagrees with the storage's `requested_faucet_id` (the MASM rejects it
        // on-chain, but client-side reconstruction must catch it first).
        let requested_faucet_id = self.storage.requested_faucet_id();
        for fill in [account_fill_asset, note_fill_asset].iter().flatten() {
            if fill.faucet_id() != requested_faucet_id {
                return Err(NoteError::other(
                    "fill asset faucet does not match the requested faucet",
                ));
            }
        }

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
    /// Must be called on the original PSWAP (depth-0 root of the lineage); the serial advance
    /// uses the absolute lineage depth and produces incorrect serials when called on a
    /// remainder mid-lineage.
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
            P2idNoteStorage::new(self.storage.creator_account_id()).into_recipient(p2id_serial);

        let fill_asset =
            FungibleAsset::new(self.storage.requested_faucet_id(), u64::from(attachment.amount()))
                .map_err(|e| NoteError::other_with_source("invalid fill amount", e))?
                .with_callbacks(self.storage.requested_asset().callbacks());
        let assets = NoteAssets::new(vec![fill_asset.into()])?;

        let metadata =
            PartialNoteMetadata::new(consumer_account_id, self.storage.payback_note_type())
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
    /// Must be called on the original PSWAP (depth-0 root of the lineage); calling on a
    /// remainder mid-lineage over-advances the serial and reconstructs the wrong note.
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
            .creator_account_id(self.storage.creator_account_id())
            .payback_note_type(self.storage.payback_note_type())
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
            P2idNoteStorage::new(self.storage.creator_account_id()).into_recipient(p2id_serial_num);

        let current_depth = self.next_depth()?;
        let fill_amount_typed = AssetAmount::new(fill_amount).map_err(|e| {
            NoteError::other_with_source("fill amount is not a valid asset amount", e)
        })?;
        let attachment: NoteAttachment =
            PswapNoteAttachment::new(fill_amount_typed, self.order_id(), current_depth).into();

        let p2id_assets = NoteAssets::new(vec![payback_asset.into()])?;
        let p2id_metadata =
            PartialNoteMetadata::new(consumer_account_id, self.storage.payback_note_type())
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
            .creator_account_id(self.storage.creator_account_id())
            .payback_note_type(self.storage.payback_note_type())
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
