//! Tests for the SP invariant.

use super::*;
use crate::Width;
use crate::ir::{Block, BlockBody, Frame, Terminator};
use crate::isa::{Add, Alloc, Drop, FrameSize, Push8, Sub};

/// A countdown loop, optionally with a balanced junk pair spliced into its body.
///
/// The junk is the point of the exercise: a pass may add and remove stack
/// traffic freely, and everything it did not touch has to stay correct.
fn countdown(junk: bool) -> Cfg {
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

    let block = builder.at(body).expect("open");
    block.load(counter).instr(Push8 { imm: 1 });
    if junk {
        block.instr(Push8 { imm: 0 }).instr(Drop);
    }
    block.instr(Sub).store(counter);
    builder.seal(body, Terminator::Jmp(head)).expect("seals");

    builder.at(exit).expect("open").load(counter);
    builder.seal(exit, Terminator::Halt).expect("seals");

    builder.build(head).expect("builds")
}

/// A graph of one block, for perturbing.
fn one_block(sp_in: u32, term: Terminator, fill: impl FnOnce(&mut BlockBody)) -> Cfg {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(sp_in);
    fill(builder.at(entry).expect("open"));
    builder.seal(entry, term).expect("seals");
    builder.build(entry).expect("builds")
}

#[test]
fn a_well_formed_loop_validates() {
    assert_eq!(validate(&countdown(false)), Ok(()));
}

/// Adding and removing a word between two accesses changes nothing about
/// whether the graph is sound — which is the whole reason a pass may do it.
#[test]
fn balanced_junk_changes_nothing() {
    assert_eq!(validate(&countdown(true)), Ok(()));

    let depths = |cfg: &Cfg| cfg.blocks().iter().map(Block::sp_in).collect::<Vec<_>>();
    assert_eq!(depths(&countdown(true)), depths(&countdown(false)));
}

#[test]
fn the_entry_starts_empty() {
    let cfg = one_block(8, Terminator::Halt, |_| {});

    assert_eq!(validate(&cfg), Err(Invalid::EntryDepth { recorded: 8 }));
}

/// The check that gives the front-end's claim something to be wrong against.
#[test]
fn a_successor_must_agree_with_what_reaches_it() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let next = builder.block(0);

    // Leaves a word behind, but the successor claims an empty stack.
    builder.at(entry).expect("open").instr(Push8 { imm: 1 });
    builder.seal(entry, Terminator::Jmp(next)).expect("seals");
    builder.at(next).expect("open").instr(Drop);
    builder.seal(next, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert_eq!(
        validate(&cfg),
        Err(Invalid::DepthMismatch {
            block: entry,
            target: next,
            recorded: 0,
            actual: 8,
        })
    );
}

/// Two paths into one block that do not agree about the stack. Only one of them
/// can be what the block recorded, so the other is named.
#[test]
fn predecessors_that_disagree_are_caught() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let other = builder.block(8);
    let join = builder.block(8);

    // One word for the branch to consume, one to hand on to the join.
    builder
        .at(entry)
        .expect("open")
        .instr(Push8 { imm: 1 })
        .instr(Push8 { imm: 1 });
    builder
        .seal(
            entry,
            Terminator::Br {
                then: join,
                els: other,
            },
        )
        .expect("seals");

    // The other path drops it, and so reaches the join with nothing pushed.
    builder.at(other).expect("open").instr(Drop);
    builder.seal(other, Terminator::Jmp(join)).expect("seals");
    builder.at(join).expect("open").instr(Drop);
    builder.seal(join, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert_eq!(
        validate(&cfg),
        Err(Invalid::DepthMismatch {
            block: other,
            target: join,
            recorded: 8,
            actual: 0,
        })
    );
}

#[test]
fn popping_past_the_frame_is_refused() {
    let cfg = one_block(0, Terminator::Halt, |block| {
        block.instr(Add);
    });

    assert_eq!(
        validate(&cfg),
        Err(Invalid::NegativeDepth {
            at: Where {
                block: cfg.entry(),
                item: 0
            },
            depth: 0,
            delta: -8,
        })
    );
}

