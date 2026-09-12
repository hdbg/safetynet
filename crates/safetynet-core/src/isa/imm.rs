//! Instruction immediates: the operand bytes that follow an opcode.
//!
//! An opcode's operand width is fixed by the opcode itself — nothing here is a
//! varint. That is a deliberate constraint rather than a simplification: a
//! variable-length encoding makes an instruction's size depend on its operand
//! value, and later passes that move code around (block reordering, opcode
//! renumbering) would then have to re-derive sizes after every edit.

use crate::ByteOrder;

/// Why a sequence of bytes is not a valid immediate.
///
/// Deliberately free of opcode context: an [`Imm`] implementation knows how many
/// bytes it wanted, not which instruction wanted them. The caller adds that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImmErr {
    /// The stream ended before the operand did.
    Truncated {
        /// Bytes the operand needs.
        needed: usize,
        /// Bytes actually available.
        found: usize,
    },
    /// Enough bytes, but they do not denote a legal value.
    Invalid {
        /// Human-readable reason, e.g. `"frame size must be a multiple of 8"`.
        why: &'static str,
    },
}

/// A value that can appear as an instruction operand.
///
/// Implemented for the primitive widths the instruction set uses plus
/// [`FrameSize`], which carries a validity rule the primitives do not.
pub trait Imm: Copy + core::fmt::Debug + PartialEq + Eq + Sized {
    /// Encoded width in bytes. Constant per type — see the module note.
    const SIZE: usize;

    /// Appends this operand to `out` in byte order `B`.
    fn write<B: ByteOrder>(self, out: &mut Vec<u8>);

    /// Reads this operand from the start of `src` in byte order `B`.
    ///
    /// `src` begins at the operand, not at the opcode byte.
    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr>;
}

/// Builds the `Truncated` error for a fixed-size operand.
fn truncated<T: Imm>(found: usize) -> ImmErr {
    ImmErr::Truncated {
        needed: T::SIZE,
        found,
    }
}

impl Imm for u8 {
    const SIZE: usize = 1;

    fn write<B: ByteOrder>(self, out: &mut Vec<u8>) {
        // A single byte has no order to speak of.
        out.push(self);
    }

    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr> {
        src.first().copied().ok_or_else(|| truncated::<Self>(0))
    }
}

impl Imm for u16 {
    const SIZE: usize = 2;

    fn write<B: ByteOrder>(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&B::write_u16(self));
    }

    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr> {
        src.first_chunk::<2>()
            .map(|bytes| B::read_u16(*bytes))
            .ok_or_else(|| truncated::<Self>(src.len()))
    }
}

impl Imm for u32 {
    const SIZE: usize = 4;

    fn write<B: ByteOrder>(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&B::write_u32(self));
    }

    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr> {
        src.first_chunk::<4>()
            .map(|bytes| B::read_u32(*bytes))
            .ok_or_else(|| truncated::<Self>(src.len()))
    }
}

impl Imm for u64 {
    const SIZE: usize = 8;

    fn write<B: ByteOrder>(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&B::write_u64(self));
    }

    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr> {
        src.first_chunk::<8>()
            .map(|bytes| B::read_u64(*bytes))
            .ok_or_else(|| truncated::<Self>(src.len()))
    }
}

impl Imm for i32 {
    const SIZE: usize = 4;

    fn write<B: ByteOrder>(self, out: &mut Vec<u8>) {
        // Two's complement: the signed form is the unsigned bit pattern, so the
        // byte order treatment is identical and only the interpretation differs.
        out.extend_from_slice(&B::write_u32(self as u32));
    }

    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr> {
        src.first_chunk::<4>()
            .map(|bytes| B::read_u32(*bytes) as Self)
            .ok_or_else(|| truncated::<Self>(src.len()))
    }
}

