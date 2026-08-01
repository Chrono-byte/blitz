extern crate thiserror;

include!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../../../crates/denko-engine/tests/ui_support.rs"
));

use value::scope::{GcContext, RootSet};
use value::tagged::{HeapHandle, HeapKind, TaggedValue};

fn main() {
  let roots = RootSet::new();
  let mut context = GcContext::new();
  let mut gc = context.scope();
  let value = TaggedValue::heap(HeapHandle::new(HeapKind::Object, 7)).bind(gc.nogc());
  let rooted = roots.root(value, gc.nogc()).unwrap();

  roots.rewrite_heap_handles(
    |handle| HeapHandle::new(handle.kind(), 3),
    gc.reborrow(),
  );

  let rebound = rooted.get(gc.nogc()).unwrap();
  assert_eq!(rebound.as_heap().map(HeapHandle::index), Some(3));
}
