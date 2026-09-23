//! Tests for the interpreter.

use super::*;
use crate::image::{Image, Layout, Sizes};
use crate::isa::*;
use crate::samples::instructions;
use crate::{Be, Le};

/// Room for a stack that some tests drive a long way up, and for an image below
/// it that absolute accesses can reach.
const MEMORY: usize = 0x3_0000;
const STACK_BASE: usize = 0x1_0000;

fn machine<B: ByteOrder>() -> Vm<B> {
    // Everything before the stack is scratch, so the absolute accesses below
    // have somewhere to land that is not the stack.
    let layout = Layout::new(Sizes {
        scratch: STACK_BASE as u32,
        stack: (MEMORY - STACK_BASE) as u32,
        ..Sizes::default()
    })
    .expect("fits");

    Vm::new(Image::new(layout))
}

/// Steps a sequence, returning the last instruction's flow.
fn run<B: ByteOrder>(vm: &mut Vm<B>, program: &[Instr]) -> Result<Flow, Trap> {
    let mut flow = Flow::Next;
    for instr in program {
        flow = vm.step(*instr)?;
    }
    Ok(flow)
}

/// Runs a sequence and returns the word left on top.
fn eval<B: ByteOrder>(program: &[Instr]) -> Result<Word, Trap> {
    let mut vm = machine::<B>();
    run(&mut vm, program)?;
    vm.pop()
}

// -- the stack ------------------------------------------------------------

/// The stack grows upward and its words are laid out in `B`'s order. The growth
/// direction is fixed and is *not* the byte order; conflating the two is the
/// classic bug here, so both are asserted at once.
fn the_stack_grows_upward_in_b_order<B: ByteOrder>() {
    const VALUE: Word = 0x0102_0304_0506_0708;

    let mut vm = machine::<B>();
    vm.push(VALUE).expect("room on the stack");

    assert_eq!(vm.sp(), STACK_BASE + WORD_SIZE);
    assert_eq!(
        vm.memory().get(STACK_BASE..vm.sp()).expect("the top word"),
        B::write_u64(VALUE),
    );

    assert_eq!(vm.pop().expect("a word"), VALUE);
    assert_eq!(vm.sp(), STACK_BASE);
}

#[test]
fn the_stack_grows_upward_in_b_order_le() {
    the_stack_grows_upward_in_b_order::<Le>();
}

#[test]
fn the_stack_grows_upward_in_b_order_be() {
    the_stack_grows_upward_in_b_order::<Be>();
}

/// Underflow is a trap, not a wrap: this check is what replaces the implicit
/// in-range indexing an absolutely-addressed locals array used to get for free.
#[test]
fn popping_below_the_stack_base_traps() {
    let mut vm = machine::<Le>();
    assert_eq!(vm.pop(), Err(Trap::StackUnderflow));

    // Bytes below the base are readable by address, but the stack may not
    // reach them.
    vm.push(1).expect("room");
    assert_eq!(run(&mut vm, &[Free { n: frame(8) }.into()]), Ok(Flow::Next));
    assert_eq!(
        run(&mut vm, &[Free { n: frame(8) }.into()]),
        Err(Trap::StackUnderflow)
    );
}

#[test]
fn growing_past_the_end_of_memory_traps() {
    let mut vm = Vm::<Le>::new(crate::samples::stack_image(2 * WORD_SIZE as u32));

    vm.push(1).expect("room");
    vm.push(2).expect("room");
    assert_eq!(vm.push(3), Err(Trap::StackOverflow));
    assert_eq!(
        run(&mut vm, &[Alloc { n: frame(8) }.into()]),
        Err(Trap::StackOverflow)
    );
}

// -- arithmetic -----------------------------------------------------------

/// `push a; push b; sub` is `a - b`: the right-hand operand is the one on top.
/// Get this backwards and every non-commutative operation is silently mirrored.
#[test]
fn the_right_hand_operand_is_on_top() {
    let program = [
        Push8 { imm: 10 }.into(),
        Push8 { imm: 3 }.into(),
        Sub.into(),
    ];
    assert_eq!(eval::<Le>(&program), Ok(7));
}

