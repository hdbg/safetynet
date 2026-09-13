//! Tests for the machine model and its derives.

use musli::storage::Encoding;

use super::*;
use crate::samples::instructions;

const WIRE: Encoding = Encoding::new();

/// `decode(encode(x)) == x` for every shape of instruction.
#[test]
fn round_trips() {
    for instr in instructions() {
        let bytes = WIRE.to_vec(&instr).expect("encodes");
        let decoded: Instr = WIRE.from_slice(&bytes).expect("decodes");
        assert_eq!(decoded, instr, "round-trip changed the instruction");
    }
}

/// Decoding a concatenated stream must land on every boundary exactly — the
/// property a whole program depends on and a single instruction cannot show.
#[test]
fn decodes_a_stream() {
    let program = instructions();

    let mut bytes = Vec::new();
    for instr in &program {
        bytes.extend_from_slice(&WIRE.to_vec(instr).expect("encodes"));
    }

    let mut cursor: &[u8] = &bytes;
    let mut decoded = Vec::new();
    while !cursor.is_empty() {
        decoded.push(WIRE.decode::<_, Instr>(&mut cursor).expect("decodes"));
    }

    assert_eq!(decoded, program);
}

/// The model assigns no opcode numbers, so the property worth pinning is the
/// one the derive owes us: no two instructions share a tag.
#[test]
fn variants_get_distinct_tags() {
    let mut tags: Vec<(u8, &str)> = instructions()
        .iter()
        .map(|instr| {
            let bytes = WIRE.to_vec(instr).expect("encodes");
            let tag = bytes.first().copied().expect("a tag byte");
            (tag, instr.mnemonic())
        })
        .collect();

    tags.sort_unstable();
    let duplicates: Vec<_> = tags
        .windows(2)
        .filter_map(|pair| match pair {
            [a, b] if a.0 == b.0 => Some((a.0, a.1, b.1)),
            _ => None,
        })
        .collect();
    assert!(
        duplicates.is_empty(),
        "instructions share a tag: {duplicates:?}"
    );
}

/// A tag beyond the last variant is not an instruction.
#[test]
fn unassigned_tag_is_rejected() {
    WIRE.from_slice::<Instr>(&[0xfe])
        .expect_err("0xfe is not a variant");
}

#[test]
fn truncated_operands_are_rejected() {
    let mut bytes = WIRE
        .to_vec(&Instr::from(Push64 { imm: u64::MAX }))
        .expect("encodes");
    bytes.truncate(2);

    WIRE.from_slice::<Instr>(&bytes)
        .expect_err("operands are short");
}

/// A misaligned frame size never becomes an instruction: it is rejected where
/// the bytes are read, not repaired where they are used.
#[test]
fn misaligned_frame_size_fails_to_decode() {
    let frame = FrameSize::new(8).expect("8 is word-aligned");
    let mut bytes = WIRE
        .to_vec(&Instr::from(Alloc { n: frame }))
        .expect("encodes");

    // Tag plus a one-byte operand; both 7 and 8 are small enough that swapping
    // them cannot change the encoded length.
    assert_eq!(bytes.len(), 2, "unexpected layout: {bytes:02x?}");
    *bytes.get_mut(1).expect("operand byte") = 7;

    let err = WIRE
        .from_slice::<Instr>(&bytes)
        .expect_err("7 is not word-aligned");
    assert!(err.to_string().contains("multiple"), "{err}");
}

/// The anchor every total pass over the instruction set is checked against, so
/// it has to actually be one instruction of each kind.
#[test]
fn one_of_each_covers_the_table_once() {
    let all = Instr::one_of_each();
    assert_eq!(all.len(), 43);

    let mut mnemonics: Vec<&str> = all.iter().map(|instr| instr.mnemonic()).collect();
    mnemonics.sort_unstable();
    mnemonics.dedup();
    assert_eq!(mnemonics.len(), all.len(), "two entries share a mnemonic");
}

/// Every instruction has a printed form, and it is the mnemonic plus the
/// operands the table gave it — nothing in the set takes more than one.
#[test]
fn every_instruction_prints_as_its_mnemonic_and_operands() {
    for instr in Instr::one_of_each() {
        let text = instr.to_string();
        let words: Vec<&str> = text.split(' ').collect();

        assert_eq!(words.first().copied(), Some(instr.mnemonic()), "{text}");
        assert!(words.len() <= 2, "more operands than expected: {text}");
    }
}

/// Operands print as plain decimals: what the assembler reads back, not what a
/// `Debug` derive would produce.
#[test]
fn operands_print_as_plain_decimals() {
    let frame = FrameSize::new(24).expect("24 is word-aligned");

    assert_eq!(Instr::from(Halt).to_string(), "halt");
    assert_eq!(
        Instr::from(Push32 { imm: 0xdead_beef }).to_string(),
        "push32 3735928559"
    );
    assert_eq!(Instr::from(Jz { offset: -12 }).to_string(), "jz -12");
    assert_eq!(Instr::from(Alloc { n: frame }).to_string(), "alloc 24");
}

/// The stack effect has to come from the operand, not a constant, or `ALLOC`
/// would be indistinguishable from a no-op to the validator.
#[test]
fn frame_ops_take_their_stack_effect_from_their_operand() {
    let size = FrameSize::new(32).expect("32 is word-aligned");
    assert_eq!(Alloc { n: size }.sp_delta(), 32);
    assert_eq!(Free { n: size }.sp_delta(), -32);

    let empty = FrameSize::new(0).expect("0 is word-aligned");
    assert_eq!(Alloc { n: empty }.sp_delta(), 0);
}

/// Spot-check the shapes the validator cares about most; a sign error here is
/// invisible until a displacement is wrong much later.
#[test]
fn stack_effects_match_the_machine_model() {
    assert_eq!(Push8 { imm: 0 }.sp_delta(), 8, "a push is one word");
    assert_eq!(Drop.sp_delta(), -8);
    assert_eq!(Add.sp_delta(), -8, "two operands in, one out");
    assert_eq!(BitNot.sp_delta(), 0, "one operand in, one out");
    assert_eq!(Ld64.sp_delta(), 0, "address out, value in");
    assert_eq!(St64.sp_delta(), -16, "address and value both consumed");
    assert_eq!(Jmp { offset: 0 }.sp_delta(), 0);
    assert_eq!(Jz { offset: 0 }.sp_delta(), -8, "the condition is consumed");
    assert_eq!(Lds64 { disp: 8 }.sp_delta(), 8);
    assert_eq!(Sts64 { disp: 8 }.sp_delta(), -8);
}
