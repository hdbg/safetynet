use safetynet::{VmLayout, safetynet};

#[derive(Clone, Copy, VmLayout)]
struct Counter {
    seq: u32,
}

#[safetynet]
fn below_limit(p: Counter) -> bool {
    let limit: u32 = 10;
    p.seq < limit
}

fn main() {
    assert!(below_limit(Counter { seq: 3 }));
    assert!(!below_limit(Counter { seq: 30 }));
}
