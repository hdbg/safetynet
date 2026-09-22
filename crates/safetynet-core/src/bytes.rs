//! Variable-length byte regions: content that lives past the fixed part of its
//! aggregate, found again through a header that never moves.

use core::ops::Deref;

use crate::ByteOrder;
use crate::marshal::{Field, Tail, TypeLayout, VmLayout};

/// Owned bytes that cross into the VM as a length-prefixed region.
///
/// In the image the field itself is an eight-byte header — where the content
/// starts in `.input`, then how many bytes there are — and the content sits
/// after every fixed-size field. The header's own offset therefore stays a
/// compile-time constant, which is what keeps the linker `const`. `String` and
/// `Vec<u8>` lay out exactly the same way; this type is the spelling that says
/// so at the field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Bytes(Vec<u8>);

impl Bytes {
    /// A region holding `bytes`.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The content.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Takes the content.
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for Bytes {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<&str> for Bytes {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

impl From<String> for Bytes {
    fn from(text: String) -> Self {
        Self(text.into_bytes())
    }
}

/// What a header field holds: an offset or a length within the image, which
/// the layout measures as a `u32` (an address fits one by construction).
type HeaderField = u32;

/// Bytes one header field occupies, from the field type's own layout.
const FIELD_SIZE: usize = <HeaderField as VmLayout>::SIZE;

/// Where the content's offset sits in the header.
const OFF: usize = 0;

/// Where the content's length sits: right after the offset, which is already
/// aligned for it since both are the same type.
const LEN: usize = OFF + FIELD_SIZE;

/// The header every region shares: where its content starts, then how long it
/// is. Named fields, so the linker resolves `.off` and `.len` like any other.
const HEADER: &TypeLayout = &TypeLayout::new(&[
    Field::new("off", OFF as u32, FIELD_SIZE as u32, None),
    Field::new("len", LEN as u32, FIELD_SIZE as u32, None),
]);

/// Bytes the header occupies.
const HEADER_SIZE: usize = LEN + FIELD_SIZE;

/// The header's alignment: its fields', since it is nothing but them.
const HEADER_ALIGN: usize = <HeaderField as VmLayout>::ALIGN;

/// Writes one header field through the field type's own marshalling.
fn write_field<B: ByteOrder>(slot: &mut [u8], at: usize, value: HeaderField, tail: &mut Tail) {
    if let Some(field) = slot.get_mut(at..) {
        value.marshal::<B>(field, tail);
    }
}

/// Reads one header field; a slot too short to hold it reads as zero.
fn read_field<B: ByteOrder>(slot: &[u8], at: usize) -> HeaderField {
    slot.get(at..)
        .map(|field| HeaderField::unmarshal::<B>(field, slot))
        .unwrap_or_default()
}

/// Appends `content` to the tail and writes its header into `slot`.
fn marshal_region<B: ByteOrder>(content: &[u8], slot: &mut [u8], tail: &mut Tail) {
    let (off, len) = tail.push(content);
    write_field::<B>(slot, OFF, off, tail);
    write_field::<B>(slot, LEN, len, tail);
}

/// Follows the header in `slot` into `input`.
///
/// The header is data, so it is not trusted: content that runs past the input
/// is cut short, and a start past the input is empty.
fn region_content<'a, B: ByteOrder>(slot: &[u8], input: &'a [u8]) -> &'a [u8] {
    let start = read_field::<B>(slot, OFF) as usize;
    let end = start.saturating_add(read_field::<B>(slot, LEN) as usize);
    input
        .get(start..end)
        .or_else(|| input.get(start..))
        .unwrap_or(&[])
}

impl VmLayout for Bytes {
    const LAYOUT: &'static TypeLayout = HEADER;
    const SIZE: usize = HEADER_SIZE;
    const ALIGN: usize = HEADER_ALIGN;

    fn marshal<B: ByteOrder>(&self, slot: &mut [u8], tail: &mut Tail) {
        marshal_region::<B>(&self.0, slot, tail);
    }

    fn unmarshal<B: ByteOrder>(slot: &[u8], input: &[u8]) -> Self {
        Self(region_content::<B>(slot, input).to_vec())
    }
}

impl VmLayout for Vec<u8> {
    const LAYOUT: &'static TypeLayout = HEADER;
    const SIZE: usize = HEADER_SIZE;
    const ALIGN: usize = HEADER_ALIGN;

    fn marshal<B: ByteOrder>(&self, slot: &mut [u8], tail: &mut Tail) {
        marshal_region::<B>(self, slot, tail);
    }

    fn unmarshal<B: ByteOrder>(slot: &[u8], input: &[u8]) -> Self {
        region_content::<B>(slot, input).to_vec()
    }
}

/// A string crosses as its UTF-8 bytes. Coming back, content that is not UTF-8
/// reads as empty: the header was data, and data does not get to make a
/// `String` invalid.
impl VmLayout for String {
    const LAYOUT: &'static TypeLayout = HEADER;
    const SIZE: usize = HEADER_SIZE;
    const ALIGN: usize = HEADER_ALIGN;

