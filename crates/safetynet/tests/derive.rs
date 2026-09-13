//! `#[derive(VmLayout)]`: it must lay out and marshal exactly as a hand impl.

#![allow(clippy::expect_used)]

use safetynet::{Be, ByteOrder, Field, Le, TypeLayout, VmLayout, VmValue};

#[derive(VmLayout, Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    seq: u32,
    flags: u8,
}

#[derive(VmLayout, Clone, Copy, Debug, PartialEq, Eq)]
struct Packet {
    kind: u8,
    header: Header,
    len: u16,
    tag: u64,
}

/// The oracle: `Header` written out by hand, exactly as Phase 4 did it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandHeader {
    seq: u32,
    flags: u8,
}

impl VmLayout for HandHeader {
    const LAYOUT: &'static TypeLayout = &TypeLayout::new(&[
        Field::new("seq", 0, 4, None),
        Field::new("flags", 4, 1, None),
    ]);
    const SIZE: usize = 8;
    const ALIGN: usize = 4;

    fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
        mem.get_mut(0..4)
            .expect("sized")
            .copy_from_slice(&B::write_u32(self.seq));
        if let Some(slot) = mem.get_mut(4) {
            *slot = self.flags;
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

fn offset(layout: &TypeLayout, path: &[&str]) -> u32 {
    layout.offset_of(path).expect("a field")
}

/// The derive computes the same offsets, size and alignment the packing rules
/// prescribe — the ones the hand impls spelled out.
#[test]
fn the_layout_follows_the_packing_rules() {
    assert_eq!((Header::SIZE, Header::ALIGN), (8, 4));
    assert_eq!(offset(Header::LAYOUT, &["seq"]), 0);
    assert_eq!(offset(Header::LAYOUT, &["flags"]), 4);

    assert_eq!((Packet::SIZE, Packet::ALIGN), (24, 8));
    assert_eq!(offset(Packet::LAYOUT, &["kind"]), 0);
    assert_eq!(offset(Packet::LAYOUT, &["header"]), 4);
    assert_eq!(offset(Packet::LAYOUT, &["header", "seq"]), 4);
    assert_eq!(offset(Packet::LAYOUT, &["header", "flags"]), 8);
    assert_eq!(offset(Packet::LAYOUT, &["len"]), 12);
    assert_eq!(offset(Packet::LAYOUT, &["tag"]), 16);
}

/// A scalar field records no nested layout; an aggregate field records its own.
#[test]
fn nested_layouts_are_recorded() {
    let field = |layout: &'static TypeLayout, name| {
        layout
            .fields()
            .iter()
            .find(|f| f.name() == name)
            .copied()
            .expect("a field")
    };
    assert!(field(Packet::LAYOUT, "kind").nested().is_none());
    assert_eq!(
        field(Packet::LAYOUT, "header").nested(),
        Some(Header::LAYOUT)
    );
}

/// The whole point: the derive marshals byte-for-byte what the hand impl does,
/// in either order.
fn derive_matches_the_hand_impl<B: ByteOrder>() {
    let derived = Header {
        seq: 0xdead_beef,
        flags: 0x5a,
    };
    let hand = HandHeader {
        seq: 0xdead_beef,
        flags: 0x5a,
    };

    let mut a = [0u8; Header::SIZE];
    let mut b = [0u8; HandHeader::SIZE];
    derived.marshal::<B>(&mut a);
    hand.marshal::<B>(&mut b);
    assert_eq!(a, b);
}

#[test]
fn derive_matches_the_hand_impl_le() {
    derive_matches_the_hand_impl::<Le>();
}

#[test]
fn derive_matches_the_hand_impl_be() {
    derive_matches_the_hand_impl::<Be>();
}

/// A nested struct round-trips through the image, padding and all, both orders.
fn round_trips<B: ByteOrder>() {
    let packet = Packet {
        kind: 0xa7,
        header: Header {
            seq: 0x1122_3344,
            flags: 0x0f,
        },
        len: 0x6070,
        tag: 0x0102_0304_0506_0708,
    };

    let mut mem = [0u8; Packet::SIZE];
    packet.marshal::<B>(&mut mem);
    assert_eq!(Packet::unmarshal::<B>(&mem), packet);
}

#[test]
fn round_trips_le() {
    round_trips::<Le>();
}

#[test]
fn round_trips_be() {
    round_trips::<Be>();
}

#[derive(VmValue, Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    A,
    B,
    C,
}

#[derive(VmValue, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Tag {
    Lo = 3,
    Hi = 200,
}

/// A field-less enum round-trips through its discriminant; an unknown word lands
/// on the first variant rather than an invalid value.
#[test]
fn a_unit_enum_round_trips() {
    for kind in [Kind::A, Kind::B, Kind::C] {
        assert_eq!(Kind::from_word(kind.to_word()), kind);
    }
    assert_eq!(Kind::A.to_word(), 0);
    assert_eq!(Kind::C.to_word(), 2);
    assert_eq!(Kind::from_word(99), Kind::A, "unknown -> first");
}

/// Explicit discriminants are the values that cross, and the gaps between them
/// still resolve to the first variant.
#[test]
fn explicit_discriminants_round_trip() {
    assert_eq!((Tag::Lo.to_word(), Tag::Hi.to_word()), (3, 200));
    assert_eq!(Tag::from_word(3), Tag::Lo);
    assert_eq!(Tag::from_word(200), Tag::Hi);
    assert_eq!(Tag::from_word(0), Tag::Lo, "unknown -> first");
}
