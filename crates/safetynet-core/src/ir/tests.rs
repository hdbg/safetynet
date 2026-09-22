//! Tests for the intermediate representation.

use super::*;
use crate::isa::{Add, CmpEq, Halt, Push8};
use crate::{Op, Width};

/// A graph with one block of every terminator shape in it, laid out so that
/// each conditional meets a different fallthrough.
fn every_shape() -> Cfg {
    let mut frame = Frame::new();
    let first = frame.add(Width::U32).expect("room");
    let second = frame.add(Width::U8).expect("room");

    let mut builder = Cfg::builder(frame);
    let blocks: Vec<_> = (0..6).map(|_| builder.block(0)).collect();
    let at = |index: usize| *blocks.get(index).expect("six blocks");

    builder
        .at(at(0))
        .expect("open")
        .base(Region::Input)
        .load(first)
        .store(second);
    builder.seal(at(0), Terminator::Jmp(at(1))).expect("seals");

    builder.at(at(1)).expect("open").instr(Push8 { imm: 7 });
    builder
        .seal(
            at(1),
            Terminator::Br {
                then: at(4),
                els: at(2),
            },
        )
        .expect("seals");

    builder.at(at(2)).expect("open").instr(Add);
    builder
        .seal(
            at(2),
            Terminator::Br {
                then: at(3),
                els: at(5),
            },
        )
        .expect("seals");

    builder
        .seal(
            at(3),
            Terminator::Br {
                then: at(0),
                els: at(1),
            },
        )
        .expect("seals");

    builder
        .seal(
            at(4),
            Terminator::Switch {
                arms: vec![at(0), at(5)],
                default: at(2),
            },
        )
        .expect("seals");

    builder.seal(at(5), Terminator::Halt).expect("seals");

    builder.build(at(1)).expect("builds")
}

#[test]
fn a_graph_debugs_as_an_assembly_listing() {
    assert_eq!(
        format!("{:?}", every_shape()),
        concat!(
            ".frame { c0: u32, c1: u8 }\n",
            ".entry b1\n",
            "b0:\n",
            "    $push .input\n",
            "    $load c0\n",
            "    $store c1\n",
            // Spelled out even though b1 is next: an edge is always visible,
            // never left for the reader to infer from block order.
            "    jmp b1\n",
            "b1:\n",
            "    push8 7\n",
            // The zero arm is next, so only the non-zero one is named.
            "    jnz b4\n",
            "b2:\n",
            "    add\n",
            "    jz b5\n",
            // Neither arm follows, so both are named.
            "b3:\n",
            "    br b0, b1\n",
            "b4:\n",
            "    switch [b0, b5] default b2\n",
            "b5:\n",
            "    halt\n",
        )
    );
}

/// A frame with no cells declares nothing, entry zero says nothing, and a table
/// with no arms still prints its brackets.
#[test]
fn the_empty_shapes_still_print_something_readable() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let default = builder.block(0);

    builder
        .seal(
            entry,
            Terminator::Switch {
                arms: Vec::new(),
                default,
            },
        )
        .expect("seals");
    builder.seal(default, Terminator::Halt).expect("seals");

    assert_eq!(
        format!("{:?}", builder.build(entry).expect("builds")),
        "b0:\n    switch [] default b1\nb1:\n    halt\n"
    );
}

/// Cells are naturally aligned, so a narrow cell before a wide one leaves a gap
/// rather than putting a word across a boundary. Frame layout and aggregate
/// layout follow the same packing rule, which is what lets an aggregate live
/// directly in the frame.
#[test]
fn cells_are_naturally_aligned() {
    let mut frame = Frame::new();
    let flag = frame.add(Width::U8).expect("room");
    let count = frame.add(Width::U32).expect("room");
    let total = frame.add(Width::U64).expect("room");

    let off = |id| frame.cell(id).map(Cell::off);
    assert_eq!(off(flag), Some(0));
    assert_eq!(off(count), Some(4), "skips three bytes to align");
    assert_eq!(off(total), Some(8));
}

