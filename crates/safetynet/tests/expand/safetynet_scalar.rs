#![allow(dead_code)]

use safetynet::safetynet;

#[safetynet]
fn inc(x: u32) -> u32 {
    x + 1
}

fn main() {}
