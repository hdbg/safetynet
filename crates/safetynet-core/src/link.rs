//! The const-eval linker: bakes compile-time constants into finished bytecode.
//!
//! A `$field` or `$tag` reference assembles to a push with a zero immediate and
//! a [`Patch`]. What fills it — a field's offset, a variant's discriminant — is
//! a constant of a type unknown when the bytecode was baked, so it is filled in
//! here, in `const`, at the call site where the type is finally in scope.
//!
//! The immediate's position and byte order are not assumed: the assembler probes
//! its encoder for them and records them on the patch, so this stays a plain
//! byte write and never a second copy of the wire format.

use crate::marshal::TypeLayout;

/// The constant a [`Patch`] resolves.
#[derive(Debug, Clone, Copy)]
pub enum Hole {
    /// A field's byte offset, resolved from its aggregate's layout.
    Field {
        /// The aggregate's layout.
        layout: &'static TypeLayout,
        /// The dotted path into it.
        path: &'static [&'static str],
    },
    /// A word the call site already computed, such as an enum discriminant.
    Word(u64),
}

/// A hole in the bytecode: where an immediate sits, how wide and in which order,
/// and the constant that fills it.
#[derive(Debug, Clone, Copy)]
pub struct Patch {
    /// Byte offset of the immediate in the code.
    pub at: usize,
    /// Its width in bytes.
    pub width: usize,
    /// Whether the most significant byte comes first.
    pub big_endian: bool,
    /// What fills it.
    pub hole: Hole,
}

/// Writes each constant into the immediate its patch names.
///
/// A field path that names no field is a compile error, since this runs in
/// `const`.
pub const fn link<const N: usize>(mut code: [u8; N], patches: &[Patch]) -> [u8; N] {
    let mut rest = patches;
    while let Some((patch, tail)) = rest.split_first() {
        let value = match patch.hole {
            Hole::Field { layout, path } => match layout.offset_of(path) {
                Some(offset) => offset as u64,
                None => panic!("a field reference resolved to no field in its layout"),
            },
            Hole::Word(word) => word,
        };
        code = write_immediate(code, patch.at, value, patch.width, patch.big_endian);
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
    value: u64,
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
    use crate::encoding::{Packed, decode, encode, push32_immediate, push64_immediate};
    use crate::isa::{Push32, Push64};
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
        const LINKED: [u8; 5] = link::<5>(
            RAW,
            &[Patch {
                at: 1,
                width: 4,
                big_endian: false,
                hole: Hole::Field {
                    layout: &OUTER,
                    path: &["header", "seq"],
                },
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

        let linked = link::<5>(
            raw,
            &[Patch {
                at: imm.at,
                width: imm.width,
                big_endian: imm.big_endian,
                hole: Hole::Field {
                    layout: &OUTER,
                    path: &["tag"],
                },
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
        let _ = link::<5>(
            [0xab, 0, 0, 0, 0],
            &[Patch {
                at: 1,
                width: 4,
                big_endian: false,
                hole: Hole::Field {
                    layout: &OUTER,
                    path: &["header", "nope"],
                },
            }],
        );
    }

    /// A word hole is written straight through — an enum discriminant baked into
    /// a `push64`.
    fn a_word_is_baked<B: crate::ByteOrder>() {
        let imm = push64_immediate(&Packed::<B>::new()).expect("packed has one");

        let mut raw = Vec::new();
        encode::<B>(Push64 { imm: 0 }.into(), &mut raw).expect("encodes");
        let raw: [u8; 9] = raw.try_into().expect("nine bytes");

        let linked = link::<9>(
            raw,
            &[Patch {
                at: imm.at,
                width: imm.width,
                big_endian: imm.big_endian,
                hole: Hole::Word(0xdead_beef_0000_0042),
            }],
        );

        let (instr, _) = decode::<B>(&linked).expect("decodes");
        assert_eq!(
            instr,
            Push64 {
                imm: 0xdead_beef_0000_0042
            }
            .into()
        );
    }

    #[test]
    fn a_word_is_baked_le() {
        a_word_is_baked::<Le>();
    }

    #[test]
    fn a_word_is_baked_be() {
        a_word_is_baked::<Be>();
    }
}
