extern crate thiserror;

include!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../../../crates/denko-engine/tests/ui_support.rs"
));

use value::scope::GcContext;
use value::tagged::{HeapHandle, HeapKind, TaggedValue};

fn main() {
  let mut context = GcContext::new();
  let mut gc = context.scope();
  let value = TaggedValue::heap(HeapHandle::new(HeapKind::Object, 7)).bind(gc.nogc());

  let _safepoint = gc.reborrow();
  let _ = value.as_heap();
}