/// A frame size or displacement in bytes, guaranteed to be word-aligned.
///
/// The operand stack moves one word at a time, so a frame whose size is not a
/// multiple of [`WORD_SIZE`](crate::WORD_SIZE) would leave `SP` misaligned and
/// every subsequent displacement off by the remainder. Enforcing it in the type
/// means the interpreter never has to check, and a corrupt image is rejected at
/// decode rather than producing a program that runs and is quietly wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FrameSize(u16);

impl FrameSize {
    /// The rule, stated once.
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
            _ => None,
        }
    }

    /// The size in bytes.
    pub const fn bytes(self) -> u16 {
        self.0
    }
}

impl Imm for FrameSize {
    const SIZE: usize = <u16 as Imm>::SIZE;

    fn write<B: ByteOrder>(self, out: &mut Vec<u8>) {
        self.0.write::<B>(out);
    }

    fn read<B: ByteOrder>(src: &[u8]) -> Result<Self, ImmErr> {
        let raw = <u16 as Imm>::read::<B>(src)?;
        Self::new(raw).ok_or(ImmErr::Invalid { why: Self::WHY })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Be, Le};

    /// Order-generic: every immediate must survive a write/read pair, and the
    /// reader must consume exactly the bytes the writer produced.
    fn round_trips<B: ByteOrder>() {
        fn check<B: ByteOrder, T: Imm>(value: T) {
            let mut bytes = Vec::new();
            value.write::<B>(&mut bytes);
            assert_eq!(bytes.len(), T::SIZE, "{value:?} wrote the wrong width");
            assert_eq!(T::read::<B>(&bytes), Ok(value));
        }

        check::<B, u8>(0xa5);
        check::<B, u16>(0xdead);
        check::<B, u32>(0xdead_beef);
        check::<B, u64>(0xdead_beef_feed_face);
        check::<B, i32>(-1);
        check::<B, i32>(i32::MIN);
        check::<B, FrameSize>(FrameSize::new(24).expect("24 is word-aligned"));
    }

    #[test]
    fn round_trips_le() {
        round_trips::<Le>();
    }

    #[test]
    fn round_trips_be() {
        round_trips::<Be>();
    }

    #[test]
    fn truncation_reports_both_counts() {
        assert_eq!(
            <u32 as Imm>::read::<Le>(&[1, 2]),
            Err(ImmErr::Truncated {
                needed: 4,
                found: 2
            })
        );
        assert_eq!(
            <u8 as Imm>::read::<Le>(&[]),
            Err(ImmErr::Truncated {
                needed: 1,
                found: 0
            })
        );
    }

    #[test]
    fn frame_size_rejects_unaligned_values() {
        assert_eq!(FrameSize::new(7), None);
        assert_eq!(FrameSize::new(0).map(FrameSize::bytes), Some(0));
        assert_eq!(FrameSize::round_up(7).map(FrameSize::bytes), Some(8));
        assert_eq!(FrameSize::round_up(8).map(FrameSize::bytes), Some(8));
        assert_eq!(FrameSize::round_up(u16::MAX), None);
    }

    /// A misaligned frame size is a decode failure, not a value to be fixed up
    /// silently: the bytes do not describe a program this machine can run.
    #[test]
    fn frame_size_decode_rejects_unaligned_bytes() {
        let mut bytes = Vec::new();
        7u16.write::<Le>(&mut bytes);
        assert!(matches!(
            <FrameSize as Imm>::read::<Le>(&bytes),
            Err(ImmErr::Invalid { .. })
        ));
    }

    /// Signed immediates must not acquire a different byte order from unsigned
    /// ones; the two's-complement pattern is the same bytes either way.
    #[test]
    fn signed_and_unsigned_agree_on_layout() {
        let mut signed = Vec::new();
        (-2i32).write::<Be>(&mut signed);
        let mut unsigned = Vec::new();
        0xffff_fffeu32.write::<Be>(&mut unsigned);
        assert_eq!(signed, unsigned);
    }
}
