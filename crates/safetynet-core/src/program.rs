//! A finalized program: bytecode, and what the machine needs to run it.

use core::fmt;
use core::marker::PhantomData;

use crate::{ByteOrder, FrameSize};

/// Bytecode ready to run, in byte order `B`.
#[derive(Clone, PartialEq, Eq)]
pub struct Program<B: ByteOrder> {
    code: Vec<u8>,
    frame: FrameSize,
    order: PhantomData<B>,
}

impl<B: ByteOrder> Program<B> {
    /// Wraps bytecode.
    ///
    /// Public because finalization is not the only thing that produces a
    /// program: a hand-written assembler is a legitimate front-end, and one that
    /// had to go through a graph to hand the machine some bytes would be a
    /// worse tool, not a safer one.
    pub const fn new(code: Vec<u8>, frame: FrameSize) -> Self {
        Self {
            code,
            frame,
            order: PhantomData,
        }
    }

    /// The bytecode, starting at its entry point.
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    /// Bytes the prologue reserves for locals.
    ///
    /// Worth having on hand even though the program reserves them itself: it is
    /// the smallest stack a machine can run this on without trapping.
    pub const fn frame(&self) -> FrameSize {
        self.frame
    }
}

impl<B: ByteOrder> fmt::Debug for Program<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
            .field("order", &B::NAME)
            .field("code", &self.code.len())
            .field("frame", &self.frame.bytes())
            .finish()
    }
}
