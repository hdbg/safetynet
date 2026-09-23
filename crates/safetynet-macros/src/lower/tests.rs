//! A function lowered to a graph, checked against the graph's `Debug` listing
//! and the validator: a lowering the validator refuses is a failing test here,
//! not a discovery at expansion time.

use safetynet_core::ir::{Cfg, resolve};

use super::build::lower;

/// Lowers a function's source to its graph.
fn graph(source: &str) -> Cfg {
    let func = syn::parse_str(source).expect("parses as a fn");
    lower(&func).expect("lowers").cfg
}

/// The reason it lowers refused the source.
fn refusal(source: &str) -> String {
    let func = syn::parse_str(source).expect("parses as a fn");
    lower(&func).expect_err("is refused").to_string()
}

/// The graph is one the validator accepts.
fn assert_resolves(cfg: &Cfg) {
    resolve(cfg).unwrap_or_else(|error| panic!("does not resolve: {error}\n{cfg:?}"));
}

#[test]
fn a_constant_is_pushed_and_halted() {
    let cfg = graph("fn answer() -> u32 { 42 }");
    assert_eq!(format!("{cfg:?}"), "b0:\n    push8 42\n    halt\n");
    assert_resolves(&cfg);
}

#[test]
fn a_parameter_is_read_from_the_input() {
    let cfg = graph("fn inc(x: u32) -> u32 { x + 1 }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .input\n    ld32\n    push8 1\n    add\n    push32 4294967295\n    and\n    halt\n"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_second_parameter_is_read_at_its_offset() {
    let cfg = graph("fn lt(a: u32, b: u32) -> bool { a < b }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .input\n    ld32\n    $push .input\n    push8 4\n    add\n    ld32\n    lt\n    halt\n"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_local_lives_in_a_frame_cell() {
    let cfg = graph("fn add2(a: u32, b: u32) -> u32 { let s: u32 = a + b; s }");
    assert_eq!(
        format!("{cfg:?}"),
        ".frame { c0: u32 }\nb0:\n    $push .input\n    ld32\n    $push .input\n    push8 4\n    add\n    ld32\n    add\n    push32 4294967295\n    and\n    $store c0\n    $load c0\n    halt\n"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_wider_parameter_reads_at_its_width() {
    let cfg = graph("fn id(x: u64) -> u64 { x }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .input\n    ld64\n    halt\n"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_non_scalar_return_is_refused() {
    assert!(refusal("fn f() -> String { todo!() }").contains("scalar"));
}

#[test]
fn a_let_without_a_type_is_refused() {
    assert!(refusal("fn f(x: u32) -> u32 { let y = x; y }").contains("type"));
}

#[test]
fn an_unsupported_expression_is_refused() {
    assert!(refusal("fn f(x: u32) -> u32 { g(x) }").contains("supported"));
}

#[test]
fn a_conditional_resolves() {
    assert_resolves(&graph(
        "fn max(a: u32, b: u32) -> u32 { if a < b { b } else { a } }",
    ));
}

#[test]
fn an_early_return_resolves() {
    assert_resolves(&graph("fn f(x: u32) -> u32 { if x < 10 { return 0; } x }"));
}

#[test]
fn an_else_if_chain_resolves() {
    assert_resolves(&graph(
        "fn sign(x: i64) -> i64 { if x < 0 { -1 } else if x > 0 { 1 } else { 0 } }",
    ));
}

#[test]
fn a_let_inside_a_branch_resolves() {
    assert_resolves(&graph(
        "fn f(c: bool) -> u32 { if c { let y: u32 = 1; y } else { 0 } }",
    ));
}

#[test]
fn a_while_loop_resolves() {
    assert_resolves(&graph(
        "fn f(n: u32) -> u32 { let mut s: u32 = 0; let mut i: u32 = 0; while i < n { s = s + i; i = i + 1; } s }",
    ));
}

#[test]
fn a_loop_with_break_resolves() {
    assert_resolves(&graph(
        "fn f(n: u32) -> u32 { let mut i: u32 = 0; loop { if i >= n { break; } i += 1; } i }",
    ));
}

#[test]
fn a_break_outside_a_loop_is_refused() {
    assert!(refusal("fn f() -> u32 { break; }").contains("loop"));
}

#[test]
fn assigning_a_parameter_is_refused() {
    assert!(refusal("fn f(x: u32) -> u32 { x = 1; x }").contains("parameter"));
}

#[test]
fn a_short_circuit_and_resolves() {
    assert_resolves(&graph("fn f(a: bool, b: bool) -> bool { a && b }"));
}

#[test]
fn a_short_circuit_or_resolves() {
    assert_resolves(&graph("fn f(x: u32) -> bool { x > 0 || x < 5 }"));
}

#[test]
fn a_range_check_resolves() {
    assert_resolves(&graph("fn f(x: u32, hi: u32) -> bool { 0 < x && x < hi }"));
}

#[test]
fn a_for_loop_resolves() {
    assert_resolves(&graph(
        "fn f(n: u32) -> u32 { let mut s: u32 = 0; for i in 0..n { s += i; } s }",
    ));
}

#[test]
fn an_inclusive_range_is_refused() {
    assert!(
        refusal("fn f(n: u32) -> u32 { let mut s: u32 = 0; for i in 0..=n { s += i; } s }")
            .contains("range")
    );
}

#[test]
fn a_typed_field_read_lowers_without_a_cell() {
    let cfg = graph("fn f(p: Packet) -> u32 { p.seq.typed::<u32>() }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .input\n    $field #0\n    add\n    $loadfield #0\n    halt\n"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_typed_field_may_go_bare_afterwards() {
    assert_resolves(&graph(
        "fn f(p: Packet) -> u32 { if p.seq.typed::<u32>() > 100 { return 100; } p.seq }",
    ));
}

#[test]
fn an_untyped_field_read_is_refused_with_the_fix_spelled_out() {
    let message = refusal("fn f(p: Packet) -> u32 { p.seq }");
    assert!(message.contains("cannot be resolved"));
    assert!(message.contains("help: name it at the field's first use: `p.seq.typed::<u32>()`"));
    assert!(refusal("fn f(p: Packet) -> u32 { p.seq + 1 }").contains(".typed"));
}

#[test]
fn a_conflicting_field_type_is_refused() {
    assert!(
        refusal("fn f(p: Packet) -> u64 { p.seq.typed::<u32>() + p.seq.typed::<u64>() }")
            .contains("already typed as `u32`")
    );
}

#[test]
fn a_missing_turbofish_is_refused() {
    assert!(refusal("fn f(p: Packet) -> u32 { p.seq.typed() }").contains("needs the type"));
}

#[test]
fn a_labeled_break_leaves_the_named_loop() {
    assert_resolves(&graph(
        "fn f(n: u64) -> u64 { let mut c: u64 = 0; 'outer: loop { loop { c += 1; if c % 2 == 0 { break; } if c >= n { break 'outer; } } } c }",
    ));
}

#[test]
fn a_labeled_continue_reaches_the_named_loop() {
    assert_resolves(&graph(
        "fn f(n: u32) -> u32 { let mut i: u32 = 0; 'outer: while i < n { i += 1; loop { continue 'outer; } } i }",
    ));
}

#[test]
fn an_unknown_label_is_refused() {
    assert!(refusal("fn f() -> u32 { 'a: loop { break 'b; } }").contains("no enclosing loop"),);
}

#[test]
fn a_suffixed_range_keeps_its_width() {
    let cfg = graph(
        "fn f() -> u64 { let mut c: u64 = 0; for _ in 4294967296u64..4294967299u64 { c += 1; } c }",
    );
    // Both bounds live in word cells, so nothing truncates.
    assert!(format!("{cfg:?}").starts_with(".frame { c0: u64, c1: u64, c2: u64 }\n"));
    assert_resolves(&cfg);
}

#[test]
fn an_unsupported_range_suffix_is_refused() {
    assert!(
        refusal("fn f() -> u32 { let mut c: u32 = 0; for _ in 0u16..9u16 { c += 1; } c }")
            .contains("scalar the machine can hold")
    );
}

#[test]
fn an_unsupported_primitive_parameter_is_refused() {
    assert!(refusal("fn f(x: u16) -> u32 { 7 }").contains("machine can hold"));
    assert!(refusal("fn f(x: f64) -> u32 { 7 }").contains("machine can hold"));
}

#[test]
fn a_byte_region_is_walked_from_its_header() {
    let cfg =
        graph("fn f(p: Msg) -> u8 { let mut x: u8 = 0; for b in p.body.iter() { x ^= *b; } x }");
    assert_eq!(
        format!("{cfg:?}"),
        "\
.frame { c0: u8, c1: u64, c2: u32, c3: u32, c4: u8 }
b0:
    push8 0
    $store c0
    $push .input
    $field #0
    add
    $loadfield #0
    $push .input
    add
    $store c1
    $push .input
    $field #1
    add
    $loadfield #1
    $store c2
    $load c1
    $load c2
    add
    $push .input
    $len .input
    add
    le
    jnz b2
b1:
    push8 0
    $store c2
    jmp b2
b2:
    push8 0
    $store c3
    jmp b3
b3:
    $load c3
    $load c2
    lt
    jz b6
b4:
    $load c1
    $load c3
    add
    ld8
    $store c4
    $load c0
    $load c4
    xor
    push8 255
    and
    $store c0
    jmp b5
b5:
    $load c3
    push8 1
    add
    $store c3
    jmp b3
b6:
    $load c0
    halt
"
    );
    assert_resolves(&cfg);
}

#[test]
fn every_spelling_of_a_byte_walk_resolves() {
    for iterable in [
        "p.body.iter()",
        "p.body.iter().copied()",
        "p.name.bytes()",
        "p.name.as_bytes().iter()",
        "p.header.body.iter()",
        "(p.body).iter()",
    ] {
        for pat in ["b", "&b", "_"] {
            assert_resolves(&graph(&format!(
                "fn f(p: Msg) -> u32 {{ let mut n: u32 = 0; for {pat} in {iterable} {{ n += 1; }} n }}"
            )));
        }
    }
}

#[test]
fn the_aggregate_itself_may_be_the_region() {
    assert_resolves(&graph(
        "fn f(s: String) -> u32 { let mut n: u32 = 0; for b in s.bytes() { n += b as u32; } n }",
    ));
}

#[test]
fn a_byte_walk_breaks_and_continues() {
    assert_resolves(&graph(
        "fn f(p: Msg) -> u8 { for b in p.body.iter() { if *b == 0 { continue; } if *b == 255 { break; } return *b; } 0 }",
    ));
}

#[test]
fn a_region_length_is_a_word_even_inside_a_condition() {
    let cfg = graph("fn f(p: Msg) -> bool { p.body.len() < 4 && p.name.len() == 0 }");
    assert!(format!("{cfg:?}").contains("$len .input"));
    assert_resolves(&cfg);
    assert_resolves(&graph("fn f(p: Msg) -> u32 { p.body.len() as u32 }"));
}

#[test]
fn a_region_walk_needs_the_aggregate() {
    assert!(
        refusal("fn f(n: u32) -> u32 { for b in n.iter() { } n }").contains("aggregate parameter")
    );
    assert!(
        refusal("fn f(p: Msg) -> u32 { let mut n: u32 = 0; for b in p.body.iter().rev() { } n }")
            .contains("`.rev()` is not lowered on a walk")
    );
    assert!(refusal("fn f(p: Msg) -> u32 { p.body.iter() as u32 }").contains("only a `for`"));
    assert!(
        refusal("fn f(p: Msg) -> u32 { p.body.len().len() as u32 }").contains("has no methods")
    );
    assert!(
        refusal("fn f(p: Msg) -> u32 { p.body.len::<u32>() as u32 }").contains("no type argument")
    );
    assert!(
        refusal("fn f(p: Msg) -> u32 { for _ in p.body.as_bytes() { } 0 }").contains("`.iter()`")
    );
    assert!(
        refusal("fn f(p: Msg) -> u32 { for (a, b) in p.body.iter() { } 0 }")
            .contains("name or `_`")
    );
    assert!(refusal("fn f(p: Msg) -> u32 { p.body.len(1) as u32 }").contains("no arguments"));
    assert!(refusal("fn f(x: u32) -> u32 { *(x + 1) }").contains("byte binding"));
}

#[test]
fn a_cast_normalizes_to_its_target() {
    let cfg = graph("fn f(x: i8) -> u32 { x as u32 }");
    assert!(format!("{cfg:?}").ends_with("push32 4294967295\n    and\n    halt\n"));

    let cfg = graph("fn f(x: u8) -> i8 { x as i8 }");
    assert!(format!("{cfg:?}").ends_with("shl\n    push8 56\n    sar\n    halt\n"));

    let cfg = graph("fn f(x: u8) -> u32 { x as u32 }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .input\n    ld8\n    halt\n",
        "widening from unsigned is already at rest"
    );
    assert_resolves(&graph("fn f(x: i32) -> u64 { x as u64 }"));
    assert!(refusal("fn f(x: u32) -> u32 { (x as u16) as u32 }").contains("machine can hold"));
}
