//! Sample challenge binary: an XOR cipher, hand-assembled and run on the VM.
//!
//! Its job is to be *built in release, stripped, and inspected*: CI will assert
//! that this artifact contains no field-name strings and no assembler symbols.
//! Until the macros exist, the program is written out instruction by instruction
//! here, which is also the clearest look at what the machine actually is.
//!
//! The cipher runs over a buffer in the VM's memory and leaves a checksum of
//! what it wrote on the stack, where a program's result lives. XOR is its own
//! inverse, so running the same bytecode over its own output restores the
//! plaintext.

use std::error::Error;
use std::fmt::Write as _;

use safetynet::encoding::{encode, encoded_len};
use safetynet::image::{Image, Layout, Region, Sizes};
use safetynet::isa::{
    Add, Alloc, CmpEq, FrameSize, Halt, Jmp, Jnz, Ld8, Lds64, Push8, Push32, St8, Sts64, Xor,
};
use safetynet::{ByteOrder, Instr, Order, Program, Vm, Word};

/// Room for the stack: the frame, plus the few words the loop keeps live.
const STACK: u32 = 256;
/// Enough for the prologue plus nineteen instructions per byte, with room over.
const FUEL: u64 = 10_000;
/// The cipher key. One byte, applied to every byte of the buffer.
const KEY: u8 = 0x5a;

/// Frame size in bytes: two word-wide cells.
const FRAME: u16 = 16;
/// Frame offset of the cursor cell — the address the loop is working on.
const CURSOR: u16 = 0;
/// Frame offset of the checksum cell.
const SUM: u16 = 8;
/// `LDS64 8` reads the word at `SP − 8`, which is the top of the stack: this
/// machine spells `DUP` that way rather than carrying a shuffle group.
const TOP: u16 = 8;

/// Displacement of the frame cell at frame offset `c`, at a point where `d`
/// bytes of operand stack have been pushed since the prologue: `k = F − c + d`.
///
/// Every local access below goes through this. Where a local lives depends on
/// how deep the operand stack currently is, so the depth is tracked by hand here
/// — which is exactly the bookkeeping the IR will do instead.
const fn cell(c: u16, d: u16) -> u16 {
    FRAME - c + d
}

/// One line of hand-written assembly.
///
/// A branch names the *line* it jumps to. Byte offsets do not exist until every
/// line has been measured, which is the same reason the IR keeps edges symbolic.
enum Line {
    /// An instruction that is already complete.
    Op(Instr),
    /// Jump to a line unconditionally.
    Jmp(usize),
    /// Pop a word and jump to a line if it is not zero.
    Jnz(usize),
}

impl Line {
    /// The instruction this line assembles to, given a resolved branch offset.
    fn instruction(&self, offset: i32) -> Instr {
        match self {
            Self::Op(instr) => *instr,
            Self::Jmp(_) => Jmp { offset }.into(),
            Self::Jnz(_) => Jnz { offset }.into(),
        }
    }
}

/// Shorthand for a line that is just an instruction.
fn op(instr: impl Into<Instr>) -> Line {
    Line::Op(instr.into())
}

