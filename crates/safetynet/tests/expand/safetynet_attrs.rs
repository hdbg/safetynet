#![allow(dead_code)]

use safetynet::safetynet;

#[safetynet]
#[must_use]
fn doubled(x: u32) -> u32 {
    x * 2
}

fn main() {}
