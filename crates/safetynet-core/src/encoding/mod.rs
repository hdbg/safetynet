//! The wire format: how an [`Instr`] becomes bytes.
//!
//! # Shape
//!
//! One instruction is one tag byte naming the operation, followed by its
//! operands at fixed width in the build's byte order. A program is those
//! encodings **packed end to end** — nothing frames them, counts them or
//! prefixes them with a length. Two properties follow, and both are why the
//! format is worth stating this plainly:
//!
//! - a byte offset into the stream is a program point, which is what makes a
//!   relative branch mean something;
//! - an instruction's size is a property of the instruction alone, so block
//!   layout can measure code before any offset exists.
//!
//! Operands are fixed-width, never variable-length. A varint would make an
//! instruction's size depend on its operand's *value*, so a pass that moves code
//! would have to re-measure after every edit and branch offsets would not settle
//! in one pass.
//!
//! # Byte order
//!
//! The type parameter `B`. [`ByteOrder::BIG_ENDIAN`] is a constant, so the
//! branch each function makes on it folds away when it is monomorphized: the
//! shipped binary holds one order's codec, with no runtime test.

use core::marker::PhantomData;

use musli::options::{self, ByteOrder as MusliOrder, Integer, Options};
use musli::storage::{Encoding, Error as WireError};
use musli::{Context, Writer};

use crate::isa::Push32;
use crate::{ByteOrder, Instr};

#[cfg(test)]
mod tests;

/// The two settings that turn musli's storage format into an instruction
/// encoding: integers at their natural width, in the order this build uses.
const fn options(big_endian: bool) -> Options {
    let order = if big_endian {
        MusliOrder::Big
    } else {
        MusliOrder::Little
    };

    options::new()
        .integer(Integer::Fixed)
        .byte_order(order)
        .build()
}

const LE_OPTIONS: Options = options(false);
const BE_OPTIONS: Options = options(true);

const LE_WIRE: Encoding<LE_OPTIONS> = Encoding::new().with_options();
const BE_WIRE: Encoding<BE_OPTIONS> = Encoding::new().with_options();

/// Binds `$wire` to the codec for byte order `$order`, then runs `$body`.
///
/// The two encodings differ in a const parameter, so they are two *types*: the
/// choice cannot be a value handed to a helper, it has to be made where the call
/// is written. `BIG_ENDIAN` is constant per instantiation, so one arm is dead
/// code in any given monomorphization and never reaches the binary.
macro_rules! with_wire {
    ($order:ty, |$wire:ident| $body:expr) => {
        if <$order as ByteOrder>::BIG_ENDIAN {
            let $wire = BE_WIRE;
            $body
        } else {
            let $wire = LE_WIRE;
            $body
        }
    };
}

/// Appends `instr` to `out` and reports how many bytes it took.
///
/// # Examples
///
/// ```
/// use safetynet_core::Le;
/// use safetynet_core::encoding;
/// use safetynet_core::isa::Push8;
///
/// let mut code = Vec::new();
/// let written = encoding::encode::<Le>(Push8 { imm: 0xa5 }.into(), &mut code)?;
///
/// assert_eq!(written, 2);
/// assert_eq!(code.last(), Some(&0xa5));
/// # Ok::<_, encoding::EncodeError>(())
/// ```
pub fn encode<B: ByteOrder>(instr: Instr, out: &mut Vec<u8>) -> Result<usize, EncodeError> {
    let before = out.len();
    with_wire!(B, |wire| wire.encode(&mut *out, &instr))?;
    Ok(out.len() - before)
}

/// Appends instructions to `out`, packed end to end, and reports the length of
/// what was written.
pub fn encode_all<B: ByteOrder>(
    program: &[Instr],
    out: &mut Vec<u8>,
) -> Result<usize, EncodeError> {
    let before = out.len();
    for instr in program {
        encode::<B>(*instr, out)?;
    }
    Ok(out.len() - before)
}