/// The cipher, as lines: XOR every byte of `.input` with [`KEY`] in place,
/// accumulating a checksum of what was written.
///
/// Takes the layout rather than a constant. The buffer's address is whatever the
/// image put it at, and the host reads it from the same place — so there is no
/// number here that someone has to remember to change twice.
fn program(layout: &Layout) -> Result<Vec<Line>, Box<dyn Error>> {
    let frame = FrameSize::new(FRAME).ok_or("the frame must be word-aligned")?;
    let buffer = layout.span(Region::Input);

    // Prologue: reserve the frame, point the cursor at the buffer, zero the sum.
    let mut lines = vec![
        op(Alloc { n: frame }),
        op(Push32 { imm: buffer.base() }),
        op(Sts64 {
            disp: cell(CURSOR, 8),
        }),
        op(Push8 { imm: 0 }),
        op(Sts64 { disp: cell(SUM, 8) }),
    ];

    // while cursor != end
    let head = lines.len();
    lines.extend([
        op(Lds64 {
            disp: cell(CURSOR, 0),
        }),
        op(Push32 { imm: buffer.end() }),
        op(CmpEq),
    ]);
    let exit = lines.len();
    lines.push(Line::Jnz(0));

    lines.extend([
        // The address `ST8` will pop last, pushed before the value it stores.
        op(Lds64 {
            disp: cell(CURSOR, 0),
        }),
        // The same address again, this time to load through.
        op(Lds64 {
            disp: cell(CURSOR, 8),
        }),
        op(Ld8),
        op(Push8 { imm: KEY }),
        op(Xor),
        // Fold the cipher byte into the checksum, keeping a copy to store.
        op(Lds64 { disp: TOP }),
        op(Lds64 {
            disp: cell(SUM, 24),
        }),
        op(Add),
        op(Sts64 {
            disp: cell(SUM, 24),
        }),
        // Write it back over the byte it came from.
        op(St8),
        // cursor += 1
        op(Lds64 {
            disp: cell(CURSOR, 0),
        }),
        op(Push8 { imm: 1 }),
        op(Add),
        op(Sts64 {
            disp: cell(CURSOR, 8),
        }),
        Line::Jmp(head),
    ]);

    // The checksum is the program's result, so it ends on top of the stack. The
    // frame is left standing: nothing runs after `HALT`, and freeing it would
    // take the result with it.
    let done = lines.len();
    lines.extend([op(Lds64 { disp: cell(SUM, 0) }), op(Halt)]);

    *lines.get_mut(exit).ok_or("the loop has no exit branch")? = Line::Jnz(done);

    Ok(lines)
}

/// Assembles lines into a program, resolving each branch against real encoded
/// sizes.
///
/// One pass is enough: an operand's width is fixed by its opcode, so a branch's
/// size never depends on the offset it ends up carrying. That is also why the
/// offsets can be measured with a placeholder in the branch.
fn assemble<B: ByteOrder>(lines: &[Line], frame: FrameSize) -> Result<Program<B>, Box<dyn Error>> {
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    let mut at = 0;
    for line in lines {
        offsets.push(at);
        at += encoded_len(line.instruction(0))?;
    }
    // One past the end, so the last line can ask where the next one would start.
    offsets.push(at);

    let offset_of = |line: usize| {
        offsets
            .get(line)
            .copied()
            .ok_or_else(|| format!("branch to line {line}, which does not exist"))
    };

    let mut code = Vec::with_capacity(at);
    for (index, line) in lines.iter().enumerate() {
        let offset = match line {
            Line::Op(_) => 0,
            // Measured from the first byte of the following instruction.
            Line::Jmp(target) | Line::Jnz(target) => {
                let next = i64::try_from(offset_of(index + 1)?)?;
                i32::try_from(i64::try_from(offset_of(*target)?)? - next)?
            }
        };

        encode::<B>(line.instruction(offset), &mut code)?;
    }

    Ok(Program::new(code, frame))
}

/// The image this cipher runs in: an `.input` region the size of the message,
/// and a stack.
fn image_layout(len: usize) -> Result<Layout, Box<dyn Error>> {
    Layout::new(Sizes {
        input: u32::try_from(len)?,
        stack: STACK,
        ..Sizes::default()
    })
    .ok_or_else(|| "the image does not fit".into())
}

/// Runs the cipher over `input`, returning what it left in `.input` and the
/// checksum it left on the stack.
fn cipher<B: ByteOrder>(
    program: &Program<B>,
    layout: Layout,
    input: &[u8],
) -> Result<(Vec<u8>, Word), Box<dyn Error>> {
    let mut image = Image::new(layout);
    image
        .write(Region::Input, input)
        .ok_or("the message does not fit in .input")?;

    let mut vm = Vm::<B>::new(image).run(program, FUEL)?;

    // The result is the word on top of the stack when the program halted; what
    // the cipher wrote is in the region it was pointed at.
    let checksum = vm.pop()?;
    Ok((vm.region(Region::Input).to_vec(), checksum))
}

