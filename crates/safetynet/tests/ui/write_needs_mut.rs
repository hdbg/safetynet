use safetynet::{Bytes, VmLayout, safetynet};

#[derive(Clone, VmLayout)]
struct Msg {
    body: Bytes,
}

// The parameter is not `mut`, so the reference copy cannot write to it. The
// macro does not check mutability; rustc rejects the hidden copy instead.
#[safetynet]
fn poke(m: Msg) -> u8 {
    m.body[0] = 1;
    0
}

fn main() {}
