//! The interpreter: the machine model made to run.
//!
//! # What this holds
//!
//! [`Vm`] is one flat byte address space and one register, `SP`.
//! There is no program counter and no code here: [`Vm::step`] executes one
//! already-decoded [`Instr`] and reports where control should go next as a
//! [`Flow`]. Fetching — turning an image into instructions and applying a
//! [`Flow::Jump`] to a byte offset — belongs to the layer that knows the
//! encoding, and a branch offset is meaningless without it.
//!
//! # Hardening
//!
//! Every dispatch is checked, unconditionally: `SP` in both directions, every
//! absolute and frame-relative access against the address space, and both
//! division traps. A knob to switch any of this off is easy to add later and
//! impossible to trust if the unhardened path was never the one under test.
//!
//! Bounding a program that does not terminate is not among them. Nothing here
//! repeats an instruction, so there is no loop to cut short; metering belongs to
//! whatever drives the fetch.
//!
//! Arithmetic wraps and never panics; the checks above are the only way an
//! instruction fails.

#[cfg(feature = "debug")]
use core::fmt;
use core::marker::PhantomData;

use crate::image::{Image, Layout, Region};
use crate::{ByteOrder, FrameSize, Instr, WORD_SIZE, Width, Word};

pub(crate) mod ret;
mod run;

pub use ret::{Ret, VmReturn};

crate::opaque_debug!(Flow, Vm<B: ByteOrder>);
crate::opaque_error!(Trap);

#[cfg(test)]
mod tests;

/// Where control goes after an instruction.
#[cfg_attr(feature = "debug", derive(Debug))]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Carry on with the instruction that follows.
    Next,
    /// Move the program counter by this many bytes, measured from the first byte
    /// of the instruction that follows.
    ///
    /// Reported rather than applied: an instruction knows its branch offset but
    /// not its own address, and only a fetch loop — which has to decode to find
    /// the next instruction — knows where "the instruction that follows" begins.
    Jump(i32),
    /// Stop. The machine ran to completion.
    Halt,
}

/// A program that ran and did something it may not.
///
/// Distinct from a decode failure, which says an artifact is not a program at
/// all. Collapsing the two would put "this image is corrupt" and "the guest
/// divided by zero" on one code path.
#[cfg_attr(feature = "debug", derive(Debug, thiserror::Error))]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trap {
    /// A push or `ALLOC` would take `SP` past the end of the address space.
    #[cfg_attr(
        feature = "debug",
        error("the stack would grow past the end of memory")
    )]
    StackOverflow,

    /// A pop, `FREE`, or a frame displacement would reach below the stack's
    /// base. This is the check that replaces an absolutely-indexed locals
    /// array's implicit in-range indexing.
    #[cfg_attr(feature = "debug", error("the stack would reach below its base"))]
    StackUnderflow,

    /// An access fell outside the address space.
    #[cfg_attr(
        feature = "debug",
        error("a {width}-byte access at {address:#x} is outside memory")
    )]
    OutOfBounds {
        /// The address the guest asked for.
        address: Word,
        /// Width of the access in bytes.
        width: usize,
    },

    /// A divisor of zero, signed or unsigned.
    #[cfg_attr(feature = "debug", error("division by zero"))]
    DivideByZero,

    /// `i64::MIN / -1`, the one signed division whose result is not
    /// representable. Left to wrap, it would quietly produce `i64::MIN`.
    #[cfg_attr(
        feature = "debug",
        error("the signed division has no representable result")
    )]
    DivideOverflow,

    /// An instruction whose opcode is reserved but whose behaviour is not
    /// defined yet. The bytes are spoken for so that numbering stays stable;
    /// running one is still an error.
    #[cfg_attr(
        feature = "debug",
        error("`{}` is reserved and does nothing yet", instr.mnemonic())
    )]
    Reserved {
        /// The reserved instruction.
        instr: Instr,
    },

    /// The guest ran an `ABORT`: a check it relies on did not hold, where
    /// the Rust it was lowered from would have panicked.
    #[cfg_attr(feature = "debug", error("the guest aborted"))]
    Aborted,

    // The rest are raised by the fetch loop rather than by an instruction: they
    // are what a program counter can do wrong, plus the budget that bounds it.
    /// The bytes at this offset do not begin an instruction.
    ///
    /// A decode failure and a trap stay separate types everywhere else — "this
    /// artifact is corrupt" and "the guest divided by zero" are different
    /// claims — but a machine that is already running has nowhere to report the
    /// former except as a trap.
    #[cfg_attr(
        feature = "debug",
        error("the bytes at {offset:#x} do not begin an instruction")
    )]
    BadInstruction {
        /// Offset of the byte the program counter was on.
        offset: usize,
    },

    /// The program counter left the code, most often by running past the last
    /// instruction without meeting a `HALT`.
    #[cfg_attr(
        feature = "debug",
        error("the program counter left the code at {offset:#x}")
    )]
    CodeOutOfRange {
        /// Offset the program counter reached.
        offset: usize,
    },

    /// A branch aimed outside the code.
    #[cfg_attr(
        feature = "debug",
        error("the branch at {from:#x} aims {delta} bytes outside the code")
    )]
    BadJump {
        /// Offset of the branch instruction.
        from: usize,
        /// The offset it carried.
        delta: i32,
    },

    /// The fuel budget ran out mid-program.
    #[cfg_attr(feature = "debug", error("out of fuel"))]
    OutOfFuel,
}

