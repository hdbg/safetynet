use safetynet::safetynet;

const SEED: u64 = 0x2545_f491_4f6c_dd1d;

#[safetynet]
fn keyed(x: u64) -> u64 {
    x ^ SEED
}

fn main() {}