/// The prologue reserves whole words, so `SP` stays aligned no matter what the
/// locals add up to.
#[test]
fn the_frame_is_rounded_to_a_word() {
    let mut frame = Frame::new();
    assert_eq!(frame.size().bytes(), 0, "an empty frame reserves nothing");

    frame.add(Width::U8).expect("room");
    assert_eq!(frame.size().bytes(), 8, "one byte still costs a word");

    frame.add(Width::U64).expect("room");
    assert_eq!(frame.size().bytes(), 16);
}

/// A frame too big to address is refused while it is still a layout decision,
/// rather than at materialization where it would surface as an unencodable
/// displacement.
#[test]
fn a_frame_stops_where_displacements_do() {
    let mut frame = Frame::new();
    let mut cells = 0;
    while frame.add(Width::U64).is_some() {
        cells += 1;
    }

    assert_eq!(cells, 8191);
    assert_eq!(frame.size().bytes(), 65528, "the last word-aligned size");
    assert_eq!(frame.cells().len(), cells);
}

/// Rebuilding a frame from its widths has to land on the same offsets adding
/// the cells one at a time does, or a graph that survived a round trip would
/// address a different frame than the one it was written against.
#[test]
fn a_frame_of_widths_is_the_frame_those_cells_built() {
    let widths = [Width::U8, Width::U64, Width::U32, Width::U8];

    let mut expected = Frame::new();
    for width in widths {
        expected.add(width).expect("room");
    }

    assert_eq!(Frame::from_widths(widths), Some(expected));
}

/// And it refuses where `add` refuses, rather than truncating the frame.
#[test]
fn a_frame_of_widths_stops_where_displacements_do() {
    assert!(Frame::from_widths(core::iter::repeat_n(Width::U64, 8191)).is_some());
    assert_eq!(
        Frame::from_widths(core::iter::repeat_n(Width::U64, 8192)),
        None
    );
}

/// A symbolic access knows its stack effect without knowing its displacement.
/// That is the whole reason the depth can be validated before anything is laid
/// out — and the displacement is then computed from that depth.
#[test]
fn symbolic_items_declare_their_stack_effect() {
    let mut frame = Frame::new();
    let cell = frame.add(Width::U32).expect("room");

    assert_eq!(Item::Load(cell).sp_delta(), 8, "a load pushes a word");
    assert_eq!(Item::Store(cell).sp_delta(), -8, "a store pops one");
    assert_eq!(Item::Len(Region::Input).sp_delta(), 8, "a length is a word");
    assert_eq!(Item::from(Instr::from(Add)).sp_delta(), Add.sp_delta());
}

/// A region's length lists like its base, by the region's name.
#[test]
fn a_region_length_lists_by_name() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    builder.at(entry).expect("open").len(Region::Input);
    builder.seal(entry, Terminator::Halt).expect("seals");
    let cfg = builder.build(entry).expect("builds");
    assert_eq!(format!("{cfg:?}"), "b0:\n    $len .input\n    halt\n");
}

#[test]
fn the_conditional_terminators_consume_their_word() {
    let block = BlockId(0);

    assert_eq!(Terminator::Jmp(block).sp_delta(), 0);
    assert_eq!(Terminator::Halt.sp_delta(), 0);
    assert_eq!(
        Terminator::Br {
            then: block,
            els: block
        }
        .sp_delta(),
        -8
    );
    assert_eq!(
        Terminator::Switch {
            arms: vec![block],
            default: block
        }
        .sp_delta(),
        -8
    );
}

#[test]
fn a_terminator_names_every_block_it_can_reach() {
    let targets = |term: &Terminator| term.targets().collect::<Vec<_>>();
    let (a, b, c) = (BlockId(0), BlockId(1), BlockId(2));

    assert_eq!(targets(&Terminator::Halt), []);
    assert_eq!(targets(&Terminator::Jmp(a)), [a]);
    assert_eq!(targets(&Terminator::Br { then: a, els: b }), [a, b]);
    assert_eq!(
        targets(&Terminator::Switch {
            arms: vec![a, b],
            default: c
        }),
        [c, a, b]
    );
}

