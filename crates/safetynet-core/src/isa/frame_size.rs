//! Word-aligned byte counts for frame operands.

use musli::alloc::Allocator;
use musli::de::Decoder;
use musli::{Context, Decode, Encode};

/// A frame size or displacement in bytes, guaranteed to be word-aligned.
///
/// The operand stack moves one word at a time, so a frame whose size is not a
/// multiple of [`WORD_SIZE`](crate::WORD_SIZE) leaves `SP` misaligned and every
/// subsequent displacement off by the remainder. Holding the rule in the type
/// means the interpreter never re-checks it, and a corrupt image is rejected
/// where its bytes are read rather than producing a program that runs and is
/// quietly wrong.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Encode)]
#[musli(transparent)]
pub struct FrameSize(u16);

impl FrameSize {
    const WHY: &'static str = "frame size must be a multiple of the 8-byte word";

    /// Wraps `bytes`, or returns `None` if it is not word-aligned.
    pub const fn new(bytes: u16) -> Option<Self> {
        if (bytes as usize).is_multiple_of(crate::WORD_SIZE) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Rounds `bytes` up to the next word boundary.
    ///
    /// Returns `None` only if rounding would overflow `u16`.
    pub const fn round_up(bytes: u16) -> Option<Self> {
        let word = crate::WORD_SIZE as u16;
        match bytes.checked_next_multiple_of(word) {
            Some(rounded) => Some(Self(rounded)),
            None => None,
        }
    }

    /// The size in bytes.
    pub const fn bytes(self) -> u16 {
        self.0
    }
}

#[cfg(feature = "debug")]
impl core::fmt::Display for FrameSize {
    /// Prints the byte count, which is what an assembly operand spells.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.bytes())
    }
}

impl<'de, M, A> Decode<'de, M, A> for FrameSize
where
    A: Allocator,
{
    fn decode<D>(decoder: D) -> Result<Self, D::Error>
    where
        D: Decoder<'de, Mode = M, Allocator = A>,
    {
        let cx = decoder.cx();
        let raw = decoder.decode_u16()?;
        Self::new(raw).ok_or_else(|| cx.message(Self::WHY))
    }
}

crate::opaque_debug!(FrameSize);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unaligned_values() {
        assert_eq!(FrameSize::new(7), None);
        assert_eq!(FrameSize::new(0).map(FrameSize::bytes), Some(0));
        assert_eq!(FrameSize::round_up(7).map(FrameSize::bytes), Some(8));
        assert_eq!(FrameSize::round_up(8).map(FrameSize::bytes), Some(8));
        assert_eq!(FrameSize::round_up(u16::MAX), None);
    }
}
