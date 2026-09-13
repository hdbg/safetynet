//! Tests for the assembler.
//!
//! The property under most of them is that printing and parsing are inverses:
//! a graph printed and read back is the same graph, and text parsed and printed
//! is the same text. The rest are the errors, which are as much of the language
//! as the grammar is — a message that points at the wrong token is a bug.
//!
//! Everything here works in `proc_macro2` types. The compiler's own token types
//! only exist inside a real expansion, so a unit test that touched them would
//! not link.

use proc_macro2::TokenStream;
use safetynet_core::asm::{print, print_listing};
use safetynet_core::encoding;
use safetynet_core::image::{Layout, Sizes};
use safetynet_core::ir::{BlockId, Cfg, Frame, Item, Terminator, finalize};
use safetynet_core::isa::{Add, CmpEq, Drop, Halt, Jmp, Ld8, Ld64, Lds64, Push8, St8, Switch, Xor};
use safetynet_core::{Be, ByteOrder, Instr, Le, Region, Width};

use super::{expand, parse_cfg, parse_cfg_raw};

/// Assembles a source that is expected to be a program.
fn assemble(source: &str) -> Cfg {
    let tokens: TokenStream = source.parse().expect("tokens");
    parse_cfg(tokens).expect("assembles").cfg
}

/// Assembles a source that is a listing rather than a program.
fn assemble_raw(source: &str) -> Cfg {
    let tokens: TokenStream = source.parse().expect("tokens");
    parse_cfg_raw(tokens).expect("assembles").cfg
}

/// The property the whole module exists for: a printed graph reads back as
/// itself, and printing what came back changes nothing.
#[track_caller]
fn is_a_fixed_point(cfg: &Cfg) {
    let text = print(cfg);
    let parsed = assemble(&text);

    assert_eq!(&parsed, cfg, "printed as:\n{text}");
    assert_eq!(print(&parsed), text);
}

/// The demo's cipher: a loop over three cells, with a store that pops two
/// words and a duplicate that reads back down the stack.
fn xor_loop() -> Cfg {
    let mut frame = Frame::new();
    let cursor = frame.add(Width::U64).expect("room");
    let end = frame.add(Width::U64).expect("room");
    let sum = frame.add(Width::U64).expect("room");

    let mut builder = Cfg::builder(frame);
    let entry = builder.block(0);
    let head = builder.block(0);
    let body = builder.block(0);
    let done = builder.block(0);

    builder
        .at(entry)
        .expect("open")
        .base(Region::Input)
        .store(cursor)
        .base(Region::Input)
        .base(Region::Scratch)
        .instr(Ld64)
        .instr(Add)
        .store(end)
        .instr(Push8 { imm: 0 })
        .store(sum);
    builder.seal(entry, Terminator::Jmp(head)).expect("seals");

    builder
        .at(head)
        .expect("open")
        .load(cursor)
        .load(end)
        .instr(CmpEq);
    builder
        .seal(
            head,
            Terminator::Br {
                then: done,
                els: body,
            },
        )
        .expect("seals");

    builder
        .at(body)
        .expect("open")
        .load(cursor)
        .load(cursor)
        .instr(Ld8)
        .instr(Push8 { imm: 0x5a })
        .instr(Xor)
        .instr(Lds64 { disp: 8 })
        .load(sum)
        .instr(Add)
        .store(sum)
        .instr(St8)
        .load(cursor)
        .instr(Push8 { imm: 1 })
        .instr(Add)
        .store(cursor);
    builder.seal(body, Terminator::Jmp(head)).expect("seals");

    builder.at(done).expect("open").load(sum);
    builder.seal(done, Terminator::Halt).expect("seals");

    builder.build(entry).expect("builds")
}

