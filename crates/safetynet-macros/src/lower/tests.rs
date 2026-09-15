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
        "b0:\n    $push .input\n    ld32\n    push8 1\n    add\n    halt\n"
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
        ".frame { c0: u32 }\nb0:\n    $push .input\n    ld32\n    $push .input\n    push8 4\n    add\n    ld32\n    add\n    $store c0\n    $load c0\n    halt\n"
    );
    assert_resolves(&cfg);
}

#[test]
fn a_wider_parameter_reads_at_its_width() {
    let cfg = graph("fn id(x: u64) -> u64 { x }");
    assert_eq!(format!("{cfg:?}"), "b0:\n    $push .input\n    ld64\n    halt\n");
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
