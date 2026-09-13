//! The intermediate representation: a control-flow graph with no byte offsets
//! in it.
//!
//! Structured control flow is gone and edges are [`BlockId`]s, so nothing here
//! has to be renumbered when code moves. Two things stay deliberately symbolic
//! until the layout pass:
//!
//! - **edges**, because a branch offset is a distance between two things that do
//!   not have addresses yet;
//! - **local access**, because a frame cell's displacement depends on how deep
//!   the operand stack is at that point, so [`Item::Load`] names the cell and
//!   lets the pass that tracks depth work out the rest.
//!
//! Both exist for the same reason: a pass may insert or delete stack traffic,
//! and everything it does not touch must stay correct.

mod builder;
mod finalize;
mod frame;
mod validate;

pub use builder::{BlockBody, BuildError, Builder};
pub use finalize::{NotFinal, finalize, finalize_with};
pub use frame::{Cell, CellId, Frame};
pub use validate::{Invalid, Limits, Where, validate, validate_with};

use crate::{Instr, WORD_SIZE};

#[cfg(test)]
mod tests;

/// Stack effect of one word, as a signed byte count.
const WORD: i32 = WORD_SIZE as i32;

/// Identifies a block within one [`Cfg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u32);

impl BlockId {
    /// Position of the block in its graph.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The block at `index`, for walking a graph by position.
    pub(crate) const fn from_index(index: usize) -> Self {
        Self(index as u32)
    }
}

/// One step of a block's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    /// An instruction with nothing left to resolve.
    Instr(Instr),
    /// Push the value of a frame cell, zero-extended to a word.
    Load(CellId),
    /// Pop a word into a frame cell, keeping as many low bytes as it holds.
    Store(CellId),
}

impl Item {
    /// Net change to `SP`, in bytes.
    ///
    /// The symbolic forms know their effect without knowing their displacement,
    /// which is exactly what lets the stack depth be validated before anything
    /// is laid out — and the displacement is computed *from* that depth.
    pub fn sp_delta(self) -> i32 {
        match self {
            Self::Instr(instr) => instr.sp_delta(),
            Self::Load(_) => WORD,
            Self::Store(_) => -WORD,
        }
    }
}

impl From<Instr> for Item {
    fn from(instr: Instr) -> Self {
        Self::Instr(instr)
    }
}

/// How a block ends. Every block ends in exactly one of these.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Terminator {
    /// Continue at another block.
    Jmp(BlockId),
    /// Pop a word and take one of two edges.
    Br {
        /// Taken when the word is not zero.
        then: BlockId,
        /// Taken when it is zero.
        els: BlockId,
    },
    /// Pop a word and index a table of edges.
    ///
    /// The default is mandatory. A table with no default is a way for an
    /// untrusted value to leave the graph.
    Switch {
        /// Edges indexed by the popped word.
        arms: Vec<BlockId>,
        /// Taken when the word indexes no arm.
        default: BlockId,
    },
    /// Stop the machine.
    Halt,
}

impl Terminator {
    /// Net change to `SP`, in bytes: the conditional forms consume the word they
    /// branch on.
    pub fn sp_delta(&self) -> i32 {
        match self {
            Self::Jmp(_) | Self::Halt => 0,
            Self::Br { .. } | Self::Switch { .. } => -WORD,
        }
    }

    /// Every block this one can reach.
    pub fn targets(&self) -> impl Iterator<Item = BlockId> + '_ {
        let (edges, table) = match self {
            Self::Jmp(next) => ([Some(*next), None], [].as_slice()),
            Self::Br { then, els } => ([Some(*then), Some(*els)], [].as_slice()),
            Self::Switch { arms, default } => ([Some(*default), None], arms.as_slice()),
            Self::Halt => ([None, None], [].as_slice()),
        };

        edges.into_iter().flatten().chain(table.iter().copied())
    }
}

/// A straight-line run of items ending in one terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    id: BlockId,
    sp_in: u32,
    code: Vec<Item>,
    term: Terminator,
}

impl Block {
    /// This block's id.
    pub const fn id(&self) -> BlockId {
        self.id
    }

    /// Operand-stack depth in bytes, above the frame, on entry to this block.
    ///
    /// Recorded by whoever built the graph and *checked* by the validator, never
    /// computed by it. A number the validator derived itself would agree with
    /// itself no matter what the front-end meant; this way the two disagree
    /// loudly instead.
    pub const fn sp_in(&self) -> u32 {
        self.sp_in
    }

    /// The block's body.
    pub fn code(&self) -> &[Item] {
        &self.code
    }

    /// How the block ends.
    pub const fn term(&self) -> &Terminator {
        &self.term
    }
}

/// A control-flow graph: one function's worth of blocks over one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cfg {
    entry: BlockId,
    blocks: Vec<Block>,
    frame: Frame,
}

impl Cfg {
    /// Starts building a graph over `frame`.
    pub fn builder(frame: Frame) -> Builder {
        Builder::new(frame)
    }

    /// Where execution starts.
    pub const fn entry(&self) -> BlockId {
        self.entry
    }

    /// The frame every local in this graph lives in.
    pub const fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Every block, indexed by [`BlockId::index`].
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// The block `id` names.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.index())
    }
}
