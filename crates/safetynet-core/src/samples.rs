//! Samples shared by the tests in this crate.

use crate::encoding::{DecodeError, Decoder, EncodeError, Encoder, decode, encode, encoded_len};
use crate::image::{Image, Layout, Sizes};
use crate::isa::*;
use crate::{ByteOrder, Instr};

/// Every instruction, each carrying an operand this machine can actually run: a
/// frame displacement that stays inside a modest prologue, and a divisor that is
/// not zero.
///
/// Operands are asymmetric where they can be — `0xdead_beef` rather than
/// `0xabab_abab` — so a byte-order mistake shows up as a wrong value instead of
/// the same bytes read backwards.
pub(crate) fn instructions() -> Vec<Instr> {
    let frame = FrameSize::new(24).expect("24 is word-aligned");
    vec![
        Halt.into(),
        Push8 { imm: 0xa5 }.into(),
        Push32 { imm: 0xdead_beef }.into(),
        Push64 {
            imm: 0x0102_0304_0506_0708,
        }
        .into(),
        Drop.into(),
        Alloc { n: frame }.into(),
        Free { n: frame }.into(),
        Lds8 { disp: 1 }.into(),
        Lds32 { disp: 12 }.into(),
        Lds64 { disp: 8 }.into(),
        Sts8 { disp: 17 }.into(),
        Sts32 { disp: 20 }.into(),
        Sts64 { disp: 24 }.into(),
        Ld8.into(),
        Ld32.into(),
        Ld64.into(),
        St8.into(),
        St32.into(),
        St64.into(),
        Add.into(),
        Sub.into(),
        Mul.into(),
        Div.into(),
        Rem.into(),
        SDiv.into(),
        SRem.into(),
        And.into(),
        Or.into(),
        Xor.into(),
        BitNot.into(),
        Shl.into(),
        Shr.into(),
        Sar.into(),
        CmpEq.into(),
        CmpLt.into(),
        CmpLe.into(),
        CmpSLt.into(),
        CmpSLe.into(),
        Jmp { offset: 0 }.into(),
        Jz { offset: -12 }.into(),
        Jnz { offset: i32::MIN }.into(),
        Switch.into(),
        Host { index: 3 }.into(),
    ]
}

/// An image that is nothing but stack, for tests with no data to place.
pub(crate) fn stack_image(bytes: u32) -> Image {
    Image::new(layout(bytes))
}

/// The layout behind [`stack_image`].
pub(crate) fn layout(stack: u32) -> Layout {
    Layout::new(Sizes {
        stack,
        ..Sizes::default()
    })
    .expect("a stack fits on its own")
}

/// A stand-in for a later, deliberately different format: the standard encoding
/// with a filler byte after every instruction.
///
/// Both halves live on one type so that they cannot drift apart, which is how a
/// real alternative format should be written too.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Padded<B>(core::marker::PhantomData<B>);

impl<B> Default for Padded<B> {
    fn default() -> Self {
        Self(core::marker::PhantomData)
    }
}

/// The byte that follows every instruction.
const FILLER: u8 = 0xff;

impl<B: ByteOrder> Encoder for Padded<B> {
    type Order = B;
    type Error = EncodeError;

    fn encode(&self, instr: Instr, out: &mut Vec<u8>) -> Result<usize, Self::Error> {
        let written = encode::<B>(instr, out)?;
        out.push(FILLER);
        Ok(written + 1)
    }

    fn encoded_len(&self, instr: Instr) -> Result<usize, Self::Error> {
        Ok(encoded_len(instr)? + 1)
    }
}

impl<B: ByteOrder> Decoder for Padded<B> {
    type Order = B;
    type Error = PaddedError;

    fn decode(&self, code: &[u8]) -> Result<(Instr, usize), Self::Error> {
        let (instr, len) = decode::<B>(code)?;

        // The length reported is what the fetch loop advances by, so the filler
        // has to be counted — and checked, or a truncated stream would slide
        // into the next instruction.
        match code.get(len) {
            Some(&FILLER) => Ok((instr, len + 1)),
            _ => Err(PaddedError::MissingFiller),
        }
    }
}

/// Why padded bytes did not begin an instruction.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PaddedError {
    /// The instruction itself did not decode.
    #[error(transparent)]
    Instruction(#[from] DecodeError),

    /// The instruction decoded, but its filler byte is not there.
    #[error("the filler byte is missing")]
    MissingFiller,
}
