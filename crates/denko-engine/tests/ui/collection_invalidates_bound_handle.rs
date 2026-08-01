extern crate thiserror;

include!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../../../crates/denko-engine/tests/ui_support.rs"
));

use value::heap::{Compaction, HeapArena, HeapIndex, HeapMarkAndSweep};
use value::scope::GcContext;

struct Node;

impl HeapMarkAndSweep for Node {
  fn mark_values(&self, _visitor: &mut dyn FnMut(HeapIndex<'static, Self>)) {}

  fn sweep_values(&mut self, _compaction: &Compaction) {}
}

fn main() {
  let mut context = GcContext::new();
  let mut gc = context.scope();
  let mut arena = HeapArena::new();
  let handle = arena.allocate(Node, gc.nogc()).unwrap();
  let stored_root = handle.unbind();

  arena.collect([stored_root], gc.reborrow()).unwrap();
  let _ = handle.index();
}
