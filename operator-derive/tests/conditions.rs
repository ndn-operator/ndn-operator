#[test]
fn derives_conditions_impl_and_errors_without_field() {
    let t = trybuild::TestCases::new();
    t.pass("tests/fixtures/derive_pass.rs");
    t.compile_fail("tests/fixtures/derive_fail.rs");
}
