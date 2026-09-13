//! Sample challenge binary: an LCG keystream cipher with an authentication tag,
//! assembled by `safetynet::asm!` and run on the VM.
//!
//! Its job is to be *built in release, stripped, and inspected*: CI will assert
//! that this artifact contains no field-name strings and no assembler symbols.
//! The assembler is a compiler plugin, so the labels and cell names below exist
//! only while the crate is being compiled; what ships is the graph they built.
//!
//! The keystream byte for a position depends on the seed and the position only,
//! never on the plaintext, so running the same bytecode over its own output
//! restores the message. While it writes, the program folds a tag over the bytes
//! it produced, stirs that tag through the rest of the instruction set, checks
//! its own bookkeeping, publishes the tag to `.scratch`, and halts with a result
//! derived from it.

use std::error::Error;
use std::fmt::Write as _;

use safetynet::asm::print_listing;
use safetynet::encoding::decode;
use safetynet::image::{Image, Layout, Region, Sizes};
use safetynet::{Artifact, ByteOrder, Le, Order, Program, Vm, VmLayout, VmValue, WORD_SIZE, Word};

/// Room for the frame, plus the few words the loop keeps live.
const STACK: u32 = 256;
/// Enough for the prologue plus forty instructions per byte, with room over.
const FUEL: u64 = 10_000;

/// Where the keystream starts. The host marshals it into `.scratch` as
/// `Config::seed` and the program reads it back by field, so unlike the three
/// below it this number is not spelled in the listing.
const SEED: Word = 0x2545_f491_4f6c_dd1d;
/// The LCG's multiplier.
const MUL: Word = 0x5851_f42d_4c95_7f2d;
/// The LCG's increment.
const INC: Word = 0x1405_7b7e_f767_814f;
/// The modulus the tag is folded through: the largest prime below `2^16`.
const PRIME: Word = 65521;

/// Where the program publishes the tag: past the `Config` the host marshals in.
/// The four bytes after the tag are its low half, which the program writes and
/// reads back.
const TAG_AT: usize = Config::SIZE;
/// `.scratch`: the `Config`, then a tag and half a tag.
const SCRATCH: u32 = (Config::SIZE + 2 * WORD_SIZE) as u32;

/// Whether the program ciphers its input or leaves it alone. The host marshals
/// the choice as a discriminant; the program reads it back and switches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, VmValue)]
#[repr(u8)]
enum Mode {
    /// Apply the keystream.
    Cipher,
    /// Skip the cipher and leave `.input` untouched.
    Passthrough,
}

/// The host's typed input: the keystream seed, the message length, and the mode,
/// marshalled into the front of `.scratch`.
#[derive(Debug, Clone, Copy, VmLayout)]
struct Config {
    /// The keystream seed.
    seed: Word,
    /// How many bytes of `.input` the host wrote.
    len: u32,
    /// The [`Mode`] discriminant the program switches on.
    mode: u8,
}

