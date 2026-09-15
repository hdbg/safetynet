use safetynet::safetynet;

#[safetynet]
fn pick(c: bool) -> u32 {
    let x: u32 = if c {
        let y: u32 = 1;
        y + 1
    } else {
        0
    };
    x
}

fn main() {
    assert_eq!(pick(true), 2);
    assert_eq!(pick(false), 0);
}