/// The encoded length of `instr`, in bytes.
///
/// Independent of byte order: each operand's width is fixed by its opcode, so
/// only the opcode decides the size. This is what block layout measures with
/// before any offset is known.
///
/// Counted by encoding into a sink that keeps nothing but the count, rather than
/// from a table of per-opcode widths. A table would be a second description of
/// the format and the first thing to go stale when an operand changes width.
pub fn encoded_len(instr: Instr) -> Result<usize, EncodeError> {
    let mut counter = Counter(0);
    Ok(LE_WIRE.encode(&mut counter, &instr)?)
}

/// Where an instruction's fixed-width immediate lands in its encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Immediate {
    /// Byte offset of the immediate within the encoded instruction.
    pub at: usize,
    /// Its width in bytes.
    pub width: usize,
    /// Whether the most significant byte comes first.
    pub big_endian: bool,
}

/// Probes `encoder` for where a `push32`'s immediate sits and how it is ordered.
///
/// Derived rather than assumed, so a linker can patch the immediate without
/// baking in the wire format. `None` if the encoder does not lay a `push32` out
/// as a contiguous fixed-width immediate.
pub fn push32_immediate<E: Encoder>(encoder: &E) -> Option<Immediate> {
    let probe = |imm: u32| {
        let mut out = Vec::new();
        encoder.encode(Push32 { imm }.into(), &mut out).ok()?;
        Some(out)
    };

    let base = probe(0)?;
    let full = probe(u32::MAX)?;
    let one = probe(1)?;
    if base.len() != full.len() || base.len() != one.len() {
        return None;
    }

    // The immediate is where flipping every bit changed a byte; its low byte is
    // where a value of one landed.
    let mut at = None;
    let mut width = 0;
    let mut low = None;
    for i in 0..base.len() {
        if base.get(i) != full.get(i) {
            at.get_or_insert(i);
            width += 1;
        }
        if base.get(i) != one.get(i) {
            if low.is_some() {
                return None;
            }
            low = Some(i);
        }
    }

    let (at, low) = (at?, low?);
    if width == 0 || low < at || low >= at + width {
        return None;
    }
    Some(Immediate {
        at,
        width,
        big_endian: low == at + width - 1,
    })
}

/// Decodes the instruction at the start of `code`, returning it and the number
/// of bytes it occupied.
///
/// Stops at the end of the instruction rather than requiring the slice to be
/// consumed, which is what lets a fetch loop hand it everything from the program
/// counter onward and learn where the next instruction begins.
///
/// # Examples
///
/// ```
/// use safetynet_core::Le;
/// use safetynet_core::encoding;
/// use safetynet_core::isa::{Halt, Push8};
///
/// let mut code = Vec::new();
/// encoding::encode_all::<Le>(&[Push8 { imm: 7 }.into(), Halt.into()], &mut code)?;
///
/// let (instr, len) = encoding::decode::<Le>(&code)?;
/// assert_eq!(instr, Push8 { imm: 7 }.into());
/// assert_eq!(len, 2);
/// assert_eq!(encoding::decode::<Le>(&code[len..])?.0, Halt.into());
/// # Ok::<_, Box<dyn std::error::Error>>(())
/// ```
pub fn decode<B: ByteOrder>(code: &[u8]) -> Result<(Instr, usize), DecodeError> {
    let mut cursor = code;
    let instr = read::<B>(&mut cursor)?;

    Ok((instr, code.len() - cursor.len()))
}

/// Reads one instruction, advancing `cursor` past it.
///
/// The reader *is* the `&mut &[u8]`, and how far it advances is how the caller
/// learns the instruction's length
fn read<B: ByteOrder>(cursor: &mut &[u8]) -> Result<Instr, WireError> {
    with_wire!(B, |wire| wire.decode(cursor))
}

/// How instructions become bytes.
///
/// Block layout is written against this rather than against the packed format
/// below, so that a later encoding — renumbered, reordered, padded — drops in
/// without layout knowing. Two rules an implementation has to keep, because
/// layout resolves offsets in a single pass:
///
/// - [`encoded_len`](Encoder::encoded_len) is exactly what
///   [`encode`](Encoder::encode) writes;
/// - a length may depend on the opcode but **not** on a branch offset's value,
///   or the layout would shift under its own resolution.
pub trait Encoder {
    /// The order multi-byte operands are laid out in.
    ///
    /// Carried by the encoder rather than chosen separately, so that a program
    /// cannot be laid out in one order and encoded in another.
    type Order: ByteOrder;

