//! The instruction set: the machine model, with no encoding in it.
//!
//! # Machine model
//!
//! A stack machine over a single flat byte address space. One register, `SP`,
//! is the byte index one past the top of the stack; the stack grows upward.
//! Locals are byte-granular *frame cells* reached by
//! displacement back from `SP`, so where a local lives depends on how deep the
//! operand stack currently is. That displacement is a compile-time constant,
//! computed from the stack depth the IR tracks at every program point.
//!
//! The operand stack is word-granular: every push and pop moves eight bytes, so
//! `SP` stays 8-aligned and widths narrower than a word exist only in frame
//! cells and in memory.
//!
//! # Operand order
//!
//! Binary operations pop the right-hand operand first: `push a; push b; sub`
//! computes `a - b`. Stores pop the value first and the address second, so a
//! store reads `push addr; push value; st8`. Loads pop an address and push the
//! value they read.
//!
//! # Arithmetic
//!
//! Everything wraps; nothing panics. Division traps on a zero divisor, and the
//! signed forms additionally trap on `i64::MIN / -1`, the single signed division
//! with no representable result. Shift counts are masked to their low six bits,
//! so a shift by 64 is a shift by 0.

mod frame_size;
mod macros;

use musli::alloc::Global;
use musli::mode::Binary;
use musli::{Decode, Encode};

use crate::vm::{Flow, Trap, Vm, div, rem, sdiv, srem};
use crate::{ByteOrder, WORD_SIZE, Width, Word};
use macros::{define_op_struct, define_ops};

pub use frame_size::FrameSize;

/// Stack effect of one word, as a signed byte count.
const WORD: i32 = WORD_SIZE as i32;

/// One operation: what it is called, what it does to `SP`, and what it does when
/// it runs.
pub trait Op:
    Copy
    + core::fmt::Debug
    + PartialEq
    + Eq
    + Sized
    + Encode<Binary>
    + for<'de> Decode<'de, Binary, Global>
{
    /// The assembly mnemonic, a debugging aid only.
    #[cfg(feature = "debug")]
    const MNEMONIC: &'static str;

    /// Net change to `SP`, in bytes; positive grows the stack.
    ///
    /// Takes `&self` because `ALLOC`/`FREE`'s effect *is* their operand.
    fn sp_delta(&self) -> i32;

    /// Runs the operation and reports where control goes next.
    ///
    /// Moving `SP` is the operation's own business; [`sp_delta`](Op::sp_delta)
    /// is the compile-time prediction of what this does, and the two agreeing is
    /// what every frame displacement rests on.
    fn exec<B: ByteOrder>(&self, vm: &mut Vm<B>) -> Result<Flow, Trap>;
}

