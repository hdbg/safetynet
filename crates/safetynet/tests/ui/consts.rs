use safetynet::safetynet;

#[safetynet]
fn keyed(x: u8) -> u8 {
    const SEED: u8 = 0x5a;
    static KEY: [u8; 4] = [1, 2, 4, 8];
    const NAME: &'static str = "ab";
    let mut out: u8 = x ^ SEED;
    for k in KEY.iter() {
        out ^= *k;
    }
    for b in NAME.bytes() {
        out += b;
    }
    out + KEY[(x & 3) as usize]
}

fn main() {
    let expected: u8 = (0x5a ^ 1 ^ 2 ^ 4 ^ 8u8)
        .wrapping_add(b'a')
        .wrapping_add(b'b')
        .wrapping_add(1);
    assert_eq!(keyed(0), expected);
}
