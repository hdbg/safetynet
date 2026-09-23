//! Sample challenge binary: an LCG keystream cipher lowered by `#[safetynet]`.
//!
//! The guest takes a chunk of up to eight bytes as a byte region, walks it, and
//! returns the ciphertext packed into a word, since a run returns one word.
//! The keystream's parameters are constants of the guest itself, folded into
//! its bytecode; the host passes only what changes per chunk. Build in release
//! to inspect the embedded bytecode without the compiler's reference function.

use std::error::Error;
use std::fmt::Write as _;

use safetynet::{Bytes, Typed, VmLayout, safetynet};

/// One chunk of the message and its position in it.
#[derive(Debug, Clone, VmLayout)]
struct Config {
    chunk: Bytes,
    offset: u64,
    passthrough: bool,
}

/// XOR a chunk with its part of the LCG keystream, packing the result
/// little-endian. Arithmetic wraps at the VM's word width, including in debug
/// builds.
#[safetynet]
fn cipher(config: Config) -> u64 {
    // Declared here rather than at module level: the macro sees only the
    // function's tokens, so these fold into the bytecode as immediates.
    const SEED: u64 = 0x2545_f491_4f6c_dd1d;
    const MUL: u64 = 0x5851_f42d_4c95_7f2d;
    const INC: u64 = 0x1405_7b7e_f767_814f;

    let mut state: u64 = SEED;
    // Advance to this chunk using exponentiation of the affine LCG step.
    // This keeps each chunk independent without replaying the entire prefix.
    let mut offset: u64 = config.offset.typed::<u64>();
    let mut multiplier: u64 = MUL;
    let mut increment: u64 = INC;
    while offset > 0 {
        if offset & 1 != 0 {
            state = state * multiplier + increment;
        }
        increment *= multiplier + 1;
        multiplier *= multiplier;
        offset >>= 1;
    }

    let passthrough: bool = config.passthrough.typed::<bool>();
    let mut output: u64 = 0;
    let mut shift: u64 = 0;
    for byte in config.chunk.iter() {
        state = state * MUL + INC;
        let key: u64 = if passthrough {
            0
        } else {
            (state ^ (state >> 33)) & 0xff
        };
        output |= ((*byte as u64) ^ key) << shift;
        shift += 8;
    }
    output
}

/// Hand each chunk to the generated wrapper and unpack its returned word.
fn transform(input: &[u8], passthrough: bool) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    for chunk in input.chunks(8) {
        let result = cipher(Config {
            chunk: Bytes::from(chunk),
            offset: output.len() as u64,
            passthrough,
        });
        output.extend(result.to_le_bytes().into_iter().take(chunk.len()));
    }
    output
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let plaintext: &[u8] = b"attack at dawn";
    println!("safetynet demo — lcg keystream cipher lowered with #[safetynet]");
    println!(
        "plaintext   {}  {}",
        hex(plaintext),
        String::from_utf8_lossy(plaintext)
    );
    let encrypted = transform(plaintext, false);
    println!("ciphertext  {}", hex(&encrypted));
    let decrypted = transform(&encrypted, false);
    println!(
        "decrypted   {}  {}",
        hex(&decrypted),
        String::from_utf8_lossy(&decrypted)
    );
    println!("passthrough {}", hex(&transform(plaintext, true)));
    if decrypted == plaintext {
        println!("round trip ok");
        Ok(())
    } else {
        Err("the round trip did not restore the plaintext".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sequential model of the original cipher, independent of chunk packing
    /// and the guest's LCG skip-ahead calculation. The parameters are a copy
    /// of the guest's, which nothing outside its body can read; the pinned
    /// ciphertext below is what keeps the two from drifting apart.
    fn model(input: &[u8]) -> Vec<u8> {
        const SEED: u64 = 0x2545_f491_4f6c_dd1d;
        const MUL: u64 = 0x5851_f42d_4c95_7f2d;
        const INC: u64 = 0x1405_7b7e_f767_814f;
        let mut state = SEED;
        input
            .iter()
            .map(|byte| {
                state = state.wrapping_mul(MUL).wrapping_add(INC);
                byte ^ ((state ^ (state >> 33)) & 0xff) as u8
            })
            .collect()
    }

    #[test]
    fn cipher_agrees_with_original_model_and_round_trips() {
        let all_bytes: Vec<u8> = (0..=255).cycle().take(1025).collect();
        for len in [0, 1, 7, 8, 9, 14, 15, 16, 17, 255, 256, 257, 1025] {
            let input: Vec<u8> = all_bytes.iter().copied().take(len).collect();
            let actual = transform(&input, false);
            assert_eq!(actual, model(&input), "ciphertext for {len} bytes");
            assert_eq!(
                transform(&actual, false),
                input,
                "round trip for {len} bytes"
            );
        }
        assert_eq!(
            hex(&transform(b"attack at dawn", false)),
            "cce073a7cb329379da478e43918e"
        );
    }

    #[test]
    fn passthrough_leaves_input_alone() {
        for input in [b"".as_slice(), b"a", b"attack at dawn"] {
            assert_eq!(transform(input, true), input);
        }
    }
}