define_ops! {
    /// Stops the machine. The result, if any, is already on top of the stack.
    Halt = "halt", sp(|_| 0), exec(|_, _| Ok(Flow::Halt));

    /// Pushes a byte constant, zero-extended to a word.
    Push8 {
        /// The constant.
        imm: u8
    } = "push8", sp(|_| WORD), exec(|vm, op| vm.push_imm(op.imm.into()));

    /// Pushes a 32-bit constant, zero-extended to a word.
    Push32 {
        /// The constant.
        imm: u32
    } = "push32", sp(|_| WORD), exec(|vm, op| vm.push_imm(op.imm.into()));

    /// Pushes a full-width constant.
    Push64 {
        /// The constant.
        imm: u64
    } = "push64", sp(|_| WORD), exec(|vm, op| vm.push_imm(op.imm));

    /// Discards the top word. Spelled separately from `FREE 8` because it is by
    /// far the most common way to shrink the stack.
    Drop = "drop", sp(|_| -WORD), exec(|vm, _| vm.drop_word());

    /// Reserves the frame in the prologue.
    Alloc {
        /// Frame size in bytes; word-aligned by construction.
        n: FrameSize
    } = "alloc", sp(|op| i32::from(op.n.bytes())), exec(|vm, op| vm.alloc(op.n));

    /// Releases the frame in the epilogue.
    Free {
        /// Bytes to release; word-aligned by construction.
        n: FrameSize
    } = "free", sp(|op| -i32::from(op.n.bytes())), exec(|vm, op| vm.free(op.n));

    /// Pushes the byte at `SP - disp`, zero-extended to a word.
    Lds8 {
        /// Displacement back from `SP`.
        disp: u16
    } = "lds8", sp(|_| WORD), exec(|vm, op| vm.load_frame(op.disp, Width::U8));

    /// Pushes the 32-bit value at `SP - disp`, zero-extended to a word.
    Lds32 {
        /// Displacement back from `SP`.
        disp: u16
    } = "lds32", sp(|_| WORD), exec(|vm, op| vm.load_frame(op.disp, Width::U32));

    /// Pushes the word at `SP - disp`.
    Lds64 {
        /// Displacement back from `SP`.
        disp: u16
    } = "lds64", sp(|_| WORD), exec(|vm, op| vm.load_frame(op.disp, Width::U64));

    /// Pops a word and stores its low byte at `SP - disp`.
    ///
    /// The narrowing is the point: storing into a one-byte cell *is* the mask a
    /// `u8` computation would otherwise need.
    Sts8 {
        /// Displacement back from `SP`, measured before the pop.
        disp: u16
    } = "sts8", sp(|_| -WORD), exec(|vm, op| vm.store_frame(op.disp, Width::U8));

    /// Pops a word and stores its low four bytes at `SP - disp`.
    Sts32 {
        /// Displacement back from `SP`, measured before the pop.
        disp: u16
    } = "sts32", sp(|_| -WORD), exec(|vm, op| vm.store_frame(op.disp, Width::U32));

    /// Pops a word and stores it at `SP - disp`.
    Sts64 {
        /// Displacement back from `SP`, measured before the pop.
        disp: u16
    } = "sts64", sp(|_| -WORD), exec(|vm, op| vm.store_frame(op.disp, Width::U64));

    /// Pops an address, pushes the byte there, zero-extended.
    Ld8 = "ld8", sp(|_| 0), exec(|vm, _| vm.load(Width::U8));
    /// Pops an address, pushes the 32-bit value there, zero-extended.
    Ld32 = "ld32", sp(|_| 0), exec(|vm, _| vm.load(Width::U32));
    /// Pops an address, pushes the word there.
    Ld64 = "ld64", sp(|_| 0), exec(|vm, _| vm.load(Width::U64));
    /// Pops a value then an address; stores the value's low byte.
    St8 = "st8", sp(|_| -2 * WORD), exec(|vm, _| vm.store(Width::U8));
    /// Pops a value then an address; stores the value's low four bytes.
    St32 = "st32", sp(|_| -2 * WORD), exec(|vm, _| vm.store(Width::U32));
    /// Pops a value then an address; stores the whole word.
    St64 = "st64", sp(|_| -2 * WORD), exec(|vm, _| vm.store(Width::U64));

    /// Wrapping addition.
    Add = "add", sp(|_| -WORD), exec(|vm, _| vm.binary(Word::wrapping_add));
    /// Wrapping subtraction.
    Sub = "sub", sp(|_| -WORD), exec(|vm, _| vm.binary(Word::wrapping_sub));
    /// Wrapping multiplication.
    Mul = "mul", sp(|_| -WORD), exec(|vm, _| vm.binary(Word::wrapping_mul));
    /// Unsigned division; traps on a zero divisor.
    Div = "div", sp(|_| -WORD), exec(|vm, _| vm.binary_checked(div));
    /// Unsigned remainder; traps on a zero divisor.
    Rem = "rem", sp(|_| -WORD), exec(|vm, _| vm.binary_checked(rem));
    /// Signed division; traps on a zero divisor and on `i64::MIN / -1`.
    SDiv = "sdiv", sp(|_| -WORD), exec(|vm, _| vm.binary_checked(sdiv));
    /// Signed remainder; traps on a zero divisor and on `i64::MIN % -1`.
    SRem = "srem", sp(|_| -WORD), exec(|vm, _| vm.binary_checked(srem));

    /// Bitwise and.
    And = "and", sp(|_| -WORD), exec(|vm, _| vm.binary(|a, b| a & b));
    /// Bitwise or.
    Or = "or", sp(|_| -WORD), exec(|vm, _| vm.binary(|a, b| a | b));
    /// Bitwise exclusive or.
    Xor = "xor", sp(|_| -WORD), exec(|vm, _| vm.binary(|a, b| a ^ b));
    /// Bitwise complement of the top word.
    BitNot = "not", sp(|_| 0), exec(|vm, _| vm.unary(|a| !a));
    /// Shift left; the count is masked to six bits.
    Shl = "shl", sp(|_| -WORD), exec(|vm, _| vm.binary(|a, b| a.wrapping_shl(b as u32)));
    /// Logical shift right; the count is masked to six bits.
    Shr = "shr", sp(|_| -WORD), exec(|vm, _| vm.binary(|a, b| a.wrapping_shr(b as u32)));
    /// Arithmetic shift right; the count is masked to six bits.
    Sar = "sar", sp(|_| -WORD),
        exec(|vm, _| vm.binary(|a, b| (a as i64).wrapping_shr(b as u32) as Word));

    /// Equality; pushes 0 or 1.
    CmpEq = "eq", sp(|_| -WORD), exec(|vm, _| vm.compare(|a, b| a == b));
    /// Unsigned less-than; pushes 0 or 1.
    CmpLt = "lt", sp(|_| -WORD), exec(|vm, _| vm.compare(|a, b| a < b));
    /// Unsigned less-or-equal; pushes 0 or 1.
    CmpLe = "le", sp(|_| -WORD), exec(|vm, _| vm.compare(|a, b| a <= b));
    /// Signed less-than; pushes 0 or 1.
    CmpSLt = "slt", sp(|_| -WORD), exec(|vm, _| vm.compare(|a, b| (a as i64) < (b as i64)));
    /// Signed less-or-equal; pushes 0 or 1.
    CmpSLe = "sle", sp(|_| -WORD), exec(|vm, _| vm.compare(|a, b| (a as i64) <= (b as i64)));

    /// Unconditional relative jump.
    Jmp {
        /// Offset from the first byte of the following instruction.
        offset: i32
    } = "jmp", sp(|_| 0), exec(|_, op| Ok(Flow::Jump(op.offset)));

    /// Pops a word; jumps if it is zero.
    Jz {
        /// Offset from the first byte of the following instruction.
        offset: i32
    } = "jz", sp(|_| -WORD), exec(|vm, op| vm.jump_if_zero(op.offset));

    /// Pops a word; jumps if it is non-zero.
    Jnz {
        /// Offset from the first byte of the following instruction.
        offset: i32
    } = "jnz", sp(|_| -WORD), exec(|vm, op| vm.jump_if_not_zero(op.offset));

    /// Jump table. **Reserved**: no operand format is fixed yet, and executing
    /// one traps.
    Switch = "switch", sp(|_| -WORD),
        exec(|_, op| Err(Trap::Reserved { instr: (*op).into() }));

    /// Host escape. **Reserved**: traps until there is a host table to index.
    Host {
        /// Index into the author-registered host function table.
        index: u8
    } = "host", sp(|_| 0), exec(|_, op| Err(Trap::Reserved { instr: (*op).into() }));

    /// Stops the machine with a trap: the guest's own panic, for a check it
    /// could not pass. A stack state is not needed, so it costs nothing.
    Abort = "abort", sp(|_| 0), exec(|_, _| Err(Trap::Aborted));
}

#[cfg(test)]
mod tests;
