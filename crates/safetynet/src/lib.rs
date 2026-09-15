//! safetynet — a Rust-embedded bytecode VM.
//!
//! A complexity-limited Rust function is annotated, lowered to an intermediate
//! representation, compiled to bytecode, and its body replaced with a call into
//! the VM interpreter.
//!
//! # Usage
//!
//! Tag a function with [`safetynet`] and it keeps its signature, so callers see
//! an ordinary function — but its body now runs on the VM over bytecode built
//! at compile time. A struct argument derives [`VmLayout`]; each field names
//! its type with [`Typed::typed`] the first time it is read.
//!
//! ```
//! use safetynet::{Typed, VmLayout, safetynet};
//!
//! #[derive(Clone, Copy, VmLayout)]
//! struct Packet {
//!     seq: u32,
//!     len: u32,
//! }
//!
//! #[safetynet]
//! fn checksum(pkt: Packet) -> u32 {
//!     let seq: u32 = pkt.seq.typed::<u32>();
//!     let len: u32 = pkt.len.typed::<u32>();
//!     let mut acc: u32 = seq ^ len;
//!     let mut i: u32 = 0;
//!     while i < len {
//!         acc = acc + i;
//!         i = i + 1;
//!     }
//!     acc
//! }
//!
//! // Called like any other function; the checksum runs on the VM.
//! assert_eq!(checksum(Packet { seq: 7, len: 4 }), 9);
//! ```
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

/// Compiles an annotated function to VM bytecode.
pub use safetynet_macros::safetynet;

/// Derives [`VmLayout`] for a struct.
pub use safetynet_macros::VmLayout;

/// Derives [`VmValue`] for a field-less enum.
pub use safetynet_macros::VmValue;

/// The byte order used when a site does not name one.
pub type Order = Le;
