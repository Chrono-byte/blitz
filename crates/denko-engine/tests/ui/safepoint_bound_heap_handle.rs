extern crate thiserror;

include!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../../../crates/denko-engine/tests/ui_support.rs"
));

use value::heap::HeapArena;
use value::scope::GcContext;

fn main() {
  let mut context = GcContext::new();
  let mut gc = context.scope();
  let mut arena = HeapArena::new();
  let handle = arena.allocate((), gc.nogc()).unwrap();

  let _safepoint = gc.reborrow();
  let _ = arena.get(handle, gc.nogc());
}
