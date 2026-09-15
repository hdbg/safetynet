use safetynet::{Typed, VmLayout, safetynet};

#[derive(Clone, Copy, VmLayout)]
struct Counter {
    seq: u32,
}

#[safetynet]
fn widened(p: Counter) -> u64 {
    p.seq.typed::<u64>()
}

fn main() {}
