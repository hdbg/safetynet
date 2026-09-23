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
//!
//! # Two stages
//!
//! Resolution splits in two. [`resolve`] does the layout- and order-independent
//! work — validation, block placement, branch backpatching, frame displacements
//! — and leaves one thing symbolic: a region's base, which only the [`Layout`]
//! knows. [`assemble`] encodes that resolution into an [`Artifact`], a byte array
//! carrying a small table of relocations, and [`Artifact::finalize`] runs the
//! relocations against a layout to hand back a [`Program`]. Splitting there lets
//! an assembler bake finished bytecode at compile time and defer only the address
//! it cannot know until the host chooses one.

use core::marker::PhantomData;
use std::borrow::Cow;

use super::{Block, BlockId, Cfg, Item, Terminator, Where, validate};
use crate::encoding::{Encoder, Packed, encoded_len};
use crate::ir::{Frame, Invalid};
use crate::isa::{
    Alloc, Instr, Jmp, Jnz, Jz, Lds8, Lds32, Lds64, Push32, Push64, Sts8, Sts32, Sts64,
};
use crate::{ByteOrder, FrameSize, Layout, Program, Region, Width};

crate::opaque_debug!(
    Resolved,
    BaseReloc,
    LenReloc,
    FieldReloc,
    TagReloc,
    LoadReloc,
    Reloc,
    Artifact<B: ByteOrder>
);
crate::opaque_error!(NotFinal);

impl From<Invalid> for NotFinal {
    fn from(error: Invalid) -> Self {
        Self::Invalid(error)
    }
}

#[cfg(test)]
mod tests;

/// Lowers a validated graph to bytecode in the standard encoding.
pub fn finalize<B: ByteOrder>(cfg: &Cfg, layout: &Layout) -> Result<Program<B>, NotFinal> {
    assemble::<B>(cfg)?.finalize(layout)
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
pub fn finalize_with<E: Encoder + Clone + 'static>(
    cfg: &Cfg,
    layout: &Layout,
    encoder: &E,
) -> Result<Program<E::Order>, NotFinal> {
    assemble_with(cfg, encoder)?.finalize(layout)
}

/// A graph resolved to instructions, still without a byte order or an image.
///
/// Everything a graph decides on its own is decided here: which block goes
/// where, what each branch reaches, how deep the stack is at every access.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone)]
pub struct Resolved {
    /// Every instruction, branches patched and frame displacements filled, with
    /// each region-base push left as a placeholder named by `relocs`.
    code: Vec<Instr>,
    /// Bytes the prologue reserves for locals.
    frame: FrameSize,
    /// The region-base pushes whose immediate the layout still owes.
    relocs: Vec<BaseReloc>,
    /// The region-length pushes whose immediate the layout still owes.
    len_relocs: Vec<LenReloc>,
    /// The field-offset pushes whose immediate a layout still owes.
    field_relocs: Vec<FieldReloc>,
    /// The discriminant pushes whose immediate the type still owes.
    tag_relocs: Vec<TagReloc>,
    /// The field loads whose width a layout still owes.
    load_relocs: Vec<LoadReloc>,
    /// The branches and where each block starts, kept so an encoder that changes
    /// instruction sizes can recompute the offsets against its own measurements.
    patches: Vec<Patch>,
    /// Where each block starts, indexed by [`BlockId::index`].
    starts: Vec<usize>,
}

impl Resolved {
    /// The resolved instruction stream.
    pub fn code(&self) -> &[Instr] {
        &self.code
    }

    /// Bytes the prologue reserves for locals.
    pub const fn frame(&self) -> FrameSize {
        self.frame
    }

    /// The region-base relocations, each naming an instruction the layout fills.
    pub fn relocs(&self) -> &[BaseReloc] {
        &self.relocs
    }

    /// The region-length relocations, each naming an instruction the layout fills.
    pub fn len_relocs(&self) -> &[LenReloc] {
        &self.len_relocs
    }

