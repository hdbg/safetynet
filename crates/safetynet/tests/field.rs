//! A `field T::a.b` reference in `safetynet::asm!`, resolved against a hand
//! written `VmLayout` at finalize.

#![allow(clippy::expect_used)]

use safetynet::encoding::decode;
use safetynet::image::{Layout, Sizes};
use safetynet::{Be, ByteOrder, Field, Instr, Le, TypeLayout, VmLayout};

/// `seq: u32 @ 0`, `flags: u8 @ 4`; align 4, `SIZE` 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    seq: u32,
    flags: u8,
}

impl VmLayout for Header {
    const LAYOUT: &'static TypeLayout = &TypeLayout::new(&[
        Field::new("seq", 0, 4, None),
        Field::new("flags", 4, 1, None),
    ]);
    const SIZE: usize = 8;

    fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
        mem.get_mut(0..4)
            .expect("sized")
            .copy_from_slice(&B::write_u32(self.seq));
        if let Some(byte) = mem.get_mut(4) {
            *byte = self.flags;
        }
    }

    fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self {
        let seq = mem
            .get(0..4)
            .and_then(|b| b.try_into().ok())
            .map(B::read_u32)
            .expect("sized");
        Self {
            seq,
            flags: mem.get(4).copied().unwrap_or_default(),
        }
    }
}

/// `kind: u8 @ 0`, `header @ 4`, `tag: u64 @ 16`; align 8, `SIZE` 24.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Packet {
    kind: u8,
    header: Header,
    tag: u64,
}

impl VmLayout for Packet {
    const LAYOUT: &'static TypeLayout = &TypeLayout::new(&[
        Field::new("kind", 0, 1, None),
        Field::new("header", 4, 8, Some(Header::LAYOUT)),
        Field::new("tag", 16, 8, None),
    ]);
    const SIZE: usize = 24;

    fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
        if let Some(byte) = mem.get_mut(0) {
            *byte = self.kind;
        }
        if let Some(slot) = mem.get_mut(4..12) {
            self.header.marshal::<B>(slot);
        }
        if let Some(slot) = mem.get_mut(16..24) {
            slot.copy_from_slice(&B::write_u64(self.tag));
        }
    }

    fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self {
        let header = mem
            .get(4..12)
            .map(Header::unmarshal::<B>)
            .unwrap_or(Header { seq: 0, flags: 0 });
        let tag = mem
            .get(16..24)
            .and_then(|b| b.try_into().ok())
            .map(B::read_u64)
            .unwrap_or_default();
        Self {
            kind: mem.first().copied().unwrap_or_default(),
            header,
            tag,
        }
    }
}

/// The first instruction a program pushes, disassembled.
fn first_push<B: ByteOrder>(artifact: safetynet::Artifact<B>) -> Instr {
    let layout = Layout::new(Sizes {
        stack: 64,
        ..Sizes::default()
    })
    .expect("fits");
    let program = artifact.finalize(&layout).expect("finalizes");

    let code = program.code();
    decode::<B>(code).expect("decodes").0
}

macro_rules! push_field {
    ($order:ident, $($path:tt)*) => {
        safetynet::asm!($order {
        entry:
            field $($path)*
            halt
        })
    };
}

/// A top-level field resolves to its offset.
#[test]
fn a_top_level_field_offset() {
    match first_push(push_field!(Le, Packet::tag)) {
        Instr::Push32(op) => assert_eq!(op.imm, 16),
        other => panic!("expected a pushed offset, got {other:?}"),
    }
}

/// A nested field resolves to the sum of the offsets along the path.
#[test]
fn a_nested_field_offset() {
    match first_push(push_field!(Le, Packet::header.seq)) {
        Instr::Push32(op) => assert_eq!(op.imm, 4),
        other => panic!("expected a pushed offset, got {other:?}"),
    }
    match first_push(push_field!(Le, Packet::header.flags)) {
        Instr::Push32(op) => assert_eq!(op.imm, 8),
        other => panic!("expected a pushed offset, got {other:?}"),
    }
}

/// The resolution is the same whatever order the bytes were baked in.
#[test]
fn the_offset_is_order_independent() {
    let le = match first_push(push_field!(Le, Packet::header.seq)) {
        Instr::Push32(op) => op.imm,
        other => panic!("got {other:?}"),
    };
    let be = match first_push(push_field!(Be, Packet::header.seq)) {
        Instr::Push32(op) => op.imm,
        other => panic!("got {other:?}"),
    };
    assert_eq!(le, be);
}
