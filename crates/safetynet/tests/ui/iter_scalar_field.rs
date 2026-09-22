use safetynet::{VmLayout, safetynet};

#[derive(Clone, Copy, VmLayout)]
struct Counter {
    seq: u32,
}

#[safetynet]
fn walk(p: Counter) -> u32 {
    let mut n: u32 = 0;
    for _ in p.seq.iter() {
        n += 1;
    }
    n
}

fn main() {}