/// The cipher: XOR every byte of `.input` with a keystream byte derived from
/// the position, folding a tag over what was written.
///
/// Nothing here is an address or an offset. Region bases arrive when the graph
/// is finalized against a layout; the seed and the length come from a `Config`
/// the host marshalled into `.scratch`, read back by field.
///
/// The order is baked into the bytes at expansion, so one listing serves both
/// through this wrapper rather than a generic function.
macro_rules! cipher_program {
    ($order:ident) => {
        safetynet::asm!($order {
        .frame { cursor: u64, end: u64, state: u64, tag: u64, last: u8, count: u32 }
    entry:
        $push .input
        $store cursor
        $push .input
        $push .scratch
        $field Config::len            // the message length the host marshalled
        add
        ld32
        add
        $store end
        $push .scratch                // and the seed, read back from the config
        $field Config::seed
        add
        ld64
        $store state
        push8 0
        $store tag
        push8 0
        $store count
        push8 0
        $store last                   // every cell is written before it is read
        $push .scratch                // the mode the host chose
        $field Config::mode
        add
        ld8
        $tag Mode::Passthrough
        eq
        jnz passthrough               // skip the cipher when the mode says so
    head:
        $load cursor
        $load end
        eq
        jnz done
    body:
        // A word of junk under the whole body: it shifts every frame
        // displacement below, and the symbolic assembler recomputes them all.
        push32 0x0badf00d
        $load state                   // state = state * MUL + INC
        push64 0x5851f42d4c957f2d
        mul
        push64 0x14057b7ef767814f
        add
        $store state
        $load cursor                  // the address `st8` pops last
        $load cursor
        ld8
        $load state                   // k = (state ^ (state >> 33)) & 0xff
        $load state
        push8 33
        shr
        xor
        push8 0xff
        and
        xor                          // the cipher byte
        lds64 8                      // dup it for the tag
        $store last
        st8
        $load tag                     // tag = rotl(tag, 7) ^ byte
        push8 7
        shl
        $load tag
        push8 57
        shr
        or
        $load last                    // one byte, zero-extended back to a word
        xor
        $store tag
        $load count                   // one more byte behind us
        push8 1
        add
        $store count
        $load cursor
        push8 1
        add
        $store cursor
        drop                         // and the junk goes with the iteration
        jmp head
    done:
        $load state                   // fold the state's two halves under 65521
        push32 65521
        rem
        $load state
        push32 65521
        div
        add
        $load tag
        xor
        $store tag
        $load state                   // and the same state read as signed
        push64 0xfffffffffffffffd    // -3
        sdiv
        $load state
        push64 0xfffffffffffffffb    // -5
        srem
        add
        $load tag
        xor
        $store tag
        $load state
        push8 13
        sar
        $load tag
        xor
        $store tag
        $load state                   // four orderings of state against tag,
        $load tag                     // packed into the low bits
        lt
        $load state
        $load tag
        le
        push8 1
        shl
        or
        $load state
        $load tag
        slt
        push8 2
        shl
        or
        $load state
        $load tag
        sle
        push8 3
        shl
        or
        $push .scratch                // and one the layout decides
        $push .stack
        lt
        push8 4
        shl
        or
        $load tag
        xor
        $store tag
        $load tag
        not
        $store tag
        $load end                     // the bytes the loop should have walked
        $push .input
        sub
        $load count
        eq
        jz bad
    good:
        $push .scratch                // the tag, past the config
        push8 16
        add
        $load tag
        st64
        $push .scratch                // its low half, in a slot of its own
        push8 24
        add
        $load tag
        st32
        $push .scratch
        push8 24
        add
        ld32                         // read back zero-extended, into the result
        $load tag
        add
        halt
    bad:
        push8 0                      // the counter disagreed with the cursor
        halt
    passthrough:
        push8 0                      // the mode said leave the input alone
        halt
        })
    };
}

/// The cipher assembled little-endian, the order `main` runs and reports.
fn program_le() -> Artifact<Le> {
    cipher_program!(Le)
}

/// The image this cipher runs in: an `.input` region the size of the message, a
/// few words of `.scratch` for the length and the tag, and a stack.
fn image_layout(len: usize) -> Result<Layout, Box<dyn Error>> {
    Layout::new(Sizes {
        input: u32::try_from(len)?,
        scratch: SCRATCH,
        stack: STACK,
    })
    .ok_or_else(|| "the image does not fit".into())
}

/// What one run left behind.
#[derive(Debug)]
struct Run {
    /// The bytes the program wrote over `.input`.
    output: Vec<u8>,
    /// The word on top of the stack when the program halted.
    result: Word,
    /// The tag the program published to `.scratch`.
    tag: Word,
}

/// Runs the cipher over `input` with the standard budget.
fn cipher<B: ByteOrder>(
    program: &Program<B>,
    layout: Layout,
    input: &[u8],
    mode: Mode,
) -> Result<Run, Box<dyn Error>> {
    run::<B>(program, layout, input, FUEL, mode)
}

