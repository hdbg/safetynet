//! A worked acceptance: a small Feistel cipher compiled by `#[safetynet]`,
//! exercising the whole subset in one algorithm, checked against a plain-Rust
//! twin with proptest.

use proptest::prelude::*;
use safetynet::{Typed, VmLayout, safetynet};

/// One block to transform, with the key material it is keyed by.
#[derive(Clone, Copy, VmLayout)]
struct Block {
    value: u64,
    seed: u64,
    rounds: u64,
}

#[safetynet]
fn encrypt(b: Block) -> u64 {
    let mut n: u64 = b.rounds.typed::<u64>();
    if n == 0 || n > 32 {
        n = 16;
    }
    let mut l: u64 = (b.value.typed::<u64>() >> 32) & 0xFFFFFFFF;
    let mut r: u64 = b.value & 0xFFFFFFFF;
    let mut i: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        // The round key: a small mix of the seed and the round number.
        let mut k: u64 = b.seed.typed::<u64>() ^ (i * 0x9E3779B97F4A7C15);
        let mut j: u64 = 0;
        while j < 5 {
            if j == 2 {
                j += 1;
                continue;
            }
            k ^= k >> 17;
            k *= 0xBF58476D1CE4E5B9;
            k ^= (!k) >> 13;
            k += b.seed / (i + 1);
            j += 1;
        }
        k &= 0xFFFFFFFF;
        // The round function, applied to the right half.
        let mut y: u64 = (r ^ k) * 0x9E3779B1;
        y &= 0xFFFFFFFF;
        let s: u64 = (k % 31) + 1;
        y = ((y << s) | (y >> (32 - s))) & 0xFFFFFFFF;
        if (k & 1) != 0 {
            y += 0x7F4A7C15;
        } else {
            y ^= 0x1B873593;
        }
        y &= 0xFFFFFFFF;
        // The Feistel step: (l, r) becomes (r, l ^ F(r, k)).
        let t: u64 = l ^ y;
        l = r;
        r = t & 0xFFFFFFFF;
        i += 1;
    }
    (l << 32) | r
}

#[safetynet]
fn decrypt(b: Block) -> u64 {
    let mut n: u64 = b.rounds.typed::<u64>();
    if n == 0 || n > 32 {
        n = 16;
    }
    let mut l: u64 = (b.value.typed::<u64>() >> 32) & 0xFFFFFFFF;
    let mut r: u64 = b.value & 0xFFFFFFFF;
    for i in 0..n {
        // The round keys are replayed in reverse.
        let rd: u64 = n - 1 - i;
        let mut k: u64 = b.seed.typed::<u64>() ^ (rd * 0x9E3779B97F4A7C15);
        let mut j: u64 = 0;
        while j < 5 {
            if j == 2 {
                j += 1;
                continue;
            }
            k ^= k >> 17;
            k *= 0xBF58476D1CE4E5B9;
            k ^= (!k) >> 13;
            k += b.seed / (rd + 1);
            j += 1;
        }
        k &= 0xFFFFFFFF;
        let mut y: u64 = (l ^ k) * 0x9E3779B1;
        y &= 0xFFFFFFFF;
        let s: u64 = (k % 31) + 1;
        y = ((y << s) | (y >> (32 - s))) & 0xFFFFFFFF;
        if (k & 1) != 0 {
            y += 0x7F4A7C15;
        } else {
            y ^= 0x1B873593;
        }
        y &= 0xFFFFFFFF;
        // The inverse step: (l, r) becomes (r ^ F(l, k), l).
        let t: u64 = r ^ y;
        r = l;
        l = t & 0xFFFFFFFF;
    }
    (l << 32) | r
}

const GOLDEN: u64 = 0x9E3779B97F4A7C15;
const MIX: u64 = 0xBF58476D1CE4E5B9;
const C1: u64 = 0x9E3779B1;
const C2: u64 = 0x7F4A7C15;
const C3: u64 = 0x1B873593;
const MASK: u64 = 0xFFFFFFFF;

/// A round of iterations the machine can afford is 1..=32; anything else runs
/// the default. Both directions clamp the same way, so they stay inverses.
fn clamp(rounds: u64) -> u64 {
    if rounds == 0 || rounds > 32 {
        16
    } else {
        rounds
    }
}

/// The round key, matching the inlined schedule above.
fn key(seed: u64, rd: u64) -> u64 {
    let mut k = seed ^ rd.wrapping_mul(GOLDEN);
    let mut j: u64 = 0;
    while j < 5 {
        if j == 2 {
            j += 1;
            continue;
        }
        k ^= k >> 17;
        k = k.wrapping_mul(MIX);
        k ^= (!k) >> 13;
        k = k.wrapping_add(seed / (rd + 1));
        j += 1;
    }
    k & MASK
}

/// The round function, matching the inlined one above.
fn round(x: u64, k: u64) -> u64 {
    let mut y = (x ^ k).wrapping_mul(C1) & MASK;
    let s = (k % 31) + 1;
    y = ((y << s) | (y >> (32 - s))) & MASK;
    if (k & 1) != 0 {
        y = y.wrapping_add(C2);
    } else {
        y ^= C3;
    }
    y & MASK
}

fn encrypt_ref(b: Block) -> u64 {
    let n = clamp(b.rounds);
    let mut l = (b.value >> 32) & MASK;
    let mut r = b.value & MASK;
    for i in 0..n {
        let t = l ^ round(r, key(b.seed, i));
        l = r;
        r = t & MASK;
    }
    (l << 32) | r
}

fn decrypt_ref(b: Block) -> u64 {
    let n = clamp(b.rounds);
    let mut l = (b.value >> 32) & MASK;
    let mut r = b.value & MASK;
    for i in 0..n {
        let rd = n - 1 - i;
        let t = r ^ round(l, key(b.seed, rd));
        r = l;
        l = t & MASK;
    }
    (l << 32) | r
}

proptest! {
    #[test]
    fn encrypt_matches_the_reference(value: u64, seed: u64, rounds in 0u64..40) {
        let b = Block { value, seed, rounds };
        prop_assert_eq!(encrypt(b), encrypt_ref(b));
    }

    #[test]
    fn decrypt_matches_the_reference(value: u64, seed: u64, rounds in 0u64..40) {
        let b = Block { value, seed, rounds };
        prop_assert_eq!(decrypt(b), decrypt_ref(b));
    }

    #[test]
    fn decrypt_undoes_encrypt(value: u64, seed: u64, rounds in 0u64..40) {
        let ciphertext = encrypt(Block { value, seed, rounds });
        let plaintext = decrypt(Block { value: ciphertext, seed, rounds });
        prop_assert_eq!(plaintext, value);
    }
}

/// The edges proptest reaches only by chance, pinned down.
#[test]
fn it_round_trips_at_the_extremes() {
    for value in [0, 1, u64::MAX, 0xFFFF_FFFF, 0x1_0000_0000] {
        for rounds in [0, 1, 32, 33, 100] {
            let seed = 0xC0FF_EE00_1234_5678;
            let ciphertext = encrypt(Block {
                value,
                seed,
                rounds,
            });
            assert_eq!(
                encrypt(Block {
                    value,
                    seed,
                    rounds
                }),
                encrypt_ref(Block {
                    value,
                    seed,
                    rounds
                })
            );
            assert_eq!(
                decrypt(Block {
                    value: ciphertext,
                    seed,
                    rounds
                }),
                value
            );
        }
    }
}
