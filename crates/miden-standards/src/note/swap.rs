use alloc::vec::Vec;

use miden_protocol::Word;
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
    NoteDetails,
    NoteRecipient,
    NoteScript,
    NoteScriptRoot,
    NoteStorage,
    NoteTag,
    NoteType,
    PartialNoteMetadata,
};
use miden_protocol::utils::sync::LazyLock;

use crate::StandardsLib;
use crate::note::P2idNoteStorage;

// NOTE SCRIPT
// ================================================================================================

/// Path to the SWAP note script procedure in the standards library.
const SWAP_SCRIPT_PATH: &str = "::miden::standards::notes::swap::main";

// Initialize the SWAP note script only once
static SWAP_SCRIPT: LazyLock<NoteScript> = LazyLock::new(|| {
    let standards_lib = StandardsLib::default();
    let path = Path::new(SWAP_SCRIPT_PATH);
    NoteScript::from_library_reference(standards_lib.as_ref(), path)
        .expect("Standards library contains SWAP note script procedure")
});

// SWAP NOTE
// ================================================================================================

/// A SWAP note: offers `offered_asset` in exchange for `requested_asset`.
///
/// Any account may consume the note: the consumer receives the `offered_asset` and, in turn,
/// creates a P2ID payback note that returns the `requested_asset` to the swap's `sender`.
///
/// Construct one with the [builder](SwapNote::builder), then call [`SwapNote::into_notes`] to
/// obtain the outgoing SWAP [`Note`] together with the [`NoteDetails`] of the payback note.
#[derive(Debug, Clone)]
pub struct SwapNote {
    sender: AccountId,
    offered_asset: Asset,
    requested_asset: Asset,
    swap_note_type: NoteType,
    payback_note_type: NoteType,
    serial_number: Word,
    payback_serial_number: Word,
    attachments: NoteAttachments,
}

#[bon::bon]
impl SwapNote {
    /// Builds a new [`SwapNote`] offering `offered_asset` in exchange for `requested_asset`.
    ///
    /// # Errors
    ///
    /// Returns an error if the requested asset is the same as the offered asset, or if the
    /// attachments exceed their protocol limit (see [`NoteAttachments::new`]).
    #[builder]
    pub fn new(
        #[builder(field)] attachments: Vec<NoteAttachment>,
        sender: AccountId,
        #[builder(into)] offered_asset: Asset,
        #[builder(into)] requested_asset: Asset,
        #[builder(default)] swap_note_type: NoteType,
        #[builder(default)] payback_note_type: NoteType,
        serial_number: Word,
        payback_serial_number: Word,
    ) -> Result<Self, NoteError> {
        if requested_asset == offered_asset {
            return Err(NoteError::other("requested asset same as offered asset"));
        }

        let attachments = NoteAttachments::new(attachments)?;

        Ok(Self {
            sender,
            offered_asset,
            requested_asset,
            swap_note_type,
            payback_note_type,
            serial_number,
            payback_serial_number,
            attachments,
        })
    }
}

impl SwapNote {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items of the SWAP note.
    pub const NUM_STORAGE_ITEMS: usize = SwapNoteStorage::NUM_ITEMS;

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the script of the SWAP note.
    pub fn script() -> NoteScript {
        SWAP_SCRIPT.clone()
    }

    /// Returns the SWAP note script root.
    pub fn script_root() -> NoteScriptRoot {
        SWAP_SCRIPT.root()
    }

    /// Returns the account ID of the note's sender.
    pub fn sender(&self) -> AccountId {
        self.sender
    }

    /// Returns the asset offered by the note.
    pub fn offered_asset(&self) -> Asset {
        self.offered_asset
    }

    /// Returns the asset requested in exchange for the offered asset.
    pub fn requested_asset(&self) -> Asset {
        self.requested_asset
    }

    /// Returns the type of the outgoing SWAP note.
    pub fn swap_note_type(&self) -> NoteType {
        self.swap_note_type
    }

    /// Returns the type of the payback note created on consumption.
    pub fn payback_note_type(&self) -> NoteType {
        self.payback_note_type
    }

    /// Returns the serial number of the outgoing SWAP note.
    pub fn serial_number(&self) -> Word {
        self.serial_number
    }

    /// Returns the serial number of the payback note.
    pub fn payback_serial_number(&self) -> Word {
        self.payback_serial_number
    }

    /// Returns the attachments carried by the SWAP note.
    pub fn attachments(&self) -> &NoteAttachments {
        &self.attachments
    }

    // INSTANCE METHODS
    // --------------------------------------------------------------------------------------------

