use core::num::NonZeroU32;

use miden_protocol::asset::AssetAmount;
use miden_protocol::errors::NoteError;
use miden_protocol::note::NoteAttachment;
use miden_protocol::{Felt, Word, ZERO};

use super::{OrderId, PswapNote};

/// Typed attachment carried by both PSWAP output notes, encoded as
/// `[amount, order_id, depth, 0]` under [`PswapNote::PSWAP_ATTACHMENT_SCHEME`].
///
/// `depth` is [`NonZeroU32`] because attachments are only stamped on payback / remainder notes
/// (depth >= 1); the original PSWAP has no PSWAP-scheme attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PswapNoteAttachment {
    amount: AssetAmount,
    order_id: OrderId,
    depth: NonZeroU32,
}

impl PswapNoteAttachment {
    /// Creates a new [`PswapNoteAttachment`]. Infallible: depth is non-zero by type, and
    /// [`AssetAmount`] is pre-validated.
    pub fn new(amount: AssetAmount, order_id: OrderId, depth: NonZeroU32) -> Self {
        Self { amount, order_id, depth }
    }

    pub fn amount(&self) -> AssetAmount {
        self.amount
    }

    pub fn order_id(&self) -> OrderId {
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
            Felt::from(attachment.order_id),
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
/// - the word's `depth` slot fits in a `u32` and is non-zero;
/// - the word's reserved slot (`word[3]`) is zero.
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

        if word[3] != ZERO {
            return Err(NoteError::other("PSWAP attachment reserved slot (word[3]) must be zero"));
        }

        Ok(Self {
            amount,
            order_id: OrderId::from(word[1]),
            depth,
        })
    }
}
