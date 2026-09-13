//! safetynet — a Rust-embedded bytecode VM.
//!
//! A complexity-limited Rust function is annotated, lowered to an intermediate
//! representation, compiled to bytecode, and its body replaced with a call into
//! the VM interpreter.
//!
//! # Byte order
//!
//! Multi-byte values in the VM image are laid out in a byte order fixed at
//! compile time by a type parameter. [`Order`] is the default used when a site
//! does not name one; write `Be` explicitly to build a big-endian program.
//!
//! ```
//! use safetynet::{Be, ByteOrder, Le, Order};
//!
//! // `Order` is the default, spelled for signatures that want to be explicit.
//! assert_eq!(<Order as ByteOrder>::NAME, "le");
//! assert_eq!(Le::write_u32(1), [1, 0, 0, 0]);
//! assert_eq!(Be::write_u32(1), [0, 0, 0, 1]);
//! ```

pub use safetynet_core::*;

/// Assembles a program at compile time.
pub use safetynet_macros::asm;

/// Derives [`VmLayout`] for a struct.
pub use safetynet_macros::VmLayout;

/// Derives [`VmValue`] for a field-less enum.
pub use safetynet_macros::VmValue;

/// The byte order used when a site does not name one.
pub type Order = Le;