/// Both arms named because neither one follows, an entry that is not block
/// zero, no frame at all, and a body holding the two instructions that are also
/// ways to end a block.
fn diamond() -> Cfg {
    let mut builder = Cfg::builder(Frame::new());
    let left = builder.block(0);
    let entry = builder.block(0);
    let join = builder.block(0);
    let right = builder.block(0);

    builder
        .at(left)
        .expect("open")
        .instr(Jmp { offset: -12 })
        .instr(Halt);
    builder.seal(left, Terminator::Jmp(join)).expect("seals");

    builder.at(entry).expect("open").instr(Push8 { imm: 1 });
    builder
        .seal(
            entry,
            Terminator::Br {
                then: right,
                els: left,
            },
        )
        .expect("seals");

    builder.seal(join, Terminator::Halt).expect("seals");
    builder.seal(right, Terminator::Jmp(join)).expect("seals");

    builder.build(entry).expect("builds")
}

/// A jump table, and the one graph shape where blocks are entered with
/// something already on the stack.
fn table() -> Cfg {
    let mut builder = Cfg::builder(Frame::new());
    let entry = builder.block(0);
    let first = builder.block(8);
    let second = builder.block(8);

    builder
        .at(entry)
        .expect("open")
        .instr(Push8 { imm: 2 })
        .instr(Push8 { imm: 3 });
    builder
        .seal(
            entry,
            Terminator::Switch {
                arms: vec![first, second],
                default: second,
            },
        )
        .expect("seals");

    builder.at(first).expect("open").instr(Drop);
    builder.seal(first, Terminator::Halt).expect("seals");

    builder.at(second).expect("open").instr(Drop);
    builder.seal(second, Terminator::Halt).expect("seals");

    builder.build(entry).expect("builds")
}

#[test]
fn a_loop_survives_being_printed() {
    is_a_fixed_point(&xor_loop());
}

#[test]
fn a_diamond_survives_being_printed() {
    is_a_fixed_point(&diamond());
}

#[test]
fn a_table_survives_being_printed() {
    is_a_fixed_point(&table());
}

/// A loop as someone would write it rather than as the printer spells it:
/// keyword labels, a hex literal, named cells, a block that falls through, and
/// a conditional that names only the arm it jumps to.
const LOOP: &str = concat!(
    ".frame { cursor: u64, limit: u64 }\n",
    "head:\n",
    "    push .input\n",
    "    store cursor\n",
    "    push .scratch\n",
    "    store limit\n",
    "loop:\n",
    "    load cursor\n",
    "    load limit\n",
    "    lt\n",
    "    jz done\n",
    "body:\n",
    "    load cursor\n",
    "    push8 0x10\n",
    "    add\n",
    "    store cursor\n",
    "    jmp loop\n",
    "done:\n",
    "    load cursor\n",
    "    halt\n",
);

/// What a hand-written source canonicalizes to: names become positions, the
/// hex literal becomes decimal, and the fallthrough becomes the jump it always
/// was. Printing that again changes nothing, which is the fixed point from the
/// text's side.
#[test]
fn a_hand_written_source_canonicalizes_and_then_holds_still() {
    let cfg = assemble(LOOP);
    let text = print(&cfg);

    assert_eq!(
        text,
        concat!(
            ".frame { c0: u64, c1: u64 }\n",
            "b0:\n",
            "    push .input\n",
            "    store c0\n",
            "    push .scratch\n",
            "    store c1\n",
            "    jmp b1\n",
            "b1:\n",
            "    load c0\n",
            "    load c1\n",
            "    lt\n",
            "    jz b3\n",
            "b2:\n",
            "    load c0\n",
            "    push8 16\n",
            "    add\n",
            "    store c0\n",
            "    jmp b1\n",
            "b3:\n",
            "    load c0\n",
            "    halt\n",
        )
    );

    assert_eq!(print(&assemble(&text)), text);
}

/// `jz` names the zero arm and falls into the other, and the graph has to say
/// the same thing: the non-zero arm is the block that follows.
#[test]
fn a_conditional_falls_through_on_the_arm_it_does_not_name() {
    let cfg = assemble("b0:\n    push8 1\n    jz b2\nb1:\n    halt\nb2:\n    halt\n");
    let block = cfg.blocks().first().expect("a block");

    assert_eq!(
        block.term(),
        &Terminator::Br {
            then: BlockId::from_index(1),
            els: BlockId::from_index(2),
        }
    );
}

