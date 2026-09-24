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
fn a_return_type_the_machine_cannot_hold_is_refused() {
    assert!(refusal("fn f() -> u16 { 7 }").contains("scalar the machine can hold"));
    // A region return type is accepted; the body still has to name a region.
    assert!(refusal("fn f() -> String { todo!() }").contains("byte region of the input"));
    assert!(refusal("fn f() -> Bytes { 7 }").contains("byte region of the input"));
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

#[test]
fn a_const_folds_into_an_immediate() {
    let cfg = graph("fn f(x: u32) -> u32 { const K: u32 = 7; x + K }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .input\n    ld32\n    push8 7\n    add\n    push32 4294967295\n    and\n    halt\n",
        "no cell, no load: the const is a push"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_const_rests_in_its_declared_form() {
    let cfg = graph("fn f() -> i8 { const M: i8 = -1; M }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    push64 18446744073709551615\n    halt\n",
        "a negative narrow const is sign-extended, as a load of it would be"
    );
    let cfg = graph("fn f() -> u8 { const B: u8 = b'a'; B }");
    assert!(format!("{cfg:?}").contains("push8 97"));
    let cfg = graph("fn f() -> bool { static YES: bool = true; YES }");
    assert!(format!("{cfg:?}").contains("push8 1"));
    let cfg = graph("fn f() -> u64 { const BIG: u64 = 0x1_0000_0000; BIG }");
    assert!(format!("{cfg:?}").contains("push64 4294967296"));
    assert_resolves(&graph("fn f() -> i64 { const N: i64 = (-5); N * 2 }"));
}

#[test]
fn a_usize_const_is_a_word_where_rust_wants_one() {
    // A `usize` bound types the counter as a word; the reference copy makes
    // the same choice.
    let cfg = graph(
        "fn f() -> u64 { const N: usize = 4; let mut c: u64 = 0; for _ in 0..N { c += 1; } c }",
    );
    assert!(format!("{cfg:?}").starts_with(".frame { c0: u64, c1: u64, c2: u64 }\n"));
    assert_resolves(&cfg);
    assert_resolves(&graph(
        "fn f(x: u32) -> u64 { const N: isize = -3; (x as isize + N) as u64 }",
    ));
    assert!(refusal("fn f() -> u64 { let n: usize = 4; n as u64 }").contains("machine can hold"));
}

#[test]
fn a_const_is_scoped_like_a_local() {
    assert_resolves(&graph(
        "fn f(c: bool) -> u32 { const K: u32 = 1; if c { const K: u32 = 2; return K; } K }",
    ));
    assert!(
        refusal("fn f(c: bool) -> u32 { if c { const K: u32 = 2; } K }")
            .contains("`K` is not declared")
    );
}

#[test]
fn a_const_is_not_a_place() {
    assert!(
        refusal("fn f() -> u32 { const K: u32 = 1; K = 2; K }")
            .contains("constant cannot be assigned")
    );
    assert!(
        refusal("fn f() -> u32 { const K: u32 = 1; K += 2; K }")
            .contains("constant cannot be assigned")
    );
}

#[test]
fn a_const_needs_a_literal_initializer() {
    assert!(refusal("fn f() -> u32 { const K: u32 = 1 + 1; K }").contains("cannot evaluate"));
    assert!(
        refusal("fn f() -> u32 { const A: u32 = 1; const B: u32 = A; B }")
            .contains("cannot evaluate")
    );
    assert!(refusal("fn f() -> u32 { const K: u32 = g(); K }").contains("cannot evaluate"));
    assert!(refusal("fn f() -> u32 { const K: u32 = -(1); K }").contains("cannot evaluate"));
    assert!(refusal("fn f() -> u32 { const K: u32 = u32::MAX; K }").contains("cannot evaluate"));
}

#[test]
fn a_const_of_an_unheld_type_is_refused() {
    assert!(refusal("fn f() -> u32 { const K: u16 = 1; K as u32 }").contains("machine can hold"));
    assert!(refusal("fn f() -> u32 { const K: f32 = 1.0; 0 }").contains("machine can hold"));
}

#[test]
fn a_static_mut_and_other_items_are_refused() {
    assert!(refusal("fn f() -> u32 { static mut K: u32 = 1; 0 }").contains("no globals"));
    assert!(refusal("fn f() -> u32 { struct S; 0 }").contains("only `const` and `static`"));
    assert!(refusal("fn f() -> u32 { fn g() {} 0 }").contains("only `const` and `static`"));
}

#[test]
fn a_constant_table_is_walked_from_rodata() {
    let cfg = graph(
        "fn f() -> u8 { const KEY: [u8; 3] = [1, 2, 3]; let mut x: u8 = 0; for b in KEY.iter() { x ^= *b; } x }",
    );
    assert_eq!(
        format!("{cfg:?}"),
        "\
.frame { c0: u8, c1: u64, c2: u32, c3: u32, c4: u8 }
b0:
    push8 0
    $store c0
    $push .rodata
    $store c1
    push8 3
    $store c2
    push8 0
    $store c3
    jmp b1
b1:
    $load c3
    $load c2
    lt
    jz b4
b2:
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
    jmp b3
b3:
    $load c3
    push8 1
    add
    $store c3
    jmp b1
b4:
    $load c0
    halt
",
        "no header to read and no clamp: the table's place and length are known"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_wide_table_scales_its_index_and_a_later_one_sits_after_it() {
    let source = "fn f() -> u64 { const T: [u64; 2] = [5, 6]; const S: &str = \"hi\"; let mut x: u64 = 0; for v in T.iter().copied() { x += v; } for b in S.bytes() { x += b as u64; } x + T.len() as u64 }";
    let cfg = graph(source);
    let listing = format!("{cfg:?}");
    assert!(listing.contains("$load c3\n    push8 3\n    shl\n    add\n    ld64\n"));
    assert!(listing.contains("$push .rodata\n    push8 16\n    add\n    $store c5\n"));
    assert!(
        listing.ends_with("$load c0\n    push8 2\n    add\n    halt\n"),
        "`.len()` is an immediate"
    );
    assert_resolves(&cfg);

    let func = syn::parse_str(source).expect("parses as a fn");
    let rodata = lower(&func).expect("lowers").rodata;
    assert_eq!(rodata.size, 18);
    let placed: Vec<(u32, Vec<u64>)> = rodata
        .items
        .iter()
        .map(|item| (item.off, item.values.clone()))
        .collect();
    assert_eq!(placed, [(0, vec![5, 6]), (16, vec![104, 105])]);
}

#[test]
fn a_table_is_aligned_to_its_element() {
    let func = syn::parse_str(
        "fn f() -> u32 { const A: [u8; 3] = b\"abc\"; const B: &[u32] = &[1, 2]; const C: [i8; 2] = [-1, 1]; 0 }",
    )
    .expect("parses as a fn");
    let rodata = lower(&func).expect("lowers").rodata;
    let offs: Vec<u32> = rodata.items.iter().map(|item| item.off).collect();
    assert_eq!(offs, [0, 4, 12], "three bytes, then a word boundary");
    assert_eq!(rodata.size, 14);
    assert_eq!(
        rodata.items.get(2).map(|item| item.values.clone()),
        Some(vec![u64::MAX, 1]),
        "elements rest the way a load of them would"
    );
}

#[test]
fn every_spelling_of_a_table_resolves() {
    for (decl, iterable) in [
        ("const K: [u8; 2] = [1, 2]", "K.iter()"),
        ("const K: [u8; 2] = [1, 2]", "K.iter().copied()"),
        ("const K: &[u8; 2] = &[1, 2]", "K.iter()"),
        ("const K: &[u8] = b\"ab\"", "K.iter()"),
        ("const K: &[u8] = &[0u8; 2]", "K.iter()"),
        ("const K: [u8; 0] = []", "K.iter()"),
        ("static K: &'static str = \"ab\"", "K.bytes()"),
        ("const K: &str = \"ab\"", "K.as_bytes().iter()"),
        ("const K: [u32; 2] = [1, 2]", "K.iter()"),
        ("const K: [i64; 2] = [-1, 2]", "K.iter()"),
        ("const K: [bool; 2] = [true, false]", "K.iter()"),
    ] {
        for pat in ["b", "&b", "_"] {
            assert_resolves(&graph(&format!(
                "fn f() -> u32 {{ {decl}; let mut n: u32 = 0; for {pat} in {iterable} {{ n += 1; }} n + K.len() as u32 }}"
            )));
        }
    }
}

#[test]
fn a_table_walk_breaks_continues_and_returns() {
    assert_resolves(&graph(
        "fn f() -> u8 { const K: [u8; 3] = [0, 255, 7]; for b in K.iter() { if *b == 0 { continue; } if *b == 255 { break; } return *b; } 0 }",
    ));
}

#[test]
fn a_table_is_a_place_not_a_value() {
    assert!(refusal("fn f() -> u32 { const K: [u8; 2] = [1, 2]; K }").contains("is a place"));
    assert!(
        refusal("fn f() -> u32 { const K: [u8; 2] = [1, 2]; K = [2, 3]; 0 }")
            .contains("cannot be assigned")
    );
    assert!(
        refusal("fn f() -> u32 { const K: [u8; 2] = [1, 2]; K.iter() as u32 }")
            .contains("only a `for`")
    );
    assert!(
        refusal("fn f() -> u32 { const K: [u8; 2] = [1, 2]; K.typed::<u32>() }")
            .contains("not lowered on a constant table")
    );
    assert!(
        refusal("fn f() -> u32 { const K: [u8; 2] = [1, 2]; for _ in K { } 0 }")
            .contains("`.iter()`")
    );
}

#[test]
fn a_table_needs_literal_elements_of_a_held_type() {
    assert!(
        refusal("fn f() -> u32 { const K: [u8; 2] = [1, g()]; 0 }").contains("cannot evaluate")
    );
    assert!(
        refusal("fn f() -> u32 { const K: [u8; 2] = [1 + 1, 2]; 0 }").contains("cannot evaluate")
    );
    assert!(
        refusal("fn f() -> u32 { const N: usize = 2; const K: [u8; 2] = [0; N]; 0 }")
            .contains("cannot evaluate")
    );
    assert!(
        refusal("fn f() -> u32 { const K: &[u8] = b\"ab\".as_slice(); 0 }")
            .contains("cannot evaluate")
    );
    assert!(refusal("fn f() -> u32 { const K: [u16; 2] = [1, 2]; 0 }").contains("array of them"));
    assert!(refusal("fn f() -> u32 { const K: &[u16] = &[1, 2]; 0 }").contains("array of them"));
    assert!(refusal("fn f() -> u32 { const K: (u8, u8) = (1, 2); 0 }").contains("array of them"));
}

#[test]
fn a_runtime_index_is_checked_and_aborts_past_the_table() {
    let cfg = graph("fn f(x: u8) -> u8 { const SBOX: [u8; 4] = [3, 1, 2, 0]; SBOX[x as usize] }");
    assert_eq!(
        format!("{cfg:?}"),
        "\
.frame { c0: u64 }
b0:
    $push .input
    ld8
    $store c0
    $load c0
    push8 4
    lt
    jz b2
b1:
    $push .rodata
    $load c0
    add
    ld8
    halt
b2:
    abort
"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_constant_index_folds_to_the_element_address() {
    let cfg = graph("fn f() -> u32 { const T: [u32; 3] = [7, 8, 9]; T[2] }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .rodata\n    push8 8\n    add\n    ld32\n    halt\n",
        "no check: the index is known to be in bounds"
    );
    let cfg = graph("fn f() -> u32 { const T: [u32; 3] = [7, 8, 9]; const I: usize = 0; T[I] }");
    assert_eq!(
        format!("{cfg:?}"),
        "b0:\n    $push .rodata\n    ld32\n    halt\n"
    );
    assert!(
        refusal("fn f() -> u32 { const T: [u32; 3] = [7, 8, 9]; T[3] }")
            .contains("index 3 is out of bounds for a constant of 3 elements")
    );
    assert!(
        refusal("fn f() -> u32 { const T: [u32; 3] = [7, 8, 9]; const I: usize = 9; T[I] }")
            .contains("out of bounds")
    );
}

#[test]
fn an_indexed_element_rests_like_a_loaded_one() {
    let cfg = graph("fn f(x: u32) -> i8 { const T: [i8; 2] = [-1, 1]; 1 + T[x as usize] }");
    let listing = format!("{cfg:?}");
    assert!(listing.contains("ld8\n    push8 56\n    shl\n    push8 56\n    sar\n    add\n"));
    assert_resolves(&cfg);
    assert_resolves(&graph(
        "fn f(x: u32) -> u64 { const T: [u64; 2] = [1, 2]; const S: &str = \"ab\"; T[x as usize] + S.as_bytes()[x as usize] as u64 }",
    ));
    assert_resolves(&graph(
        "fn f() -> u32 { const N: usize = 3; const T: [u32; 3] = [1, 2, 3]; let mut s: u32 = 0; for i in 0..N { s += T[i]; } s }",
    ));
    assert_resolves(&graph(
        "fn f(x: u32) -> bool { const T: [u8; 2] = [1, 2]; x < 2 && T[x as usize] == 2 }",
    ));
}

#[test]
fn an_index_must_be_a_word_into_a_table() {
    assert!(
        refusal("fn f(i: u32) -> u32 { const T: [u32; 2] = [1, 2]; T[i] }")
            .contains("cast it with `as usize`")
    );
    assert!(
        refusal("fn f(p: Msg) -> u8 { p.body[p.kind.typed::<u32>()] }")
            .contains("cast it with `as usize`")
    );
    assert!(
        refusal("fn f() -> u32 { const T: [u32; 2] = [1, 2]; T.iter()[0] }")
            .contains("cannot be indexed")
    );
    assert!(refusal("fn f(x: u32) -> u32 { x[0] }").contains("constant table"));
}

#[test]
fn a_const_the_macro_cannot_see_is_refused_with_where_to_put_it() {
    let message = refusal("fn f(x: u64) -> u64 { x * SEED }");
    assert!(message.contains("`SEED` is not declared in this function"));
    assert!(message.contains("declared inside it"));
    assert!(
        refusal("fn f() -> u32 { let mut n: u32 = 0; for b in KEY.iter() { n += 1; } n }")
            .contains("`KEY` is not declared")
    );
    assert!(refusal("fn f(x: u64) -> u64 { x * Self::SEED }").contains("outside the function"));
    assert!(refusal("fn f(x: u64) -> u64 { x * consts::SEED }").contains("outside the function"));
    assert!(refusal("fn f(x: u64) -> u64 { x * u64::MAX }").contains("outside the function"));
    assert!(refusal("fn f(x: u64) -> u64 { x * seed }").contains("no such parameter or local"));
}

#[test]
fn a_byte_region_is_indexed_behind_its_checked_length() {
    let cfg = graph("fn f(p: Msg) -> u8 { p.body[1] }");
    assert_eq!(
        format!("{cfg:?}"),
        "\
.frame { c0: u64, c1: u32, c2: u64 }
b0:
    $push .input
    $field #0
    add
    $loadfield #0
    $push .input
    add
    $store c0
    $push .input
    $field #1
    add
    $loadfield #1
    $store c1
    $load c0
    $load c1
    add
    $push .input
    $len .input
    add
    le
    jnz b2
b1:
    push8 0
    $store c1
    jmp b2
b2:
    push8 1
    $store c2
    $load c2
    $load c1
    lt
    jz b4
b3:
    $load c0
    $load c2
    add
    ld8
    halt
b4:
    abort
",
        "the header is clamped first, then the index is checked against it"
    );
    assert_resolves(&cfg);
}

#[test]
fn every_spelling_of_a_region_index_resolves() {
    assert_resolves(&graph("fn f(p: Msg) -> u8 { p.body[p.body.len() - 1] }"));
    assert_resolves(&graph("fn f(p: Msg) -> u8 { p.name.as_bytes()[0] }"));
    assert_resolves(&graph("fn f(v: Vec<u8>) -> u8 { v[0] }"));
    assert_resolves(&graph("fn f(s: String) -> u8 { s.as_bytes()[2] }"));
    assert_resolves(&graph(
        "fn f(p: Msg) -> u8 { p.body[p.kind.typed::<u8>() as usize] + p.header.body[0] }",
    ));
    assert_resolves(&graph(
        "fn f(p: Msg) -> bool { p.body.len() > 0 && p.body[0] == 7 }",
    ));
    assert_resolves(&graph(
        "fn f(p: Msg) -> u32 { let mut s: u32 = 0; for i in 0..4 { s += p.body[i as usize] as u32; } s }",
    ));
}

#[test]
fn a_whole_region_returns_a_pointer_and_a_length() {
    let cfg = graph("fn f(m: Msg) -> Bytes { m.body }");
    assert_eq!(
        format!("{cfg:?}"),
        "\
.frame { c0: u64, c1: u32, c2: u64, c3: u64 }
b0:
    $push .input
    $field #0
    add
    $loadfield #0
    $push .input
    add
    $store c0
    $push .input
    $field #1
    add
    $loadfield #1
    $store c1
    $load c0
    $load c1
    add
    $push .input
    $len .input
    add
    le
    jnz b2
b1:
    push8 0
    $store c1
    jmp b2
b2:
    push8 0
    $store c2
    $load c1
    $store c3
    $load c3
    $load c2
    sub
    $load c0
    $load c2
    add
    halt
",
        "no guard: neither end was written; the length is deep and the pointer on top"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_returned_range_checks_both_of_its_ends() {
    let cfg = graph("fn f(m: Msg) -> Bytes { m.body[2..5] }");
    let listing = format!("{cfg:?}");
    assert!(
        listing.contains(
            "b2:\n    push8 2\n    $store c2\n    push8 5\n    $store c3\n    \
             $load c3\n    $load c1\n    le\n    jnz b4\nb3:\n    abort\n"
        ),
        "the end is checked against the clamped length\n{listing}"
    );
    assert!(
        listing.contains("b4:\n    $load c2\n    $load c3\n    le\n    jz b3\n"),
        "then the start against the end, sharing one abort\n{listing}"
    );
    assert!(
        listing.ends_with(
            "$load c3\n    $load c2\n    sub\n    $load c0\n    $load c2\n    add\n    halt\n"
        ),
        "the result is length then pointer, no store\n{listing}"
    );
    assert_resolves(&cfg);
}

#[test]
fn only_the_ends_that_were_written_are_checked() {
    let count = |source: &str| format!("{:?}", graph(source)).matches("abort").count();
    assert_eq!(count("fn f(m: Msg) -> Bytes { m.body }"), 0);
    assert_eq!(count("fn f(m: Msg) -> Bytes { m.body[..] }"), 0);
    assert_eq!(count("fn f(m: Msg) -> Bytes { m.body[2..] }"), 1);
    assert_eq!(count("fn f(m: Msg) -> Bytes { m.body[..2] }"), 1);
    assert_eq!(count("fn f(m: Msg) -> Bytes { m.body[1..2] }"), 1);
}

#[test]
fn every_spelling_of_a_returned_slice_resolves() {
    for body in [
        "m.body",
        "Bytes::from(&m.body[2..])",
        "m.body[..2].to_vec()",
        "m.name.as_bytes().to_vec()",
        "m.name[1..].to_string()",
        "Bytes::new(&m.body[..])",
        "m.body[..]",
        "m.body[1..]",
        "m.body[..1]",
        "m.body[1..2]",
        "(m.body)[1..]",
        "m.body[m.body.len() - 1..]",
        "m.body[m.kind.typed::<u8>() as usize..]",
        "m.name.as_bytes()[1..]",
        "m.header.body[1..]",
        "return m.body[1..]",
        "if m.kind.typed::<u8>() == 0 { return m.body; } else { return m.name.as_bytes()[..1]; }",
    ] {
        assert_resolves(&graph(&format!("fn f(m: Msg) -> Bytes {{ {body} }}")));
    }
    assert_resolves(&graph("fn f(s: String) -> String { s[1..] }"));
    assert_resolves(&graph("fn f(v: Vec<u8>) -> Vec<u8> { v }"));
    assert_resolves(&graph(
        "fn f(m: Msg) -> Bytes { let mut i: u64 = 0; while i < 4 { i += 1; } m.body[i as usize..] }",
    ));
}

#[test]
fn a_returned_slice_must_name_the_input() {
    assert!(refusal("fn f(m: Msg) -> Bytes { m.body[1..=2] }").contains("`..=`"));
    assert!(
        refusal("fn f(m: Msg) -> Bytes { m.body[m.kind.typed::<u8>()..] }")
            .contains("a slice bound must be a `usize`")
    );
    assert!(
        refusal("fn f(m: Msg) -> Bytes { const K: [u8; 2] = [1, 2]; K[..] }")
            .contains("lives in the program, not the input")
    );
    assert!(refusal("fn f(m: Msg) -> Bytes { m.body.iter() }").contains("walk cannot be returned"));
    assert!(refusal("fn f(m: Msg) -> Bytes { m.body[1] }").contains("needs a range"));
    assert!(
        refusal("fn f(m: Msg) -> u8 { m.body[1..2] }")
            .contains("a slice is only a function's result")
    );
    assert!(refusal("fn f(m: Msg) -> Bytes { 7 }").contains("byte region of the input"));
    assert!(
        refusal("fn f(m: Msg) -> Bytes { Bytes::from(&7[1..]) }")
            .contains("byte region of the input")
    );
}

/// The conversion is the reference copy's to check; what the machine needs is
/// the slice under it, whichever way it was spelled.
#[test]
fn an_owning_conversion_lowers_to_the_slice_under_it() {
    let bare = format!("{:?}", graph("fn f(m: Msg) -> Bytes { m.body[2..] }"));
    for spelling in [
        "Bytes::from(&m.body[2..])",
        "Bytes::new(&m.body[2..])",
        "m.body[2..].to_vec()",
        "m.body[2..].to_owned()",
        "(&m.body[2..]).to_vec()",
    ] {
        assert_eq!(
            format!(
                "{:?}",
                graph(&format!("fn f(m: Msg) -> Bytes {{ {spelling} }}"))
            ),
            bare,
            "{spelling} lowers the same as the slice it owns"
        );
    }
}

#[test]
fn an_indexed_store_writes_a_byte_in_place() {
    let cfg = graph("fn f(mut m: Msg) -> u8 { m.body[1] = 7; 0 }");
    assert_eq!(
        format!("{cfg:?}"),
        "\
.frame { c0: u64, c1: u32, c2: u64 }
b0:
    $push .input
    $field #0
    add
    $loadfield #0
    $push .input
    add
    $store c0
    $push .input
    $field #1
    add
    $loadfield #1
    $store c1
    $load c0
    $load c1
    add
    $push .input
    $len .input
    add
    le
    jnz b2
b1:
    push8 0
    $store c1
    jmp b2
b2:
    push8 1
    $store c2
    $load c2
    $load c1
    lt
    jz b4
b3:
    $load c0
    $load c2
    add
    push8 7
    st8
    push8 0
    halt
b4:
    abort
",
        "the index is checked against the clamped length, then base + i is the store address"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_compound_indexed_store_reads_modifies_and_writes() {
    let cfg = graph("fn f(mut m: Msg) -> u8 { m.body[1] ^= 9; 0 }");
    let listing = format!("{cfg:?}");
    // The address is named once in a cell, then loaded twice: once under the
    // new byte for the store, once to read the old byte.
    assert!(
        listing.contains(
            "add\n    $store c3\n    $load c3\n    $load c3\n    ld8\n    push8 9\n    xor\n    push8 255\n    and\n    st8\n"
        ),
        "{listing}"
    );
    assert_resolves(&cfg);
}

#[test]
fn every_spelling_of_an_indexed_store_resolves() {
    for stmt in [
        "m.body[0] = 1",
        "m.body[i as usize] = 2",
        "(m.body)[0] = 3",
        "m.name.as_bytes()[0] = 4",
        "m.body[m.body.len() - 1] = 5",
        "m.body[i as usize] ^= 0x5a",
        "m.body[0] += 1",
        "m.body[0] -= 1",
        "m.body[0] &= 15",
        "m.body[0] |= 8",
    ] {
        assert_resolves(&graph(&format!(
            "fn f(mut m: Msg) -> u8 {{ let i: u32 = 0; {stmt}; 0 }}"
        )));
    }
    assert_resolves(&graph("fn f(mut v: Vec<u8>) -> u8 { v[0] = 9; 0 }"));
    assert_resolves(&graph(
        "fn f(mut m: Msg) -> u8 { let mut i: u64 = 0; while i < m.body.len() { m.body[i as usize] ^= 7; i += 1; } 0 }",
    ));
}

#[test]
fn an_indexed_store_refuses_what_it_cannot_write() {
    assert!(
        refusal("fn f() -> u8 { const K: [u8; 2] = [1, 2]; K[0] = 3; 0 }")
            .contains("lives in the program, not the input")
    );
    assert!(
        refusal("fn f(mut m: Msg) -> u8 { m.body[0..1] = 3; 0 }")
            .contains("a slice cannot be assigned")
    );
    assert!(
        refusal("fn f(mut m: Msg) -> u8 { m.body[m.kind.typed::<u8>()] = 0; 0 }")
            .contains("cast it with `as usize`")
    );
}