    /// The field-offset relocations, each naming an instruction and its hole.
    pub fn field_relocs(&self) -> &[FieldReloc] {
        &self.field_relocs
    }

    /// The discriminant relocations, each naming an instruction and its hole.
    pub fn tag_relocs(&self) -> &[TagReloc] {
        &self.tag_relocs
    }

    /// The field-load relocations, each naming a load and its hole.
    pub fn load_relocs(&self) -> &[LoadReloc] {
        &self.load_relocs
    }
}

/// A region-base push whose immediate is filled in once a layout is chosen.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BaseReloc {
    /// Index of the push in [`Resolved::code`].
    pub index: usize,
    /// The region whose base it wants.
    pub region: Region,
}

/// A region-length push whose immediate is filled in once a layout is chosen.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LenReloc {
    /// Index of the push in [`Resolved::code`].
    pub index: usize,
    /// The region whose length it wants.
    pub region: Region,
}

/// A field-offset push whose immediate is filled in from an aggregate's layout.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FieldReloc {
    /// Index of the push in [`Resolved::code`].
    pub index: usize,
    /// Names the hole; what it resolves to lives outside the graph.
    pub hole: u32,
}

/// A discriminant push whose immediate is filled in from an enum type.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TagReloc {
    /// Index of the push in [`Resolved::code`].
    pub index: usize,
    /// Names the hole; what it resolves to lives outside the graph.
    pub hole: u32,
}

/// A field load whose opcode width is filled in from an aggregate's layout.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LoadReloc {
    /// Index of the load in [`Resolved::code`].
    pub index: usize,
    /// Names the hole; the field it reads lives outside the graph.
    pub hole: u32,
}

/// Validates a graph and resolves it to instructions, without a byte order.
///
/// Everything but a region's base is decided here, so the result is the same
/// whatever order it is later encoded in and whatever image it runs against.
pub fn resolve(cfg: &Cfg) -> Result<Resolved, NotFinal> {
    validate(cfg)?;

    let order = block_order(cfg);
    let Emitted {
        mut code,
        patches,
        starts,
        relocs,
        len_relocs,
        field_relocs,
        tag_relocs,
        load_relocs,
    } = emit(cfg, &order)?;

    // Branch offsets, computed against the standard packed sizes. Order does not
    // enter into it — a length is a property of the opcode — so the resolution
    // holds for either byte order. An encoder that changes sizes recomputes these
    // against its own measurements when it encodes.
    let offsets = offsets(&code, &encoded_len_all(&code)?);
    patch_branches(&mut code, &patches, &starts, &offsets)?;

    Ok(Resolved {
        code,
        frame: cfg.frame().size(),
        relocs,
        len_relocs,
        field_relocs,
        tag_relocs,
        load_relocs,
        patches,
        starts,
    })
}

/// Resolves a graph and encodes it into an [`Artifact`] in the standard encoding.
pub fn assemble<B: ByteOrder>(cfg: &Cfg) -> Result<Artifact<B>, NotFinal> {
    assemble_with(cfg, &Packed::<B>::new())
}