/// Runs the cipher over `input`, reading back everything it left behind.
fn run<B: ByteOrder>(
    program: &Program<B>,
    layout: Layout,
    input: &[u8],
    fuel: u64,
    mode: Mode,
) -> Result<Run, Box<dyn Error>> {
    let mut image = Image::new(layout);
    image
        .write(Region::Input, input)
        .ok_or("the message does not fit in .input")?;
    // The host's half of the bargain: the seed and the length, marshalled into
    // the front of .scratch for the program to read back by field.
    let config = Config {
        seed: SEED,
        len: u32::try_from(input.len())?,
        mode: mode.to_word() as u8,
    };
    let mut scratch = [0u8; Config::SIZE];
    config.marshal::<B>(&mut scratch);
    image
        .write(Region::Scratch, &scratch)
        .ok_or("the config does not fit in .scratch")?;

    let mut vm = Vm::<B>::new(image).run(program, fuel)?;

    // The result is the word on top of the stack when the program halted; what
    // the cipher wrote is in the region it was pointed at, and the tag is in the
    // slot of `.scratch` the program published it to.
    let result = vm.pop()?;
    let tag = word::<B>(vm.region(Region::Scratch), TAG_AT).ok_or("the tag is not in .scratch")?;

    Ok(Run {
        output: vm.region(Region::Input).to_vec(),
        result,
        tag,
    })
}

/// The word at `at` bytes into `bytes`, in the program's byte order.
fn word<B: ByteOrder>(bytes: &[u8], at: usize) -> Option<Word> {
    let end = at.checked_add(WORD_SIZE)?;
    let slot: [u8; WORD_SIZE] = bytes.get(at..end)?.try_into().ok()?;

    Some(B::read_u64(slot))
}

/// Lowercase hex, for showing bytes that are not text any more.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The finalized bytecode read back out as a listing, the way a disassembler
/// shows it: the `alloc` prologue, the region bases resolved to addresses, and
/// the branches as offsets.
fn disassembly<B: ByteOrder>(program: &Program<B>) -> String {
    let mut code = program.code();
    let mut decoded = Vec::new();

    while let Some((instr, len)) = (!code.is_empty()).then(|| decode::<B>(code).ok()).flatten() {
        decoded.push(instr);
        code = code.get(len..).unwrap_or_default();
    }

    print_listing(&decoded)
}

