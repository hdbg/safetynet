//! Snapshot of what `#[derive(VmLayout)]` expands to, checked against the
//! committed `.expanded.rs` files. Regenerate with `MACROTEST=overwrite`.

#[test]
fn derive_vm_layout_expands() {
    macrotest::expand("tests/expand/*.rs");
}
