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
pub use finalize::{
    Artifact, BaseReloc, FieldReloc, LoadReloc, NotFinal, Reloc, Resolved, TagReloc, assemble,
    assemble_with, finalize, finalize_with, resolve,
};
pub use frame::{Cell, CellId, Frame};
pub use validate::{Invalid, Limits, Where, validate, validate_with};

use crate::isa::{Ld64, Lds64, Push32, Sts64};
use crate::{Instr, Op, Region, Width};

#[cfg(test)]
mod tests;

/// Identifies a block within one [`Cfg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(u32);

impl BlockId {
    /// Position of the block in its graph.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The block at `index`, for naming a block by position.
    ///
    /// Nothing is checked here — a `BlockId` is only ever a position, and one
    /// no graph holds is reported as [`Invalid::NoSuchBlock`] when the graph
    /// that uses it is validated.
    pub const fn from_index(index: usize) -> Self {
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
    /// Push the address a region begins at.
    ///
    /// Symbolic for the same reason a cell is: where a region sits is decided
    /// when the image is laid out, and a program that baked the number in would
    /// have to be rebuilt every time anything before it changed size.
    Base(Region),
    /// Push a field's byte offset, resolved once the aggregate's layout is
    /// known. The `u32` names the hole; what it resolves to lives outside the
    /// graph.
    Field(u32),
    /// Push an enum variant's discriminant word, resolved once the type is
    /// known. The `u32` names the hole, like [`Item::Field`].
    Tag(u32),
    /// Pop an address and push the field there, at a width resolved once the
    /// aggregate's layout is known. The `u32` names the hole, like
    /// [`Item::Field`], which supplies the offset this load reads from.
    LoadField(u32),
}

impl Item {
    /// Net change to `SP`, in bytes.
    ///
    /// The symbolic forms know their effect without knowing their displacement,
    /// which is exactly what lets the stack depth be validated before anything
    /// is laid out — and the displacement is computed *from* that depth.
    /// Asks the instruction each form stands for rather than restating a
    /// number, so the effect cannot go stale if the instruction's ever changes.
    /// The operands are placeholders: a stack effect is a property of the
    /// opcode, never of what it carries.
    pub fn sp_delta(self) -> i32 {
        match self {
            Self::Instr(instr) => instr.sp_delta(),
            Self::Load(_) => Lds64 { disp: 0 }.sp_delta(),
            Self::Store(_) => Sts64 { disp: 0 }.sp_delta(),
            Self::Base(_) | Self::Field(_) | Self::Tag(_) => Push32 { imm: 0 }.sp_delta(),
            Self::LoadField(_) => Ld64.sp_delta(),
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
            Self::Jmp(_) | Self::Halt => crate::isa::Jmp { offset: 0 }.sp_delta(),
            Self::Br { .. } | Self::Switch { .. } => crate::isa::Jz { offset: 0 }.sp_delta(),
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
///
/// Its [`Debug`](core::fmt::Debug) form is an assembly-like listing:
///
/// ```
/// use safetynet_core::ir::{Cfg, Frame, Terminator};
/// use safetynet_core::isa::Push8;
///
/// let mut builder = Cfg::builder(Frame::new());
/// let entry = builder.block(0);
/// builder.at(entry).expect("open").instr(Push8 { imm: 7 });
/// builder.seal(entry, Terminator::Halt).expect("seals");
/// let cfg = builder.build(entry).expect("builds");
///
/// assert_eq!(format!("{cfg:?}"), "b0:\n    push8 7\n    halt\n");
/// ```
#[derive(Clone, PartialEq, Eq)]
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

impl core::fmt::Debug for Cfg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let cells = self.frame.cells();
        if !cells.is_empty() {
            f.write_str(".frame {")?;
            for (index, cell) in cells.iter().enumerate() {
                let separator = if index == 0 { " " } else { ", " };
                write!(f, "{separator}c{index}: {}", width_name(cell.width()))?;
            }
            f.write_str(" }\n")?;
        }

        // Block zero is where execution starts unless something says otherwise,
        // so the common case spells nothing.
        if self.entry.index() != 0 {
            writeln!(f, ".entry b{}", self.entry.index())?;
        }

        for (position, block) in self.blocks.iter().enumerate() {
            let next = self.blocks.get(position + 1).map(Block::id);

            writeln!(f, "b{}:", block.id().index())?;
            for item in block.code() {
                f.write_str(INDENT)?;
                write_item(f, *item)?;
                f.write_str("\n")?;
            }

            f.write_str(INDENT)?;
            write_term(f, block.term(), next)?;
            f.write_str("\n")?;
        }

        Ok(())
    }
}

/// Indent for everything inside a block in the `Debug` listing.
const INDENT: &str = "    ";

fn write_item(f: &mut core::fmt::Formatter<'_>, item: Item) -> core::fmt::Result {
    match item {
        Item::Instr(instr) => write!(f, "{instr}"),
        Item::Load(cell) => write!(f, "$load c{}", cell.index()),
        Item::Store(cell) => write!(f, "$store c{}", cell.index()),
        Item::Base(region) => write!(f, "$push .{}", region_name(region)),
        // The type and path live outside the graph, so only the hole shows.
        Item::Field(hole) => write!(f, "$field #{hole}"),
        Item::Tag(hole) => write!(f, "$tag #{hole}"),
        Item::LoadField(hole) => write!(f, "$loadfield #{hole}"),
    }
}

fn write_term(
    f: &mut core::fmt::Formatter<'_>,
    term: &Terminator,
    next: Option<BlockId>,
) -> core::fmt::Result {
    match term {
        Terminator::Halt => f.write_str("halt"),
        Terminator::Jmp(target) => write!(f, "jmp b{}", target.index()),

        // A conditional spells the arm that is not reached by falling through.
        // `then` is the non-zero arm, so falling into `els` leaves `jnz`.
        Terminator::Br { then, els } if next == Some(*els) => {
            write!(f, "jnz b{}", then.index())
        }
        Terminator::Br { then, els } if next == Some(*then) => {
            write!(f, "jz b{}", els.index())
        }
        Terminator::Br { then, els } => {
            write!(f, "br b{}, b{}", then.index(), els.index())
        }

        Terminator::Switch { arms, default } => {
            f.write_str("switch [")?;
            for (index, arm) in arms.iter().enumerate() {
                let separator = if index == 0 { "" } else { ", " };
                write!(f, "{separator}b{}", arm.index())?;
            }
            write!(f, "] default b{}", default.index())
        }
    }
}

/// How a width is spelled in a `.frame` line.
const fn width_name(width: Width) -> &'static str {
    match width {
        Width::U8 => "u8",
        Width::U32 => "u32",
        Width::U64 => "u64",
    }
}

/// How a region is spelled after `$push .`.
const fn region_name(region: Region) -> &'static str {
    match region {
        Region::Input => "input",
        Region::Scratch => "scratch",
        Region::Stack => "stack",
    }
}