    /// Consumes the note and returns the outgoing SWAP [`Note`] together with the [`NoteDetails`]
    /// of the payback P2ID note that the consumer will create for the sender.
    ///
    /// # Errors
    ///
    /// Returns an error if constructing the note's assets fails (see [`NoteAssets::new`]).
    pub fn into_notes(self) -> Result<(Note, NoteDetails), NoteError> {
        let swap_storage = SwapNoteStorage::new(
            self.sender,
            self.requested_asset,
            self.payback_note_type,
            self.payback_serial_number,
        );
        let recipient = swap_storage.into_recipient(self.serial_number);

        let tag = Self::build_tag(self.swap_note_type, &self.offered_asset, &self.requested_asset);
        let metadata = PartialNoteMetadata::new(self.sender, self.swap_note_type).with_tag(tag);
        let assets = NoteAssets::new(vec![self.offered_asset])?;
        let note = Note::with_attachments(assets, metadata, recipient, self.attachments);

        let payback_recipient =
            P2idNoteStorage::new(self.sender).into_recipient(self.payback_serial_number);
        let payback_assets = NoteAssets::new(vec![self.requested_asset])?;
        let payback_note = NoteDetails::new(payback_assets, payback_recipient);

        Ok((note, payback_note))
    }

    /// Returns a note tag for a swap note with the specified parameters.
    ///
    /// The tag is laid out as follows:
    ///
    /// ```text
    /// [
    ///   note_type (1 bit) | script_root (15 bits)
    ///   | offered_asset_faucet_id (8 bits) | requested_asset_faucet_id (8 bits)
    /// ]
    /// ```
    ///
    /// The script root serves as the use case identifier of the SWAP tag.
    pub fn build_tag(
        note_type: NoteType,
        offered_asset: &Asset,
        requested_asset: &Asset,
    ) -> NoteTag {
        let swap_root_bytes = Self::script().root().as_bytes();
        // Construct the swap use case ID from the 15 most significant bits of the script root. This
        // leaves the most significant bit zero.
        let mut swap_use_case_id = (swap_root_bytes[0] as u16) << 7;
        swap_use_case_id |= (swap_root_bytes[1] >> 1) as u16;

        // Get bits 0..8 from the faucet IDs of both assets which will form the tag payload.
        let offered_asset_id: u64 = offered_asset.faucet_id().prefix().into();
        let offered_asset_tag = (offered_asset_id >> 56) as u8;

        let requested_asset_id: u64 = requested_asset.faucet_id().prefix().into();
        let requested_asset_tag = (requested_asset_id >> 56) as u8;

        let asset_pair = ((offered_asset_tag as u16) << 8) | (requested_asset_tag as u16);

        let tag = ((note_type as u8 as u32) << 31)
            | ((swap_use_case_id as u32) << 16)
            | asset_pair as u32;

        NoteTag::new(tag)
    }
}

// BUILDER EXTENSIONS
// ================================================================================================

impl<S: swap_note_builder::State> SwapNoteBuilder<S> {
    /// Adds a single attachment to the SWAP note.
    pub fn attachment(mut self, attachment: impl Into<NoteAttachment>) -> Self {
        self.attachments.push(attachment.into());
        self
    }

    /// Adds multiple attachments to the SWAP note.
    pub fn attachments(
        mut self,
        attachments: impl IntoIterator<Item = impl Into<NoteAttachment>>,
    ) -> Self {
        self.attachments.extend(attachments.into_iter().map(Into::into));
        self
    }
}

impl<S: swap_note_builder::State> SwapNoteBuilder<S>
where
    S::SerialNumber: swap_note_builder::IsUnset,
    S::PaybackSerialNumber: swap_note_builder::IsUnset,
{
    /// Draws the SWAP and payback serial numbers from `rng` (payback first) and sets them.
    pub fn generate_serial_numbers(
        self,
        rng: &mut impl FeltRng,
    ) -> SwapNoteBuilder<
        swap_note_builder::SetSerialNumber<swap_note_builder::SetPaybackSerialNumber<S>>,
    > {
        let payback_serial_number = rng.draw_word();
        let serial_number = rng.draw_word();
        self.payback_serial_number(payback_serial_number).serial_number(serial_number)
    }
}

// SWAP NOTE STORAGE
// ================================================================================================

/// Canonical storage representation for a SWAP note.
///
/// Contains the payback note configuration and the requested asset that the
/// swap creator wants to receive in exchange for the offered asset contained
/// in the note's vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapNoteStorage {
    payback_note_type: NoteType,
    payback_tag: NoteTag,
    requested_asset: Asset,
    payback_recipient_digest: Word,
}

