//! Aggregates crossing the host/VM boundary: their canonical flat layout, and
//! how they become image bytes and come back.

use crate::ByteOrder;

/// The canonical flat layout of an aggregate: its fields, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeLayout {
    fields: &'static [Field],
}

impl TypeLayout {
    /// A layout with these fields, laid out already.
    pub const fn new(fields: &'static [Field]) -> Self {
        Self { fields }
    }

    /// The fields, in layout order.
    pub const fn fields(&self) -> &'static [Field] {
        self.fields
    }

    /// The byte offset a dotted field path resolves to, or `None` if a name is
    /// unknown or a non-final name is a scalar.
    ///
    /// `const` so the linker can bake the offset in at compile time.
    pub const fn offset_of(&self, path: &[&str]) -> Option<u32> {
        let (&last, parents) = match path.split_last() {
            Some(split) => split,
            None => return None,
        };

        let mut here = self;
        let mut base: u32 = 0;
        let mut rest = parents;
        while let Some((&name, tail)) = rest.split_first() {
            let field = match here.field(name) {
                Some(field) => field,
                None => return None,
            };
            base = match base.checked_add(field.offset) {
                Some(sum) => sum,
                None => return None,
            };
            here = match field.nested {
                Some(inner) => inner,
                None => return None,
            };
            rest = tail;
        }

        match here.field(last) {
            Some(field) => base.checked_add(field.offset),
            None => None,
        }
    }

    /// The byte size of the field a dotted path resolves to, or `None` if a name
    /// is unknown or a non-final name is a scalar.
    ///
    /// `const` so the linker can pick a load's width at compile time.
    pub const fn size_of(&self, path: &[&str]) -> Option<u32> {
        let (&last, parents) = match path.split_last() {
            Some(split) => split,
            None => return None,
        };

        let mut here = self;
        let mut rest = parents;
        while let Some((&name, tail)) = rest.split_first() {
            let field = match here.field(name) {
                Some(field) => field,
                None => return None,
            };
            here = match field.nested {
                Some(inner) => inner,
                None => return None,
            };
            rest = tail;
        }

        match here.field(last) {
            Some(field) => Some(field.size),
            None => None,
        }
    }

    const fn field(&self, name: &str) -> Option<Field> {
        let mut rest = self.fields;
        while let Some((field, tail)) = rest.split_first() {
            if str_eq(field.name, name) {
                return Some(*field);
            }
            rest = tail;
        }
        None
    }
}

/// One field: its name, where it sits, how wide, and its own layout if nested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    name: &'static str,
    offset: u32,
    size: u32,
    nested: Option<&'static TypeLayout>,
}

impl Field {
    /// A field at `offset`, `size` bytes wide; `nested` if it is an aggregate.
    pub const fn new(
        name: &'static str,
        offset: u32,
        size: u32,
        nested: Option<&'static TypeLayout>,
    ) -> Self {
        Self {
            name,
            offset,
            size,
            nested,
        }
    }

    /// The field name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Byte offset from the start of the aggregate.
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Bytes occupied.
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// The field's own layout, if it is an aggregate.
    pub const fn nested(&self) -> Option<&'static TypeLayout> {
        self.nested
    }
}

/// A type that lays out in the image and can cross the host/VM boundary.
///
/// Aggregates get a derive; the scalars implement it below with an empty layout,
/// so a derive can size, place and marshal every field the same way rather than
/// telling a scalar from a nested struct by its tokens.
pub trait VmLayout: Sized {
    /// The canonical flat layout: the single source of truth for offsets. Empty
    /// for a scalar.
    const LAYOUT: &'static TypeLayout;

    /// Total bytes occupied in the image.
    const SIZE: usize;

    /// Alignment: a scalar's is its size, an aggregate's its widest field.
    const ALIGN: usize;

    /// Writes `self` into the first [`SIZE`](Self::SIZE) bytes of `mem`, in
    /// order `B`. The caller sizes `mem`; padding between fields is left as it
    /// was found.
    fn marshal<B: ByteOrder>(&self, mem: &mut [u8]);

    /// Reads a value back out of `mem`, interpreting multi-byte fields in order
    /// `B`. The inverse of [`marshal`](Self::marshal).
    fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self;
}

/// A scalar is a degenerate layout: no fields, and its own bytes are the whole
/// of it.
const SCALAR: &TypeLayout = &TypeLayout::new(&[]);

