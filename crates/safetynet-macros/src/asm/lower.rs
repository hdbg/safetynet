//! Pass two: labels become edges, and the result is handed to the validator.
//!
//! Two things are worked out here that the text never spells. Falling through
//! is an edge to whatever block comes next, which only this pass knows; and the
//! stack depth a block is entered at is followed along the edges from the entry,
//! where nothing is pushed yet.
//!
//! The depths are a claim, not a proof — the validator recomputes them and says
//! so when they disagree. That is the point of writing them down: a number this
//! pass derived and then checked against itself would agree no matter what the
//! program meant.

use std::collections::HashMap;
use std::collections::VecDeque;

use proc_macro2::Span;
use safetynet_core::ir::{
    BlockId, BuildError, Cfg, Frame, Invalid, Item, Terminator, Where, validate,
};

use super::ast::{CellDecl, Program, RawBlock, RawItem, RawTerm};
use crate::backend::{FieldRef, TagRef};

/// A program that has been assembled: the graph, and what is needed to talk
/// about it in the user's own words.
#[derive(Debug)]
pub(crate) struct Lowered {
    /// Position of the block execution starts at.
    pub(crate) entry: u32,
    /// The graph itself.
    pub(crate) cfg: Cfg,
    /// Field references, indexed by the hole id in [`Item::Field`].
    pub(crate) field_refs: Vec<FieldRef>,
    /// Enum-variant references, indexed by the hole id in [`Item::Tag`].
    pub(crate) tag_refs: Vec<TagRef>,
    spans: SpanTable,
    names: Names,
}

impl Lowered {
    /// Checks the graph, reporting a failure at the statement that caused it.
    pub(crate) fn check(&self) -> syn::Result<()> {
        validate(&self.cfg).map_err(|invalid| self.diagnose(invalid))
    }

    /// Turns a rejection into an error the compiler can point at.
    fn diagnose(&self, invalid: Invalid) -> syn::Error {
        let span = match invalid {
            // These name a statement, and the statement is where the mistake is
            // visible even when its cause is further back.
            Invalid::NegativeDepth { at, .. }
            | Invalid::FrameOp { at }
            | Invalid::NoSuchCell { at, .. }
            | Invalid::DisplacementTooLarge { at, .. }
            | Invalid::TooDeep { at, .. } => self.spans.at(at),

            // The edge is the disagreement, so blame the jump that draws it
            // rather than the block that recorded the other number.
            Invalid::DepthMismatch { block, .. } => self.spans.term(block),

            Invalid::Unreachable(block) => self.spans.label(block),
            Invalid::EntryDepth { .. } => {
                self.spans.label(BlockId::from_index(self.entry as usize))
            }

            // Nothing smaller than the whole program is to blame.
            Invalid::NoSuchBlock(_)
            | Invalid::TooManyBlocks { .. }
            | Invalid::TooManyItems { .. } => Span::call_site(),
        };

        syn::Error::new(span, self.names.rewrite(&invalid.to_string()))
    }
}

/// Assembles a parsed program into a graph.
pub(crate) fn lower(program: Program) -> syn::Result<Lowered> {
    let Program {
        cells,
        entry,
        blocks,
    } = program;

    if blocks.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "a program needs at least one block",
        ));
    }

    // The frame is laid out one cell at a time rather than in one call, so that
    // a frame that grows out of reach names the cell that pushed it there.
    let mut frame = Frame::new();
    for cell in &cells {
        frame.add(cell.width).ok_or_else(|| {
            syn::Error::new(cell.name.span(), "the frame has no room left for this cell")
        })?;
    }

    let labels = label_table(&blocks)?;
    let entry = match entry {
        Some(name) => *labels
            .get(&name.to_string())
            .ok_or_else(|| unknown(&name))?,
        None => 0,
    };

    let mut terms = Vec::with_capacity(blocks.len());
    for (position, block) in blocks.iter().enumerate() {
        terms.push(terminator(block, position, blocks.len(), &labels)?);
    }

    let depths = depths(&blocks, &terms, entry);
    let spans = SpanTable::of(&blocks);
    let names = Names::of(&blocks, &cells);

    let mut builder = Cfg::builder(frame);
    let ids: Vec<BlockId> = depths.iter().map(|depth| builder.block(*depth)).collect();

    let mut field_refs = Vec::new();
    let mut tag_refs = Vec::new();
    for ((block, term), id) in blocks.into_iter().zip(terms).zip(&ids) {
        let body = builder.at(*id).map_err(internal)?;
        for (item, _) in block.items {
            match item {
                RawItem::Core(Item::Instr(instr)) => {
                    body.instr(instr);
                }
                RawItem::Core(Item::Load(cell)) => {
                    body.load(cell);
                }
                RawItem::Core(Item::Store(cell)) => {
                    body.store(cell);
                }
                RawItem::Core(Item::Base(region)) => {
                    body.base(region);
                }
                RawItem::Core(Item::Field(hole)) => {
                    body.field(hole);
                }
                RawItem::Core(Item::Tag(hole)) => {
                    body.tag(hole);
                }
                RawItem::Core(Item::LoadField(hole)) => {
                    body.load_field(hole);
                }
                RawItem::Field { ty, path } => {
                    let hole = u32::try_from(field_refs.len()).map_err(|_| {
                        syn::Error::new(
                            Span::call_site(),
                            "this program has too many field references",
                        )
                    })?;
                    body.field(hole);
                    let ty: syn::Type = syn::parse_quote!(#ty);
                    field_refs.push(FieldRef { ty, path });
                }
                RawItem::Tag(path) => {
                    let hole = u32::try_from(tag_refs.len()).map_err(|_| {
                        syn::Error::new(
                            Span::call_site(),
                            "this program has too many tag references",
                        )
                    })?;
                    body.tag(hole);
                    tag_refs.push(TagRef { path });
                }
            }
        }
        builder.seal(*id, term).map_err(internal)?;
    }

    let cfg = builder
        .build(BlockId::from_index(entry))
        .map_err(internal)?;

    Ok(Lowered {
        entry: u32::try_from(entry)
            .map_err(|_| syn::Error::new(Span::call_site(), "this program has too many blocks"))?,
        cfg,
        field_refs,
        tag_refs,
        spans,
        names,
    })
}

