use safetynet::{VmLayout, safetynet};

#[derive(Clone, Copy, VmLayout)]
struct Frame {
    len: u16,
}

#[safetynet]
fn len_is_zero(f: Frame) -> bool {
    f.len == 0
}

fn main() {}