fn main() -> Result<(), Box<dyn Error>> {
    let plaintext: &[u8] = b"attack at dawn";
    let layout = image_layout(plaintext.len())?;
    let program = program_le().finalize(&layout)?;

    println!(
        "safetynet demo — lcg keystream cipher, {} bytes of bytecode, byte order {}",
        program.code().len(),
        <Order as ByteOrder>::NAME
    );
    println!(
        "image       .input at {:#x}, {} bytes; .scratch at {:#x}; .stack at {:#x}",
        layout.span(Region::Input).base(),
        layout.span(Region::Input).len(),
        layout.span(Region::Scratch).base(),
        layout.span(Region::Stack).base(),
    );
    println!("keystream   seed {SEED:#018x}, mul {MUL:#018x}, inc {INC:#018x}");
    println!("tag         the state folded under {PRIME}, published to .scratch");
    println!(
        "plaintext   {}  {}",
        hex(plaintext),
        String::from_utf8_lossy(plaintext)
    );

    let encrypted = cipher::<Order>(&program, layout, plaintext, Mode::Cipher)?;
    println!(
        "ciphertext  {}  tag {:#018x}",
        hex(&encrypted.output),
        encrypted.tag
    );
    println!("result      {:#018x}", encrypted.result);

    let decrypted = cipher::<Order>(&program, layout, &encrypted.output, Mode::Cipher)?;
    println!(
        "decrypted   {}  {}",
        hex(&decrypted.output),
        String::from_utf8_lossy(&decrypted.output)
    );

    // The bytecode the macro baked, read back out as text.
    println!("\ndisassembly\n{}", disassembly::<Order>(&program));

    if decrypted.output == plaintext {
        println!("round trip ok");
        Ok(())
    } else {
        Err("the round trip did not restore the plaintext".into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use safetynet::{Be, Instr, Trap};

    use super::*;

    /// The cipher assembled big-endian, for the tests that pin the byte order.
    fn program_be() -> Artifact<Be> {
        cipher_program!(Be)
    }

    /// The config the host marshals survives a round trip in either order, so
    /// what the program reads back is what the host put in.
    fn config_round_trips<B: ByteOrder>() {
        let config = Config {
            seed: SEED,
            len: 0x1234_5678,
            mode: Mode::Passthrough.to_word() as u8,
        };
        let mut mem = [0u8; Config::SIZE];
        config.marshal::<B>(&mut mem);
        let back = Config::unmarshal::<B>(&mem);

        assert_eq!(back.seed, config.seed);
        assert_eq!(back.len, config.len);
    }

    #[test]
    fn config_round_trips_le() {
        config_round_trips::<Le>();
    }

    #[test]
    fn config_round_trips_be() {
        config_round_trips::<Be>();
    }

    /// The mode discriminant round-trips through the derive.
    #[test]
    fn mode_round_trips() {
        for mode in [Mode::Cipher, Mode::Passthrough] {
            assert_eq!(Mode::from_word(mode.to_word()), mode);
        }
    }

    /// Passthrough mode branches past the cipher on the discriminant the host
    /// marshalled, leaving `.input` exactly as it was.
    #[test]
    fn passthrough_mode_leaves_the_input_alone() {
        let plaintext: &[u8] = b"attack at dawn";
        let (program, layout) = build(&program_le(), plaintext.len());

        let run = run::<Order>(&program, layout, plaintext, FUEL, Mode::Passthrough).expect("runs");
        assert_eq!(run.output, plaintext, "the input is untouched");
        assert_eq!(run.result, 0, "and the passthrough result is zero");
    }

    /// Finalizes an artifact for a message of `len` bytes.
    fn build<B: ByteOrder>(artifact: &Artifact<B>, len: usize) -> (Program<B>, Layout) {
        let layout = image_layout(len).expect("an image");
        let program = artifact.finalize(&layout).expect("finalizes");

        (program, layout)
    }

    /// The same cipher and the same tag, computed here instead. Written against
    /// the listing's semantics rather than against its output, so a program that
    /// quietly changes what it computes fails rather than agreeing with itself.
    fn model(input: &[u8], layout: &Layout) -> Run {
        let mut state = SEED;
        let mut tag: Word = 0;
        let mut output = Vec::with_capacity(input.len());

        for byte in input {
            state = state.wrapping_mul(MUL).wrapping_add(INC);
            let key = ((state ^ (state >> 33)) & 0xff) as u8;
            let written = byte ^ key;

            output.push(written);
            tag = tag.rotate_left(7) ^ Word::from(written);
        }

        tag ^= (state % PRIME).wrapping_add(state / PRIME);
        tag ^= ((state as i64).wrapping_div(-3) as Word)
            .wrapping_add((state as i64).wrapping_rem(-5) as Word);
        tag ^= ((state as i64) >> 13) as Word;

        // The same five facts the listing packs, in the same bit positions.
        let scratch = layout.span(Region::Scratch).base();
        let stack = layout.span(Region::Stack).base();
        tag ^= Word::from(state < tag)
            | Word::from(state <= tag) << 1
            | Word::from((state as i64) < (tag as i64)) << 2
            | Word::from((state as i64) <= (tag as i64)) << 3
            | Word::from(scratch < stack) << 4;
        tag = !tag;

        Run {
            output,
            result: tag.wrapping_add(tag & 0xffff_ffff),
            tag,
        }
    }

    /// The whole stack, end to end: assemble, run, read the buffer back out.
    /// The keystream never looks at the plaintext, so the program is its own
    /// decryptor even though the tag it computes differs between the passes.
    fn the_cipher_is_its_own_inverse<B: ByteOrder>(artifact: &Artifact<B>) {
        let plaintext: &[u8] = b"attack at dawn";
        let (program, layout) = build(artifact, plaintext.len());

        let encrypted = cipher::<B>(&program, layout, plaintext, Mode::Cipher).expect("encrypts");
        assert_ne!(encrypted.output, plaintext, "the keystream did nothing");

        let decrypted =
            cipher::<B>(&program, layout, &encrypted.output, Mode::Cipher).expect("decrypts");
        assert_eq!(decrypted.output, plaintext);
    }

    #[test]
    fn the_cipher_is_its_own_inverse_le() {
        the_cipher_is_its_own_inverse(&program_le());
    }

    #[test]
    fn the_cipher_is_its_own_inverse_be() {
        the_cipher_is_its_own_inverse(&program_be());
    }

    /// The bytes, the tag and the result all have to agree with the model — the
    /// smallest form of the differential check the harness will make.
    #[test]
    fn the_result_is_the_tag_the_model_predicts() {
        let plaintext: &[u8] = b"attack at dawn";
        let (program, layout) = build(&program_le(), plaintext.len());

        let run = cipher::<Order>(&program, layout, plaintext, Mode::Cipher).expect("encrypts");
        let expected = model(plaintext, &layout);

        assert_eq!(run.output, expected.output, "the keystream");
        assert_eq!(run.tag, expected.tag, "the tag published to .scratch");
        assert_eq!(run.result, expected.result, "the result on the stack");
        assert_ne!(run.result, 0, "zero is what the failed self-check halts on");
    }

    /// A byte-wise cipher cannot see the byte order, even though every word the
    /// program pushes — and the length word the host writes — does.
    #[test]
    fn the_orders_agree_on_the_output() {
        let plaintext: &[u8] = b"attack at dawn";
        let (little, layout) = build(&program_le(), plaintext.len());
        let (big, _) = build(&program_be(), plaintext.len());

        assert_eq!(
            cipher::<Le>(&little, layout, plaintext, Mode::Cipher)
                .expect("little-endian")
                .output,
            cipher::<Be>(&big, layout, plaintext, Mode::Cipher)
                .expect("big-endian")
                .output,
        );
    }

    /// Moving a region moves the address the program pushes, with nothing in the
    /// source to keep in step.
    #[test]
    fn the_program_follows_the_layout() {
        let (short, _) = build(&program_le(), 4);
        let (long, _) = build(&program_le(), 4096);

        assert_ne!(short.code(), long.code(), "different bases, same shape");
    }

    /// Every instruction the machine can run appears in the bytecode, so a new
    /// one fails here until this program exercises it too.
    ///
    /// Three cannot appear. `free` has nowhere to be: finalization emits an
    /// `alloc` prologue and deliberately no epilogue, and a frame op written
    /// inside a block is rejected by the validator. `switch` and `host` are
    /// reserved and trap when executed, so a program that runs cannot hold one.
    #[test]
    fn the_program_uses_the_whole_instruction_set() {
        let (program, _) = build(&program_le(), b"attack at dawn".len());

        let mut seen = BTreeSet::new();
        let mut at = 0;
        while let Some(rest) = program.code().get(at..).filter(|rest| !rest.is_empty()) {
            let (instr, len) = decode::<Order>(rest).expect("decodes");
            seen.insert(instr.mnemonic());
            at += len;
        }

        let reserved = ["free", "switch", "host"];
        let expected: BTreeSet<&str> = Instr::one_of_each()
            .into_iter()
            .map(Instr::mnemonic)
            .filter(|mnemonic| !reserved.contains(mnemonic))
            .collect();

        assert_eq!(seen, expected);
    }

    /// The budget is the only thing standing between a host and a guest that
    /// never stops, so a program given too little of it has to stop anyway.
    #[test]
    fn too_little_fuel_stops_the_run() {
        let plaintext: &[u8] = b"attack at dawn";
        let (program, layout) = build(&program_le(), plaintext.len());

        let error =
            run::<Order>(&program, layout, plaintext, 8, Mode::Cipher).expect_err("runs out");

        assert_eq!(error.downcast_ref::<Trap>(), Some(&Trap::OutOfFuel));
    }
}
