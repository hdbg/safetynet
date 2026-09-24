use safetynet::{Bytes, VmLayout, safetynet};

#[derive(Clone, VmLayout)]
struct Msg {
    body: Bytes,
    name: String,
}

#[safetynet]
fn payload(m: Msg) -> Bytes {
    Bytes::from(&m.body[2..])
}

#[safetynet]
fn whole_name(m: Msg) -> String {
    m.name
}

fn main() {
    let m = Msg {
        body: Bytes::from("abcdef"),
        name: String::from("xy"),
    };
    assert_eq!(payload(m.clone()).as_slice(), b"cdef");
    assert_eq!(whole_name(m), "xy");
}
