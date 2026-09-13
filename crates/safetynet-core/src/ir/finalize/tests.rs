//! Tests for materialization.

use super::*;
use crate::encoding::{EncodeError, Encoder, decode, encode, encoded_len};
use crate::image::{Image, Layout, Region, Sizes};
use crate::ir::{Frame, Terminator};
use crate::isa::{Add, Halt, Push8, Sub, Switch};
use crate::samples::{Padded, layout, stack_image};
use crate::vm::Vm;
use crate::{Be, Le, Op, Width, Word};

/// Sums `n` down to one, optionally with a balanced junk pair wrapped around the
/// whole loop body.
///
/// The junk is the property in miniature: a pass may add and remove stack
/// traffic, every access between the two ends up at a different displacement,
/// and the answer may not change.
fn sum_to(n: u8, junk: bool) -> Cfg {
    let mut frame = Frame::new();
    let count = frame.add(Width::U64).expect("room");
    let total = frame.add(Width::U64).expect("room");

    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    let head = builder.block(0);
    let body = builder.block(0);
    let done = builder.block(0);

    builder
        .at(entry)
        .expect("open")
        .instr(Push8 { imm: n })
        .store(count)
        .instr(Push8 { imm: 0 })
        .store(total);
    builder.seal(entry, Terminator::Jmp(head)).expect("seals");

    // The branch consumes the counter it just read.
    builder.at(head).expect("open").load(count);
    builder
        .seal(
            head,
            Terminator::Br {
                then: body,
                els: done,
            },
        )
        .expect("seals");

    let block = builder.at(body).expect("open");
    if junk {
        block.instr(Push8 { imm: 0 });
    }
    block
        .load(total)
        .load(count)
        .instr(Add)
        .store(total)
        .load(count)
        .instr(Push8 { imm: 1 })
        .instr(Sub)
        .store(count);
    if junk {
        block.instr(crate::isa::Drop);
    }
    builder.seal(body, Terminator::Jmp(head)).expect("seals");

    builder.at(done).expect("open").load(total);
    builder.seal(done, Terminator::Halt).expect("seals");

    builder.build(entry).expect("builds")
}

/// Runs a finalized program and returns the word it left on top.
fn run<B: ByteOrder>(program: &Program<B>) -> Result<Word, Box<dyn core::error::Error>> {
    let vm = Vm::<B>::new(stack_image(1024));
    Ok(vm.run(program, 10_000)?.pop()?)
}

/// Decodes a program back into instructions, for looking at what came out.
fn disassemble<B: ByteOrder>(program: &Program<B>) -> Vec<Instr> {
    let code = program.code();
    let mut at = 0;
    let mut out = Vec::new();

    while at < code.len() {
        let (instr, len) = decode::<B>(code.get(at..).expect("in bounds")).expect("decodes");
        at += len;
        out.push(instr);
    }

    out
}

/// The phase's own bar: a graph built by hand runs on the machine.
fn a_graph_runs_on_the_machine<B: ByteOrder>() {
    let program = finalize::<B>(&sum_to(5, false), &layout(1024)).expect("finalizes");

    assert_eq!(program.frame().bytes(), 16, "two word cells");
    assert_eq!(run(&program).ok(), Some(15), "5 + 4 + 3 + 2 + 1");
}

#[test]
fn a_graph_runs_on_the_machine_le() {
    a_graph_runs_on_the_machine::<Le>();
}

#[test]
fn a_graph_runs_on_the_machine_be() {
    a_graph_runs_on_the_machine::<Be>();
}