/// A stack machine over one flat byte address space.
///
/// The stack is a region of that space and grows upward; `SP` is the byte index
/// one past the top. Locals are not a separate array — they are bytes below
/// `SP`, reached by displacement — so `LD`/`ST` can address the stack like any
/// other memory. Nothing on the stack is a return address, because there are no
/// calls, which is what makes that harmless.
///
/// `B` fixes the byte order of every multi-byte access, operand stack and frame
/// cells included. It is a type parameter so that a program and the host that
/// marshals for it cannot disagree: a mismatch is a type error rather than a
/// wrong answer.
pub struct Vm<B: ByteOrder> {
    memory: Vec<u8>,
    layout: Layout,
    stack: core::ops::Range<usize>,
    sp: usize,
    order: PhantomData<B>,
}

impl<B: ByteOrder> Vm<B> {
    /// Builds a machine over an image, with an empty stack.
    ///
    /// Everything outside `.stack` is still addressable by `LD*`/`ST*` — there
    /// is one address space, and the regions are a convention about who owns
    /// what. `SP` is the exception: it is bounded to `.stack` in both
    /// directions, which is the check that replaced an absolutely-indexed locals
    /// array's implicit in-range indexing.
    ///
    /// # Examples
    ///
    /// ```
    /// use safetynet_core::image::{Image, Layout, Sizes};
    /// use safetynet_core::isa::{Add, Push8};
    /// use safetynet_core::vm::{Flow, Vm};
    /// use safetynet_core::Le;
    ///
    /// let layout = Layout::new(Sizes { stack: 64, ..Sizes::default() }).expect("fits");
    /// let mut vm = Vm::<Le>::new(Image::new(layout));
    ///
    /// vm.step(Push8 { imm: 2 }.into())?;
    /// vm.step(Push8 { imm: 40 }.into())?;
    /// assert_eq!(vm.step(Add.into())?, Flow::Next);
    ///
    /// assert_eq!(vm.pop()?, 42);
    /// # Ok::<_, safetynet_core::vm::Trap>(())
    /// ```
    pub fn new(image: Image) -> Self {
        let layout = *image.layout();
        let stack = layout.span(Region::Stack).range();

        Self {
            memory: image.into_memory(),
            layout,
            sp: stack.start,
            stack,
            order: PhantomData,
        }
    }