/// Resolves a graph and encodes it with `encoder` into an [`Artifact`].
///
/// The branch offsets from [`resolve`] are recomputed here against whatever
/// `encoder` measures, so a format that pads or renumbers still lands its
/// branches. Each region-base push becomes a [`Reloc`] whose closure re-encodes
/// it in place once a layout supplies the address.
pub fn assemble_with<E: Encoder + Clone + 'static>(
    cfg: &Cfg,
    encoder: &E,
) -> Result<Artifact<E::Order>, NotFinal> {
    let resolved = resolve(cfg)?;

    // Field offsets and discriminants need a type, which this path does not
    // carry; only an assembler that knows the type can resolve them.
    if !resolved.field_relocs.is_empty()
        || !resolved.tag_relocs.is_empty()
        || !resolved.load_relocs.is_empty()
    {
        return Err(NotFinal::FieldWithoutLayout);
    }

    let mut code = resolved.code;

    // Byte offset of every instruction, plus one past the last, measured by this
    // encoder. A pass that pads moves each instruction, so the branches are
    // re-patched against these before anything is written.
    let sizes = code
        .iter()
        .map(|instr| encoder.encoded_len(*instr).map_err(NotFinal::encode))
        .collect::<Result<Vec<_>, _>>()?;
    let offsets = offsets(&code, &sizes);

    // The branches were patched once against packed sizes; redo them now that the
    // real sizes are known. For the standard encoding this changes nothing.
    patch_branches(&mut code, &resolved.patches, &resolved.starts, &offsets)?;

    let mut bytes = Vec::with_capacity(offsets.last().copied().unwrap_or(0));
    for (index, instr) in code.iter().enumerate() {
        let written = encoder
            .encode(*instr, &mut bytes)
            .map_err(NotFinal::encode)?;

        // Every offset above came from `encoded_len`. An encoder whose two halves
        // disagree would move each instruction out from under the branches aimed
        // at it, so it is caught here rather than at run time.
        let measured = offset(&offsets, index + 1)? - offset(&offsets, index)?;
        if i64::try_from(written).unwrap_or(i64::MAX) != measured {
            return Err(NotFinal::EncoderDisagrees { measured, written });
        }
    }

    let span = |index: usize| -> Result<(usize, usize), NotFinal> {
        let start = offsets.get(index).copied().ok_or(NotFinal::Overflow)?;
        let end = offsets.get(index + 1).copied().ok_or(NotFinal::Overflow)?;
        Ok((start, end - start))
    };

    let mut relocs = Vec::with_capacity(resolved.relocs.len() + resolved.len_relocs.len());
    for base in &resolved.relocs {
        let (start, len) = span(base.index)?;
        let region = base.region;
        let encoder = encoder.clone();
        relocs.push(Reloc::new(start, len, move |layout, slice| {
            reencode(&encoder, base_push(region, layout), slice)
        }));
    }
    for reloc in &resolved.len_relocs {
        let (start, len) = span(reloc.index)?;
        let region = reloc.region;
        let encoder = encoder.clone();
        relocs.push(Reloc::new(start, len, move |layout, slice| {
            reencode(&encoder, len_push(region, layout), slice)
        }));
    }

    Ok(Artifact::new(bytes, resolved.frame, relocs))
}

/// Bytecode with the addresses still to fill: a byte array plus the relocations
/// that a layout resolves.
///
/// The form an assembler hands back. Everything a graph can settle on its own is
/// already bytes; a region's base is the one thing left, carried as a closure
/// that rewrites its push once [`finalize`](Artifact::finalize) is given a
/// layout.
pub struct Artifact<B: ByteOrder> {
    code: Cow<'static, [u8]>,
    rodata: Cow<'static, [u8]>,
    frame: FrameSize,
    relocs: Vec<Reloc>,
    order: PhantomData<B>,
}

impl<B: ByteOrder> Artifact<B> {
    /// Wraps bytecode and its relocation table, with no constants.
    pub fn new(code: impl Into<Cow<'static, [u8]>>, frame: FrameSize, relocs: Vec<Reloc>) -> Self {
        Self {
            code: code.into(),
            rodata: Cow::Borrowed(&[]),
            frame,
            relocs,
            order: PhantomData,
        }
    }

    /// Attaches the constants the code reads from `.rodata`, already in the
    /// order `B` the code was built in.
    ///
    /// They are the program's to bring, not the host's: whoever runs the
    /// artifact writes them into an image whose `.rodata` is at least this long.
    #[must_use]
    pub fn with_rodata(mut self, rodata: impl Into<Cow<'static, [u8]>>) -> Self {
        self.rodata = rodata.into();
        self
    }

    /// The bytecode, region bases still at their placeholders.
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    /// The constants to place in `.rodata` before a run.
    pub fn rodata(&self) -> &[u8] {
        &self.rodata
    }