/// And `jnz` is the same statement with the arms the other way round.
#[test]
fn the_other_conditional_is_its_mirror() {
    let cfg = assemble("b0:\n    push8 1\n    jnz b2\nb1:\n    halt\nb2:\n    halt\n");
    let block = cfg.blocks().first().expect("a block");

    assert_eq!(
        block.term(),
        &Terminator::Br {
            then: BlockId::from_index(2),
            els: BlockId::from_index(1),
        }
    );
}

/// A table with no arms is still a table, and still reads back.
#[test]
fn an_empty_table_reads_back() {
    let cfg = assemble("b0:\n    push8 0\n    switch [] default b1\nb1:\n    halt\n");
    is_a_fixed_point(&cfg);
}

/// The reserved instructions stay spellable as instructions: a `switch` with no
/// table after it, and a `halt` with more block after it.
#[test]
fn the_ambiguous_mnemonics_read_as_instructions_in_the_middle_of_a_block() {
    let cfg = assemble_raw("b0:\n    switch\n    halt\n    drop\n    halt\n");
    let block = cfg.blocks().first().expect("a block");

    assert_eq!(
        block.code(),
        [
            Item::Instr(Switch.into()),
            Item::Instr(Halt.into()),
            Item::Instr(Drop.into()),
        ]
    );
    assert_eq!(block.term(), &Terminator::Halt);
}

/// Totality: every instruction the machine has prints as a line the assembler
/// reads back as the same instruction. A new one that nothing here can spell
/// fails this rather than a build somewhere else.
#[test]
fn every_instruction_reads_back_from_its_own_listing() {
    let code = Instr::one_of_each();
    assert_eq!(code.len(), 43, "the instruction set changed");

    let listing = print_listing(&code);
    let cfg = assemble_raw(&format!("b0:\n{listing}halt\n"));
    let block = cfg.blocks().first().expect("a block");

    let read_back: Vec<Instr> = block
        .code()
        .iter()
        .map(|item| match item {
            Item::Instr(instr) => *instr,
            other => panic!("{other:?} is not an instruction"),
        })
        .collect();

    assert_eq!(read_back, code);
}

/// The whole invocation expands to one expression: a byte array wrapped in an
/// `Artifact`, and no panic in sight.
#[test]
fn an_invocation_expands_to_one_expression() {
    let tokens = expand("Le { b0: push8 1 drop halt }".parse().expect("tokens")).expect("expands");
    let text = tokens.to_string();

    syn::parse2::<syn::Expr>(tokens.clone()).expect("one expression");
    assert!(text.contains("Artifact"), "{text}");
    assert!(text.contains("CODE"), "{text}");
    assert!(!text.contains("unwrap ("), "{text}");
    assert!(!text.contains("expect ("), "{text}");
}

/// A region base is the one address the bytes cannot carry, so it leaves a
/// relocation behind in the expansion.
#[test]
fn a_region_base_becomes_a_relocation() {
    let tokens =
        expand("Le { b0: push .input drop halt }".parse().expect("tokens")).expect("expands");
    assert!(tokens.to_string().contains("region_base"), "{tokens}");
}

/// The bytes are baked at expansion, so the order cannot be a generic parameter:
/// there is no one encoding for both.
#[test]
fn a_generic_byte_order_is_refused() {
    let error = expand("B { b0: halt }".parse().expect("tokens")).expect_err("refused");
    assert!(error.to_string().contains("concrete byte order"), "{error}");
}

/// Every instruction that made it into the bytecode: decode the program, print
/// the listing, and read it back.
fn listing_of<B: ByteOrder>(cfg: &Cfg) -> String {
    let layout = Layout::new(Sizes {
        input: 32,
        scratch: 8,
        stack: 1024,
    })
    .expect("fits");

    let program = finalize::<B>(cfg, &layout).expect("finalizes");
    let mut code = program.code();
    let mut decoded = Vec::new();

    while !code.is_empty() {
        let (instr, len) = encoding::decode::<B>(code).expect("decodes");
        decoded.push(instr);
        code = code.get(len..).unwrap_or_default();
    }

    print_listing(&decoded)
}