    /// Where every region of this machine's address space lives.
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// A view of what the machine left, for reading a result after it halts.
    ///
    /// The result sits on top of the operand stack; this hands back a reader
    /// over it and the memory a returned pointer can name.
    pub fn ret(&self) -> Ret<'_, B> {
        Ret::new(&self.memory, self.stack.start, self.sp)
    }

    /// The bytes of one region, for a host reading a result back out.
    pub fn region(&self, region: Region) -> &[u8] {
        self.memory
            .get(self.layout.span(region).range())
            .unwrap_or_default()
    }

    /// Executes one instruction and reports where control goes next.
    ///
    /// One instruction, once: counting them, and deciding when there have been
    /// too many, is the caller's.
    #[inline(always)]
    pub fn step(&mut self, instr: Instr) -> Result<Flow, Trap> {
        instr.exec(self)
    }

    /// The stack pointer: the byte index one past the top of the stack.
    pub const fn sp(&self) -> usize {
        self.sp
    }

    /// The address space, for a host reading results out of it.
    pub fn memory(&self) -> &[u8] {
        &self.memory
    }

    /// Takes the address space back.
    pub fn into_memory(self) -> Vec<u8> {
        self.memory
    }

    /// Pushes a word, growing the stack by [`WORD_SIZE`] bytes.
    #[inline(always)]
    pub fn push(&mut self, value: Word) -> Result<(), Trap> {
        let top = self.sp.checked_add(WORD_SIZE).ok_or(Trap::StackOverflow)?;
        if top > self.stack.end {
            return Err(Trap::StackOverflow);
        }

        let slot = self
            .memory
            .get_mut(self.sp..top)
            .ok_or(Trap::StackOverflow)?;

        slot.copy_from_slice(&B::write_u64(value));
        self.sp = top;
        Ok(())
    }

    /// Pops a word, shrinking the stack by [`WORD_SIZE`] bytes.
    #[inline(always)]
    pub fn pop(&mut self) -> Result<Word, Trap> {
        let bottom = self.sp.checked_sub(WORD_SIZE).ok_or(Trap::StackUnderflow)?;
        if bottom < self.stack.start {
            return Err(Trap::StackUnderflow);
        }

        let bytes = self
            .memory
            .get(bottom..self.sp)
            .and_then(<[u8]>::first_chunk::<WORD_SIZE>)
            .ok_or(Trap::StackUnderflow)?;

        self.sp = bottom;
        Ok(B::read_u64(*bytes))
    }

    /// Pushes a constant that an instruction carried as an immediate.
    #[inline(always)]
    pub(crate) fn push_imm(&mut self, value: Word) -> Result<Flow, Trap> {
        self.push(value)?;
        Ok(Flow::Next)
    }

    /// Discards the top word.
    #[inline(always)]
    pub(crate) fn drop_word(&mut self) -> Result<Flow, Trap> {
        self.pop()?;
        Ok(Flow::Next)
    }

    /// Reserves a frame. The reserved bytes keep whatever the last frame left
    /// there: nothing reads a cell before writing it, and zeroing would charge
    /// every prologue for a guarantee no correct program needs.
    #[inline(always)]
    pub(crate) fn alloc(&mut self, n: FrameSize) -> Result<Flow, Trap> {
        let top = self
            .sp
            .checked_add(usize::from(n.bytes()))
            .ok_or(Trap::StackOverflow)?;

        if top > self.stack.end {
            return Err(Trap::StackOverflow);
        }

        self.sp = top;
        Ok(Flow::Next)
    }

    /// Releases a frame.
    #[inline(always)]
    pub(crate) fn free(&mut self, n: FrameSize) -> Result<Flow, Trap> {
        let bottom = self
            .sp
            .checked_sub(usize::from(n.bytes()))
            .ok_or(Trap::StackUnderflow)?;

        if bottom < self.stack.start {
            return Err(Trap::StackUnderflow);
        }

        self.sp = bottom;
        Ok(Flow::Next)
    }

    /// Pops an address and pushes the value there, zero-extended to a word.
    #[inline(always)]
    pub(crate) fn load(&mut self, width: Width) -> Result<Flow, Trap> {
        let address = self.pop()?;
        let value = self.read(Self::offset(address, width)?, width, address)?;
        self.push(value)?;
        Ok(Flow::Next)
    }

    /// Pops a value, then an address, and writes the value's low bytes there.
    #[inline(always)]
    pub(crate) fn store(&mut self, width: Width) -> Result<Flow, Trap> {
        let value = self.pop()?;
        let address = self.pop()?;
        self.write(Self::offset(address, width)?, width, value, address)?;
        Ok(Flow::Next)
    }

    /// Pushes the frame cell at `SP - disp`, zero-extended to a word.
    #[inline(always)]
    pub(crate) fn load_frame(&mut self, disp: u16, width: Width) -> Result<Flow, Trap> {
        let cell = self.cell(disp)?;
        let value = self.read(cell, width, cell as Word)?;
        self.push(value)?;
        Ok(Flow::Next)
    }

    /// Pops a word and writes its low bytes to the frame cell at `SP - disp`.
    ///
    /// The displacement is measured against `SP` *before* the pop, which is the
    /// depth the IR tracks at this instruction — the value being stored is still
    /// counted as on the stack.
    #[inline(always)]
    pub(crate) fn store_frame(&mut self, disp: u16, width: Width) -> Result<Flow, Trap> {
        let cell = self.cell(disp)?;
        let value = self.pop()?;
        self.write(cell, width, value, cell as Word)?;
        Ok(Flow::Next)
    }

    /// Pops two operands and pushes the result. The right-hand operand is on
    /// top, so `push a; push b; sub` computes `a - b`.
    #[inline(always)]
    pub(crate) fn binary(&mut self, op: impl FnOnce(Word, Word) -> Word) -> Result<Flow, Trap> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(op(lhs, rhs))?;
        Ok(Flow::Next)
    }

    /// [`Vm::binary`] for the operations that can fail: the four divisions.
    #[inline(always)]
    pub(crate) fn binary_checked(
        &mut self,
        op: impl FnOnce(Word, Word) -> Result<Word, Trap>,
    ) -> Result<Flow, Trap> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(op(lhs, rhs)?)?;
        Ok(Flow::Next)
    }

    /// Replaces the top word with a function of itself.
    #[inline(always)]
    pub(crate) fn unary(&mut self, op: impl FnOnce(Word) -> Word) -> Result<Flow, Trap> {
        let value = self.pop()?;
        self.push(op(value))?;
        Ok(Flow::Next)
    }

    /// Pops two operands and pushes a full word, 0 or 1.
    #[inline(always)]
    pub(crate) fn compare(&mut self, op: impl FnOnce(Word, Word) -> bool) -> Result<Flow, Trap> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.push(Word::from(op(lhs, rhs)))?;
        Ok(Flow::Next)
    }

    /// Pops a word and branches if it is zero.
    #[inline(always)]
    pub(crate) fn jump_if_zero(&mut self, offset: i32) -> Result<Flow, Trap> {
        let taken = self.pop()? == 0;
        Ok(if taken {
            Flow::Jump(offset)
        } else {
            Flow::Next
        })
    }

    /// Pops a word and branches if it is not zero.
    #[inline(always)]
    pub(crate) fn jump_if_not_zero(&mut self, offset: i32) -> Result<Flow, Trap> {
        let taken = self.pop()? != 0;
        Ok(if taken {
            Flow::Jump(offset)
        } else {
            Flow::Next
        })
    }

    /// Turns a guest address into an offset into the address space.
    ///
    /// `width` and `address` only travel so that a failure can say what was
    /// attempted; on a 64-bit host the conversion itself cannot fail, and the
    /// bounds check that matters happens at the access.
    #[inline(always)]
    fn offset(address: Word, width: Width) -> Result<usize, Trap> {
        usize::try_from(address).map_err(|_| Trap::OutOfBounds {
            address,
            width: usize::from(width.bytes()),
        })
    }

    /// The address of the frame cell `disp` bytes back from `SP`.
    #[inline(always)]
    fn cell(&self, disp: u16) -> Result<usize, Trap> {
        let cell = self
            .sp
            .checked_sub(usize::from(disp))
            .ok_or(Trap::StackUnderflow)?;

        if cell < self.stack.start {
            return Err(Trap::StackUnderflow);
        }

        Ok(cell)
    }

    /// Reads `width` bytes, zero-extended to a word.
    #[inline(always)]
    fn read(&self, offset: usize, width: Width, address: Word) -> Result<Word, Trap> {
        let out_of_bounds = || Trap::OutOfBounds {
            address,
            width: usize::from(width.bytes()),
        };
        let from = self.memory.get(offset..).ok_or_else(out_of_bounds)?;

        match width {
            Width::U8 => from.first().map(|byte| Word::from(*byte)),
            Width::U32 => from.first_chunk::<4>().map(|b| Word::from(B::read_u32(*b))),
            Width::U64 => from.first_chunk::<8>().map(|b| B::read_u64(*b)),
        }
        .ok_or_else(out_of_bounds)
    }

    /// Writes the low `width` bytes of `value`.
    ///
    /// The narrowing is the point rather than a loss: storing into a one-byte
    /// cell *is* the mask a `u8` computation would otherwise have to apply.
    #[inline(always)]
    fn write(
        &mut self,
        offset: usize,
        width: Width,
        value: Word,
        address: Word,
    ) -> Result<(), Trap> {
        let out_of_bounds = || Trap::OutOfBounds {
            address,
            width: usize::from(width.bytes()),
        };
        let into = self.memory.get_mut(offset..).ok_or_else(out_of_bounds)?;

        match width {
            Width::U8 => {
                *into.first_mut().ok_or_else(out_of_bounds)? = value as u8;
            }
            Width::U32 => {
                *into.first_chunk_mut::<4>().ok_or_else(out_of_bounds)? =
                    B::write_u32(value as u32);
            }
            Width::U64 => {
                *into.first_chunk_mut::<8>().ok_or_else(out_of_bounds)? = B::write_u64(value);
            }
        }

        Ok(())
    }
}

