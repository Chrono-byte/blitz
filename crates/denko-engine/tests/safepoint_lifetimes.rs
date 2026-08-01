#[test]
fn safepoint_lifetime_ui() {
  let tests = trybuild::TestCases::new();
  tests.compile_fail("tests/ui/safepoint_bound_tagged_value.rs");
  tests.compile_fail("tests/ui/safepoint_bound_heap_handle.rs");
  tests.compile_fail("tests/ui/collection_invalidates_bound_handle.rs");
  tests.pass("tests/ui/scoped_root_rebind.rs");
}
