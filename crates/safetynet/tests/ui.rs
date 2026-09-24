//! Valid subset code must expand and run; code the machine cannot hold must
//! be declined with an error spanned to the offending source, not a panic
//! inside the generated constants.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/value_block_local.rs");
    t.pass("tests/ui/field_width_match.rs");
    t.pass("tests/ui/bytes_iter.rs");
    t.pass("tests/ui/consts.rs");
    t.pass("tests/ui/return_slice.rs");
    t.compile_fail("tests/ui/iter_scalar_field.rs");
    t.compile_fail("tests/ui/const_outside_body.rs");
    t.compile_fail("tests/ui/static_mut.rs");
    t.compile_fail("tests/ui/u16_field.rs");
    t.compile_fail("tests/ui/u16_param.rs");
    t.compile_fail("tests/ui/wrong_field_type.rs");
}