#[test]
fn arithmetic_wraps_rather_than_panicking() {
    let program = [Push8 { imm: 0 }.into(), Push8 { imm: 1 }.into(), Sub.into()];
    assert_eq!(eval::<Le>(&program), Ok(Word::MAX));
}

#[test]
fn division_by_zero_traps() {
    for divide in [
        Instr::from(crate::isa::Div),
        crate::isa::Rem.into(),
        SDiv.into(),
    ] {
        let program = [Push8 { imm: 1 }.into(), Push8 { imm: 0 }.into(), divide];
        assert_eq!(eval::<Le>(&program), Err(Trap::DivideByZero), "{divide:?}");
    }
}

/// `i64::MIN / -1` is the one signed division with no representable answer.
/// Left to wrap it would quietly produce `i64::MIN` where Rust panics.
#[test]
fn the_unrepresentable_signed_division_traps() {
    let program = [
        Push64 {
            imm: i64::MIN as Word,
        }
        .into(),
        Push64 {
            imm: (-1i64) as Word,
        }
        .into(),
        SDiv.into(),
    ];
    assert_eq!(eval::<Le>(&program), Err(Trap::DivideOverflow));
}

#[test]
fn signed_division_is_signed() {
    let program = [
        Push64 {
            imm: (-6i64) as Word,
        }
        .into(),
        Push8 { imm: 3 }.into(),
        SDiv.into(),
    ];
    assert_eq!(eval::<Le>(&program), Ok((-2i64) as Word));
}

/// A shift by 64 is a shift by 0. Leaving that undefined is how a VM and its
/// reference come to disagree on the one input nobody tested.
#[test]
fn shift_counts_are_masked_to_six_bits() {
    let by = |count: u8| {
        [
            Push8 { imm: 1 }.into(),
            Push8 { imm: count }.into(),
            Shl.into(),
        ]
    };

    assert_eq!(eval::<Le>(&by(64)), eval::<Le>(&by(0)));
    assert_eq!(eval::<Le>(&by(65)), eval::<Le>(&by(1)));
}

#[test]
fn the_two_right_shifts_differ_in_sign() {
    let shift = |op: Instr| {
        [
            Push64 {
                imm: (-8i64) as Word,
            }
            .into(),
            Push8 { imm: 1 }.into(),
            op,
        ]
    };

    assert_eq!(eval::<Le>(&shift(Sar.into())), Ok((-4i64) as Word));
    assert_eq!(eval::<Le>(&shift(Shr.into())), Ok(Word::MAX / 2 - 3));
}

#[test]
fn the_two_less_thans_differ_in_sign() {
    let compare = |op: Instr| {
        [
            Push64 {
                imm: (-1i64) as Word,
            }
            .into(),
            Push8 { imm: 1 }.into(),
            op,
        ]
    };

    assert_eq!(eval::<Le>(&compare(CmpSLt.into())), Ok(1), "-1 < 1");
    assert_eq!(
        eval::<Le>(&compare(CmpLt.into())),
        Ok(0),
        "as unsigned, huge"
    );
}

// -- frames ---------------------------------------------------------------

fn frame(bytes: u16) -> FrameSize {
    FrameSize::new(bytes).expect("a word-aligned frame")
}

/// The displacement formula, exercised: for a frame of size `F`, a cell at frame
/// offset `c`, and `d` bytes of operand stack pushed since the prologue,
/// `k = F − c + d`.
///
/// The two displacements below differ *only* in `d`, which is what makes this
/// the test for the whole mechanism: `STS` measures against `SP` before its pop,
/// so the value being stored still counts toward the depth.
fn frame_cells_are_reached_by_displacement<B: ByteOrder>() {
    const CELL: Word = 0xdead_beef;

    let program = [
        Alloc { n: frame(16) }.into(),      // F = 16
        Push32 { imm: CELL as u32 }.into(), // d = 8
        Sts32 { disp: 24 }.into(),          // k = 16 − 0 + 8
        Lds32 { disp: 16 }.into(),          // k = 16 − 0 + 0
    ];

    assert_eq!(eval::<B>(&program), Ok(CELL));
}

#[test]
fn frame_cells_are_reached_by_displacement_le() {
    frame_cells_are_reached_by_displacement::<Le>();
}