/// A loop needs to name a block that does not exist yet, which is the reason
/// ids are handed out before bodies are written.
#[test]
fn a_block_can_branch_to_one_that_does_not_exist_yet() {
    let mut frame = Frame::new();
    let counter = frame.add(Width::U64).expect("room");

    let mut builder = Cfg::builder(frame);
    let head = builder.block(0);
    let body = builder.block(0);
    let exit = builder.block(0);

    builder.at(head).expect("open").load(counter);
    builder
        .seal(
            head,
            Terminator::Br {
                then: body,
                els: exit,
            },
        )
        .expect("seals");

    builder
        .at(body)
        .expect("open")
        .load(counter)
        .instr(Push8 { imm: 1 })
        .instr(Add)
        .store(counter);
    builder.seal(body, Terminator::Jmp(head)).expect("seals");

    builder.at(exit).expect("open").load(counter);
    builder.seal(exit, Terminator::Halt).expect("seals");

    let cfg = builder.build(head).expect("builds");

    assert_eq!(cfg.entry(), head);
    assert_eq!(cfg.blocks().len(), 3);
    assert_eq!(cfg.frame().size().bytes(), 8);
    assert_eq!(
        cfg.block(body).map(|block| block.code().len()),
        Some(4),
        "the body is four items"
    );
    assert_eq!(
        cfg.block(body).map(Block::term),
        Some(&Terminator::Jmp(head)),
        "the back edge closes the loop"
    );
}

/// The depth is what the front-end claims, not something the builder works out,
/// so that the validator's own derivation has something to disagree with.
#[test]
fn the_recorded_depth_is_the_callers_claim() {
    let mut builder = Cfg::builder(Frame::new());
    let block = builder.block(24);

    builder.at(block).expect("open").instr(Push8 { imm: 1 });
    builder.seal(block, Terminator::Halt).expect("seals");

    let cfg = builder.build(block).expect("builds");
    assert_eq!(
        cfg.block(block).map(Block::sp_in),
        Some(24),
        "untouched by what the body does"
    );
}

#[test]
fn a_sealed_block_is_finished() {
    let mut builder = Cfg::builder(Frame::new());
    let block = builder.block(0);
    builder.seal(block, Terminator::Halt).expect("seals");

    assert_eq!(
        builder.at(block).err(),
        Some(BuildError::AlreadySealed(block))
    );
    assert_eq!(
        builder.seal(block, Terminator::Halt),
        Err(BuildError::AlreadySealed(block))
    );
}

#[test]
fn an_incomplete_graph_is_refused() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let dangling = builder.block(0);
    builder
        .seal(entry, Terminator::Jmp(dangling))
        .expect("seals");

    assert_eq!(builder.build(entry), Err(BuildError::NotSealed(dangling)));
}

#[test]
fn a_block_from_nowhere_is_refused() {
    let stranger = BlockId(7);
    let mut builder = Cfg::builder(Frame::new());
    let block = builder.block(0);
    builder.seal(block, Terminator::Halt).expect("seals");

    assert_eq!(
        builder.at(stranger).err(),
        Some(BuildError::NoSuchBlock(stranger))
    );
    assert_eq!(
        builder.build(stranger),
        Err(BuildError::NoSuchBlock(stranger))
    );
}

/// Halting from the entry is the smallest graph there is.
#[test]
fn the_smallest_graph() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    builder.at(entry).expect("open").instr(Halt);
    builder.seal(entry, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert_eq!(cfg.blocks().len(), 1);
    assert_eq!(
        cfg.block(entry).map(Block::code),
        Some(&[Item::from(Instr::from(Halt))][..])
    );
    assert_eq!(CmpEq.sp_delta(), -8, "the op table is still the source");
}
