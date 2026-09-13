//! The SP invariant, checked.
//!
//! The validator folds each instruction's own `sp_delta` over a block — asking
//! the ops, never a table — and reconciles the result with what the front-end
//! recorded. Two properties come out of it. `SP` is a static function of the
//! program point, which is the only reason displacement addressing can be
//! decided at compile time at all; and a depth the front-end got wrong is a
//! rejected graph rather than a program that reads the cell next door.
//!
//! That second one is why `sp_in` is recorded and checked rather than computed.
//! A depth this pass derived on its own would agree with itself no matter what
//! the front-end meant. Here the front-end's claim comes from its own structure
//! and this pass's number comes from the graph's edges, so the two can disagree
//! — and a disagreement is exactly the bug worth catching.

use super::{BlockId, CellId, Cfg, Item};
use crate::isa::Instr;

#[cfg(test)]
mod tests;

/// Whole-graph limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most blocks one graph may hold.
    pub blocks: usize,
    /// Most items one graph may hold, across every block.
    pub items: usize,
    /// Deepest the operand stack may get, in bytes above the frame.
    pub depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            blocks: 4096,
            items: 65_536,
            depth: 4096,
        }
    }
}

/// Checks a graph against the SP invariant and the default [`Limits`].
pub fn validate(cfg: &Cfg) -> Result<(), Invalid> {
    validate_with(cfg, &Limits::default())
}

/// Checks a graph against the SP invariant and the given limits.
pub fn validate_with(cfg: &Cfg, limits: &Limits) -> Result<(), Invalid> {
    if cfg.blocks().len() > limits.blocks {
        return Err(Invalid::TooManyBlocks {
            blocks: cfg.blocks().len(),
            limit: limits.blocks,
        });
    }

    let items: usize = cfg.blocks().iter().map(|block| block.code().len()).sum();
    if items > limits.items {
        return Err(Invalid::TooManyItems {
            items,
            limit: limits.items,
        });
    }

    // Nothing is on the stack before the program starts.
    let entry = cfg
        .block(cfg.entry())
        .ok_or(Invalid::NoSuchBlock(cfg.entry()))?;
    if entry.sp_in() != 0 {
        return Err(Invalid::EntryDepth {
            recorded: entry.sp_in(),
        });
    }

    for block in cfg.blocks() {
        let at = block.id();
        let mut depth = block.sp_in();

        for (index, item) in block.code().iter().enumerate() {
            let where_ = Where {
                block: at,
                item: index,
            };
            check_item(cfg, where_, *item, depth)?;

            depth = step(depth, item.sp_delta()).ok_or(Invalid::NegativeDepth {
                at: where_,
                depth,
                delta: item.sp_delta(),
            })?;

            if depth > limits.depth {
                return Err(Invalid::TooDeep {
                    at: where_,
                    depth,
                    limit: limits.depth,
                });
            }
        }

        // The terminator's own effect: the conditional forms consume the word
        // they branch on, and doing that with nothing on the stack would reach
        // into the frame.
        let end = Where {
            block: at,
            item: block.code().len(),
        };
        let delta = block.term().sp_delta();
        let out = step(depth, delta).ok_or(Invalid::NegativeDepth {
            at: end,
            depth,
            delta,
        })?;

        // Every edge has to agree with what its target recorded. Checking each
        // predecessor against the recorded number rather than against the other
        // predecessors is what makes the message name the disagreement.
        for target in block.term().targets() {
            let successor = cfg.block(target).ok_or(Invalid::NoSuchBlock(target))?;

            if successor.sp_in() != out {
                return Err(Invalid::DepthMismatch {
                    block: at,
                    target,
                    recorded: successor.sp_in(),
                    actual: out,
                });
            }
        }
    }

    unreachable_block(cfg).map_or(Ok(()), |block| Err(Invalid::Unreachable(block)))
}

/// Checks what one item needs of the frame it names.
fn check_item(cfg: &Cfg, at: Where, item: Item, depth: u32) -> Result<(), Invalid> {
    let cell = match item {
        // The frame belongs to the graph, not to a block: finalization reserves
        // it from `Cfg::frame` in the prologue. A block that reserved its own
        // would make `sp_in` ambiguous about which side of the frame it counts.
        Item::Instr(Instr::Alloc(_) | Instr::Free(_)) => return Err(Invalid::FrameOp { at }),
        // A region's address is resolved from the image layout, which this
        // pass has no opinion about.
        Item::Instr(_) | Item::Base(_) => return Ok(()),
        Item::Load(cell) | Item::Store(cell) => cell,
    };

    let Some(found) = cfg.frame().cell(cell) else {
        return Err(Invalid::NoSuchCell { at, cell });
    };

    // `k = F − c + d`, the displacement this access will be given. It has to fit
    // the operand of `LDS`/`STS`, and failing here names the access rather than
    // waiting for materialization to fail on an anonymous number.
    let frame = u32::from(cfg.frame().size().bytes());
    let displacement = frame - u32::from(found.off()) + depth;
    if u16::try_from(displacement).is_err() {
        return Err(Invalid::DisplacementTooLarge {
            at,
            cell,
            displacement,
        });
    }

    Ok(())
}

