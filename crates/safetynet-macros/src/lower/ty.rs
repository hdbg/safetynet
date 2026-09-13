//! Shadow types: the scalar a local carries at compile time.

use safetynet_core::Width;

/// A scalar's storage: how wide it is, and whether it reads back signed.
///
/// Signedness picks the signed opcode for division, shifts and comparisons; it
/// is not a separate width.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Scalar {
    pub(crate) width: Width,
    pub(crate) signed: bool,
}

impl Scalar {
    /// Boolean: one byte, unsigned.
    pub(crate) const BOOL: Self = Self {
        width: Width::U8,
        signed: false,
    };

    /// The scalar a type name denotes, if the machine can hold it.
    ///
    /// There is no two-byte access, so `u16`/`i16` are declined rather than
    /// silently widened.
    pub(crate) fn of(ty: &syn::Type) -> Option<Self> {
        let name = ident(ty)?;
        Some(match name.as_str() {
            "u8" => Self {
                width: Width::U8,
                signed: false,
            },
            "i8" => Self {
                width: Width::U8,
                signed: true,
            },
            "bool" => Self::BOOL,
            "u32" => Self {
                width: Width::U32,
                signed: false,
            },
            "i32" => Self {
                width: Width::U32,
                signed: true,
            },
            "u64" => Self {
                width: Width::U64,
                signed: false,
            },
            "i64" => Self {
                width: Width::U64,
                signed: true,
            },
            _ => return None,
        })
    }

    /// Bytes it occupies.
    pub(crate) const fn size(self) -> u32 {
        self.width.bytes() as u32
    }

    /// Alignment: a scalar's is its size.
    pub(crate) const fn align(self) -> u32 {
        self.size()
    }
}

/// The single identifier a plain type path names, if that is all it is.
fn ident(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(path) if path.qself.is_none() => {
            path.path.get_ident().map(ToString::to_string)
        }
        _ => None,
    }
}