#[test]
fn frame_cells_are_reached_by_displacement_be() {
    frame_cells_are_reached_by_displacement::<Be>();
}

/// Storing into a narrow cell truncates, and that is the point: the store *is*
/// the mask a `u8` computation would otherwise have to apply.
fn narrow_cells_truncate<B: ByteOrder>() {
    let program = [
        Alloc { n: frame(8) }.into(),
        Push32 { imm: 0x1234_56ff }.into(),
        Sts8 { disp: 16 }.into(),
        Lds8 { disp: 8 }.into(),
    ];

    assert_eq!(eval::<B>(&program), Ok(0xff));
}

#[test]
fn narrow_cells_truncate_le() {
    narrow_cells_truncate::<Le>();
}

#[test]
fn narrow_cells_truncate_be() {
    narrow_cells_truncate::<Be>();
}

#[test]
fn a_displacement_past_the_stack_base_traps() {
    let mut vm = machine::<Le>();
    assert_eq!(
        run(&mut vm, &[Lds64 { disp: 8 }.into()]),
        Err(Trap::StackUnderflow),
        "the stack is empty; there is no cell to read"
    );
}

// -- memory ---------------------------------------------------------------

/// Absolute accesses see the same byte order as the stack, and the stack is
/// addressable like any other memory: one address space, one order.
fn absolute_accesses_use_b_order<B: ByteOrder>() {
    const ADDRESS: Word = 0x100;
    const VALUE: u32 = 0x1234_5678;

    let mut vm = machine::<B>();
    run(
        &mut vm,
        &[
            Push32 {
                imm: ADDRESS as u32,
            }
            .into(),
            Push32 { imm: VALUE }.into(),
            St32.into(),
        ],
    )
    .expect("the store runs");

    let written = vm
        .memory()
        .get(ADDRESS as usize..ADDRESS as usize + 4)
        .expect("in bounds");
    assert_eq!(written, B::write_u32(VALUE));

    run(
        &mut vm,
        &[
            Push32 {
                imm: ADDRESS as u32,
            }
            .into(),
            Ld32.into(),
        ],
    )
    .expect("the load runs");
    assert_eq!(vm.pop(), Ok(Word::from(VALUE)));
}

#[test]
fn absolute_accesses_use_b_order_le() {
    absolute_accesses_use_b_order::<Le>();
}

#[test]
fn absolute_accesses_use_b_order_be() {
    absolute_accesses_use_b_order::<Be>();
}

/// A single byte has no order, so `ST8` must write the same byte in either
/// build — even though the stack it was pushed through does not agree with
/// itself, which is the whole reason the distinction is worth a test.
#[test]
fn byte_accesses_are_order_blind() {
    const ADDRESS: usize = 0x200;

    let program = [
        Push32 {
            imm: ADDRESS as u32,
        }
        .into(),
        Push32 { imm: 0xabcd }.into(),
        St8.into(),
    ];

    let stored = |image: Vec<u8>| image.get(ADDRESS).copied().expect("in bounds");
    let mut little = machine::<Le>();
    let mut big = machine::<Be>();
    run(&mut little, &program).expect("runs");
    run(&mut big, &program).expect("runs");

    let little = stored(little.into_memory());
    assert_eq!(little, 0xcd, "the low byte, truncated");
    assert_eq!(little, stored(big.into_memory()));
}

#[test]
fn an_access_outside_memory_traps() {
    let past_the_end = MEMORY as u32;
    let program = [Push32 { imm: past_the_end }.into(), Ld64.into()];
    assert_eq!(
        eval::<Le>(&program),
        Err(Trap::OutOfBounds {
            address: Word::from(past_the_end),
            width: 8
        })
    );

    // The last four bytes are readable; a word starting there is not.
    let last_word = (MEMORY - 4) as u32;
    assert!(eval::<Le>(&[Push32 { imm: last_word }.into(), Ld32.into()]).is_ok());
    assert!(eval::<Le>(&[Push32 { imm: last_word }.into(), Ld64.into()]).is_err());
}

// -- control and metering -------------------------------------------------

