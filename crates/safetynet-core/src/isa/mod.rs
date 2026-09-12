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

use crate::WORD_SIZE;
use macros::{define_op_struct, define_ops};

pub use frame_size::FrameSize;

/// Stack effect of one word, as a signed byte count.
const WORD: i32 = WORD_SIZE as i32;

/// One operation: what it is called, what it does to `SP`, and how its operands
/// travel.
pub trait Op:
    Copy
    + core::fmt::Debug
    + PartialEq
    + Eq
    + Sized
    + Encode<Binary>
    + for<'de> Decode<'de, Binary, Global>
{
    /// The assembly mnemonic.
    const MNEMONIC: &'static str;

    /// Net change to `SP`, in bytes; positive grows the stack.
    ///
    /// Takes `&self` because `ALLOC`/`FREE`'s effect *is* their operand.
    fn sp_delta(&self) -> i32;
}

define_ops! {
    /// Stops the machine. The return value, if any, is already in `.ret`.
    Halt = "halt", sp(|_| 0);

    /// Pushes a byte constant, zero-extended to a word.
    Push8 {
        /// The constant.
        imm: u8
    } = "push8", sp(|_| WORD);

    /// Pushes a 32-bit constant, zero-extended to a word.
    Push32 {
        /// The constant.
        imm: u32
    } = "push32", sp(|_| WORD);

    /// Pushes a full-width constant.
    Push64 {
        /// The constant.
        imm: u64
    } = "push64", sp(|_| WORD);

    /// Discards the top word. Spelled separately from `FREE 8` because it is by
    /// far the most common way to shrink the stack.
    Drop = "drop", sp(|_| -WORD);

    /// Reserves the frame in the prologue.
    Alloc {
        /// Frame size in bytes; word-aligned by construction.
        n: FrameSize
    } = "alloc", sp(|op: &Alloc| i32::from(op.n.bytes()));

    /// Releases the frame in the epilogue.
    Free {
        /// Bytes to release; word-aligned by construction.
        n: FrameSize
    } = "free", sp(|op: &Free| -i32::from(op.n.bytes()));

    /// Pushes the byte at `SP - disp`, zero-extended to a word.
    Lds8 {
        /// Displacement back from `SP`.
        disp: u16
    } = "lds8", sp(|_| WORD);

    /// Pushes the 32-bit value at `SP - disp`, zero-extended to a word.
    Lds32 {
        /// Displacement back from `SP`.
        disp: u16
    } = "lds32", sp(|_| WORD);

    /// Pushes the word at `SP - disp`.
    Lds64 {
        /// Displacement back from `SP`.
        disp: u16
    } = "lds64", sp(|_| WORD);

    /// Pops a word and stores its low byte at `SP - disp`.
    ///
    /// The narrowing is the point: storing into a one-byte cell *is* the mask a
    /// `u8` computation would otherwise need.
    Sts8 {
        /// Displacement back from `SP`, measured before the pop.
        disp: u16
    } = "sts8", sp(|_| -WORD);

    /// Pops a word and stores its low four bytes at `SP - disp`.
    Sts32 {
        /// Displacement back from `SP`, measured before the pop.
        disp: u16
    } = "sts32", sp(|_| -WORD);

    /// Pops a word and stores it at `SP - disp`.
    Sts64 {
        /// Displacement back from `SP`, measured before the pop.
        disp: u16
    } = "sts64", sp(|_| -WORD);

    /// Pops an address, pushes the byte there, zero-extended.
    Ld8 = "ld8", sp(|_| 0);
    /// Pops an address, pushes the 32-bit value there, zero-extended.
    Ld32 = "ld32", sp(|_| 0);
    /// Pops an address, pushes the word there.
    Ld64 = "ld64", sp(|_| 0);
    /// Pops a value then an address; stores the value's low byte.
    St8 = "st8", sp(|_| -2 * WORD);
    /// Pops a value then an address; stores the value's low four bytes.
    St32 = "st32", sp(|_| -2 * WORD);
    /// Pops a value then an address; stores the whole word.
    St64 = "st64", sp(|_| -2 * WORD);

    /// Wrapping addition.
    Add = "add", sp(|_| -WORD);
    /// Wrapping subtraction.
    Sub = "sub", sp(|_| -WORD);
    /// Wrapping multiplication.
    Mul = "mul", sp(|_| -WORD);
    /// Unsigned division; traps on a zero divisor.
    Div = "div", sp(|_| -WORD);
    /// Unsigned remainder; traps on a zero divisor.
    Rem = "rem", sp(|_| -WORD);
    /// Signed division; traps on a zero divisor and on `i64::MIN / -1`.
    SDiv = "sdiv", sp(|_| -WORD);
    /// Signed remainder; traps on a zero divisor and on `i64::MIN % -1`.
    SRem = "srem", sp(|_| -WORD);

    /// Bitwise and.
    And = "and", sp(|_| -WORD);
    /// Bitwise or.
    Or = "or", sp(|_| -WORD);
    /// Bitwise exclusive or.
    Xor = "xor", sp(|_| -WORD);
    /// Bitwise complement of the top word.
    BitNot = "not", sp(|_| 0);
    /// Shift left; the count is masked to six bits.
    Shl = "shl", sp(|_| -WORD);
    /// Logical shift right; the count is masked to six bits.
    Shr = "shr", sp(|_| -WORD);
    /// Arithmetic shift right; the count is masked to six bits.
    Sar = "sar", sp(|_| -WORD);

    /// Equality; pushes 0 or 1.
    CmpEq = "eq", sp(|_| -WORD);
    /// Unsigned less-than; pushes 0 or 1.
    CmpLt = "lt", sp(|_| -WORD);
    /// Unsigned less-or-equal; pushes 0 or 1.
    CmpLe = "le", sp(|_| -WORD);
    /// Signed less-than; pushes 0 or 1.
    CmpSLt = "slt", sp(|_| -WORD);
    /// Signed less-or-equal; pushes 0 or 1.
    CmpSLe = "sle", sp(|_| -WORD);

    /// Unconditional relative jump.
    Jmp {
        /// Offset from the first byte of the following instruction.
        offset: i32
    } = "jmp", sp(|_| 0);

    /// Pops a word; jumps if it is zero.
    Jz {
        /// Offset from the first byte of the following instruction.
        offset: i32
    } = "jz", sp(|_| -WORD);

    /// Pops a word; jumps if it is non-zero.
    Jnz {
        /// Offset from the first byte of the following instruction.
        offset: i32
    } = "jnz", sp(|_| -WORD);

    /// Jump table. **Reserved**: no operand format is fixed yet, and executing
    /// one traps.
    Switch = "switch", sp(|_| -WORD);

    /// Host escape. **Reserved**: traps until there is a host table to index.
    Host {
        /// Index into the author-registered host function table.
        index: u8
    } = "host", sp(|_| 0);
}

#[cfg(test)]
mod tests;