/// Implements [`VmLayout`] for a single-byte scalar, where byte order does not
/// enter into it.
macro_rules! byte_layout {
    ($ty:ty, |$byte:ident| $from:expr) => {
        impl VmLayout for $ty {
            const LAYOUT: &'static TypeLayout = SCALAR;
            const SIZE: usize = 1;
            const ALIGN: usize = 1;

            fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
                if let Some(slot) = mem.first_mut() {
                    *slot = *self as u8;
                }
            }

            fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self {
                let $byte = mem.first().copied().unwrap_or_default();
                $from
            }
        }
    };
}

byte_layout!(u8, |byte| byte);
byte_layout!(i8, |byte| byte as i8);
byte_layout!(bool, |byte| byte != 0);

/// Implements [`VmLayout`] for a multi-byte scalar through its unsigned twin, so
/// the two's-complement bits move whichever the sign.
macro_rules! word_layout {
    ($ty:ty, $size:literal, $unsigned:ty, $write:ident, $read:ident) => {
        impl VmLayout for $ty {
            const LAYOUT: &'static TypeLayout = SCALAR;
            const SIZE: usize = $size;
            const ALIGN: usize = $size;

            fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
                if let Some(slot) = mem.get_mut(..$size) {
                    slot.copy_from_slice(&B::$write(*self as $unsigned));
                }
            }

            fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self {
                mem.get(..$size)
                    .and_then(|slot| slot.try_into().ok())
                    .map(B::$read)
                    .unwrap_or_default() as $ty
            }
        }
    };
}

word_layout!(u16, 2, u16, write_u16, read_u16);
word_layout!(i16, 2, u16, write_u16, read_u16);
word_layout!(u32, 4, u32, write_u32, read_u32);
word_layout!(i32, 4, u32, write_u32, read_u32);
word_layout!(u64, 8, u64, write_u64, read_u64);
word_layout!(i64, 8, u64, write_u64, read_u64);