    /// Bytes the prologue reserves for locals.
    pub const fn frame(&self) -> FrameSize {
        self.frame
    }

    /// Runs every relocation against `layout`, then hands back runnable bytecode.
    pub fn finalize(&self, layout: &Layout) -> Result<Program<B>, NotFinal> {
        let mut code = self.code.clone().into_owned();
        for reloc in &self.relocs {
            let slice = code
                .get_mut(reloc.at..reloc.at + reloc.len)
                .ok_or(NotFinal::Overflow)?;
            (reloc.apply)(layout, slice)?;
        }

        Ok(Program::new(code, self.frame))
    }
}

#[cfg(feature = "debug")]
impl<B: ByteOrder> core::fmt::Debug for Artifact<B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Artifact")
            .field("order", &B::NAME)
            .field("code", &self.code.len())
            .field("rodata", &self.rodata.len())
            .field("frame", &self.frame.bytes())
            .field("relocs", &self.relocs.len())
            .finish()
    }
}

/// One region-base fixup: where its push sits, how long it is, and how to rewrite
/// it once the address is known.
///
/// The closure is the whole of it — it re-encodes a single `Push32` in the byte
/// order the artifact was built in, so the relocation carries its own codec and
/// nothing else has to know which order that was.
pub struct Reloc {
    at: usize,
    len: usize,
    apply: Apply,
}

/// The closure a [`Reloc`] runs: given the layout, rewrite the bytes of one slot.
type Apply = Box<dyn Fn(&Layout, &mut [u8]) -> Result<(), NotFinal>>;

impl Reloc {
    /// A relocation over the `len` bytes at `at`, rewritten by `apply`.
    #[inline(always)]
    pub fn new(
        at: usize,
        len: usize,
        apply: impl Fn(&Layout, &mut [u8]) -> Result<(), NotFinal> + 'static,
    ) -> Self {
        Self {
            at,
            len,
            apply: Box::new(apply),
        }
    }

    /// The standard region-base relocation: re-encode the region's base as a
    /// `Push32` with the encoder `E`, where the base is where `region` ends up in
    /// the layout.
    ///
    /// Generic over the encoder rather than tied to one wire format. The encoder
    /// carries its own byte order, and a macro expansion hands it the packed one
    /// — the only format a shipped program decodes.
    #[inline(always)]
    pub fn region_base<E: Encoder + Default + 'static>(at: usize, region: Region) -> Self {
        let encoder = E::default();
        // Measure the slot from the push it holds rather than assuming a width.
        let len = encoder
            .encoded_len(Push32 { imm: 0 }.into())
            .unwrap_or_default();
        Self::new(at, len, move |layout, slice| {
            reencode(&encoder, base_push(region, layout), slice)
        })
    }

    /// The region-length relocation: re-encode the region's size as a `Push32`
    /// with the encoder `E`, the way [`region_base`](Self::region_base) does
    /// its address.
    #[inline(always)]
    pub fn region_len<E: Encoder + Default + 'static>(at: usize, region: Region) -> Self {
        let encoder = E::default();
        let len = encoder
            .encoded_len(Push32 { imm: 0 }.into())
            .unwrap_or_default();
        Self::new(at, len, move |layout, slice| {
            reencode(&encoder, len_push(region, layout), slice)
        })
    }
}

#[cfg(feature = "debug")]
impl core::fmt::Debug for Reloc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reloc")
            .field("at", &self.at)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

// The relocation path is folded into each closure `finalize` runs, so the
// binary carries one body per relocation and no helper to find them by.
/// The push a region base lowers to: a narrow push, four bytes shorter than a
/// wide one for an address that fits a `u32` by construction.
#[inline(always)]
fn base_push(region: Region, layout: &Layout) -> Instr {
    Push32 {
        imm: layout.span(region).base(),
    }
    .into()
}

