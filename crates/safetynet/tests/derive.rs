//! `#[derive(VmLayout)]`: it must lay out and marshal exactly as a hand impl.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use safetynet::{Be, ByteOrder, Bytes, Field, Le, Tail, TypeLayout, VmLayout, VmValue};

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

/// The oracle: `Header`'s layout and marshalling written out by hand.
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

    fn marshal<B: ByteOrder>(&self, mem: &mut [u8], _: &mut Tail) {
        mem.get_mut(0..4)
            .expect("sized")
            .copy_from_slice(&B::write_u32(self.seq));
        if let Some(slot) = mem.get_mut(4) {
            *slot = self.flags;
        }
    }

    fn unmarshal<B: ByteOrder>(mem: &[u8], _: &[u8]) -> Self {
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

/// The derive marshals byte-for-byte what the hand impl does, in either order.
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
    derived.marshal::<B>(&mut a, &mut Tail::new(Header::SIZE));
    hand.marshal::<B>(&mut b, &mut Tail::new(HandHeader::SIZE));
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
    packet.marshal::<B>(&mut mem, &mut Tail::new(Packet::SIZE));
    assert_eq!(Packet::unmarshal::<B>(&mem, &mem), packet);
}

#[test]
fn round_trips_le() {
    round_trips::<Le>();
}

#[test]
fn round_trips_be() {
    round_trips::<Be>();
}

#[derive(VmLayout, Clone, Debug, PartialEq, Eq)]
struct Msg {
    kind: u8,
    body: Bytes,
    name: String,
    tag: u64,
}

/// The oracle for `Msg`: two regions, each an eight-byte header at a fixed
/// offset, with their content appended to the tail in field order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HandMsg {
    kind: u8,
    body: Vec<u8>,
    name: String,
    tag: u64,
}

impl VmLayout for HandMsg {
    const LAYOUT: &'static TypeLayout = &TypeLayout::new(&[
        Field::new("kind", 0, 1, None),
        Field::new("body", 4, 8, Some(Bytes::LAYOUT)),
        Field::new("name", 12, 8, Some(Bytes::LAYOUT)),
        Field::new("tag", 24, 8, None),
    ]);
    const SIZE: usize = 32;
    const ALIGN: usize = 8;

    fn marshal<B: ByteOrder>(&self, mem: &mut [u8], tail: &mut Tail) {
        mem[0] = self.kind;
        let (off, len) = tail.push(&self.body);
        mem[4..8].copy_from_slice(&B::write_u32(off));
        mem[8..12].copy_from_slice(&B::write_u32(len));
        let (off, len) = tail.push(self.name.as_bytes());
        mem[12..16].copy_from_slice(&B::write_u32(off));
        mem[16..20].copy_from_slice(&B::write_u32(len));
        mem[24..32].copy_from_slice(&B::write_u64(self.tag));
    }

    fn unmarshal<B: ByteOrder>(_: &[u8], _: &[u8]) -> Self {
        unreachable!("the oracle only marshals")
    }
}

fn sample_msg() -> (Msg, HandMsg) {
    let derived = Msg {
        kind: 3,
        body: Bytes::from("payload"),
        name: String::from("nm"),
        tag: 0x0102_0304_0506_0708,
    };
    let hand = HandMsg {
        kind: 3,
        body: b"payload".to_vec(),
        name: String::from("nm"),
        tag: 0x0102_0304_0506_0708,
    };
    (derived, hand)
}

/// A region field is an eight-byte header the packing rules place like any
/// other four-aligned field; the content is not in the fixed part at all.
#[test]
fn a_region_field_is_its_header() {
    assert_eq!((Msg::SIZE, Msg::ALIGN), (32, 8));
    assert_eq!(offset(Msg::LAYOUT, &["body"]), 4);
    assert_eq!(offset(Msg::LAYOUT, &["body", "off"]), 4);
    assert_eq!(offset(Msg::LAYOUT, &["body", "len"]), 8);
    assert_eq!(offset(Msg::LAYOUT, &["name", "len"]), 16);
    assert_eq!(offset(Msg::LAYOUT, &["tag"]), 24);
}

/// The derive lays out two regions exactly as the hand impl does, header and
/// tail alike, in either order.
fn regions_match_the_hand_impl<B: ByteOrder>() {
    let (derived, hand) = sample_msg();

    let mut a = [0u8; Msg::SIZE];
    let mut a_tail = Tail::new(Msg::SIZE);
    derived.marshal::<B>(&mut a, &mut a_tail);

    let mut b = [0u8; HandMsg::SIZE];
    let mut b_tail = Tail::new(HandMsg::SIZE);
    hand.marshal::<B>(&mut b, &mut b_tail);

    assert_eq!(a, b);
    assert_eq!(a_tail, b_tail);
    assert_eq!(a_tail.as_slice(), b"payloadnm");

    let mut input = a.to_vec();
    input.extend(a_tail.into_bytes());
    assert_eq!(Msg::unmarshal::<B>(&input, &input), derived);
}

#[test]
fn regions_match_the_hand_impl_le() {
    regions_match_the_hand_impl::<Le>();
}

#[test]
fn regions_match_the_hand_impl_be() {
    regions_match_the_hand_impl::<Be>();
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