impl SwapNoteStorage {
    // CONSTANTS
    // --------------------------------------------------------------------------------------------

    /// Expected number of storage items of the SWAP note.
    pub const NUM_ITEMS: usize = 14;

    // CONSTRUCTORS
    // --------------------------------------------------------------------------------------------

    /// Creates new SWAP note storage with the specified parameters.
    pub fn new(
        sender: AccountId,
        requested_asset: Asset,
        payback_note_type: NoteType,
        payback_serial_number: Word,
    ) -> Self {
        let payback_recipient = P2idNoteStorage::new(sender).into_recipient(payback_serial_number);
        let payback_tag = NoteTag::with_account_target(sender);

        Self::from_parts(
            payback_note_type,
            payback_tag,
            requested_asset,
            payback_recipient.digest(),
        )
    }

    /// Creates a [`SwapNoteStorage`] from raw parts.
    pub fn from_parts(
        payback_note_type: NoteType,
        payback_tag: NoteTag,
        requested_asset: Asset,
        payback_recipient_digest: Word,
    ) -> Self {
        Self {
            payback_note_type,
            payback_tag,
            requested_asset,
            payback_recipient_digest,
        }
    }

    /// Returns the payback note type.
    pub fn payback_note_type(&self) -> NoteType {
        self.payback_note_type
    }

    /// Returns the payback note tag.
    pub fn payback_tag(&self) -> NoteTag {
        self.payback_tag
    }

    /// Returns the requested asset.
    pub fn requested_asset(&self) -> Asset {
        self.requested_asset
    }

    /// Returns the payback recipient digest.
    pub fn payback_recipient_digest(&self) -> Word {
        self.payback_recipient_digest
    }

    /// Consumes the storage and returns a SWAP [`NoteRecipient`] with the provided serial number.
    ///
    /// Notes created with this recipient will be SWAP notes whose storage encodes the payback
    /// configuration and the requested asset stored in this [`SwapNoteStorage`].
    pub fn into_recipient(self, serial_num: Word) -> NoteRecipient {
        NoteRecipient::new(serial_num, SwapNote::script(), NoteStorage::from(self))
    }
}

