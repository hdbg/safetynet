//! Materialization: the pass where everything symbolic acquires a number.
//!
//! Three things resolve here, and they resolve together because each needs what
//! the others know: a cell becomes a displacement back from `SP`, computed from
//! the depth this pass tracks; a block becomes a byte offset, computed from the
//! sizes of the instructions before it; and an edge becomes a relative branch,
//! computed from both.
//!
//! One pass is enough. An operand's width is fixed by its opcode, so no
//! instruction's size depends on the offset it ends up carrying, and the layout
//! cannot shift under its own resolution. That is what a variable-length operand
//! would have cost.

use super::{Block, BlockId, Cfg, Item, Terminator, Where, validate};
use crate::encoding::{Encoder, Packed};
use crate::ir::{Frame, Invalid};
use crate::isa::{Alloc, Instr, Jmp, Jnz, Jz, Lds8, Lds32, Lds64, Sts8, Sts32, Sts64};
use crate::{ByteOrder, Program, Width};

#[cfg(test)]
mod tests;

/// Lowers a validated graph to bytecode in the standard encoding.
pub fn finalize<B: ByteOrder>(cfg: &Cfg) -> Result<Program<B>, NotFinal> {
    finalize_with(cfg, &Packed::<B>::new())
}

/// Lowers a validated graph to bytecode, using `encoder` to measure and write.
///
/// Layout asks the encoder for sizes and never assumes any: an encoding that
/// pads, renumbers or reorders changes every offset in the program, and this is
/// the pass that would otherwise have to know about it.
///
/// Validation runs first rather than being assumed. A graph reaching here has
/// been through whatever passes were applied to it, and the invariant every
/// displacement below depends on has to hold *now*, not when it was built.
pub fn finalize_with<E: Encoder>(cfg: &Cfg, encoder: &E) -> Result<Program<E::Order>, NotFinal> {
    validate(cfg)?;

    let order = layout(cfg);
    let Emitted {
        mut code,
        patches,
        starts,
    } = emit(cfg, &order)?;

    // Byte offset of every instruction, plus one past the last, so a branch can
    // ask where the instruction after it begins.
    let mut offsets = Vec::with_capacity(code.len() + 1);
    let mut at = 0;
    for instr in &code {
        offsets.push(at);
        at += encoder.encoded_len(*instr).map_err(NotFinal::encode)?;
    }
    offsets.push(at);

    for patch in &patches {
        let target = *starts
            .get(patch.target.index())
            .ok_or(Invalid::NoSuchBlock(patch.target))?;

        let to = offset(&offsets, target)?;
        let next = offset(&offsets, patch.at + 1)?;
        let delta = i32::try_from(to - next).map_err(|_| NotFinal::BranchTooFar {
            target: patch.target,
            delta: to - next,
        })?;

        let slot = code.get_mut(patch.at).ok_or(NotFinal::Overflow)?;
        *slot = patch.kind.instr(delta);
    }

    let mut bytes = Vec::with_capacity(at);
    for (index, instr) in code.into_iter().enumerate() {
        let written = encoder
            .encode(instr, &mut bytes)
            .map_err(NotFinal::encode)?;

        // Every offset resolved above came from `encoded_len`. An encoder whose
        // two halves disagree would move each instruction out from under the
        // branches aimed at it, so it is caught here rather than at run time.
        let measured = offset(&offsets, index + 1)? - offset(&offsets, index)?;
        if i64::try_from(written).unwrap_or(i64::MAX) != measured {
            return Err(NotFinal::EncoderDisagrees { measured, written });
        }
    }

    Ok(Program::new(bytes, cfg.frame().size()))
}

/// Which block goes where.
///
/// Entry first, then the rest in the order they were created. Layout is a real
/// choice — it decides which edges become fallthroughs and cost nothing — but a
/// deterministic order matters more than a clever one while the graph is small,
/// and a reordering pass belongs with the other mutations.
fn layout(cfg: &Cfg) -> Vec<BlockId> {
    let entry = cfg.entry();
    core::iter::once(entry)
        .chain(cfg.blocks().iter().map(Block::id).filter(|id| *id != entry))
        .collect()
}

/// The instruction stream, before its branches know where they are going.
struct Emitted {
    /// Every instruction, with branch offsets left at zero.
    code: Vec<Instr>,
    /// The branches, and what each one aims at.
    patches: Vec<Patch>,
    /// Where each block starts, indexed by [`BlockId::index`].
    starts: Vec<usize>,
}

/// A branch whose offset is not known until every instruction has been measured.
struct Patch {
    /// Index of the branch in the instruction stream.
    at: usize,
    /// The block it aims at.
    target: BlockId,
    /// Which branch it is.
    kind: Branch,
}

/// The three branch shapes a terminator lowers to.
#[derive(Debug, Clone, Copy)]
enum Branch {
    Always,
    IfZero,
    IfNotZero,
}

impl Branch {
    /// The instruction, once the offset is known.
    fn instr(self, offset: i32) -> Instr {
        match self {
            Self::Always => Jmp { offset }.into(),
            Self::IfZero => Jz { offset }.into(),
            Self::IfNotZero => Jnz { offset }.into(),
        }
    }
}

