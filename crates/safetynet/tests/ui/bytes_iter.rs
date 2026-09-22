use safetynet::{Bytes, VmLayout, safetynet};

#[derive(Clone, VmLayout)]
struct Msg {
    body: Bytes,
    name: String,
}

#[safetynet]
fn xor_body(m: Msg) -> u8 {
    let mut x: u8 = 0;
    for &b in m.body.iter() {
        x ^= b;
    }
    x
}

#[safetynet]
fn name_bytes(m: Msg) -> u32 {
    let mut s: u32 = 0;
    for b in m.name.bytes() {
        s += b as u32;
    }
    s
}

fn main() {
    let m = Msg {
        body: Bytes::from("abc"),
        name: String::from("xy"),
    };
    assert_eq!(xor_body(m.clone()), b'a' ^ b'b' ^ b'c');
    assert_eq!(name_bytes(m), u32::from(b'x') + u32::from(b'y'));
}
