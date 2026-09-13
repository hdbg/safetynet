#![allow(dead_code)]

use safetynet::VmValue;

#[derive(VmValue, Clone, Copy)]
enum Kind {
    A,
    B,
    C,
}

#[derive(VmValue, Clone, Copy)]
#[repr(u8)]
enum Tag {
    Lo = 3,
    Hi = 200,
}

fn main() {}