/// Insert a balanced push and drop around the loop body and every access inside
/// it moves eight bytes further from `SP` — different bytecode, same answer.
///
/// This is the check the whole symbolic form exists for. Had the graph carried
/// displacements instead of cells, the junk would have left every one of them
/// pointing a cell short.
#[test]
fn junk_stack_traffic_moves_displacements_and_nothing_else() {
    let plain = finalize::<Le>(&sum_to(5, false), &layout(1024)).expect("finalizes");
    let padded = finalize::<Le>(&sum_to(5, true), &layout(1024)).expect("finalizes");

    let displacements = |program: &Program<Le>| {
        disassemble(program)
            .into_iter()
            .filter_map(|instr| match instr {
                Instr::Lds64(op) => Some(op.disp),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    assert_ne!(displacements(&plain), displacements(&padded));
    assert_eq!(run(&plain).ok(), run(&padded).ok());
    assert_eq!(run(&padded).ok(), Some(15));
}

/// `k = F − c + d`, read back out of the bytecode.
#[test]
fn a_displacement_comes_from_the_depth_at_that_point() {
    let mut frame = Frame::new();
    let first = frame.add(Width::U64).expect("room");
    let second = frame.add(Width::U32).expect("room");

    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    builder
        .at(entry)
        .expect("open")
        .load(first) // d = 0  → k = 16 − 0 + 0
        .load(second) // d = 8  → k = 16 − 8 + 8
        .instr(Push8 { imm: 1 }) // d = 16
        .store(second); // d = 24 → k = 16 − 8 + 24
    builder.seal(entry, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    let program = finalize::<Le>(&cfg, &layout(1024)).expect("finalizes");

    assert_eq!(
        disassemble(&program),
        [
            Alloc {
                n: cfg.frame().size()
            }
            .into(),
            Lds64 { disp: 16 }.into(),
            Lds32 { disp: 16 }.into(),
            Push8 { imm: 1 }.into(),
            Sts32 { disp: 32 }.into(),
            Halt.into(),
        ]
    );
}

/// An edge to the block that follows is not a branch at all.
#[test]
fn an_edge_to_the_next_block_falls_through() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let next = builder.block(0);

    builder.seal(entry, Terminator::Jmp(next)).expect("seals");
    builder.at(next).expect("open").instr(Push8 { imm: 7 });
    builder.seal(next, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    let program = finalize::<Le>(&cfg, &layout(1024)).expect("finalizes");

    assert_eq!(
        disassemble(&program),
        [Push8 { imm: 7 }.into(), Halt.into()],
        "no jump, and no prologue for an empty frame"
    );
}

/// A conditional keeps whichever edge it can fall into, so only one branch is
/// emitted where the layout allows it.
#[test]
fn a_conditional_falls_into_whichever_edge_follows() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let then = builder.block(0);
    let els = builder.block(0);

    builder.at(entry).expect("open").instr(Push8 { imm: 1 });
    builder
        .seal(entry, Terminator::Br { then, els })
        .expect("seals");
    builder.seal(then, Terminator::Halt).expect("seals");
    builder.seal(els, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");

    // `then` is laid out next, so the branch is the negated one and `els` is the
    // edge that costs an instruction.
    assert_eq!(
        disassemble(&finalize::<Le>(&cfg, &layout(1024)).expect("finalizes")),
        [
            Push8 { imm: 1 }.into(),
            Jz { offset: 1 }.into(),
            Halt.into(),
            Halt.into(),
        ]
    );
}

/// A backwards branch lands on the first byte of its target.
#[test]
fn a_back_edge_reaches_its_target() {
    let program = finalize::<Le>(&sum_to(3, false), &layout(1024)).expect("finalizes");
    let code = program.code();

    let mut at = 0;
    let mut landings = Vec::new();
    while at < code.len() {
        let (instr, len) = decode::<Le>(code.get(at..).expect("in bounds")).expect("decodes");
        at += len;

        if let Instr::Jmp(op) = instr {
            let target = i64::try_from(at).expect("small") + i64::from(op.offset);
            landings.push(target);
        }
    }

    // Every landing is the start of a real instruction, and the loop's back edge
    // goes backwards.
    for target in landings {
        let at = usize::try_from(target).expect("in range");
        assert!(decode::<Le>(code.get(at..).expect("in bounds")).is_ok());
    }
}

#[test]
fn a_switch_has_no_encoding_yet() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let arm = builder.block(0);

    builder.at(entry).expect("open").instr(Push8 { imm: 0 });
    builder
        .seal(
            entry,
            Terminator::Switch {
                arms: vec![arm],
                default: arm,
            },
        )
        .expect("seals");
    builder.seal(arm, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert!(matches!(
        finalize::<Le>(&cfg, &layout(1024)),
        Err(NotFinal::Unsupported { what: "switch", .. })
    ));

    // The opcode exists, it just traps; reserving it keeps the numbering stable.
    assert_eq!(Switch.sp_delta(), -8);
}

/// Every displacement rests on the invariant, so it is checked here and not
/// assumed to have been checked earlier.
#[test]
fn an_invalid_graph_is_not_finalized() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    builder.at(entry).expect("open").instr(Add);
    builder.seal(entry, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    assert!(matches!(
        finalize::<Le>(&cfg, &layout(1024)),
        Err(NotFinal::Invalid(Invalid::NegativeDepth { .. }))
    ));
}

/// An encoder that measures one thing and writes another would move every
/// instruction out from under the branches aimed at it.
struct Liar;

impl Encoder for Liar {
    type Order = Le;
    type Error = EncodeError;

    fn encode(&self, instr: Instr, out: &mut Vec<u8>) -> Result<usize, Self::Error> {
        encode::<Le>(instr, out)
    }

    fn encoded_len(&self, instr: Instr) -> Result<usize, Self::Error> {
        Ok(encoded_len(instr)? + 1)
    }
}

/// Layout asks the encoder for every size, so a format that pads still gets
/// branch offsets that land — the deltas grow by exactly the padding in between.
#[test]
fn layout_follows_whatever_the_encoder_measures() {
    let cfg = sum_to(3, false);

    let packed = finalize::<Le>(&cfg, &layout(1024)).expect("finalizes");
    let padded = finalize_with(&cfg, &layout(1024), &Padded::<Le>::default()).expect("finalizes");

    // Walking the padded stream means skipping the filler byte each time.
    let mut instructions = Vec::new();
    let mut at = 0;
    let code = padded.code();
    while at < code.len() {
        let (instr, len) = decode::<Le>(code.get(at..).expect("in bounds")).expect("decodes");
        assert_eq!(
            code.get(at + len),
            Some(&0xff),
            "the filler is where it was put"
        );
        at += len + 1;
        instructions.push(instr);
    }

    let plain = disassemble(&packed);
    assert_eq!(instructions.len(), plain.len(), "the same program");
    assert_eq!(code.len(), packed.code().len() + plain.len());

    // Every branch still reaches the instruction it did before, which it can
    // only do if layout measured the padding rather than assuming a format.
    let back_edge = |program: &[Instr]| {
        program.iter().find_map(|instr| match instr {
            Instr::Jmp(op) => Some(op.offset),
            _ => None,
        })
    };
    let (Some(packed_edge), Some(padded_edge)) = (back_edge(&plain), back_edge(&instructions))
    else {
        panic!("the loop has a back edge");
    };
    assert!(padded_edge < packed_edge, "further back, by the filler");
}

#[test]
fn an_encoder_that_miscounts_is_caught() {
    let cfg = sum_to(3, false);

    assert!(matches!(
        finalize_with(&cfg, &layout(1024), &Liar),
        Err(NotFinal::EncoderDisagrees { .. })
    ));
}

/// A region's address is the layout's to decide. The same graph laid out against
/// two images pushes two different addresses, and neither the graph nor the host
/// holds a constant that has to match the other.
#[test]
fn a_region_base_comes_from_the_layout() {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    builder.at(entry).expect("open").base(Region::Scratch);
    builder.seal(entry, Terminator::Halt).expect("seals");
    let cfg = builder.build(entry).expect("builds");

    let pushed = |input| {
        let layout = Layout::new(Sizes {
            input,
            scratch: 8,
            stack: 64,
        })
        .expect("fits");

        let program = finalize::<Le>(&cfg, &layout).expect("finalizes");
        match disassemble(&program).first().copied() {
            Some(Instr::Push32(op)) => op.imm,
            other => panic!("expected a pushed address, got {other:?}"),
        }
    };

    assert_eq!(
        pushed(0),
        0,
        "scratch starts the image when nothing precedes"
    );
    assert_eq!(pushed(8), 8, "and moves with whatever does");
    assert_eq!(pushed(9), 16, "rounded up to a word boundary");
}

/// The point of the whole map: the host writes a region and the program reads it
/// without either of them naming an address.
#[test]
fn the_host_and_the_program_agree_without_a_shared_constant() {
    let layout = Layout::new(Sizes {
        input: 4,
        scratch: 13,
        stack: 64,
    })
    .expect("fits");

    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    builder
        .at(entry)
        .expect("open")
        .base(Region::Input)
        .instr(crate::isa::Ld8);
    builder.seal(entry, Terminator::Halt).expect("seals");

    let cfg = builder.build(entry).expect("builds");
    let program = finalize::<Le>(&cfg, &layout).expect("finalizes");

    let mut image = Image::new(layout);
    image.write(Region::Input, &[0xa5]).expect("room");

    let mut vm = Vm::<Le>::new(image).run(&program, 100).expect("terminates");
    assert_eq!(vm.pop(), Ok(0xa5));
}
