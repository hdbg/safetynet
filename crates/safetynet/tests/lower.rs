//! `#[safetynet]` functions run on the VM and agree with the plain Rust they
//! were written as.

// The fixtures write out `s = s + i` and `i % 3 == 0` on purpose, to exercise
// plain assignment and remainder — the tidier idioms clippy suggests are either
// what a separate test already covers or something the subset cannot lower.
#![allow(clippy::assign_op_pattern, clippy::manual_is_multiple_of)]

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

#[safetynet]
fn max(a: u32, b: u32) -> u32 {
    if a < b { b } else { a }
}

#[safetynet]
fn clamp_low(x: u32) -> u32 {
    if x < 10 {
        return 0;
    }
    x
}

#[safetynet]
fn sign(x: i64) -> i64 {
    if x < 0 {
        -1
    } else if x > 0 {
        1
    } else {
        0
    }
}

#[safetynet]
fn magnitude(x: i64) -> i64 {
    if x < 0 { -x } else { x }
}

#[safetynet]
fn pick(c: bool, a: u32, b: u32) -> u32 {
    if c {
        return a;
    }
    b
}

#[test]
fn a_conditional_chooses_the_branch() {
    assert_eq!(max(3, 5), 5);
    assert_eq!(max(5, 3), 5);
    assert_eq!(max(4, 4), 4);
}

#[test]
fn an_early_return_leaves_the_function() {
    assert_eq!(clamp_low(5), 0);
    assert_eq!(clamp_low(9), 0);
    assert_eq!(clamp_low(20), 20);
}

#[test]
fn an_else_if_chain_runs() {
    assert_eq!(sign(-5), -1);
    assert_eq!(sign(7), 1);
    assert_eq!(sign(0), 0);
}

#[test]
fn a_unary_negation_runs() {
    assert_eq!(magnitude(-4), 4);
    assert_eq!(magnitude(4), 4);
    assert_eq!(magnitude(0), 0);
}

#[test]
fn a_branch_on_a_bool_returns_early() {
    assert_eq!(pick(true, 1, 2), 1);
    assert_eq!(pick(false, 1, 2), 2);
}

#[safetynet]
fn triangular(n: u32) -> u32 {
    let mut s: u32 = 0;
    let mut i: u32 = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    s
}

#[safetynet]
fn power_of_two(n: u32) -> u64 {
    let mut r: u64 = 1;
    let mut i: u32 = 0;
    while i < n {
        r = r * 2;
        i += 1;
    }
    r
}

#[safetynet]
fn count_up(limit: u32) -> u32 {
    let mut i: u32 = 0;
    loop {
        if i >= limit {
            break;
        }
        i += 1;
    }
    i
}

#[safetynet]
fn sum_skipping_threes(n: u32) -> u32 {
    let mut s: u32 = 0;
    let mut i: u32 = 0;
    while i < n {
        i += 1;
        if i % 3 == 0 {
            continue;
        }
        s += i;
    }
    s
}

#[test]
fn a_while_loop_accumulates() {
    assert_eq!(triangular(0), 0);
    assert_eq!(triangular(1), 0);
    assert_eq!(triangular(5), 10);
    assert_eq!(triangular(10), 45);
}

#[test]
fn a_loop_multiplies_across_iterations() {
    assert_eq!(power_of_two(0), 1);
    assert_eq!(power_of_two(10), 1024);
}

#[test]
fn a_loop_breaks() {
    assert_eq!(count_up(0), 0);
    assert_eq!(count_up(7), 7);
}

#[test]
fn a_loop_continues() {
    // 1..=6 without the multiples of three: 1+2+4+5 = 12.
    assert_eq!(sum_skipping_threes(6), 12);
}

#[safetynet]
fn both(a: bool, b: bool) -> bool {
    a && b
}

#[safetynet]
fn either(a: bool, b: bool) -> bool {
    a || b
}

#[safetynet]
fn in_range(x: u32, lo: u32, hi: u32) -> bool {
    lo <= x && x < hi
}

#[safetynet]
fn all_three(a: bool, b: bool, c: bool) -> bool {
    a && b && c
}

#[safetynet]
fn safe_divisor(x: u32, d: u32) -> bool {
    // If `d` is zero the right side would trap, so `&&` must not evaluate it.
    d != 0 && x / d > 0
}