/// Byte-for-byte string equality, in `const`.
const fn str_eq(a: &str, b: &str) -> bool {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.split_first(), b.split_first()) {
            (Some((x, at)), Some((y, bt))) => {
                if *x != *y {
                    return false;
                }
                a = at;
                b = bt;
            }
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Be, Le};

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
        const ALIGN: usize = 4;

        fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
            put(mem, 0, &B::write_u32(self.seq));
            put(mem, 4, &[self.flags]);
        }

        fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self {
            Self {
                seq: B::read_u32(get(mem, 0)),
                flags: get::<1>(mem, 4)[0],
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Packet {
        kind: u8,
        header: Header,
        len: u16,
        tag: u64,
    }

    impl VmLayout for Packet {
        const LAYOUT: &'static TypeLayout = &TypeLayout::new(&[
            Field::new("kind", 0, 1, None),
            Field::new("header", 4, 8, Some(Header::LAYOUT)),
            Field::new("len", 12, 2, None),
            Field::new("tag", 16, 8, None),
        ]);
        const SIZE: usize = 24;
        const ALIGN: usize = 8;

        fn marshal<B: ByteOrder>(&self, mem: &mut [u8]) {
            put(mem, 0, &[self.kind]);
            self.header
                .marshal::<B>(mem.get_mut(4..12).expect("header fits"));
            put(mem, 12, &B::write_u16(self.len));
            put(mem, 16, &B::write_u64(self.tag));
        }

        fn unmarshal<B: ByteOrder>(mem: &[u8]) -> Self {
            Self {
                kind: get::<1>(mem, 0)[0],
                header: Header::unmarshal::<B>(mem.get(4..12).expect("header fits")),
                len: B::read_u16(get(mem, 12)),
                tag: B::read_u64(get(mem, 16)),
            }
        }
    }

    fn put(mem: &mut [u8], off: usize, bytes: &[u8]) {
        mem.get_mut(off..off + bytes.len())
            .expect("mem sized to SIZE")
            .copy_from_slice(bytes);
    }

    fn get<const N: usize>(mem: &[u8], off: usize) -> [u8; N] {
        mem.get(off..off + N)
            .and_then(|slice| slice.try_into().ok())
            .expect("mem sized to SIZE")
    }

    fn sample() -> Packet {
        Packet {
            kind: 0xa7,
            header: Header {
                seq: 0x1122_3344,
                flags: 0x5f,
            },
            len: 0x6070,
            tag: 0x0102_0304_0506_0708,
        }
    }

    fn round_trips<B: ByteOrder>() {
        let packet = sample();
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

    #[test]
    fn orders_disagree_in_the_image() {
        let packet = sample();
        let mut le = [0u8; Packet::SIZE];
        let mut be = [0u8; Packet::SIZE];
        packet.marshal::<Le>(&mut le);
        packet.marshal::<Be>(&mut be);
        assert_ne!(le, be);
    }

    #[test]
    fn fields_land_at_their_declared_offsets() {
        let mut mem = [0xeeu8; Packet::SIZE];
        sample().marshal::<Le>(&mut mem);

        assert_eq!(mem.first(), Some(&0xa7), "kind");
        assert_eq!(mem.get(1..4), Some(&[0xee, 0xee, 0xee][..]), "pad kept");
        assert_eq!(mem.get(4..8), Some(&[0x44, 0x33, 0x22, 0x11][..]), "seq");
        assert_eq!(mem.get(8), Some(&0x5f), "flags");
        assert_eq!(mem.get(14..16), Some(&[0xee, 0xee][..]), "pad kept");
        assert_eq!(
            mem.get(16..24),
            Some(&0x0102_0304_0506_0708u64.to_le_bytes()[..])
        );
    }

    #[test]
    fn the_descriptor_is_internally_consistent() {
        for field in Packet::LAYOUT.fields() {
            let end = field.offset() as usize + field.size() as usize;
            assert!(end <= Packet::SIZE, "{} overruns", field.name());
        }

        let header = Packet::LAYOUT
            .fields()
            .iter()
            .find(|f| f.name() == "header")
            .expect("header field");
        assert_eq!(header.nested(), Some(Header::LAYOUT));
        assert_eq!(header.size() as usize, Header::SIZE);
    }

    /// A path resolves to the sum of the offsets along it.
    #[test]
    fn offset_of_walks_into_nested_aggregates() {
        let at = |path: &[&str]| Packet::LAYOUT.offset_of(path);
        assert_eq!(at(&["kind"]), Some(0));
        assert_eq!(at(&["header"]), Some(4));
        assert_eq!(at(&["header", "seq"]), Some(4));
        assert_eq!(at(&["header", "flags"]), Some(8));
        assert_eq!(at(&["tag"]), Some(16));
    }

    /// A missing name, an empty path, or descending into a scalar is `None`.
    #[test]
    fn offset_of_rejects_paths_with_no_field() {
        let at = |path: &[&str]| Packet::LAYOUT.offset_of(path);
        assert_eq!(at(&["nope"]), None);
        assert_eq!(at(&["header", "nope"]), None);
        assert_eq!(at(&["kind", "seq"]), None, "kind is a scalar");
        assert_eq!(at(&[]), None);
    }

    /// A scalar lays out as itself: the VM's own width, its own alignment, and no
    /// fields to descend into.
    #[test]
    fn a_scalar_is_a_degenerate_layout() {
        assert_eq!((<u8 as VmLayout>::SIZE, <u8 as VmLayout>::ALIGN), (1, 1));
        assert_eq!(
            (<bool as VmLayout>::SIZE, <bool as VmLayout>::ALIGN),
            (1, 1)
        );
        assert_eq!((<u16 as VmLayout>::SIZE, <u16 as VmLayout>::ALIGN), (2, 2));
        assert_eq!((<i32 as VmLayout>::SIZE, <i32 as VmLayout>::ALIGN), (4, 4));
        assert_eq!((<u64 as VmLayout>::SIZE, <u64 as VmLayout>::ALIGN), (8, 8));
        assert!(<u32 as VmLayout>::LAYOUT.fields().is_empty());
    }

    /// A scalar marshals its bytes and reads them back, negatives and all.
    fn scalar_round_trips<B: ByteOrder>() {
        let mut mem = [0u8; 8];

        0x1122_3344u32.marshal::<B>(&mut mem);
        assert_eq!(u32::unmarshal::<B>(&mem), 0x1122_3344);

        (-1234i16).marshal::<B>(&mut mem);
        assert_eq!(i16::unmarshal::<B>(&mem), -1234);

        true.marshal::<B>(&mut mem);
        assert!(bool::unmarshal::<B>(&mem));
    }

    #[test]
    fn scalar_round_trips_le() {
        scalar_round_trips::<Le>();
    }

    #[test]
    fn scalar_round_trips_be() {
        scalar_round_trips::<Be>();
    }

    /// A scalar writes exactly what the byte-order helper writes — the fact a
    /// derived `marshal` relies on to match a hand-written one.
    #[test]
    fn a_scalar_matches_the_byte_order_helper() {
        let mut mem = [0u8; 4];
        0x0102_0304u32.marshal::<Le>(&mut mem);
        assert_eq!(mem, Le::write_u32(0x0102_0304));
        0x0102_0304u32.marshal::<Be>(&mut mem);
        assert_eq!(mem, Be::write_u32(0x0102_0304));
    }
}
