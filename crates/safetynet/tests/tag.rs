//! A `$tag` reference in `safetynet::asm!`, resolved to a variant's discriminant
//! word at compile time and run on the VM.

#![allow(clippy::expect_used)]

use safetynet::encoding::decode;
use safetynet::image::{Layout, Sizes};
use safetynet::{Artifact, Be, ByteOrder, Instr, Le, VmValue};

#[derive(VmValue, Clone, Copy)]
#[repr(u8)]
enum Op {
    Halt = 0,
    Add = 7,
    Xor = 200,
}

/// The first instruction the program pushes, disassembled.
fn first_push<B: ByteOrder>(artifact: Artifact<B>) -> Instr {
    let layout = Layout::new(Sizes {
        stack: 64,
        ..Sizes::default()
    })
    .expect("fits");
    let program = artifact.finalize(&layout).expect("finalizes");
    decode::<B>(program.code()).expect("decodes").0
}

macro_rules! push_tag {
    ($order:ident, $($variant:tt)*) => {
        safetynet::asm!($order {
        entry:
            $tag $($variant)*
            halt
        })
    };
}

/// A `$tag` bakes the variant's discriminant into a `push64`.
#[test]
fn a_tag_pushes_the_discriminant() {
    let imm = |instr| match instr {
        Instr::Push64(op) => op.imm,
        other => panic!("expected a push64, got {other:?}"),
    };
    assert_eq!(imm(first_push(push_tag!(Le, Op::Halt))), 0);
    assert_eq!(imm(first_push(push_tag!(Le, Op::Add))), 7);
    assert_eq!(imm(first_push(push_tag!(Le, Op::Xor))), 200);
}

/// The discriminant is the same word whatever order the bytes were baked in.
#[test]
fn the_discriminant_is_order_independent() {
    let of = |instr| match instr {
        Instr::Push64(op) => op.imm,
        other => panic!("got {other:?}"),
    };
    assert_eq!(
        of(first_push(push_tag!(Le, Op::Xor))),
        of(first_push(push_tag!(Be, Op::Xor))),
    );
}
