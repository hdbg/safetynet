//! Shared logic for safetynet: the byte-order policy, the opcode table, the IR
//! and the interpreter.
//!
//! Proc-macro crates may only export macros, so everything reusable lives here
//! and both `safetynet-macros` and the runtime depend on it. That is what keeps
//! the encoder and the decoder from drifting: they are declared together, once.

mod opaque;
pub(crate) use opaque::{opaque_debug, opaque_error};

pub mod byte_order;
pub mod encoding;
pub mod failure;
pub mod image;
pub mod ir;
pub mod isa;
pub mod link;
pub mod program;
pub mod vm;

mod bytes;
mod marshal;
mod value;
mod width;

pub use byte_order::{Be, ByteOrder, Le};
pub use bytes::Bytes;
pub use encoding::{Immediate, push32_immediate, push64_immediate};
pub use image::{Image, Layout, Region};
pub use ir::{Artifact, Reloc};
pub use isa::{FrameSize, Instr, Op};
pub use link::{Hole, Patch, link};
pub use marshal::{Field, Tail, TypeLayout, VmLayout};
pub use program::Program;
pub use value::{Same, Typed, VmValue};

/// Implementation detail of the derives; not a stable API.
#[doc(hidden)]
#[allow(missing_docs)]
pub mod __private {
    pub use crate::failure::{Failure, fail};
    pub use crate::value::sealed::Sealed;
}
pub use vm::{Flow, Trap, Vm};
pub use width::Width;

#[cfg(test)]
mod samples;

/// The VM's machine word: one `u64`, wrapping, monomorphic.
pub type Word = u64;

/// Size of one [`Word`] in bytes: the granularity of every push and pop.
pub const WORD_SIZE: usize = core::mem::size_of::<Word>();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_is_eight_bytes() {
        // The frame layout rounds to this, the operand stack steps by it, and
        // `LDS`/`STS` displacements are expressed against it. If it ever
        // changes, a great deal of arithmetic elsewhere changes with it.
        assert_eq!(WORD_SIZE, 8);
    }
}
