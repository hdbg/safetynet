//! Access widths.

/// Width of a memory or frame access, in bytes.
///
/// An enum rather than a number, so that every read and write matches over the
/// same three cases and a width can only ever come from an opcode or a frame
/// cell — never from data.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Width {
    /// One byte.
    U8,
    /// Four bytes.
    U32,
    /// Eight bytes: one whole [`Word`](crate::Word).
    U64,
}

impl Width {
    /// The width in bytes: 1, 4 or 8.
    pub const fn bytes(self) -> u16 {
        match self {
            Self::U8 => 1,
            Self::U32 => 4,
            Self::U64 => 8,
        }
    }

    /// The width in bits: 8, 32 or 64.
    pub const fn bits(self) -> u16 {
        self.bytes() * 8
    }

    /// The narrowest width that holds `bytes`, if any does.
    ///
    /// There is no two-byte access, so a `u16` local occupies a four-byte cell.
    pub const fn holding(bytes: u16) -> Option<Self> {
        match bytes {
            0 | 1 => Some(Self::U8),
            2..=4 => Some(Self::U32),
            5..=8 => Some(Self::U64),
            _ => None,
        }
    }
}

crate::opaque_debug!(Width);