/// Where each label points, rejecting the second of two with the same name.
fn label_table(blocks: &[RawBlock]) -> syn::Result<HashMap<String, usize>> {
    let mut labels = HashMap::with_capacity(blocks.len());

    for (position, block) in blocks.iter().enumerate() {
        let name = block.label.to_string();
        if labels.insert(name, position).is_some() {
            return Err(syn::Error::new(
                block.label.span(),
                format!(
                    "the label `{}` is already used by another block",
                    block.label
                ),
            ));
        }
    }

    Ok(labels)
}

/// Resolves one block's ending, drawing the fallthrough edge where the text
/// leaves one implied.
fn terminator(
    block: &RawBlock,
    position: usize,
    count: usize,
    labels: &HashMap<String, usize>,
) -> syn::Result<Terminator> {
    let next = || {
        let after = position + 1;
        (after < count).then(|| BlockId::from_index(after))
    };

    let Some((raw, at)) = block.term.as_ref() else {
        return next().map(Terminator::Jmp).ok_or_else(|| {
            syn::Error::new(
                block.label.span(),
                "this block never ends: the last block needs a terminator",
            )
        });
    };

    let target = |name| resolve(labels, name);
    let fallthrough = || {
        next().ok_or_else(|| {
            syn::Error::new(
                *at,
                "nothing to fall through to: this is the last block, and the other arm needs one",
            )
        })
    };

    Ok(match raw {
        RawTerm::Halt => Terminator::Halt,
        RawTerm::Jmp(name) => Terminator::Jmp(target(name)?),

        // `then` is the non-zero arm, so a jump taken when the word is zero
        // names the zero arm and falls into the other one.
        RawTerm::Jz(name) => Terminator::Br {
            then: fallthrough()?,
            els: target(name)?,
        },
        RawTerm::Jnz(name) => Terminator::Br {
            then: target(name)?,
            els: fallthrough()?,
        },

        RawTerm::Br { then, els } => Terminator::Br {
            then: target(then)?,
            els: target(els)?,
        },
        RawTerm::Switch { arms, default } => Terminator::Switch {
            arms: arms.iter().map(target).collect::<syn::Result<_>>()?,
            default: target(default)?,
        },
    })
}

/// The block a label names.
fn resolve(labels: &HashMap<String, usize>, name: &syn::Ident) -> syn::Result<BlockId> {
    labels
        .get(&name.to_string())
        .map(|position| BlockId::from_index(*position))
        .ok_or_else(|| unknown(name))
}