/// The push a region length lowers to: a `u32` like the address, since a
/// region is a span of the same address space.
#[inline(always)]
fn len_push(region: Region, layout: &Layout) -> Instr {
    Push32 {
        imm: layout.span(region).len(),
    }
    .into()
}

/// Re-encodes `instr` with `encoder` and writes it over `slice`.
#[inline(always)]
fn reencode<E: Encoder>(encoder: &E, instr: Instr, slice: &mut [u8]) -> Result<(), NotFinal> {
    let mut tmp = Vec::new();
    encoder.encode(instr, &mut tmp).map_err(NotFinal::encode)?;
    copy_reencoded(&tmp, slice)
}

/// Writes freshly encoded bytes over the slot they belong in, refusing a length
/// that would not fit — a region base is always five bytes, so this only ever
/// fires if the slot was measured against a different encoder.
#[inline(always)]
fn copy_reencoded(bytes: &[u8], slice: &mut [u8]) -> Result<(), NotFinal> {
    if bytes.len() != slice.len() {
        return Err(NotFinal::EncoderDisagrees {
            measured: i64::try_from(slice.len()).unwrap_or(i64::MAX),
            written: bytes.len(),
        });
    }

    slice.copy_from_slice(bytes);
    Ok(())
}

/// Which block goes where.
///
/// Entry first, then the rest in the order they were created. Layout is a real
/// choice — it decides which edges become fallthroughs and cost nothing — but a
/// deterministic order matters more than a clever one while the graph is small,
/// and a reordering pass belongs with the other mutations.
fn block_order(cfg: &Cfg) -> Vec<BlockId> {
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
    /// The region-base pushes left as placeholders.
    relocs: Vec<BaseReloc>,
    /// The region-length pushes left as placeholders.
    len_relocs: Vec<LenReloc>,
    /// The field-offset holes left as placeholders.
    field_relocs: Vec<FieldReloc>,
    /// The discriminant holes left as placeholders.
    tag_relocs: Vec<TagReloc>,
    /// The field loads left as placeholders.
    load_relocs: Vec<LoadReloc>,
}

/// A branch whose offset is not known until every instruction has been measured.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy)]
struct Patch {
    /// Index of the branch in the instruction stream.
    at: usize,
    /// The block it aims at.
    target: BlockId,
    /// Which branch it is.
    kind: Branch,
}

/// The three branch shapes a terminator lowers to.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy)]
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
    let mut relocs = Vec::new();
    let mut len_relocs = Vec::new();
    let mut field_relocs = Vec::new();
    let mut tag_relocs = Vec::new();
    let mut load_relocs = Vec::new();
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

            // A region's base and size are what layout still owes: emit a
            // placeholder and record where it sits, rather than reading a number
            // that does not exist yet.
            match *item {
                Item::Base(region) => relocs.push(BaseReloc {
                    index: code.len(),
                    region,
                }),
                Item::Len(region) => len_relocs.push(LenReloc {
                    index: code.len(),
                    region,
                }),
                Item::Field(hole) => field_relocs.push(FieldReloc {
                    index: code.len(),
                    hole,
                }),
                Item::Tag(hole) => tag_relocs.push(TagReloc {
                    index: code.len(),
                    hole,
                }),
                Item::LoadField(hole) => load_relocs.push(LoadReloc {
                    index: code.len(),
                    hole,
                }),
                _ => {}
            }

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
            Terminator::Abort => code.push(crate::isa::Abort.into()),
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
        relocs,
        len_relocs,
        field_relocs,
        tag_relocs,
        load_relocs,
    })
}

/// The encoded length of every instruction in the standard packed format.
fn encoded_len_all(code: &[Instr]) -> Result<Vec<usize>, NotFinal> {
    code.iter()
        .map(|instr| encoded_len(*instr).map_err(NotFinal::encode))
        .collect()
}