/// Branches report a byte delta instead of applying one: an instruction knows
/// its offset but not its own address.
#[test]
fn branches_report_their_delta() {
    let mut vm = machine::<Le>();

    assert_eq!(
        run(&mut vm, &[Jmp { offset: -12 }.into()]),
        Ok(Flow::Jump(-12))
    );

    let taken = [Push8 { imm: 0 }.into(), Jz { offset: 4 }.into()];
    assert_eq!(run(&mut vm, &taken), Ok(Flow::Jump(4)));

    let not_taken = [Push8 { imm: 1 }.into(), Jz { offset: 4 }.into()];
    assert_eq!(run(&mut vm, &not_taken), Ok(Flow::Next));

    let taken = [Push8 { imm: 1 }.into(), Jnz { offset: 4 }.into()];
    assert_eq!(run(&mut vm, &taken), Ok(Flow::Jump(4)));

    assert_eq!(run(&mut vm, &[crate::isa::Halt.into()]), Ok(Flow::Halt));
}

/// A loop, driven by hand. This is what a fetch loop will do with [`Flow`] once
/// there is one, minus the arithmetic on byte offsets — and `LDS64 8` is how a
/// stack machine with no shuffle group spells `DUP`.
#[test]
fn a_countdown_loop_terminates() {
    let mut vm = machine::<Le>();
    vm.step(Push8 { imm: 3 }.into()).expect("room");

    let body: [Instr; 4] = [
        Push8 { imm: 1 }.into(),
        Sub.into(),
        Lds64 { disp: 8 }.into(),
        Jnz { offset: -16 }.into(),
    ];

    // The bound is the test's own: with metering gone from the machine, nothing
    // below stops a body that never reports `Next`.
    let mut iterations = 0;
    while run(&mut vm, &body).expect("the body runs") != Flow::Next {
        iterations += 1;
        assert!(iterations < 10, "the counter never reached zero");
    }

    assert_eq!(iterations, 2, "three passes, the last one falling through");
    assert_eq!(vm.pop(), Ok(0));
    assert_eq!(vm.sp(), STACK_BASE, "the loop is stack-neutral");
}

/// The reserved opcodes hold their numbering without pretending to work.
#[test]
fn reserved_instructions_trap() {
    let mut vm = machine::<Le>();
    vm.push(0).expect("room");

    assert_eq!(
        vm.step(Switch.into()),
        Err(Trap::Reserved {
            instr: Switch.into()
        })
    );
    let host = Host { index: 3 };
    assert_eq!(
        vm.step(host.into()),
        Err(Trap::Reserved { instr: host.into() })
    );
    assert_eq!(
        Trap::Reserved { instr: host.into() }.to_string(),
        "`host` is reserved and does nothing yet"
    );
}

// -- the SP model ---------------------------------------------------------

/// Every instruction moves `SP` by exactly what it said it would.
///
/// This is the load-bearing one. Since locals are addressed by displacement back
/// from `SP`, a `sp_delta` that disagrees with the interpreter does not show up
/// as a stack runaway the validator catches — it shows up as a silent read of
/// the wrong cell, arbitrarily far away.
fn sp_delta_predicts_execution<B: ByteOrder>() {
    for instr in instructions() {
        // The reserved opcodes never run, so there is nothing to predict.
        if matches!(instr, Instr::Switch(_) | Instr::Host(_)) {
            continue;
        }

        let mut vm = machine::<B>();

        // A frame to reach into and to release, then two operands so that every
        // shape has something to work on: binary ops get their pair, the
        // divisions get a non-zero divisor, and the stores get an address that
        // is in range.
        run(
            &mut vm,
            &[
                Alloc { n: frame(64) }.into(),
                Push8 { imm: 1 }.into(),
                Push8 { imm: 1 }.into(),
            ],
        )
        .expect("the prologue runs");

        let before = vm.sp();
        let flow = vm.step(instr);
        assert!(flow.is_ok(), "{} trapped: {flow:?}", instr.mnemonic());
        let moved = vm.sp() as i64 - before as i64;

        assert_eq!(
            moved,
            i64::from(instr.sp_delta()),
            "{} moved SP by {moved}",
            instr.mnemonic()
        );
    }
}

#[test]
fn sp_delta_predicts_execution_le() {
    sp_delta_predicts_execution::<Le>();
}

#[test]
fn sp_delta_predicts_execution_be() {
    sp_delta_predicts_execution::<Be>();
}
