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

    /// The type an integer literal takes with nothing else to go on.
    pub(crate) const I32: Self = Self {
        width: Width::U32,
        signed: true,
    };

    /// A byte: what a region's `.iter()` yields.
    pub(crate) const U8: Self = Self {
        width: Width::U8,
        signed: false,
    };

    /// A region header's `off` and `len`, and the counter that walks it.
    pub(crate) const U32: Self = Self {
        width: Width::U32,
        signed: false,
    };

    /// A `usize` as the machine holds it: the reference copy's `.len()`.
    pub(crate) const U64: Self = Self {
        width: Width::U64,
        signed: false,
    };

    /// An `isize` as the machine holds it.
    pub(crate) const I64: Self = Self {
        width: Width::U64,
        signed: true,
    };

    /// The scalar a type name denotes, if the machine can hold it.
    ///
    /// There is no two-byte access, so `u16`/`i16` are declined rather than
    /// silently widened.
    pub(crate) fn of(ty: &syn::Type) -> Option<Self> {
        Self::of_name(&ident(ty)?)
    }

    /// The scalar a type denotes where nothing is marshalled, so a `usize`
    /// or `isize` is simply the word the machine holds it in. A const's type
    /// and a cast's target are the two such places; a local or a parameter
    /// crosses the boundary and keeps the strict rule.
    pub(crate) fn held(ty: &syn::Type) -> Option<Self> {
        match ident(ty)?.as_str() {
            "usize" => Some(Self::U64),
            "isize" => Some(Self::I64),
            name => Self::of_name(name),
        }
    }

    /// The scalar a bare name — a type ident, a literal suffix — denotes.
    pub(crate) fn of_name(name: &str) -> Option<Self> {
        Some(match name {
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

/// A primitive name the machine cannot hold, to decline rather than route to
/// the aggregate path a struct type takes.
pub(crate) fn unsupported_primitive(ty: &syn::Type) -> bool {
    matches!(
        ident(ty).as_deref(),
        Some("u16" | "i16" | "u128" | "i128" | "usize" | "isize" | "f32" | "f64" | "char")
    )
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