    /// Why an instruction could not be written.
    type Error: core::error::Error + Send + Sync + 'static;

    /// Appends `instr` to `out`, returning how many bytes it took.
    fn encode(&self, instr: Instr, out: &mut Vec<u8>) -> Result<usize, Self::Error>;

    /// How many bytes `instr` will take.
    fn encoded_len(&self, instr: Instr) -> Result<usize, Self::Error>;
}

/// The standard encoding: a tag byte, then fixed-width operands, packed end to
/// end.
pub struct Packed<B: ByteOrder>(PhantomData<B>);

impl<B: ByteOrder> Packed<B> {
    /// The encoder for byte order `B`.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<B: ByteOrder> Clone for Packed<B> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<B: ByteOrder> Default for Packed<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: ByteOrder> core::fmt::Debug for Packed<B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Packed").field(&B::NAME).finish()
    }
}

impl<B: ByteOrder> Encoder for Packed<B> {
    type Order = B;
    type Error = EncodeError;

    fn encode(&self, instr: Instr, out: &mut Vec<u8>) -> Result<usize, Self::Error> {
        encode::<B>(instr, out)
    }

    fn encoded_len(&self, instr: Instr) -> Result<usize, Self::Error> {
        encoded_len(instr)
    }
}

impl<B: ByteOrder> Decoder for Packed<B> {
    type Order = B;
    type Error = DecodeError;

    fn decode(&self, code: &[u8]) -> Result<(Instr, usize), Self::Error> {
        decode::<B>(code)
    }
}

/// How bytes become instructions again.
///
/// The other half of an [`Encoder`], and the reason they are separate traits
/// rather than one: a disassembler needs only this side, and a machine only ever
/// needs this side. What pairs them is a shared implementation — [`Packed`] is
/// both — so the standard format cannot drift apart from itself.
pub trait Decoder {
    /// The order multi-byte operands are laid out in. Has to match the machine
    /// running the code, which the bound on the machine's side enforces.
    type Order: ByteOrder;

    /// Why bytes did not begin an instruction.
    type Error: core::error::Error + Send + Sync + 'static;

    /// Decodes the instruction at the start of `code`, returning it and how many
    /// bytes it occupied.
    ///
    /// Must stop at the end of the instruction rather than consuming `code`, and
    /// must report the length a fetch loop should advance by — padding and
    /// framing included, whether or not they carried anything.
    fn decode(&self, code: &[u8]) -> Result<(Instr, usize), Self::Error>;
}

/// An instruction could not be turned into bytes.
#[derive(Debug, thiserror::Error)]
#[error("could not encode an instruction: {0}")]
pub struct EncodeError(#[from] WireError);

/// Bytes that do not begin an instruction.
#[derive(Debug, thiserror::Error)]
#[error("not a valid instruction: {0}")]
pub struct DecodeError(#[from] WireError);

/// A [`Writer`] that discards the bytes and keeps the count.
struct Counter(usize);

impl Writer for Counter {
    type Ok = usize;
    type Mut<'this> = &'this mut Self;

    fn finish<C>(&mut self, _cx: C) -> Result<Self::Ok, C::Error>
    where
        C: Context,
    {
        Ok(self.0)
    }

    fn borrow_mut(&mut self) -> Self::Mut<'_> {
        self
    }

    fn extend<C>(
        &mut self,
        _cx: C,
        buffer: musli::alloc::Vec<u8, C::Allocator>,
    ) -> Result<(), C::Error>
    where
        C: Context,
    {
        self.0 += buffer.len();
        Ok(())
    }

    fn write_bytes<C>(&mut self, _cx: C, bytes: &[u8]) -> Result<(), C::Error>
    where
        C: Context,
    {
        self.0 += bytes.len();
        Ok(())
    }
}