/// A terminator's own effect counts: the conditional forms consume a word, and
/// the item index names the terminator as one past the last item.
#[test]
fn a_conditional_terminator_needs_its_word() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let next = builder.block(0);

    builder
        .seal(
            entry,
            Terminator::Br {
                then: next,
                els: next,
            },
        )
        .expect("seals");
    builder.seal(next, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert_eq!(
        validate(&cfg),
        Err(Invalid::NegativeDepth {
            at: Where {
                block: entry,
                item: 0
            },
            depth: 0,
            delta: -8,
        })
    );
}

/// The frame is the graph's, reserved once by the prologue. A block that
/// reserved its own would make every recorded depth ambiguous about which side
/// of the frame it counts.
#[test]
fn a_block_may_not_reserve_a_frame() {
    let size = FrameSize::new(16).expect("word-aligned");
    let cfg = one_block(0, Terminator::Halt, |block| {
        block.instr(Alloc { n: size });
    });

    assert_eq!(
        validate(&cfg),
        Err(Invalid::FrameOp {
            at: Where {
                block: cfg.entry(),
                item: 0
            }
        })
    );
}

#[test]
fn a_cell_from_another_frame_is_refused() {
    let mut elsewhere = Frame::new();
    let stranger = elsewhere.add(Width::U64).expect("room");

    let cfg = one_block(0, Terminator::Halt, |block| {
        block.load(stranger);
    });

    assert_eq!(
        validate(&cfg),
        Err(Invalid::NoSuchCell {
            at: Where {
                block: cfg.entry(),
                item: 0
            },
            cell: stranger,
        })
    );
}

/// A frame at the ceiling plus a word of operand stack puts the first cell one
/// byte beyond what a displacement can name. Caught here, where the access has
/// a name, rather than at materialization where it would be an anonymous number.
#[test]
fn a_displacement_that_cannot_be_encoded_is_refused() {
    let mut frame = Frame::new();
    let first = frame.add(Width::U64).expect("room");
    while frame.add(Width::U64).is_some() {}
    assert_eq!(frame.size().bytes(), 65528);

    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    builder
        .at(entry)
        .expect("open")
        .instr(Push8 { imm: 1 })
        .load(first);
    builder.seal(entry, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert_eq!(
        validate(&cfg),
        Err(Invalid::DisplacementTooLarge {
            at: Where {
                block: entry,
                item: 1
            },
            cell: first,
            displacement: 65536,
        })
    );
}

#[test]
fn an_edge_out_of_the_graph_is_refused() {
    let stranger = BlockId::from_index(9);
    let cfg = one_block(0, Terminator::Jmp(stranger), |_| {});

    assert_eq!(validate(&cfg), Err(Invalid::NoSuchBlock(stranger)));
}

/// Not unsound by itself, but its recorded depth is a claim nothing checks.
#[test]
fn an_unreachable_block_is_refused() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let orphan = builder.block(0);

    builder.seal(entry, Terminator::Halt).expect("seals");
    builder.seal(orphan, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert_eq!(validate(&cfg), Err(Invalid::Unreachable(orphan)));
}

#[test]
fn the_limits_are_enforced() {
    let cfg = countdown(false);

    let blocks = Limits {
        blocks: 2,
        ..Limits::default()
    };
    assert_eq!(
        validate_with(&cfg, &blocks),
        Err(Invalid::TooManyBlocks {
            blocks: 3,
            limit: 2
        })
    );

    let items = Limits {
        items: 2,
        ..Limits::default()
    };
    assert_eq!(
        validate_with(&cfg, &items),
        Err(Invalid::TooManyItems { items: 6, limit: 2 })
    );

    let depth = Limits {
        depth: 0,
        ..Limits::default()
    };
    assert!(matches!(
        validate_with(&cfg, &depth),
        Err(Invalid::TooDeep { .. })
    ));
}
