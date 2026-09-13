//! Tests for the disassembler, and for the resolve/assemble split it sits beside.

use super::{print, print_listing};
use crate::encoding::decode;
use crate::image::Sizes;
use crate::ir::{BaseReloc, Cfg, Frame, Terminator, assemble, finalize, resolve};
use crate::isa::{Add, Drop, Push8, Push32};
use crate::samples::layout;
use crate::{Be, ByteOrder, Instr, Layout, Le, Region, Width};

/// A graph with one block of every terminator shape in it, laid out so that
/// each conditional meets a different fallthrough.
///
/// Never finalized — a `switch` has no encoding — which is the point: printing
/// has to be total over the IR, not over the subset that lowers today.
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

/// A graph small enough to finalize, with a branch and a cell in it.
fn adds_two() -> Cfg {
    let mut frame = Frame::new();
    let total = frame.add(Width::U64).expect("room");

    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    let done = builder.block(0);

    builder
        .at(entry)
        .expect("open")
        .instr(Push32 { imm: 0xdead_beef })
        .instr(Drop)
        .instr(Push8 { imm: 40 })
        .instr(Push8 { imm: 2 })
        .instr(Add)
        .store(total);
    builder.seal(entry, Terminator::Jmp(done)).expect("seals");

    builder.at(done).expect("open").load(total);
    builder.seal(done, Terminator::Halt).expect("seals");

    builder.build(entry).expect("builds")
}

/// A graph whose only instruction pushes a region's base, so the one thing left
/// to resolve is the address the layout owns.
fn pushes(region: Region) -> Cfg {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    builder.at(entry).expect("open").base(region);
    builder.seal(entry, Terminator::Halt).expect("seals");
    builder.build(entry).expect("builds")
}

/// Decodes the first instruction of a finalized program.
fn first<B: ByteOrder>(code: &[u8]) -> Instr {
    decode::<B>(code).expect("decodes").0
}

/// The whole printed form, spelled out. It is the assembler's input as much as
/// it is a report, so the characters are the contract and a diff here is a
/// change to the language.
#[test]
fn a_graph_prints_as_the_text_that_spells_it() {
    assert_eq!(
        print(&every_shape()),
        concat!(
            ".frame { c0: u32, c1: u8 }\n",
            ".entry b1\n",
            "b0:\n",
            "    push .input\n",
            "    load c0\n",
            "    store c1\n",
            // Spelled out even though b1 is next: the parser turns a
            // fallthrough into the same `Jmp`, so printing one keeps the text a
            // fixed point without needing the reader to infer the edge.
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
/// with no arms still prints its brackets — a form the parser can read back.
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
        print(&builder.build(entry).expect("builds")),
        "b0:\n    switch [] default b1\nb1:\n    halt\n"
    );
}

/// The linear half: no labels, no indent, one instruction per line.
#[test]
fn a_listing_is_one_instruction_per_line() {
    let code = Instr::one_of_each();
    let listing = print_listing(&code);

    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), code.len());
    for (line, instr) in lines.iter().zip(&code) {
        assert_eq!(*line, instr.to_string());
    }

    assert_eq!(print_listing(&[]), "");
}

/// Resolution takes no layout, so what it produces is the same whatever image
/// the artifact later runs against: the code is one constant list, and a region
/// base is left as a single relocation naming where it sits.
#[test]
fn resolving_is_independent_of_any_layout() {
    let resolved = resolve(&pushes(Region::Input)).expect("resolves");
    assert_eq!(
        resolved.relocs(),
        [BaseReloc {
            index: 0,
            region: Region::Input,
        }],
        "one push, one relocation, at the front of an empty frame",
    );

    let none = resolve(&adds_two()).expect("resolves");
    assert!(
        none.relocs().is_empty(),
        "a graph that names no region owes no address",
    );

    // Nothing about the resolution depends on where it is called from.
    assert_eq!(resolve(&adds_two()).expect("resolves").code(), none.code());
}

/// The relocation is what turns a placeholder push into the region's real base.
#[test]
fn a_relocation_fills_in_the_region_base() {
    let layout = layout(1024);
    let program = assemble::<Le>(&pushes(Region::Input))
        .expect("assembles")
        .finalize(&layout)
        .expect("finalizes");

    match first::<Le>(program.code()) {
        Instr::Push32(op) => assert_eq!(op.imm, layout.span(Region::Input).base()),
        other => panic!("expected a base push, got {other:?}"),
    }
}

/// The public entry points agree: assembling then finalizing is exactly what the
/// `finalize` shorthand does, in either byte order.
fn assemble_then_finalize_matches_finalize<B: ByteOrder>() {
    let cfg = adds_two();
    let layout = layout(1024);

    let staged = assemble::<B>(&cfg)
        .expect("assembles")
        .finalize(&layout)
        .expect("finalizes");
    let direct = finalize::<B>(&cfg, &layout).expect("finalizes");

    assert_eq!(staged.code(), direct.code());
    assert_eq!(staged.frame(), direct.frame());
}

#[test]
fn assemble_then_finalize_matches_finalize_le() {
    assemble_then_finalize_matches_finalize::<Le>();
}

#[test]
fn assemble_then_finalize_matches_finalize_be() {
    assemble_then_finalize_matches_finalize::<Be>();
}

/// One artifact, many images: the resolved instruction list is a single constant,
/// and only the relocation moves as the region it names lands at a new address.
#[test]
fn one_artifact_lays_out_against_many_images() {
    let cfg = pushes(Region::Scratch);
    let artifact = assemble::<Le>(&cfg).expect("assembles");

    let base = |input| {
        let layout = Layout::new(Sizes {
            input,
            scratch: 8,
            stack: 64,
        })
        .expect("fits");

        let program = artifact.finalize(&layout).expect("finalizes");
        match first::<Le>(program.code()) {
            Instr::Push32(op) => op.imm,
            other => panic!("expected a base push, got {other:?}"),
        }
    };

    assert_ne!(base(0), base(16), "scratch moves with whatever precedes it");

    // And the thing that was laid out twice was one resolution, its placeholder
    // push identical no matter which image it is later given.
    let resolved = resolve(&cfg).expect("resolves");
    assert_eq!(resolved.code(), resolve(&cfg).expect("resolves").code());
    assert!(matches!(
        resolved.code().first(),
        Some(Instr::Push32(op)) if op.imm == 0
    ));
}