/// Deliberately omits the address space: a dump of every byte is never what a
/// reader of a panic message wants.
#[cfg(feature = "debug")]
impl<B: ByteOrder> fmt::Debug for Vm<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("order", &B::NAME)
            .field("memory", &self.memory.len())
            .field("stack", &self.stack)
            .field("sp", &self.sp)
            .finish()
    }
}

/// Unsigned division, trapping on a zero divisor.
#[inline(always)]
pub(crate) fn div(lhs: Word, rhs: Word) -> Result<Word, Trap> {
    lhs.checked_div(rhs).ok_or(Trap::DivideByZero)
}

/// Unsigned remainder, trapping on a zero divisor.
#[inline(always)]
pub(crate) fn rem(lhs: Word, rhs: Word) -> Result<Word, Trap> {
    lhs.checked_rem(rhs).ok_or(Trap::DivideByZero)
}

/// Signed division, trapping on a zero divisor and on `i64::MIN / -1`.
#[inline(always)]
pub(crate) fn sdiv(lhs: Word, rhs: Word) -> Result<Word, Trap> {
    signed(lhs, rhs, i64::checked_div)
}

/// Signed remainder, with the same two traps as [`sdiv`].
#[inline(always)]
pub(crate) fn srem(lhs: Word, rhs: Word) -> Result<Word, Trap> {
    signed(lhs, rhs, i64::checked_rem)
}

/// Shared body of the signed pair: the two failures are indistinguishable in
/// `checked_*`, so they are told apart here rather than in each operation.
#[inline(always)]
fn signed(lhs: Word, rhs: Word, op: fn(i64, i64) -> Option<i64>) -> Result<Word, Trap> {
    match op(lhs as i64, rhs as i64) {
        Some(result) => Ok(result as Word),
        None if rhs == 0 => Err(Trap::DivideByZero),
        None => Err(Trap::DivideOverflow),
    }
}
