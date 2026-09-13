//! Tests for the wire format.

use super::*;
use crate::isa::{Alloc, FrameSize, Halt, Push8, Push32, Push64};
use crate::samples::instructions;
use crate::{Be, Le};

/// Encodes one instruction on its own.
fn bytes_of<B: ByteOrder>(instr: Instr) -> Vec<u8> {
    let mut out = Vec::new();
    encode::<B>(instr, &mut out).expect("encodes");
    out
}

/// Order-generic body. Every behavioural test is instantiated for both orders,
/// because the order is a source literal and the one nobody builds is otherwise
/// the one nobody tests.
fn round_trips<B: ByteOrder>() {
    for instr in instructions() {
        let bytes = bytes_of::<B>(instr);
        let (decoded, len) = decode::<B>(&bytes).expect("decodes");

        assert_eq!(decoded, instr);
        assert_eq!(len, bytes.len(), "{instr:?} reported the wrong length");
    }
}

#[test]
fn round_trips_le() {
    round_trips::<Le>();
}

#[test]
fn round_trips_be() {
    round_trips::<Be>();
}

/// Nothing frames an instruction: a program is the concatenation of its parts,
/// and decoding walks it by adding up lengths. This is the property a byte
/// offset being a program point rests on.
fn a_stream_is_its_instructions_packed_end_to_end<B: ByteOrder>() {
    let program = instructions();

    let mut code = Vec::new();
    let written = encode_all::<B>(&program, &mut code).expect("encodes");

    let measured: usize = program
        .iter()
        .map(|instr| encoded_len(*instr).expect("measures"))
        .sum();
    assert_eq!(
        written, measured,
        "the stream carries no framing of its own"
    );
    assert_eq!(written, code.len());

    let mut offset = 0;
    let mut decoded = Vec::new();
    while offset < code.len() {
        let (instr, len) = decode::<B>(code.get(offset..).expect("in bounds")).expect("decodes");
        offset += len;
        decoded.push(instr);
    }

    assert_eq!(decoded, program);
    assert_eq!(offset, code.len(), "the walk landed on every boundary");
}

#[test]
fn a_stream_is_its_instructions_packed_end_to_end_le() {
    a_stream_is_its_instructions_packed_end_to_end::<Le>();
}

#[test]
fn a_stream_is_its_instructions_packed_end_to_end_be() {
    a_stream_is_its_instructions_packed_end_to_end::<Be>();
}

/// What block layout relies on: the measured size is the size that will be
/// written, in either order.
#[test]
fn measured_length_matches_the_bytes_written() {
    for instr in instructions() {
        let measured = encoded_len(instr).expect("measures");

        assert_eq!(measured, bytes_of::<Le>(instr).len(), "{instr:?}");
        assert_eq!(measured, bytes_of::<Be>(instr).len(), "{instr:?}");
    }
}

/// The layout, pinned. The tag is the variant's position, so inserting an
/// instruction into the middle of the table renumbers everything after it and
/// every image encoded before stops decoding. That should be a decision rather
/// than an accident, which is what this test makes it.
#[test]
fn the_layout_is_a_tag_then_fixed_width_operands() {
    assert_eq!(bytes_of::<Le>(Halt.into()), [0x00]);
    assert_eq!(bytes_of::<Le>(Push8 { imm: 0xa5 }.into()), [0x01, 0xa5]);

    let push = Push32 { imm: 0x1234_5678 }.into();
    assert_eq!(bytes_of::<Le>(push), [0x02, 0x78, 0x56, 0x34, 0x12]);
    assert_eq!(bytes_of::<Be>(push), [0x02, 0x12, 0x34, 0x56, 0x78]);

    let alloc = Alloc {
        n: FrameSize::new(24).expect("24 is word-aligned"),
    }
    .into();
    assert_eq!(bytes_of::<Le>(alloc), [0x05, 0x18, 0x00]);
    assert_eq!(bytes_of::<Be>(alloc), [0x05, 0x00, 0x18]);
}

/// Tags are variable-length integers, so a set that grows past 127 instructions
/// silently widens every instruction by a byte — including the ones already
/// numbered.
#[test]
fn every_tag_fits_in_one_byte() {
    for instr in instructions() {
        let bytes = bytes_of::<Le>(instr);
        let tag = bytes.first().copied().expect("a tag byte");

        assert!(tag < 0x80, "{} has a two-byte tag", instr.mnemonic());
    }
}

/// The order has to reach the operands, not just the image around them — and
/// reaching them means a program read in the wrong order is wrong *quietly*,
/// which is why the order is a type parameter rather than a flag.
#[test]
fn byte_order_reaches_the_operands() {
    let single = Push8 { imm: 0xa5 }.into();
    assert_eq!(
        bytes_of::<Le>(single),
        bytes_of::<Be>(single),
        "a one-byte operand has no order"
    );

    let wide = Push64 {
        imm: 0x0102_0304_0506_0708,
    }
    .into();
    assert_ne!(bytes_of::<Le>(wide), bytes_of::<Be>(wide));

    let bytes = bytes_of::<Le>(Push32 { imm: 0x1234_5678 }.into());
    let (misread, _) = decode::<Be>(&bytes).expect("still decodes");
    assert_eq!(misread, Push32 { imm: 0x7856_3412 }.into());
}

/// A tag past the last variant is not an instruction.
#[test]
fn an_unassigned_tag_is_rejected() {
    decode::<Le>(&[0x7f]).expect_err("0x7f names no instruction");
}

/// Trailing bytes are the next instruction's business, but missing ones are an
/// error: a stream that ends mid-operand is not code.
#[test]
fn truncated_operands_are_rejected() {
    let bytes = bytes_of::<Le>(Push64 { imm: u64::MAX }.into());

    decode::<Le>(bytes.get(..4).expect("in bounds")).expect_err("the operand is short");
    decode::<Le>(&[]).expect_err("there is no tag");
}

/// An operand's own validity rule is part of decoding: a frame size that would
/// leave `SP` misaligned never becomes an instruction.
#[test]
fn a_misaligned_frame_size_is_rejected() {
    let alloc = Alloc {
        n: FrameSize::new(24).expect("24 is word-aligned"),
    }
    .into();
    let mut bytes = bytes_of::<Le>(alloc);

    // Little-endian: the operand's low byte sits directly after the tag.
    *bytes.get_mut(1).expect("low operand byte") = 7;

    let err = decode::<Le>(&bytes).expect_err("7 is not word-aligned");
    assert!(err.to_string().contains("multiple"), "{err}");
}