/// The round trip in full: source, graph, bytecode, listing, graph, text. The
/// listing is what a disassembler shows, and it has to be text this assembler
/// reads — with the prologue in it, which no block is allowed to say, so it is
/// read back unchecked.
fn a_program_survives_the_whole_round_trip<B: ByteOrder>() {
    let listing = listing_of::<B>(&assemble(LOOP));
    let cfg = assemble_raw(&format!("b0:\n{listing}halt\n"));

    let text = print(&cfg);
    assert_eq!(assemble_raw(&text), cfg);
    assert_eq!(print(&assemble_raw(&text)), text);

    // The prologue the graph never spelled is in the listing, which is the
    // reason this half is read back unchecked.
    assert!(text.contains("alloc 16"), "{text}");
}

#[test]
fn a_program_survives_the_whole_round_trip_le() {
    a_program_survives_the_whole_round_trip::<Le>();
}

#[test]
fn a_program_survives_the_whole_round_trip_be() {
    a_program_survives_the_whole_round_trip::<Be>();
}

/// Byte order decides the bytes, not the program: the two listings are the same
/// text.
#[test]
fn the_two_orders_disassemble_to_the_same_listing() {
    let cfg = assemble(LOOP);
    assert_eq!(listing_of::<Le>(&cfg), listing_of::<Be>(&cfg));
}

/// The message, and where it points: line counted from one, column from zero,
/// as a span reports them.
#[track_caller]
fn assert_rejects(source: &str, at: (usize, usize), message: &str) {
    let tokens: TokenStream = source.parse().expect("tokens");
    let error = parse_cfg(tokens).expect_err("rejected");
    let start = error.span().start();

    assert!(
        error.to_string().contains(message),
        "expected {message:?}, got {error:?}"
    );
    assert_eq!((start.line, start.column), at, "{error}");
}

/// Line and column of the first occurrence of `needle`.
fn at(source: &str, needle: &str) -> (usize, usize) {
    position(source, source.find(needle).expect("in the source"))
}

/// Line and column of the last occurrence of `needle`.
fn at_last(source: &str, needle: &str) -> (usize, usize) {
    position(source, source.rfind(needle).expect("in the source"))
}

fn position(source: &str, offset: usize) -> (usize, usize) {
    let before = source.get(..offset).unwrap_or_default();
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count();

    (line, column)
}

#[test]
fn a_jump_to_nowhere_names_the_label() {
    let source = "b0:\n    jmp nowhere\n";
    assert_rejects(source, at(source, "nowhere"), "unknown label `nowhere`");
}

#[test]
fn a_second_block_of_the_same_name_is_the_one_reported() {
    let source = "entry:\n    halt\nentry:\n    halt\n";
    assert_rejects(source, at_last(source, "entry"), "already used");
}

#[test]
fn an_access_to_a_cell_that_was_never_declared_names_it() {
    let source = "b0:\n    load ghost\n    halt\n";
    assert_rejects(source, at(source, "ghost"), "unknown cell `ghost`");
}

#[test]
fn a_second_cell_of_the_same_name_is_the_one_reported() {
    let source = ".frame { a: u8, a: u32 }\nb0:\n    halt\n";
    assert_rejects(source, at(source, "a: u32"), "already has a cell named `a`");
}

#[test]
fn a_block_ends_where_its_terminator_is() {
    let source = "b0:\n    jmp b1\n    add\nb1:\n    halt\n";
    assert_rejects(source, at(source, "add"), "after the block's terminator");
}

#[test]
fn the_last_block_has_to_end() {
    let source = "b0:\n    drop\n";
    assert_rejects(source, at(source, "b0"), "never ends");
}

#[test]
fn a_conditional_in_the_last_block_has_nothing_to_fall_through_to() {
    let source = "b0:\n    push8 1\n    jz b0\n";
    assert_rejects(source, at(source, "jz"), "nothing to fall through to");
}

