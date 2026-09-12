//! Byte order as a compile-time type parameter.
//!
//! Because the implementors are zero-sized and every method is a `const`-shaped
//! byte shuffle, monomorphization leaves no `ByteOrder` value and no branch in
//! the shipped binary: just the one order's load/store sequence, inlined.

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Le {}
    impl Sealed for super::Be {}
}

/// How multi-byte values are laid out in the VM's byte image.
///
/// Implemented only by [`Le`] and [`Be`]; the trait is sealed. Single-byte
/// accesses (`LD8`/`ST8`) are order-independent and deliberately absent here.
///
/// # Examples
///
/// ```
/// use safetynet_core::{Be, ByteOrder, Le};
///
/// assert_eq!(Le::read_u32([0x78, 0x56, 0x34, 0x12]), 0x1234_5678);
/// assert_eq!(Be::read_u32([0x12, 0x34, 0x56, 0x78]), 0x1234_5678);
/// ```
pub trait ByteOrder: sealed::Sealed + Copy + Clone + core::fmt::Debug + 'static {
    /// Whether this order puts the most significant byte first.
    const BIG_ENDIAN: bool;

    /// Short lowercase name (`"le"` / `"be"`).
    const NAME: &'static str;

    /// Decodes four image bytes into a `u32`.
    fn read_u32(bytes: [u8; 4]) -> u32;

    /// Encodes a `u32` into four image bytes.
    fn write_u32(value: u32) -> [u8; 4];

    /// Decodes eight image bytes into a `u64`.
    fn read_u64(bytes: [u8; 8]) -> u64;

    /// Encodes a `u64` into eight image bytes.
    fn write_u64(value: u64) -> [u8; 8];
}

/// Little-endian: least significant byte at the lowest address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Le;

/// Big-endian: most significant byte at the lowest address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Be;

impl ByteOrder for Le {
    const BIG_ENDIAN: bool = false;
    const NAME: &'static str = "le";

    fn read_u32(bytes: [u8; 4]) -> u32 {
        u32::from_le_bytes(bytes)
    }

    fn write_u32(value: u32) -> [u8; 4] {
        value.to_le_bytes()
    }

    fn read_u64(bytes: [u8; 8]) -> u64 {
        u64::from_le_bytes(bytes)
    }

    fn write_u64(value: u64) -> [u8; 8] {
        value.to_le_bytes()
    }
}

impl ByteOrder for Be {
    const BIG_ENDIAN: bool = true;
    const NAME: &'static str = "be";

    fn read_u32(bytes: [u8; 4]) -> u32 {
        u32::from_be_bytes(bytes)
    }

    fn write_u32(value: u32) -> [u8; 4] {
        value.to_be_bytes()
    }

    fn read_u64(bytes: [u8; 8]) -> u64 {
        u64::from_be_bytes(bytes)
    }

    fn write_u64(value: u64) -> [u8; 8] {
        value.to_be_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Order-generic body. Every behavioural test in this workspace is written
    /// like this and instantiated once per order. Because the order is a source
    /// literal rather than a build flag, parameterized tests are the only thing
    /// keeping the order nobody builds from becoming the order nobody has ever
    /// tested.
    fn round_trips<B: ByteOrder>() {
        for value in [0u64, 1, 0xff, 0x0102_0304_0506_0708, u64::MAX] {
            assert_eq!(B::read_u64(B::write_u64(value)), value, "u64 {value:#x}");
        }
        for value in [0u32, 1, 0xff, 0x0102_0304, u32::MAX] {
            assert_eq!(B::read_u32(B::write_u32(value)), value, "u32 {value:#x}");
        }
    }

    #[test]
    fn round_trips_le() {
        round_trips::<Le>();
    }

    #[test]
    fn round_trips_be() {
        round_trips::<Be>();
    }

    /// The orders must actually differ, or a "both orders" suite proves nothing.
    #[test]
    fn orders_disagree_on_multibyte_values() {
        assert_ne!(Le::write_u32(0x1234_5678), Be::write_u32(0x1234_5678));
        assert_eq!(Le::write_u32(0x1234_5678), [0x78, 0x56, 0x34, 0x12]);
        assert_eq!(Be::write_u32(0x1234_5678), [0x12, 0x34, 0x56, 0x78]);
        assert_eq!(
            Le::write_u64(0x0102_0304_0506_0708),
            [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(
            Be::write_u64(0x0102_0304_0506_0708),
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    /// A byte-symmetric value is the one case where the orders agree; useful as
    /// a reminder that such values cannot distinguish a wrong-order bug.
    #[test]
    fn palindromic_values_are_order_blind() {
        assert_eq!(Le::write_u32(0xabab_abab), Be::write_u32(0xabab_abab));
    }
}
