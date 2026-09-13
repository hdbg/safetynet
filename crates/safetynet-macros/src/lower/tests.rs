//! A function lowered to a graph, checked by printing it as assembly.
//!
//! The graph is printed and read back through the assembler: a lowering the
//! disassembler cannot spell is a failing test here, not a discovery later.

use safetynet_core::asm::print;
use safetynet_core::ir::Cfg;

use super::build::lower;
use crate::asm::parse_cfg_raw;

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

/// A printed graph reads back as itself.
fn assert_round_trips(cfg: &Cfg) {
    let text = print(cfg);
    let parsed = parse_cfg_raw(text.parse().expect("tokens"))
        .expect("parses")
        .cfg;
    assert_eq!(&parsed, cfg, "printed as:\n{text}");
}

#[test]
fn a_constant_is_pushed_and_halted() {
    let cfg = graph("fn answer() -> u32 { 42 }");
    assert_eq!(print(&cfg), "b0:\n    push8 42\n    halt\n");
    assert_round_trips(&cfg);
}

#[test]
fn a_parameter_is_read_from_the_input() {
    let cfg = graph("fn inc(x: u32) -> u32 { x + 1 }");
    assert_eq!(
        print(&cfg),
        "b0:\n    $push .input\n    ld32\n    push8 1\n    add\n    halt\n"
    );
    assert_round_trips(&cfg);
}

#[test]
fn a_second_parameter_is_read_at_its_offset() {
    let cfg = graph("fn lt(a: u32, b: u32) -> bool { a < b }");
    assert_eq!(
        print(&cfg),
        "b0:\n    $push .input\n    ld32\n    $push .input\n    push8 4\n    add\n    ld32\n    lt\n    halt\n"
    );
    assert_round_trips(&cfg);
}

#[test]
fn a_local_lives_in_a_frame_cell() {
    let cfg = graph("fn add2(a: u32, b: u32) -> u32 { let s: u32 = a + b; s }");
    assert_eq!(
        print(&cfg),
        ".frame { c0: u32 }\nb0:\n    $push .input\n    ld32\n    $push .input\n    push8 4\n    add\n    ld32\n    add\n    $store c0\n    $load c0\n    halt\n"
    );
    assert_round_trips(&cfg);
}

#[test]
fn a_wider_parameter_reads_at_its_width() {
    let cfg = graph("fn id(x: u64) -> u64 { x }");
    assert_eq!(print(&cfg), "b0:\n    $push .input\n    ld64\n    halt\n");
    assert_round_trips(&cfg);
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
fn a_conditional_round_trips() {
    assert_round_trips(&graph(
        "fn max(a: u32, b: u32) -> u32 { if a < b { b } else { a } }",
    ));
}

#[test]
fn an_early_return_round_trips() {
    assert_round_trips(&graph("fn f(x: u32) -> u32 { if x < 10 { return 0; } x }"));
}

#[test]
fn an_else_if_chain_round_trips() {
    assert_round_trips(&graph(
        "fn sign(x: i64) -> i64 { if x < 0 { -1 } else if x > 0 { 1 } else { 0 } }",
    ));
}

#[test]
fn a_let_inside_a_branch_is_refused() {
    assert!(
        refusal("fn f(c: bool) -> u32 { if c { let y: u32 = 1; y } else { 0 } }")
            .contains("branch")
    );
}