#[test]
fn logical_and_computes() {
    assert!(both(true, true));
    assert!(!both(true, false));
    assert!(!both(false, true));
}

#[test]
fn logical_or_computes() {
    assert!(either(false, true));
    assert!(either(true, false));
    assert!(!either(false, false));
}

#[test]
fn a_range_check_uses_two_comparisons() {
    assert!(in_range(5, 1, 10));
    assert!(!in_range(0, 1, 10));
    assert!(!in_range(10, 1, 10));
}

#[test]
fn a_chain_of_ands_runs() {
    assert!(all_three(true, true, true));
    assert!(!all_three(true, false, true));
}

#[test]
fn and_short_circuits_before_a_trap() {
    // Were the right side evaluated, `x / 0` would trap and panic the run.
    assert!(!safe_divisor(10, 0));
    assert!(safe_divisor(10, 2));
    assert!(!safe_divisor(1, 2));
}

#[safetynet]
fn sum_range(n: u32) -> u32 {
    let mut s: u32 = 0;
    for i in 0..n {
        s += i;
    }
    s
}

#[safetynet]
fn count_between(a: u32, b: u32) -> u32 {
    let mut c: u32 = 0;
    for _ in a..b {
        c += 1;
    }
    c
}

#[safetynet]
fn first_at_least(n: u32, t: u32) -> u32 {
    for i in 0..n {
        if i >= t {
            return i;
        }
    }
    n
}

#[safetynet]
fn sum_odds_below(n: u32) -> u32 {
    let mut s: u32 = 0;
    for i in 0..n {
        if i % 2 == 0 {
            continue;
        }
        s += i;
    }
    s
}

#[test]
fn a_for_loop_sums_a_range() {
    assert_eq!(sum_range(0), 0);
    assert_eq!(sum_range(5), 10);
    assert_eq!(sum_range(10), 45);
}

#[test]
fn a_for_loop_counts_between_bounds() {
    assert_eq!(count_between(2, 7), 5);
    assert_eq!(count_between(4, 4), 0);
}

#[test]
fn a_for_loop_returns_early() {
    assert_eq!(first_at_least(10, 3), 3);
    assert_eq!(first_at_least(10, 0), 0);
    assert_eq!(first_at_least(3, 9), 3);
}

#[test]
fn a_for_loop_continues_to_the_increment() {
    // Odd numbers below 6: 1 + 3 + 5 = 9.
    assert_eq!(sum_odds_below(6), 9);
}

use safetynet::{Typed, VmLayout};

#[derive(Clone, Copy, VmLayout)]
struct Packet {
    seq: u32,
    len: u32,
    flags: u8,
    tag: u64,
}

#[derive(Clone, Copy, VmLayout)]
struct Header {
    seq: u32,
    kind: u8,
}

#[derive(Clone, Copy, VmLayout)]
struct Message {
    header: Header,
    tag: u64,
}

#[safetynet]
fn read_seq(p: Packet) -> u32 {
    p.seq.typed::<u32>()
}

#[safetynet]
fn read_flags(p: Packet) -> u8 {
    p.flags.typed::<u8>()
}

#[safetynet]
fn read_tag(p: Packet) -> u64 {
    p.tag.typed::<u64>()
}

#[safetynet]
fn sum_fields(p: Packet) -> u32 {
    p.seq.typed::<u32>() + p.len.typed::<u32>()
}

#[safetynet]
fn flag_is_set(p: Packet) -> bool {
    p.flags.typed::<u8>() & 1 == 1
}

#[safetynet]
fn clamp_seq(p: Packet) -> u32 {
    // The first use asserts the type; the second may go bare.
    if p.seq.typed::<u32>() > 100 {
        return 100;
    }
    p.seq
}

#[safetynet]
fn nested_seq(m: Message) -> u32 {
    m.header.seq.typed::<u32>()
}

fn sample() -> Packet {
    Packet {
        seq: 7,
        len: 35,
        flags: 0b101,
        tag: 0xdead_beef_0000_0042,
    }
}

#[test]
fn a_field_reads_at_its_width() {
    let p = sample();
    assert_eq!(read_seq(p), 7);
    assert_eq!(read_flags(p), 0b101);
    assert_eq!(read_tag(p), 0xdead_beef_0000_0042);
}

#[test]
fn two_fields_combine() {
    assert_eq!(sum_fields(sample()), 42);
}

