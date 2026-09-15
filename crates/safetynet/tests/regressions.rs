//! Rust lowering regressions

#![allow(clippy::manual_is_multiple_of)]

use safetynet::{Typed, VmLayout, safetynet};

#[safetynet]
fn outer_after_shadow(c: bool) -> u64 {
    let v: u64 = 1000;
    if c {
        let v: u64 = 1;
        let _inner: u64 = v + 1;
    }
    v
}

#[safetynet]
fn param_after_loop(x: u32) -> u32 {
    for x in 0..2 {
        let _inner: u32 = x;
    }
    x
}

#[test]
fn a_shadow_dies_with_its_block() {
    assert_eq!(outer_after_shadow(true), 1000);
    assert_eq!(outer_after_shadow(false), 1000);
}

#[test]
fn a_loop_variable_dies_with_its_loop() {
    assert_eq!(param_after_loop(99), 99);
}

#[derive(Clone, Copy, VmLayout)]
struct Reading {
    value: i64,
}

#[safetynet]
fn is_negative(r: Reading) -> bool {
    r.value.typed::<i64>() < 0
}

#[safetynet]
fn halved(r: Reading) -> i64 {
    r.value.typed::<i64>() / 2
}

#[test]
fn a_signed_field_compares_signed() {
    assert!(is_negative(Reading { value: -1 }));
    assert!(!is_negative(Reading { value: 1 }));
}

#[test]
fn a_signed_field_divides_signed() {
    assert_eq!(halved(Reading { value: -10 }), -5);
    assert_eq!(halved(Reading { value: 10 }), 5);
}

// The unreachable `_dead` locals below get frame cells even though lowering
// never reaches their statements; the cells that ARE consumed must still line
// up with the right locals.

#[safetynet]
#[allow(unreachable_code)]
fn after_divergence(c: u64) -> u64 {
    if c == 0 {
        return 1;
        let _dead: u8 = 0;
    }
    let x: u64 = 1 << 40;
    x
}

#[safetynet]
#[allow(unreachable_code)]
fn value_block_local(c: bool) -> u64 {
    if c {
        let x: u64 = {
            let y: u64 = 500;
            y
        };
        return x;
        let _dead: u8 = 0;
    }
    0
}

#[test]
fn a_local_after_a_divergence_keeps_its_width() {
    assert_eq!(after_divergence(0), 1);
    assert_eq!(after_divergence(1), 1 << 40);
}

#[test]
fn a_local_inside_a_value_block_keeps_its_width() {
    assert_eq!(value_block_local(true), 500);
    assert_eq!(value_block_local(false), 0);
}

#[safetynet]
fn count_to_break_outer(n: u64) -> u64 {
    let mut c: u64 = 0;
    'outer: loop {
        loop {
            c += 1;
            if c % 2 == 0 {
                break;
            }
            if c >= n {
                break 'outer;
            }
        }
    }
    c
}

#[test]
fn a_labeled_break_leaves_the_labeled_loop() {
    assert_eq!(count_to_break_outer(3), 3);
}

#[safetynet]
fn counted_above_u32() -> u64 {
    let mut c: u64 = 0;
    for i in 4294967296u64..4294967299u64 {
        if i == 4294967297u64 {
            c += 1;
        }
    }
    c
}

#[test]
fn a_range_keeps_its_64_bit_bounds() {
    assert_eq!(counted_above_u32(), 1);
}

#[safetynet]
fn inverted_matches(a: u32) -> bool {
    !a == 0xFFFF_FFF0u32
}

#[test]
fn a_narrow_not_stays_at_its_width() {
    assert!(inverted_matches(0xF));
    assert!(!inverted_matches(0));
}

#[safetynet]
fn negation_matches(a: i32, b: i32) -> bool {
    -a == b
}

#[safetynet]
fn narrow_less(a: i32, b: i32) -> bool {
    a < b
}

#[safetynet]
fn narrow_halved(x: i32) -> i32 {
    x / 2
}

#[test]
fn a_narrow_negation_stays_at_its_width() {
    assert!(negation_matches(5, -5));
    assert!(!negation_matches(5, 5));
}

#[test]
fn a_narrow_signed_comparison_respects_the_sign() {
    assert!(narrow_less(-1, 1));
    assert!(!narrow_less(1, -1));
}

#[test]
fn a_narrow_signed_division_rounds_toward_zero() {
    assert_eq!(narrow_halved(-10), -5);
    assert_eq!(narrow_halved(10), 5);
}
