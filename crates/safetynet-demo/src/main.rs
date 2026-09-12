//! Sample challenge binary.
//!
//! Its job is to be *built in release, stripped, and inspected*: CI will assert
//! that this artifact contains no field-name strings and no assembler symbols.
//! Until there is a VM to embed, it exists to keep the façade linked and the
//! release profile exercised.

use safetynet::{ByteOrder, Order};

fn main() {
    println!(
        "safetynet demo — byte order: {}",
        <Order as ByteOrder>::NAME
    );
}