    fn marshal<B: ByteOrder>(&self, slot: &mut [u8], tail: &mut Tail) {
        marshal_region::<B>(self.as_bytes(), slot, tail);
    }

    fn unmarshal<B: ByteOrder>(slot: &[u8], input: &[u8]) -> Self {
        Self::from_utf8(region_content::<B>(slot, input).to_vec()).unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::{Be, Le};

    /// A fixed part of one region, then the tail, as a call site lays them out.
    fn image<B: ByteOrder>(value: &impl VmLayout) -> Vec<u8> {
        let mut fixed = vec![0u8; HEADER_SIZE];
        let mut tail = Tail::new(HEADER_SIZE);
        value.marshal::<B>(&mut fixed, &mut tail);
        fixed.extend(tail.into_bytes());
        fixed
    }

    fn round_trips<B: ByteOrder>() {
        let bytes = Bytes::from("hello");
        let input = image::<B>(&bytes);
        assert_eq!(input.len(), HEADER_SIZE + 5);
        assert_eq!(Bytes::unmarshal::<B>(&input, &input), bytes);

        let text = String::from("héllo");
        let input = image::<B>(&text);
        assert_eq!(String::unmarshal::<B>(&input, &input), text);

        let raw = vec![0u8, 255, 7];
        let input = image::<B>(&raw);
        assert_eq!(Vec::<u8>::unmarshal::<B>(&input, &input), raw);
    }

    #[test]
    fn round_trips_le() {
        round_trips::<Le>();
    }

    #[test]
    fn round_trips_be() {
        round_trips::<Be>();
    }

    /// The header records where the tail begins, in the order asked for.
    #[test]
    fn the_header_points_past_the_fixed_part() {
        let input = image::<Le>(&Bytes::from("ab"));
        assert_eq!(input.get(0..4), Some(&8u32.to_le_bytes()[..]), "off");
        assert_eq!(input.get(4..8), Some(&2u32.to_le_bytes()[..]), "len");
        assert_eq!(input.get(8..), Some(&b"ab"[..]));

        let input = image::<Be>(&Bytes::from("ab"));
        assert_eq!(input.get(0..4), Some(&8u32.to_be_bytes()[..]));
    }

    /// An empty region is a header and nothing after it.
    #[test]
    fn an_empty_region_appends_nothing() {
        let input = image::<Le>(&Bytes::default());
        assert_eq!(input.len(), HEADER_SIZE);
        assert!(Bytes::unmarshal::<Le>(&input, &input).is_empty());
    }

    /// Two regions in one tail land one after the other.
    #[test]
    fn regions_share_a_tail_without_overlapping() {
        let mut fixed = vec![0u8; 16];
        let mut tail = Tail::new(16);
        Bytes::from("one").marshal::<Le>(&mut fixed[0..8], &mut tail);
        Bytes::from("two").marshal::<Le>(&mut fixed[8..16], &mut tail);
        fixed.extend(tail.into_bytes());

        assert_eq!(
            Bytes::unmarshal::<Le>(&fixed[0..8], &fixed),
            Bytes::from("one")
        );
        assert_eq!(
            Bytes::unmarshal::<Le>(&fixed[8..16], &fixed),
            Bytes::from("two")
        );
    }

    /// A header is data: pointing past the input yields nothing, and a length
    /// that overruns is cut at the end of the input.
    #[test]
    fn a_forged_header_cannot_read_past_the_input() {
        let mut input = image::<Le>(&Bytes::from("abc"));
        input[4..8].copy_from_slice(&100u32.to_le_bytes());
        assert_eq!(Bytes::unmarshal::<Le>(&input, &input), Bytes::from("abc"));

        input[0..4].copy_from_slice(&1000u32.to_le_bytes());
        assert!(Bytes::unmarshal::<Le>(&input, &input).is_empty());

        input[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        input[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Bytes::unmarshal::<Le>(&input, &input).is_empty());
    }

    /// Content that is not UTF-8 does not become a `String`.
    #[test]
    fn a_string_refuses_bytes_that_are_not_utf8() {
        let input = image::<Le>(&vec![0xffu8, 0xfe]);
        assert_eq!(String::unmarshal::<Le>(&input, &input), "");
    }

    /// The header's fields resolve by name, as the lowerer reads them.
    #[test]
    fn the_header_layout_names_its_fields() {
        assert_eq!(Bytes::LAYOUT.offset_of(&["off"]), Some(0));
        assert_eq!(Bytes::LAYOUT.offset_of(&["len"]), Some(4));
        assert_eq!(Bytes::LAYOUT.size_of(&["len"]), Some(4));
        assert_eq!((Bytes::SIZE, Bytes::ALIGN), (8, 4));
        assert_eq!(String::LAYOUT, Bytes::LAYOUT);
        assert_eq!(<Vec<u8> as VmLayout>::LAYOUT, Bytes::LAYOUT);
    }
}