#[test]
fn a_flag_bit_is_tested() {
    assert!(flag_is_set(sample()));
    assert!(!flag_is_set(Packet {
        flags: 0b100,
        ..sample()
    }));
}

#[test]
fn a_field_drives_a_branch() {
    assert_eq!(clamp_seq(sample()), 7);
    assert_eq!(
        clamp_seq(Packet {
            seq: 500,
            ..sample()
        }),
        100
    );
}

#[test]
fn a_nested_field_reads_through_the_path() {
    let m = Message {
        header: Header { seq: 9, kind: 2 },
        tag: 1,
    };
    assert_eq!(nested_seq(m), 9);
}

use safetynet::Bytes;

#[derive(Clone, VmLayout)]
struct Msg {
    kind: u8,
    body: Bytes,
    name: String,
    tag: u64,
}

#[safetynet]
fn xor_fold(m: Msg) -> u8 {
    let mut x: u8 = 0;
    for b in m.body.iter() {
        x ^= *b;
    }
    x
}

#[safetynet]
fn byte_sum(m: Msg) -> u32 {
    let mut s: u32 = 0;
    for &b in m.body.iter() {
        s += b as u32;
    }
    s
}

#[safetynet]
fn count_a(m: Msg) -> u32 {
    let mut n: u32 = 0;
    for b in m.name.bytes() {
        if b == b'a' {
            n += 1;
        }
    }
    n
}

#[safetynet]
fn first_nonzero(m: Msg) -> u8 {
    for b in m.body.iter().copied() {
        if b != 0 {
            return b;
        }
    }
    0
}

#[safetynet]
fn lengths(m: Msg) -> u32 {
    (m.body.len() + m.name.len()) as u32
}

#[safetynet]
fn both_short(m: Msg) -> bool {
    m.body.len() < 4 && m.name.len() < 4
}

#[safetynet]
fn kind_plus_bytes(m: Msg) -> u32 {
    let mut s: u32 = m.kind.typed::<u8>() as u32;
    for b in m.body.iter() {
        s += *b as u32;
    }
    for c in m.name.as_bytes().iter() {
        s += *c as u32;
    }
    s
}

#[safetynet]
fn raw_sum(v: Vec<u8>) -> u32 {
    let mut s: u32 = 0;
    for b in v.iter() {
        s += *b as u32;
    }
    s
}

#[safetynet]
fn text_len(s: String) -> u64 {
    s.len() as u64
}

fn msg(body: &str, name: &str) -> Msg {
    Msg {
        kind: 5,
        body: Bytes::from(body),
        name: String::from(name),
        tag: 9,
    }
}

#[test]
fn a_region_is_walked_byte_by_byte() {
    assert_eq!(xor_fold(msg("abc", "")), b'a' ^ b'b' ^ b'c');
    assert_eq!(
        byte_sum(msg("abc", "")),
        u32::from(b'a' + b'b') + u32::from(b'c')
    );
    assert_eq!(xor_fold(msg("", "x")), 0, "empty");
}

#[test]
fn a_string_walks_its_utf8_bytes() {
    assert_eq!(count_a(msg("", "banana")), 3);
    assert_eq!(count_a(msg("aaa", "")), 0, "the other region is not read");
}

#[test]
fn a_walk_may_return_early() {
    assert_eq!(first_nonzero(msg("\0\0\x07\x09", "")), 7);
    assert_eq!(first_nonzero(msg("\0\0", "")), 0);
}

#[test]
fn a_region_length_is_the_header_length() {
    assert_eq!(lengths(msg("abc", "defgh")), 8);
    assert!(both_short(msg("abc", "d")));
    assert!(!both_short(msg("abcd", "d")));
}

#[test]
fn regions_and_scalars_mix() {
    assert_eq!(kind_plus_bytes(msg("\x01\x02", "\x03")), 5 + 1 + 2 + 3);
}

#[test]
fn the_region_may_be_the_whole_input() {
    assert_eq!(raw_sum(vec![1, 2, 3, 250]), 256);
    assert_eq!(raw_sum(Vec::new()), 0);
    assert_eq!(text_len(String::from("héllo")), 6);
}

#[test]
fn a_long_region_is_walked_within_the_fuel() {
    let body: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let expected: u32 = body.iter().map(|b| u32::from(*b)).sum();
    assert_eq!(raw_sum(body), expected);
}
