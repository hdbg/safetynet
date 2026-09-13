#![allow(dead_code)]

use safetynet::VmLayout;

#[derive(VmLayout)]
struct Header {
    seq: u32,
    flags: u8,
}

#[derive(VmLayout)]
struct Packet {
    kind: u8,
    header: Header,
    len: u16,
    tag: u64,
}

fn main() {}
