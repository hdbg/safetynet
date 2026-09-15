//! Scalars that fit one VM [`Word`]. The conversions carry no byte order:
//! widening and narrowing a word, not laying out bytes.

use crate::Word;

pub(crate) mod sealed {
    /// Seals [`VmValue`](super::VmValue): only this crate and its derives name it.
    pub trait Sealed {}
}

/// A scalar that fits one VM [`Word`].
///
/// Sealed: the set of word-sized scalars is fixed by the machine, not open for
/// downstream types to join. Field-less enums earn it through a derive (their
/// discriminant is one of these underneath); aggregates do not — they are
/// [`VmLayout`](crate::VmLayout), and live in memory rather than a word.
pub trait VmValue: sealed::Sealed + Copy {
    /// Widen into a word: unsigned values zero-extend, signed values
    /// sign-extend, so the full-width bit pattern is what the signed opcodes
    /// read.
    fn to_word(self) -> Word;

    /// Narrow a word back, keeping the low bits this type owns.
    fn from_word(word: Word) -> Self;
}

/// `as` already widens and narrows each scalar the right way.
macro_rules! impl_vm_value {
    ($($ty:ty),+ $(,)?) => {$(
        impl sealed::Sealed for $ty {}
        impl VmValue for $ty {
            fn to_word(self) -> Word {
                self as Word
            }

            fn from_word(word: Word) -> Self {
                word as Self
            }
        }
    )+};
}

impl_vm_value!(u8, u16, u32, u64, i8, i16, i32, i64);

impl sealed::Sealed for bool {}
impl VmValue for bool {
    fn to_word(self) -> Word {
        Word::from(self)
    }

    // Any non-zero word is true; an untrusted discriminant has no range to break.
    fn from_word(word: Word) -> Self {
        word != 0
    }
}

/// Holds only when `Self` is exactly `U` — the bound behind [`Typed::typed`].
pub trait Same<U> {
    /// Returns `self`, unchanged.
    fn same(self) -> U;
}

impl<T> Same<T> for T {
    fn same(self) -> T {
        self
    }
}

/// Names a value's exact type in guest code: `pkt.seq.typed::<u32>()`.
///
/// `#[safetynet]` cannot see the aggregate's definition, so a field's first
/// use names its type this way. The kept reference copy compiles only when
/// the named type is exactly the field's own, so the name cannot lie
pub trait Typed: Sized {
    /// Returns `self` unchanged
    fn typed<U>(self) -> U
    where
        Self: Same<U>,
    {
        self.same()
    }
}

impl<T> Typed for T {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scalar round-trips through a word, negatives included.
    #[test]
    fn scalars_round_trip_through_a_word() {
        macro_rules! check {
            ($($ty:ty: [$($v:expr),*]),+ $(,)?) => {$($(
                let value: $ty = $v;
                assert_eq!(
                    <$ty>::from_word(value.to_word()),
                    value,
                    concat!(stringify!($ty), " ", stringify!($v)),
                );
            )*)+};
        }

        check! {
            u8:  [0, 1, u8::MAX],
            u16: [0, 1, u16::MAX],
            u32: [0, 1, u32::MAX],
            u64: [0, 1, u64::MAX],
            i8:  [i8::MIN, -1, 0, 1, i8::MAX],
            i16: [i16::MIN, -1, 0, 1, i16::MAX],
            i32: [i32::MIN, -1, 0, 1, i32::MAX],
            i64: [i64::MIN, -1, 0, 1, i64::MAX],
        };

        assert!(bool::from_word(true.to_word()));
        assert!(!bool::from_word(false.to_word()));
    }

    /// `-1` of any width is all ones in a word.
    #[test]
    fn signed_values_sign_extend() {
        assert_eq!((-1i8).to_word(), u64::MAX);
        assert_eq!((-1i32).to_word(), u64::MAX);
        assert_eq!(i8::MIN.to_word(), 0xffff_ffff_ffff_ff80);
    }

    /// Narrowing keeps the low bits the type owns.
    #[test]
    fn unsigned_values_narrow() {
        assert_eq!(u8::from_word(0x1234_5678_9abc_deff), 0xff);
        assert_eq!(u32::from_word(0x1234_5678_9abc_deff), 0x9abc_deff);
    }

    /// Any non-zero word is `true`.
    #[test]
    fn bool_reads_any_nonzero_as_true() {
        assert!(bool::from_word(2));
        assert!(bool::from_word(u64::MAX));
        assert!(!bool::from_word(0));
    }
}
