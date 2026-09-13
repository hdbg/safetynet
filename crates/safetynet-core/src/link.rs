//! The const-eval field linker: bakes field offsets into finished bytecode.
//!
//! A `field` reference assembles to a `push32` with a zero immediate and a
//! [`FieldPatch`]. The offset lives on another type's layout, unknown when the
//! bytecode was baked, so it is filled in here — in `const`, at the call site,
//! where the type is finally in scope.

use crate::ByteOrder;
use crate::marshal::TypeLayout;

/// A field-offset hole: where its `push32` sits, and the field to resolve.
#[derive(Debug, Clone, Copy)]
pub struct FieldPatch {
    /// Byte offset of the push in the code.
    pub at: usize,
    /// The aggregate's layout.
    pub layout: &'static TypeLayout,
    /// The dotted path into it.
    pub path: &'static [&'static str],
}

/// Writes each field offset over the four immediate bytes of its `push32`.
///
/// A path that names no field is a compile error, since this runs in `const`.
pub const fn link_fields<B: ByteOrder, const N: usize>(
    mut code: [u8; N],
    patches: &[FieldPatch],
) -> [u8; N] {
    let mut rest = patches;
    while let Some((patch, tail)) = rest.split_first() {
        let offset = match patch.layout.offset_of(patch.path) {
            Some(offset) => offset,
            None => panic!("a field reference resolved to no field in its layout"),
        };
        let bytes = if B::BIG_ENDIAN {
            offset.to_be_bytes()
        } else {
            offset.to_le_bytes()
        };
        code = write_immediate(code, patch.at, bytes);
        rest = tail;
    }
    code
}

/// Overwrites the four immediate bytes of the push at `at` (its tag comes first).
// The array is fixed-size and the patch offsets come from the assembler; a patch
// past the end panics at compile time rather than silently missing.
#[allow(clippy::indexing_slicing)]
const fn write_immediate<const N: usize>(mut code: [u8; N], at: usize, bytes: [u8; 4]) -> [u8; N] {
    code[at + 1] = bytes[0];
    code[at + 2] = bytes[1];
    code[at + 3] = bytes[2];
    code[at + 4] = bytes[3];
    code
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::Le;
    use crate::encoding::{decode, encode};
    use crate::isa::Push32;

    static INNER: TypeLayout = TypeLayout::new(&[
        crate::Field::new("seq", 0, 4, None),
        crate::Field::new("flags", 4, 1, None),
    ]);
    static OUTER: TypeLayout = TypeLayout::new(&[
        crate::Field::new("kind", 0, 1, None),
        crate::Field::new("header", 4, 8, Some(&INNER)),
        crate::Field::new("tag", 16, 8, None),
    ]);

    /// The linker runs in `const`: the offset is baked before the program exists
    /// at run time. Only the four immediate bytes change; the tag is untouched.
    #[test]
    fn a_field_offset_is_baked_in_const() {
        const RAW: [u8; 5] = [0xab, 0, 0, 0, 0];
        const LINKED: [u8; 5] = link_fields::<Le, 5>(
            RAW,
            &[FieldPatch {
                at: 0,
                layout: &OUTER,
                path: &["header", "seq"],
            }],
        );
        assert_eq!(LINKED, [0xab, 4, 0, 0, 0]);
    }

    /// A linked placeholder decodes to a push of the resolved offset.
    #[test]
    fn a_linked_push_decodes_to_the_offset() {
        let mut raw = Vec::new();
        encode::<Le>(Push32 { imm: 0 }.into(), &mut raw).expect("encodes");
        let raw: [u8; 5] = raw.try_into().expect("five bytes");

        let linked = link_fields::<Le, 5>(
            raw,
            &[FieldPatch {
                at: 0,
                layout: &OUTER,
                path: &["tag"],
            }],
        );

        let (instr, _) = decode::<Le>(&linked).expect("decodes");
        assert_eq!(instr, Push32 { imm: 16 }.into());
    }

    /// A path that names no field is rejected; in `const` this is a compile
    /// error, and at run time a panic.
    #[test]
    #[should_panic(expected = "no field")]
    fn an_unknown_field_is_rejected() {
        let _ = link_fields::<Le, 5>(
            [0xab, 0, 0, 0, 0],
            &[FieldPatch {
                at: 0,
                layout: &OUTER,
                path: &["header", "nope"],
            }],
        );
    }
}
