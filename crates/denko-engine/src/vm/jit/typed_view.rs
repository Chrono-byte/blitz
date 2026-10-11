// SPDX-License-Identifier: MPL-2.0
pub(crate) use super::native::typed_view::TypedViewFrame;
use super::*;
use crate::vm::region::typed_view::{TypedViewProgram, TypedViewRegion};
use std::sync::Arc;

pub(crate) struct CompiledTypedView {
  _owner: Arc<Chunk>,
  pub plan: TypedViewProgram,
  native: Option<NativeCode>,
}
impl CompiledTypedView {
  pub(crate) fn is_native(&self) -> bool {
    self.native.is_some()
  }
  pub(crate) fn execute(
    &self,
    frame: &mut TypedViewFrame,
    elements: &mut [u8],
  ) {
    #[cfg(all(unix, target_arch = "x86_64"))]
    // SAFETY: native code belongs to this decoded typed plan. It checks every
    // dynamic index against the number of complete eight-byte elements. MOVSD
    // supports unaligned addresses, and x86-64 uses the backing store's little
    // endian representation. No aligned Rust f64 reference is created. Both
    // exclusive borrows outlive the call, which invokes no callbacks or allocation.
    unsafe {
      self
        .native
        .as_ref()
        .expect("native plan")
        .call_typed_view(frame, elements);
    }
    #[cfg(not(all(unix, target_arch = "x86_64")))]
    {
      let _ = (frame, elements);
      unreachable!("typed view native code unavailable");
    }
  }
}
impl JitRuntime {
  pub(crate) fn typed_view(
    &mut self,
    owner: &Arc<Chunk>,
    region: TypedViewRegion,
  ) -> Option<Rc<CompiledTypedView>> {
    let key = (Arc::as_ptr(owner) as usize, region.entry_pc);
    if let Some(compiled) = self.typed_views.get(&key) {
      return Some(Rc::clone(compiled));
    }
    let plan = region.program(owner)?;
    let native = NativeCode::typed_view(&plan).ok();
    let compiled = Rc::new(CompiledTypedView {
      _owner: Arc::clone(owner),
      plan,
      native,
    });
    self.typed_views.insert(key, Rc::clone(&compiled));
    Some(compiled)
  }
}

#[cfg(all(test, unix, target_arch = "x86_64"))]
mod tests {
  use super::*;

  #[test]
  fn native_byte_access_handles_unaligned_slices_and_partial_elements() {
    let session = crate::parser::Parser::parse_source(
      "function fill(values) { for (var i = 0; i < values.length; i = i + 1) values[i] = i / -2; }",
    )
    .expect("parse");
    let root = crate::compiler::compile_session(&session).expect("compile");
    let owner = Arc::clone(&root.functions[0].chunk);
    let region = (0..owner.words.len())
      .find_map(|pc| TypedViewRegion::decode(&owner, pc))
      .expect("typed region");
    let plan = region.program(&owner).expect("plan");
    let compiled = CompiledTypedView {
      native: Some(NativeCode::typed_view(&plan).expect("native code")),
      plan,
      _owner: owner,
    };
    #[repr(align(8))]
    struct AlignedBytes([u8; 26]);
    let mut backing = AlignedBytes([0xa5u8; 26]);
    let storage = &mut backing.0;
    let mut frame = TypedViewFrame::new([0.0; 16], 4);
    compiled.execute(&mut frame, &mut storage[1..]);
    assert_eq!(frame.iterations, 3);
    assert_eq!(frame.exit, 2);
    assert_eq!(storage[0], 0xa5);
    assert_eq!(storage[25], 0xa5);
    for (index, expected) in [-0.0f64, -0.5, -1.0].into_iter().enumerate() {
      assert_eq!(
        &storage[1 + index * 8..9 + index * 8],
        &expected.to_le_bytes()
      );
    }
  }
}