/// The stack depth each block is entered at, followed from the entry.
///
/// A block that no edge reaches keeps zero, and a path that would leave a
/// negative depth is clamped: both are graphs the validator rejects, and it
/// does so with a message that names the item rather than the block.
fn depths(blocks: &[RawBlock], terms: &[Terminator], entry: usize) -> Vec<u32> {
    let mut depths: Vec<Option<u32>> = vec![None; blocks.len()];
    let mut queue = VecDeque::new();

    if let Some(slot) = depths.get_mut(entry) {
        *slot = Some(0);
        queue.push_back(entry);
    }

    while let Some(position) = queue.pop_front() {
        let Some(Some(depth)) = depths.get(position).copied() else {
            continue;
        };
        let (Some(block), Some(term)) = (blocks.get(position), terms.get(position)) else {
            continue;
        };

        let effect: i64 = block
            .items
            .iter()
            .map(|(item, _)| i64::from(item.sp_delta()))
            .sum::<i64>()
            + i64::from(term.sp_delta());
        let out = u32::try_from(i64::from(depth) + effect).unwrap_or(0);

        for target in term.targets() {
            let index = target.index();
            if let Some(slot) = depths.get_mut(index)
                && slot.is_none()
            {
                *slot = Some(out);
                queue.push_back(index);
            }
        }
    }

    depths.into_iter().map(Option::unwrap_or_default).collect()
}

/// The error for a label nothing declares.
fn unknown(name: &syn::Ident) -> syn::Error {
    syn::Error::new(name.span(), format!("unknown label `{name}`"))
}

/// A builder failure, which means this pass built something impossible.
fn internal(error: BuildError) -> syn::Error {
    syn::Error::new(
        Span::call_site(),
        format!("internal error while assembling: {error}"),
    )
}

/// Where every statement of the program was written.
#[derive(Debug)]
struct SpanTable {
    blocks: Vec<BlockSpans>,
}

#[derive(Debug)]
struct BlockSpans {
    label: Span,
    items: Vec<Span>,
    /// A block that falls through has no terminator to point at, so its label
    /// stands in.
    term: Span,
}

impl SpanTable {
    fn of(blocks: &[RawBlock]) -> Self {
        Self {
            blocks: blocks
                .iter()
                .map(|block| BlockSpans {
                    label: block.label.span(),
                    items: block.items.iter().map(|(_, span)| *span).collect(),
                    term: block
                        .term
                        .as_ref()
                        .map_or_else(|| block.label.span(), |(_, span)| *span),
                })
                .collect(),
        }
    }

    /// The statement a position names; one past the last item is the
    /// terminator, which is how the validator counts.
    fn at(&self, at: Where) -> Span {
        self.blocks
            .get(at.block.index())
            .map_or_else(Span::call_site, |block| {
                block.items.get(at.item).copied().unwrap_or(block.term)
            })
    }

    fn term(&self, block: BlockId) -> Span {
        self.blocks
            .get(block.index())
            .map_or_else(Span::call_site, |block| block.term)
    }

    fn label(&self, block: BlockId) -> Span {
        self.blocks
            .get(block.index())
            .map_or_else(Span::call_site, |block| block.label)
    }
}

/// What the program called its blocks and cells.
#[derive(Debug)]
struct Names {
    labels: Vec<String>,
    cells: Vec<String>,
}

impl Names {
    fn of(blocks: &[RawBlock], cells: &[CellDecl]) -> Self {
        Self {
            labels: blocks.iter().map(|block| block.label.to_string()).collect(),
            cells: cells.iter().map(|cell| cell.name.to_string()).collect(),
        }
    }

    /// Puts the program's own names back into a message written in ids.
    ///
    /// The validator talks about positions because that is all a graph has;
    /// the names are here, so the message it wrote is repaired rather than
    /// restated in a second wording that could drift from it.
    fn rewrite(&self, message: &str) -> String {
        let quoted =
            |names: &[String], index: usize| names.get(index).map(|name| format!("`{name}`"));

        let ids = substitute("BlockId(", ")", message, |index| {
            quoted(&self.labels, index)
        });
        let cells = substitute("CellId(", ")", &ids, |index| quoted(&self.cells, index));

        // `Where` prints a block by position rather than as an id.
        substitute("block ", ",", &cells, |index| {
            quoted(&self.labels, index).map(|name| format!("block {name},"))
        })
    }
}

/// Replaces every `prefix<number><suffix>` the replacement has a name for.
fn substitute(
    prefix: &str,
    suffix: &str,
    message: &str,
    replacement: impl Fn(usize) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while let Some((before, after)) = rest.split_once(prefix) {
        out.push_str(before);

        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let tail = after.strip_prefix(digits.as_str()).unwrap_or(after);

        match (
            digits.parse::<usize>().ok().and_then(&replacement),
            tail.strip_prefix(suffix),
        ) {
            (Some(name), Some(tail)) => {
                out.push_str(&name);
                rest = tail;
            }
            // Not an id after all: put back what was consumed and carry on.
            _ => {
                out.push_str(prefix);
                rest = after;
            }
        }
    }

    out.push_str(rest);
    out
}