/// Lowercase hex, for showing bytes that are not text any more.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let plaintext: &[u8] = b"attack at dawn";
    let layout = image_layout(plaintext.len())?;
    let lines = program(&layout)?;
    let program = assemble::<Order>(&lines, FrameSize::new(FRAME).ok_or("word-aligned")?)?;

    println!(
        "safetynet demo — xor cipher, {} bytes of bytecode, byte order {}",
        program.code().len(),
        <Order as ByteOrder>::NAME
    );
    println!(
        "image       .input at {:#x}, {} bytes; .stack at {:#x}",
        layout.span(Region::Input).base(),
        layout.span(Region::Input).len(),
        layout.span(Region::Stack).base(),
    );
    println!("key         {KEY:#04x}");
    println!(
        "plaintext   {}  {}",
        hex(plaintext),
        String::from_utf8_lossy(plaintext)
    );

    let (ciphertext, checksum) = cipher::<Order>(&program, layout, plaintext)?;
    println!("ciphertext  {}  checksum {checksum:#x}", hex(&ciphertext));

    let (decrypted, _) = cipher::<Order>(&program, layout, &ciphertext)?;
    println!(
        "decrypted   {}  {}",
        hex(&decrypted),
        String::from_utf8_lossy(&decrypted)
    );

    if decrypted == plaintext {
        println!("round trip ok");
        Ok(())
    } else {
        Err("the round trip did not restore the plaintext".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use safetynet::{Be, Le};

    /// The whole stack, end to end: assemble, run, read the buffer back out.
    /// XOR is an involution, so the program is its own decryptor.
    /// Assembles the cipher for a message of `len` bytes.
    fn build<B: ByteOrder>(len: usize) -> (Program<B>, Layout) {
        let layout = image_layout(len).expect("an image");
        let lines = program(&layout).expect("a program");
        let frame = FrameSize::new(FRAME).expect("word-aligned");

        (assemble::<B>(&lines, frame).expect("assembles"), layout)
    }

    fn the_cipher_is_its_own_inverse<B: ByteOrder>() {
        let plaintext: &[u8] = b"attack at dawn";
        let (program, layout) = build::<B>(plaintext.len());

        let (ciphertext, _) = cipher::<B>(&program, layout, plaintext).expect("encrypts");
        assert_ne!(ciphertext, plaintext, "the key did nothing");

        let (decrypted, _) = cipher::<B>(&program, layout, &ciphertext).expect("decrypts");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn the_cipher_is_its_own_inverse_le() {
        the_cipher_is_its_own_inverse::<Le>();
    }

    #[test]
    fn the_cipher_is_its_own_inverse_be() {
        the_cipher_is_its_own_inverse::<Be>();
    }

    /// The result taken off the stack has to agree with the same sum computed
    /// here — the smallest form of the differential check the harness will make.
    #[test]
    fn the_result_is_the_checksum_of_what_was_written() {
        let plaintext: &[u8] = b"attack at dawn";
        let (program, layout) = build::<Order>(plaintext.len());

        let (ciphertext, checksum) =
            cipher::<Order>(&program, layout, plaintext).expect("encrypts");
        let expected: Word = ciphertext.iter().map(|byte| Word::from(*byte)).sum();

        assert_eq!(checksum, expected);
    }

    /// A byte-wise cipher cannot see the byte order, even though every word the
    /// program pushes does.
    #[test]
    fn the_orders_agree_on_the_output() {
        let plaintext: &[u8] = b"attack at dawn";
        let (little, layout) = build::<Le>(plaintext.len());
        let (big, _) = build::<Be>(plaintext.len());

        assert_eq!(
            cipher::<Le>(&little, layout, plaintext)
                .expect("little-endian")
                .0,
            cipher::<Be>(&big, layout, plaintext).expect("big-endian").0,
        );
    }

    /// Moving a region moves the address the program pushes, with nothing in the
    /// source to keep in step.
    #[test]
    fn the_program_follows_the_layout() {
        let (short, _) = build::<Order>(4);
        let (long, _) = build::<Order>(4096);

        assert_ne!(short.code(), long.code(), "different end, same shape");
    }
}
