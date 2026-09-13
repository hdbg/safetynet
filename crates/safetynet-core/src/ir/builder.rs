//! Building a [`Cfg`].

use super::{Block, BlockId, Cfg, Frame, Item, Terminator};
use crate::ir::CellId;
use crate::{Instr, Region};

/// Builds a [`Cfg`].
///
/// A block's id is handed out before the block has any content, so a branch can
/// name a target that does not exist yet — which every loop needs, and which is
/// why this is a builder rather than a constructor.
#[derive(Debug)]
pub struct Builder {
    frame: Frame,
    slots: Vec<Slot>,
}

/// A block that has been given an id, and may or may not have been finished.
#[derive(Debug)]
enum Slot {
    Open(BlockBody),
    Sealed(Block),
}

/// The body of a block that is still being written.
#[derive(Debug)]
pub struct BlockBody {
    sp_in: u32,
    code: Vec<Item>,
}

impl BlockBody {
    /// Appends an instruction.
    pub fn instr(&mut self, instr: impl Into<Instr>) -> &mut Self {
        self.code.push(Item::Instr(instr.into()));
        self
    }

    /// Appends a load from a frame cell.
    pub fn load(&mut self, cell: CellId) -> &mut Self {
        self.code.push(Item::Load(cell));
        self
    }

    /// Appends a store into a frame cell.
    pub fn store(&mut self, cell: CellId) -> &mut Self {
        self.code.push(Item::Store(cell));
        self
    }

    /// Appends the address a region begins at.
    pub fn base(&mut self, region: Region) -> &mut Self {
        self.code.push(Item::Base(region));
        self
    }

    /// Appends a field-offset hole, named by `hole`.
    pub fn field(&mut self, hole: u32) -> &mut Self {
        self.code.push(Item::Field(hole));
        self
    }

    /// What has been written so far.
    pub fn code(&self) -> &[Item] {
        &self.code
    }
}

impl Builder {
    /// Starts a graph over `frame`.
    pub fn new(frame: Frame) -> Self {
        Self {
            frame,
            slots: Vec::new(),
        }
    }

    /// Opens a block whose operand-stack depth on entry is `sp_in` bytes.
    ///
    /// The depth is the caller's claim about its own structure, not something
    /// derived here. The validator recomputes it from the graph's edges, and the
    /// point of recording it is that the two can disagree.
    pub fn block(&mut self, sp_in: u32) -> BlockId {
        let id = BlockId(self.slots.len() as u32);
        self.slots.push(Slot::Open(BlockBody {
            sp_in,
            code: Vec::new(),
        }));
        id
    }

    /// The body of an open block, to append to.
    pub fn at(&mut self, id: BlockId) -> Result<&mut BlockBody, BuildError> {
        match self.slots.get_mut(id.index()) {
            Some(Slot::Open(body)) => Ok(body),
            Some(Slot::Sealed(_)) => Err(BuildError::AlreadySealed(id)),
            None => Err(BuildError::NoSuchBlock(id)),
        }
    }

    /// Finishes a block with the terminator it ends in.
    pub fn seal(&mut self, id: BlockId, term: Terminator) -> Result<(), BuildError> {
        let slot = self
            .slots
            .get_mut(id.index())
            .ok_or(BuildError::NoSuchBlock(id))?;

        let Slot::Open(body) = slot else {
            return Err(BuildError::AlreadySealed(id));
        };

        *slot = Slot::Sealed(Block {
            id,
            sp_in: body.sp_in,
            code: core::mem::take(&mut body.code),
            term,
        });

        Ok(())
    }

    /// Finishes the graph, starting at `entry`.
    ///
    /// Checks only that the graph is *complete* — every block sealed, and the
    /// entry a block that exists. Whether it makes sense is the validator's
    /// question, not this one's.
    pub fn build(self, entry: BlockId) -> Result<Cfg, BuildError> {
        let mut blocks = Vec::with_capacity(self.slots.len());
        for (index, slot) in self.slots.into_iter().enumerate() {
            match slot {
                Slot::Sealed(block) => blocks.push(block),
                Slot::Open(_) => return Err(BuildError::NotSealed(BlockId(index as u32))),
            }
        }

        if entry.index() >= blocks.len() {
            return Err(BuildError::NoSuchBlock(entry));
        }

        Ok(Cfg {
            entry,
            blocks,
            frame: self.frame,
        })
    }
}

/// A graph that could not be assembled at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// A block id from another graph, or one never handed out.
    #[error("block {0:?} does not exist")]
    NoSuchBlock(BlockId),

    /// A block was written to or sealed after it had already been sealed.
    #[error("block {0:?} is already sealed")]
    AlreadySealed(BlockId),

    /// A block was opened and never given a terminator.
    #[error("block {0:?} has no terminator")]
    NotSealed(BlockId),
}
