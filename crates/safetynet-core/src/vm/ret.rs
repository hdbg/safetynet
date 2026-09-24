//! What a halted machine left, for the host to read a result out of.
//!
//! A run ends with its result on top of the operand stack: a scalar is one
//! word, a region is a pointer and a length, a tuple is its parts in order. The
//! host reads them back through this view and nothing else — it can pull the
//! next word and read the memory a word points into, and it cannot step the
//! machine, change it, or reach its fuel or program counter. Keeping the
//! surface this small is what lets a return type be defined without fixing the
//! rest of the machine around it.

use core::marker::PhantomData;

use crate::{ByteOrder, WORD_SIZE, Word};

/// A read-only view of a halted machine's stack and memory.
///
/// The words come off the top of the stack downward, and `bytes` reads the
/// address space a pointer among them names. Both borrow the machine's memory,
/// so the view lasts only as long as the machine it was taken from.
pub struct Ret<'a, B: ByteOrder> {
    memory: &'a [u8],
    /// One past the topmost word not yet taken.
    top: usize,
    /// The stack's base: nothing below it is a result.
    floor: usize,
    order: PhantomData<B>,
}

impl<'a, B: ByteOrder> Ret<'a, B> {
    /// A view over `memory`, whose stack holds the result between `floor` and
    /// `top`.
    pub(crate) fn new(memory: &'a [u8], floor: usize, top: usize) -> Self {
        Self {
            memory,
            top,
            floor,
            order: PhantomData,
        }
    }

    /// The next result word, off the top of the stack, or `None` once the
    /// result is spent. Advances past it, so each part of a composite result
    /// reads the words it needs and leaves the rest for the next.
    pub fn word(&mut self) -> Option<Word> {
        let bottom = self.top.checked_sub(WORD_SIZE)?;
        if bottom < self.floor {
            return None;
        }
        let chunk = self
            .memory
            .get(bottom..self.top)
            .and_then(<[u8]>::first_chunk::<WORD_SIZE>)?;
        self.top = bottom;
        Some(B::read_u64(*chunk))
    }

    /// The bytes at an absolute address, clamped to what is there.
    ///
    /// The pointer is the guest's, so it is a claim, not a fact: a start past
    /// the end is empty and an overlong length is cut, never a panic. This is
    /// the same discipline the guest applies to a header the host wrote, in the
    /// other direction.
    pub fn bytes(&self, at: Word, len: Word) -> &'a [u8] {
        let start = usize::try_from(at).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX));
        self.memory
            .get(start..end)
            .or_else(|| self.memory.get(start..))
            .unwrap_or(&[])
    }
}

#[cfg(feature = "debug")]
impl<B: ByteOrder> core::fmt::Debug for Ret<'_, B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ret")
            .field("order", &B::NAME)
            .field("top", &self.top)
            .field("floor", &self.floor)
            .finish()
    }
}

#[cfg(not(feature = "debug"))]
impl<B: ByteOrder> core::fmt::Debug for Ret<'_, B> {
    fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Ok(())
    }
}

/// Seals [`VmReturn`]: the ways to leave a run are the machine's to define.
pub(crate) mod sealed {
    pub trait Return {}
}

/// A type a `#[safetynet]` function can return: it rebuilds itself from the
/// words a halted machine left and the memory they point into.
///
/// Sealed, like [`VmValue`](crate::VmValue), because what a machine can hand
/// back is fixed by the machine. A scalar takes one word; a region takes a
/// pointer and a length and reads the bytes between them; a tuple takes its
/// parts in order, which is why the reader is threaded rather than handed a
/// fixed count.
pub trait VmReturn: sealed::Return + Sized {
    /// Reads one value out of what the machine left, advancing the reader past
    /// the words it used.
    fn from_ret<B: ByteOrder>(ret: &mut Ret<'_, B>) -> Self;
}
