use safetynet::safetynet;

#[safetynet]
fn count(x: u32) -> u32 {
    static mut CALLS: u32 = 0;
    x + 1
}

fn main() {}
