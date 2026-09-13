//! The const-eval field linker: bakes field offsets into finished bytecode.
//!
//! A `field` reference assembles to a push with a zero immediate and a
//! [`FieldPatch`]. The offset lives on another type's layout, unknown when the
//! bytecode was baked, so it is filled in here — in `const`, at the call site,
//! where the type is finally in scope.
//!
//! The immediate's position and byte order are not assumed: the assembler probes
//! its encoder for them and records them on the patch, so this stays a plain
//! byte write and never a second copy of the wire format.

use crate::marshal::TypeLayout;

/// A field-offset hole: where the immediate to fill sits, how wide and in which
/// order, and the field whose offset fills it.
#[derive(Debug, Clone, Copy)]
pub struct FieldPatch {
    /// Byte offset of the immediate in the code.
    pub at: usize,
    /// Its width in bytes.
    pub width: usize,
    /// Whether the most significant byte comes first.
    pub big_endian: bool,
    /// The aggregate's layout.
    pub layout: &'static TypeLayout,
    /// The dotted path into it.
    pub path: &'static [&'static str],
}

/// Writes each field offset into the immediate its patch names.
///
/// A path that names no field is a compile error, since this runs in `const`.
pub const fn link_fields<const N: usize>(mut code: [u8; N], patches: &[FieldPatch]) -> [u8; N] {
    let mut rest = patches;
    while let Some((patch, tail)) = rest.split_first() {
        let offset = match patch.layout.offset_of(patch.path) {
            Some(offset) => offset,
            None => panic!("a field reference resolved to no field in its layout"),
        };
        code = write_immediate(code, patch.at, offset, patch.width, patch.big_endian);
        rest = tail;
    }
    code
}

/// Writes the low `width` bytes of `value` at `at`, most significant first when
/// `big_endian`.
// The array is fixed-size and the offsets come from the assembler; one past the
// end panics at compile time rather than silently missing.
#[allow(clippy::indexing_slicing)]
const fn write_immediate<const N: usize>(
    mut code: [u8; N],
    at: usize,
    value: u32,
    width: usize,
    big_endian: bool,
) -> [u8; N] {
    let bytes = value.to_le_bytes();
    let mut i = 0;
    while i < width {
        let pos = if big_endian {
            at + width - 1 - i
        } else {
            at + i
        };
        code[pos] = bytes[i];
        i += 1;
    }
    code
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::encoding::{Packed, decode, encode, push32_immediate};
    use crate::isa::Push32;
    use crate::{Be, Le};

    static INNER: TypeLayout = TypeLayout::new(&[
        crate::Field::new("seq", 0, 4, None),
        crate::Field::new("flags", 4, 1, None),
    ]);
    static OUTER: TypeLayout = TypeLayout::new(&[
        crate::Field::new("kind", 0, 1, None),
        crate::Field::new("header", 4, 8, Some(&INNER)),
        crate::Field::new("tag", 16, 8, None),
    ]);

    /// The probe finds the immediate the linker writes into: one byte past the
    /// tag, four wide, in the encoder's order.
    #[test]
    fn the_probe_locates_the_immediate() {
        let le = push32_immediate(&Packed::<Le>::new()).expect("packed has one");
        assert_eq!((le.at, le.width, le.big_endian), (1, 4, false));

        let be = push32_immediate(&Packed::<Be>::new()).expect("packed has one");
        assert_eq!((be.at, be.width, be.big_endian), (1, 4, true));
    }

    /// The linker runs in `const`: the offset is baked before the program exists
    /// at run time. Only the immediate bytes change; the tag is untouched.
    #[test]
    fn a_field_offset_is_baked_in_const() {
        const RAW: [u8; 5] = [0xab, 0, 0, 0, 0];
        const LINKED: [u8; 5] = link_fields::<5>(
            RAW,
            &[FieldPatch {
                at: 1,
                width: 4,
                big_endian: false,
                layout: &OUTER,
                path: &["header", "seq"],
            }],
        );
        assert_eq!(LINKED, [0xab, 4, 0, 0, 0]);
    }

    /// End to end for one order: probe the encoder, link, and decode back.
    fn a_linked_push_decodes_to_the_offset<B: crate::ByteOrder>() {
        let imm = push32_immediate(&Packed::<B>::new()).expect("packed has one");

        let mut raw = Vec::new();
        encode::<B>(Push32 { imm: 0 }.into(), &mut raw).expect("encodes");
        let raw: [u8; 5] = raw.try_into().expect("five bytes");

        let linked = link_fields::<5>(
            raw,
            &[FieldPatch {
                at: imm.at,
                width: imm.width,
                big_endian: imm.big_endian,
                layout: &OUTER,
                path: &["tag"],
            }],
        );

        let (instr, _) = decode::<B>(&linked).expect("decodes");
        assert_eq!(instr, Push32 { imm: 16 }.into());
    }

    #[test]
    fn a_linked_push_decodes_to_the_offset_le() {
        a_linked_push_decodes_to_the_offset::<Le>();
    }

    #[test]
    fn a_linked_push_decodes_to_the_offset_be() {
        a_linked_push_decodes_to_the_offset::<Be>();
    }

    /// A path that names no field is rejected; in `const` this is a compile
    /// error, and at run time a panic.
    #[test]
    #[should_panic(expected = "no field")]
    fn an_unknown_field_is_rejected() {
        let _ = link_fields::<5>(
            [0xab, 0, 0, 0, 0],
            &[FieldPatch {
                at: 1,
                width: 4,
                big_endian: false,
                layout: &OUTER,
                path: &["header", "nope"],
            }],
        );
    }
}