/// Two paths into one block that leave different depths: the jump is what
/// disagrees, so the jump is what the error points at — under the label the
/// program gave it.
#[test]
fn a_depth_disagreement_names_the_jump_that_causes_it() {
    let source = "b0:\n    push8 1\n    jz b2\nb1:\n    push8 7\n    jmp b2\nb2:\n    halt\n";
    assert_rejects(source, at(source, "jmp b2"), "block `b1` leaves depth 8");
}

#[test]
fn popping_with_nothing_on_the_stack_names_the_item_that_pops() {
    let source = "b0:\n    add\n    halt\n";
    assert_rejects(source, at(source, "add"), "pops past the frame");
}

#[test]
fn a_block_may_not_reserve_a_frame_of_its_own() {
    let source = "b0:\n    alloc 8\n    halt\n";
    assert_rejects(source, at(source, "alloc"), "reserves or releases a frame");
}

#[test]
fn a_frame_size_has_to_be_a_whole_number_of_words() {
    let source = "b0:\n    alloc 7\n    halt\n";
    assert_rejects(source, at(source, "7"), "multiple of 8");
}

#[test]
fn a_displacement_wider_than_its_operand_is_refused() {
    let source = "b0:\n    lds64 70000\n    halt\n";
    assert_rejects(source, at(source, "70000"), "too large");
}

#[test]
fn a_constant_wider_than_its_operand_is_refused() {
    let source = "b0:\n    push8 300\n    halt\n";
    assert_rejects(source, at(source, "300"), "too large");
}

/// A suffix looks like it says something about the operand's width. It does
/// not, and a number that seems to disagree with its instruction is worth
/// stopping for.
#[test]
fn a_suffixed_literal_is_refused_rather_than_ignored() {
    let source = "b0:\n    push8 1u8\n    halt\n";
    assert_rejects(source, at(source, "1u8"), "drop the `u8` suffix");
}

#[test]
fn a_bare_push_says_what_it_is_missing() {
    let source = "b0:\n    push 1\n    halt\n";
    assert_rejects(source, at(source, "push"), "push8");
}

#[test]
fn an_unknown_mnemonic_is_named() {
    let source = "b0:\n    frobnicate\n    halt\n";
    assert_rejects(source, at(source, "frobnicate"), "unknown instruction");
}

#[test]
fn a_block_no_edge_reaches_is_reported_at_its_label() {
    let source = "b0:\n    halt\nb1:\n    halt\n";
    assert_rejects(source, at(source, "b1"), "`b1` is unreachable");
}

#[test]
fn an_entry_that_names_nothing_is_reported_at_the_name() {
    let source = ".entry ghost\nb0:\n    halt\n";
    assert_rejects(source, at(source, "ghost"), "unknown label `ghost`");
}

#[test]
fn an_unknown_region_lists_the_ones_there_are() {
    let source = "b0:\n    push .heap\n    halt\n";
    assert_rejects(source, at(source, "heap"), "unknown region `.heap`");
}

#[test]
fn an_unknown_directive_lists_the_ones_there_are() {
    let source = ".rodata { }\nb0:\n    halt\n";
    assert_rejects(source, at(source, "rodata"), "unknown directive `.rodata`");
}

#[test]
fn a_program_without_a_label_says_so() {
    let source = "push8 1\n    halt\n";
    assert_rejects(source, at(source, "push8"), "expected a block label");
}

#[test]
fn a_program_needs_a_block() {
    let error = parse_cfg(TokenStream::new()).expect_err("rejected");
    assert!(error.to_string().contains("at least one block"), "{error}");
}

/// A negative displacement is a number the machine has and the language keeps:
/// it is how a decoded jump reads back.
#[test]
fn a_raw_jump_keeps_its_sign() {
    let cfg = assemble_raw("b0:\n    jmp -2147483648\n    halt\n");
    let block = cfg.blocks().first().expect("a block");

    assert_eq!(block.code(), [Item::Instr(Jmp { offset: i32::MIN }.into())]);
}
