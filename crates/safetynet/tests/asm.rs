//! The assembler end to end: a hand-written loop, assembled by `safetynet::asm!`,
//! disassembled back to a listing, and run under both byte orders.
//!
//! This is the only place the real expansion compiles, so it is also where the
//! `::safetynet::` paths it names are checked against the façade.

// An integration test is its own crate, where the allowance test code gets for
// `expect` reaches `#[test]` functions only — and the helpers here are shared.
#![allow(clippy::expect_used)]

use safetynet::asm::print_listing;
use safetynet::encoding::decode;
use safetynet::image::{Image, Layout, Region, Sizes};
use safetynet::{Be, ByteOrder, Instr, Le, Program, Vm, Word};

/// The bytes the loop folds. Its length is spelled in the program too, as the
/// count the loop runs down.
const INPUT: [u8; 4] = [0x12, 0x34, 0x56, 0x78];

/// Room for the frame and the few words the loop keeps live.
const STACK: u32 = 256;

/// Far more than the loop needs; a runaway back edge still stops.
const FUEL: u64 = 10_000;

/// XOR-folds `.input` a byte at a time, leaves the result in `.scratch`, and
/// halts with it.
///
/// The order is a concrete `Le`/`Be` because the bytes are baked at expansion,
/// so one listing serves both through a `macro_rules!` wrapper rather than a
/// generic function.
///
/// The pieces that have to work: a frame of named cells, a conditional on a
/// counter, a back edge, and two region bases that are not numbers anyone here
/// can write down.
macro_rules! xor_loop {
    ($order:ident) => {
        safetynet::asm!($order {
            .frame { cursor: u64, left: u64, acc: u64 }
        entry:
            push .input
            store cursor
            push8 4
            store left
            push8 0
            store acc
        head:
            load left
            jz done
        body:
            load cursor
            ld8
            load acc
            xor
            store acc
            load cursor
            push8 1
            add
            store cursor
            load left
            push8 1
            sub
            store left
            jmp head
        done:
            push .scratch
            load acc
            st64
            load acc
            halt
        })
    };
}

/// Where the loop runs: the bytes it reads, a word for what it leaves behind,
/// and a stack.
fn layout() -> Layout {
    Layout::new(Sizes {
        input: u32::try_from(INPUT.len()).expect("four bytes"),
        scratch: 8,
        stack: STACK,
    })
    .expect("the image fits")
}

/// What the fold comes to, computed the other way round.
fn folded() -> Word {
    INPUT.iter().fold(0, |acc, byte| acc ^ Word::from(*byte))
}

/// The finalized bytecode read back out, instruction by instruction.
fn disassemble<B: ByteOrder>(program: &Program<B>) -> Vec<Instr> {
    let mut code = program.code();
    let mut decoded = Vec::new();

    while !code.is_empty() {
        let (instr, len) = decode::<B>(code).expect("decodes");
        decoded.push(instr);
        code = code.get(len..).unwrap_or_default();
    }

    decoded
}

/// The bytecode disassembles to the listing it was built from: the `alloc`
/// prologue the graph never spelled, the region bases resolved to addresses, and
/// the branches as offsets.
#[test]
fn the_loop_disassembles_to_a_listing() {
    let program = xor_loop!(Le).finalize(&layout()).expect("finalizes");
    let listing = print_listing(&disassemble(&program));

    assert_eq!(
        listing,
        "\
alloc 24
push32 0
sts64 32
push8 4
sts64 24
push8 0
sts64 16
lds64 16
jz 34
lds64 24
ld8
lds64 16
xor
sts64 16
lds64 24
push8 1
add
sts64 32
lds64 16
push8 1
sub
sts64 24
jmp -42
push32 8
lds64 16
st64
lds64 8
halt
"
    );
}

/// Finalize, run, and take the result off the stack — then read the same value
/// back out of `.scratch`, where only the byte order under test could have put
/// those eight bytes.
fn the_loop_runs_to_the_fold<B: ByteOrder>(program: &Program<B>, layout: &Layout) {
    let mut image = Image::new(*layout);
    image.write(Region::Input, &INPUT).expect("the input fits");

    let mut vm = Vm::<B>::new(image).run(program, FUEL).expect("runs");
    assert_eq!(vm.pop().expect("a result"), folded());

    let word: [u8; 8] = vm
        .region(Region::Scratch)
        .try_into()
        .expect("one word of scratch");
    assert_eq!(B::read_u64(word), folded());
}

#[test]
fn the_loop_runs_to_the_fold_le() {
    let layout = layout();
    let program = xor_loop!(Le).finalize(&layout).expect("finalizes");
    the_loop_runs_to_the_fold::<Le>(&program, &layout);
}

#[test]
fn the_loop_runs_to_the_fold_be() {
    let layout = layout();
    let program = xor_loop!(Be).finalize(&layout).expect("finalizes");
    the_loop_runs_to_the_fold::<Be>(&program, &layout);
}

/// The order is a type parameter all the way down: the same listing lowers to
/// different bytes.
#[test]
fn the_orders_reach_the_bytes() {
    let layout = layout();
    let little = xor_loop!(Le).finalize(&layout).expect("finalizes");
    let big = xor_loop!(Be).finalize(&layout).expect("finalizes");

    assert_ne!(little.code(), big.code());
}