/// Applies a stack effect, refusing to go below the frame.
fn step(depth: u32, delta: i32) -> Option<u32> {
    u32::try_from(i64::from(depth) + i64::from(delta)).ok()
}

/// The first block no edge reaches, if there is one.
fn unreachable_block(cfg: &Cfg) -> Option<BlockId> {
    let mut seen = vec![false; cfg.blocks().len()];
    let mut worklist = vec![cfg.entry()];

    while let Some(id) = worklist.pop() {
        let Some(visited) = seen.get_mut(id.index()) else {
            continue;
        };
        if core::mem::replace(visited, true) {
            continue;
        }

        if let Some(block) = cfg.block(id) {
            worklist.extend(block.term().targets());
        }
    }

    seen.iter()
        .position(|visited| !visited)
        .map(BlockId::from_index)
}

/// Where in a graph something went wrong, in the terms a front-end can map back
/// to a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Where {
    /// The block it happened in.
    pub block: BlockId,
    /// Position in that block's body; the terminator is one past the last item.
    pub item: usize,
}

impl core::fmt::Display for Where {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "block {}, item {}", self.block.index(), self.item)
    }
}

/// Why a graph is not a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    /// An edge names a block that is not in this graph.
    #[error("block {0:?} is not in this graph")]
    NoSuchBlock(BlockId),

    /// The entry block claims something is already on the stack.
    #[error("the entry block starts at depth {recorded}, but nothing is pushed yet")]
    EntryDepth {
        /// The depth it recorded.
        recorded: u32,
    },

    /// A predecessor leaves a depth its successor does not expect.
    ///
    /// Either the recorded number is wrong, or the path that reaches it is —
    /// and the front-end knows which, because it is the one that recorded it.
    #[error("block {block:?} leaves depth {actual}, but {target:?} records {recorded}")]
    DepthMismatch {
        /// The block the edge leaves.
        block: BlockId,
        /// The block it reaches.
        target: BlockId,
        /// The depth the target recorded.
        recorded: u32,
        /// The depth this path actually leaves.
        actual: u32,
    },

    /// A pop with nothing to pop: the next word down belongs to the frame.
    #[error("{at} pops past the frame: depth {depth}, effect {delta}")]
    NegativeDepth {
        /// Where it happened.
        at: Where,
        /// Depth before the item ran.
        depth: u32,
        /// The item's stack effect.
        delta: i32,
    },

    /// A block reserves or releases a frame of its own.
    #[error("{at} reserves or releases a frame; the frame belongs to the graph")]
    FrameOp {
        /// Where it happened.
        at: Where,
    },

    /// An access names a cell the frame does not have.
    #[error("{at} names {cell:?}, which is not in this frame")]
    NoSuchCell {
        /// Where it happened.
        at: Where,
        /// The cell it named.
        cell: CellId,
    },

    /// The access is real, but too far back from `SP` to encode.
    #[error("{at} needs displacement {displacement}, which does not fit")]
    DisplacementTooLarge {
        /// Where it happened.
        at: Where,
        /// The cell it named.
        cell: CellId,
        /// The displacement it would need.
        displacement: u32,
    },

    /// A block no edge reaches.
    ///
    /// Not unsound on its own, but its recorded depth is a claim nothing checks,
    /// and code the graph cannot reach is nearly always a lowering that lost
    /// track of an edge.
    #[error("block {0:?} is unreachable")]
    Unreachable(BlockId),

    /// More blocks than the limits allow.
    #[error("{blocks} blocks exceeds the limit of {limit}")]
    TooManyBlocks {
        /// How many the graph has.
        blocks: usize,
        /// The limit it passed.
        limit: usize,
    },

    /// More items than the limits allow.
    #[error("{items} items exceeds the limit of {limit}")]
    TooManyItems {
        /// How many the graph has.
        items: usize,
        /// The limit it passed.
        limit: usize,
    },

    /// The operand stack goes deeper than the limits allow.
    #[error("{at} reaches depth {depth}, past the limit of {limit}")]
    TooDeep {
        /// Where it happened.
        at: Where,
        /// The depth reached.
        depth: u32,
        /// The limit it passed.
        limit: u32,
    },
}