/// Builds the instruction stream, with branch offsets left for the caller.
fn emit(cfg: &Cfg, order: &[BlockId]) -> Result<Emitted, NotFinal> {
    let frame = cfg.frame();
    let mut code = Vec::new();
    let mut patches = Vec::new();
    let mut starts = vec![0; cfg.blocks().len()];

    // The prologue, reserving the frame the whole graph shares. There is no
    // matching epilogue: nothing runs after `HALT`, and releasing the frame
    // would take the result on top of the stack with it.
    if frame.size().bytes() > 0 {
        code.push(Alloc { n: frame.size() }.into());
    }

    for (position, id) in order.iter().enumerate() {
        let block = cfg.block(*id).ok_or(Invalid::NoSuchBlock(*id))?;
        *starts
            .get_mut(id.index())
            .ok_or(Invalid::NoSuchBlock(*id))? = code.len();

        let mut depth = block.sp_in();
        for (index, item) in block.code().iter().enumerate() {
            let at = Where {
                block: *id,
                item: index,
            };
            code.push(materialize(frame, *item, depth, at)?);
            depth = depth.wrapping_add_signed(item.sp_delta());
        }

        let next = order.get(position + 1).copied();
        let mut branch = |kind: Branch, target: BlockId, code: &mut Vec<Instr>| {
            patches.push(Patch {
                at: code.len(),
                target,
                kind,
            });
            code.push(kind.instr(0));
        };

        match block.term() {
            Terminator::Halt => code.push(crate::isa::Halt.into()),
            // An edge to the block that follows is not a branch at all.
            Terminator::Jmp(target) if next == Some(*target) => {}
            Terminator::Jmp(target) => branch(Branch::Always, *target, &mut code),
            Terminator::Br { then, els } if next == Some(*els) => {
                branch(Branch::IfNotZero, *then, &mut code);
            }
            Terminator::Br { then, els } if next == Some(*then) => {
                branch(Branch::IfZero, *els, &mut code);
            }
            Terminator::Br { then, els } => {
                branch(Branch::IfNotZero, *then, &mut code);
                branch(Branch::Always, *els, &mut code);
            }
            Terminator::Switch { .. } => {
                return Err(NotFinal::Unsupported {
                    block: *id,
                    what: "switch",
                });
            }
        }
    }

    Ok(Emitted {
        code,
        patches,
        starts,
    })
}

/// Turns one item into the instruction it stands for.
fn materialize(frame: &Frame, item: Item, depth: u32, at: Where) -> Result<Instr, NotFinal> {
    let (cell, storing) = match item {
        Item::Instr(instr) => return Ok(instr),
        Item::Load(cell) => (cell, false),
        Item::Store(cell) => (cell, true),
    };

    let found = frame.cell(cell).ok_or(Invalid::NoSuchCell { at, cell })?;

    // `k = F − c + d`. For a store the depth still counts the word being
    // stored, because the displacement is measured before the pop.
    let displacement = u32::from(frame.size().bytes()) - u32::from(found.off()) + depth;
    let disp = u16::try_from(displacement).map_err(|_| Invalid::DisplacementTooLarge {
        at,
        cell,
        displacement,
    })?;

    Ok(match (found.width(), storing) {
        (Width::U8, false) => Lds8 { disp }.into(),
        (Width::U32, false) => Lds32 { disp }.into(),
        (Width::U64, false) => Lds64 { disp }.into(),
        (Width::U8, true) => Sts8 { disp }.into(),
        (Width::U32, true) => Sts32 { disp }.into(),
        (Width::U64, true) => Sts64 { disp }.into(),
    })
}

/// The byte offset of the instruction at `index`, as an `i64` for the
/// subtraction a relative branch is.
fn offset(offsets: &[usize], index: usize) -> Result<i64, NotFinal> {
    offsets
        .get(index)
        .copied()
        .and_then(|at| i64::try_from(at).ok())
        .ok_or(NotFinal::Overflow)
}

/// Why a graph did not become bytecode.
#[derive(Debug, thiserror::Error)]
pub enum NotFinal {
    /// The graph does not hold up. Every displacement below rests on this, so
    /// finalization refuses to guess.
    #[error("the graph is not valid: {0}")]
    Invalid(#[from] Invalid),

    /// Something the instruction set does not lower yet.
    #[error("block {block:?} ends in `{what}`, which has no encoding yet")]
    Unsupported {
        /// The block it is in.
        block: BlockId,
        /// What was found there.
        what: &'static str,
    },

    /// A branch further than a relative offset can reach.
    #[error("a branch to {target:?} is {delta} bytes away, too far to encode")]
    BranchTooFar {
        /// The block it aims at.
        target: BlockId,
        /// How far away it turned out to be.
        delta: i64,
    },

    /// The program is larger than an offset can describe.
    #[error("the program is too large to lay out")]
    Overflow,

    /// The encoder measured one length and wrote another, which would leave
    /// every branch aimed at the wrong byte.
    #[error("the encoder measured {measured} bytes and wrote {written}")]
    EncoderDisagrees {
        /// What `encoded_len` promised.
        measured: i64,
        /// What `encode` produced.
        written: usize,
    },

    /// An instruction would not encode.
    #[error("could not encode an instruction: {0}")]
    Encode(Box<dyn core::error::Error + Send + Sync>),
}

impl NotFinal {
    /// Wraps whatever error the encoder in use reports.
    fn encode(error: impl core::error::Error + Send + Sync + 'static) -> Self {
        Self::Encode(Box::new(error))
    }
}
