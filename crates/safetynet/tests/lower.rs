//! `#[safetynet]` functions run on the VM and agree with the plain Rust they
//! were written as.

use safetynet::safetynet;

#[safetynet]
fn answer() -> u32 {
    42
}

#[safetynet]
fn inc(x: u32) -> u32 {
    x + 1
}

#[safetynet]
fn add2(a: u32, b: u32) -> u32 {
    let s: u32 = a + b;
    s
}

#[safetynet]
fn xor(a: u32, b: u32) -> u32 {
    a ^ b
}

#[safetynet]
fn below(a: u32, b: u32) -> bool {
    a < b
}

#[safetynet]
fn product(a: u64, b: u64) -> u64 {
    a * b
}

#[safetynet]
fn difference(a: i32, b: i32) -> i32 {
    a - b
}

#[safetynet]
fn greater(a: i64, b: i64) -> bool {
    a > b
}

#[test]
fn a_constant_function_returns_it() {
    assert_eq!(answer(), 42);
}

#[test]
fn one_argument_reads_back() {
    assert_eq!(inc(41), 42);
    assert_eq!(inc(0), 1);
}

#[test]
fn two_arguments_add_through_a_local() {
    assert_eq!(add2(20, 22), 42);
    assert_eq!(add2(0, 0), 0);
}

#[test]
fn a_bitwise_operator_runs() {
    assert_eq!(xor(0xff, 0x0f), 0xf0);
}

#[test]
fn an_unsigned_comparison_runs() {
    assert!(below(1, 2));
    assert!(!below(2, 1));
    assert!(!below(2, 2));
}

#[test]
fn a_wider_type_reads_and_multiplies() {
    assert_eq!(product(6, 7), 42);
    assert_eq!(product(1 << 20, 1 << 20), 1 << 40);
}

#[test]
fn a_narrow_result_goes_negative() {
    assert_eq!(difference(1, 5), -4);
    assert_eq!(difference(5, 1), 4);
}

#[test]
fn a_signed_comparison_respects_the_sign() {
    assert!(greater(-1, -2));
    assert!(!greater(-2, -1));
    assert!(greater(0, -100));
}