/// The byte offset of every instruction, plus one past the last, from a table of
/// per-instruction sizes.
fn offsets(code: &[Instr], sizes: &[usize]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(code.len() + 1);
    let mut at = 0;
    for size in sizes {
        offsets.push(at);
        at += size;
    }
    offsets.push(at);
    offsets
}

/// Fills each branch with the offset from it to the block it aims at.
fn patch_branches(
    code: &mut [Instr],
    patches: &[Patch],
    starts: &[usize],
    offsets: &[usize],
) -> Result<(), NotFinal> {
    for patch in patches {
        let target = *starts
            .get(patch.target.index())
            .ok_or(Invalid::NoSuchBlock(patch.target))?;

        let to = offset(offsets, target)?;
        let next = offset(offsets, patch.at + 1)?;
        let delta = i32::try_from(to - next).map_err(|_| NotFinal::BranchTooFar {
            target: patch.target,
            delta: to - next,
        })?;

        let slot = code.get_mut(patch.at).ok_or(NotFinal::Overflow)?;
        *slot = patch.kind.instr(delta);
    }

    Ok(())
}

/// Turns one item into the instruction it stands for.
///
/// A region base becomes a placeholder push; its address is left to a relocation,
/// so this pass needs no layout.
fn materialize(frame: &Frame, item: Item, depth: u32, at: Where) -> Result<Instr, NotFinal> {
    let (cell, storing) = match item {
        Item::Instr(instr) => return Ok(instr),
        // The address, size or offset arrives at finalize; a zero holds its place.
        Item::Base(_) | Item::Len(_) | Item::Field(_) => return Ok(Push32 { imm: 0 }.into()),
        // A discriminant is a whole word, so it holds a wider place.
        Item::Tag(_) => return Ok(Push64 { imm: 0 }.into()),
        // The width arrives at link; the narrowest load holds its place.
        Item::LoadField(_) => return Ok(crate::isa::Ld8.into()),
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
#[cfg_attr(feature = "debug", derive(Debug, thiserror::Error))]
pub enum NotFinal {
    /// The graph does not hold up. Every displacement below rests on this, so
    /// finalization refuses to guess.
    #[cfg_attr(feature = "debug", error("the graph is not valid: {0}"))]
    Invalid(Invalid),

    /// Something the instruction set does not lower yet.
    #[cfg_attr(
        feature = "debug",
        error("block {block:?} ends in `{what}`, which has no encoding yet")
    )]
    Unsupported {
        /// The block it is in.
        block: BlockId,
        /// What was found there.
        what: &'static str,
    },

    /// A branch further than a relative offset can reach.
    #[cfg_attr(
        feature = "debug",
        error("a branch to {target:?} is {delta} bytes away, too far to encode")
    )]
    BranchTooFar {
        /// The block it aims at.
        target: BlockId,
        /// How far away it turned out to be.
        delta: i64,
    },

    /// The program is larger than an offset can describe.
    #[cfg_attr(feature = "debug", error("the program is too large to lay out"))]
    Overflow,

    /// The encoder measured one length and wrote another, which would leave
    /// every branch aimed at the wrong byte.
    #[cfg_attr(
        feature = "debug",
        error("the encoder measured {measured} bytes and wrote {written}")
    )]
    EncoderDisagrees {
        /// What `encoded_len` promised.
        measured: i64,
        /// What `encode` produced.
        written: usize,
    },

    /// A field-offset hole reached this path, which has no layout to resolve it.
    #[cfg_attr(
        feature = "debug",
        error("a field offset can only be resolved with the aggregate's layout")
    )]
    FieldWithoutLayout,

    /// An instruction would not encode.
    #[cfg_attr(feature = "debug", error("could not encode an instruction: {0}"))]
    Encode(Box<dyn core::error::Error + Send + Sync>),
}

impl NotFinal {
    /// Wraps whatever error the encoder in use reports.
    fn encode(error: impl core::error::Error + Send + Sync + 'static) -> Self {
        Self::Encode(Box::new(error))
    }
}