impl From<SwapNoteStorage> for NoteStorage {
    fn from(storage: SwapNoteStorage) -> Self {
        let mut storage_values = Vec::with_capacity(SwapNoteStorage::NUM_ITEMS);
        storage_values.extend_from_slice(&storage.requested_asset.as_elements());
        storage_values.extend_from_slice(storage.payback_recipient_digest.as_elements());
        storage_values
            .extend_from_slice(&[storage.payback_note_type.into(), storage.payback_tag.into()]);

        NoteStorage::new(storage_values)
            .expect("number of storage items should not exceed max storage items")
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {

    use miden_protocol::account::{AccountIdVersion, AccountType};
    use miden_protocol::asset::{FungibleAsset, NonFungibleAsset, NonFungibleAssetDetails};
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::{NoteStorage, NoteTag, NoteType};
    use miden_protocol::testing::account_id::{
        ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET,
        ACCOUNT_ID_PUBLIC_NON_FUNGIBLE_FAUCET,
    };

    use super::*;

    fn fungible_faucet() -> AccountId {
        ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET.try_into().unwrap()
    }

    fn non_fungible_faucet() -> AccountId {
        ACCOUNT_ID_PUBLIC_NON_FUNGIBLE_FAUCET.try_into().unwrap()
    }

    fn fungible_asset() -> Asset {
        Asset::Fungible(FungibleAsset::new(fungible_faucet(), 1000).unwrap())
    }

    fn non_fungible_asset() -> Asset {
        let details = NonFungibleAssetDetails::new(non_fungible_faucet(), vec![0xaa, 0xbb]);
        Asset::NonFungible(NonFungibleAsset::new(&details))
    }

    #[test]
    fn swap_note_storage() {
        let payback_note_type = NoteType::Private;
        let payback_tag = NoteTag::new(0x12345678);
        let requested_asset = fungible_asset();
        let payback_recipient_digest = Word::from([1_u32, 2_u32, 3_u32, 4_u32]);

        let storage = SwapNoteStorage::from_parts(
            payback_note_type,
            payback_tag,
            requested_asset,
            payback_recipient_digest,
        );

        assert_eq!(storage.payback_note_type(), payback_note_type);
        assert_eq!(storage.payback_tag(), payback_tag);
        assert_eq!(storage.requested_asset(), requested_asset);
        assert_eq!(storage.payback_recipient_digest(), payback_recipient_digest);

        // Convert to NoteStorage
        let note_storage = NoteStorage::from(storage);
        assert_eq!(note_storage.num_items() as usize, SwapNoteStorage::NUM_ITEMS);
    }

    #[test]
    fn swap_note_storage_with_non_fungible_asset() {
        let payback_note_type = NoteType::Public;
        let payback_tag = NoteTag::new(0xaabbccdd);
        let requested_asset = non_fungible_asset();
        let payback_recipient_digest = Word::from([10_u32, 20_u32, 30_u32, 40_u32]);

        let storage = SwapNoteStorage::from_parts(
            payback_note_type,
            payback_tag,
            requested_asset,
            payback_recipient_digest,
        );

        assert_eq!(storage.payback_note_type(), payback_note_type);
        assert_eq!(storage.requested_asset(), requested_asset);

        let note_storage = NoteStorage::from(storage);
        assert_eq!(note_storage.num_items() as usize, SwapNoteStorage::NUM_ITEMS);
    }

    #[test]
    fn swap_tag() {
        // Construct an ID that starts with 0xcdb1.
        let mut fungible_faucet_id_bytes = [0; 15];
        fungible_faucet_id_bytes[0] = 0xcd;
        fungible_faucet_id_bytes[1] = 0xb1;

        // Construct an ID that starts with 0xabec.
        let mut non_fungible_faucet_id_bytes = [0; 15];
        non_fungible_faucet_id_bytes[0] = 0xab;
        non_fungible_faucet_id_bytes[1] = 0xec;

        let offered_asset = Asset::Fungible(
            FungibleAsset::new(
                AccountId::dummy(
                    fungible_faucet_id_bytes,
                    AccountIdVersion::Version1,
                    AccountType::Public,
                ),
                2500,
            )
            .unwrap(),
        );

        let requested_asset =
            Asset::NonFungible(NonFungibleAsset::new(&NonFungibleAssetDetails::new(
                AccountId::dummy(
                    non_fungible_faucet_id_bytes,
                    AccountIdVersion::Version1,
                    AccountType::Public,
                ),
                vec![0xaa, 0xbb, 0xcc, 0xdd],
            )));

        // The fungible ID starts with 0xcdb1.
        // The non fungible ID starts with 0xabec.
        // The expected tag payload is thus 0xcdab.
        let expected_asset_pair = 0xcdab;

        let note_type = NoteType::Public;
        let actual_tag = SwapNote::build_tag(note_type, &offered_asset, &requested_asset);

        assert_eq!(actual_tag.as_u32() as u16, expected_asset_pair, "asset pair should match");
        assert_eq!((actual_tag.as_u32() >> 31) as u8, note_type as u8, "note type should match");
        // Check the 8 bits of the first script root byte.
        assert_eq!(
            (actual_tag.as_u32() >> 23) as u8,
            SwapNote::script_root().as_bytes()[0],
            "swap script root byte 0 should match"
        );
        // Extract the 7 bits of the second script root byte and shift for comparison.
        assert_eq!(
            ((actual_tag.as_u32() & 0b00000000_01111111_00000000_00000000) >> 16) as u8,
            SwapNote::script_root().as_bytes()[1] >> 1,
            "swap script root byte 1 should match with the highest bit set to zero"
        );
    }

    #[test]
    fn builder_produces_swap_and_payback_notes() {
        // The builder produces a SWAP note whose accessors echo the inputs, and `into_notes`
        // splits it into the public SWAP note and its payback note, each carrying one asset.
        let sender = AccountId::dummy([7u8; 15], AccountIdVersion::Version1, AccountType::Private);
        let offered_asset = fungible_asset();
        let requested_asset = non_fungible_asset();
        let mut rng = RandomCoin::new(Word::empty());

        let swap = SwapNote::builder()
            .sender(sender)
            .offered_asset(offered_asset)
            .requested_asset(requested_asset)
            .swap_note_type(NoteType::Public)
            .payback_note_type(NoteType::Private)
            .generate_serial_numbers(&mut rng)
            .build()
            .unwrap();

        assert_eq!(swap.sender(), sender);
        assert_eq!(swap.offered_asset(), offered_asset);
        assert_eq!(swap.requested_asset(), requested_asset);

        let (note, payback_note) = swap.into_notes().unwrap();
        assert_eq!(note.metadata().note_type(), NoteType::Public);
        assert_eq!(note.assets().num_assets(), 1);
        assert_eq!(payback_note.assets().num_assets(), 1);
    }
}
