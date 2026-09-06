// SPDX-License-Identifier: MPL-2.0
pub(crate) use super::native::typed_view::TypedViewFrame;
use super::*;
use crate::vm::region::typed_view::{TypedViewProgram, TypedViewRegion};
use std::sync::Arc;

pub(crate) struct CompiledTypedView {
  _owner: Arc<Chunk>,
  pub plan: TypedViewProgram,
  native: NativeCode,
}
impl CompiledTypedView {
  pub(crate) fn execute(
    &self,
    frame: &mut TypedViewFrame,
    elements: &mut [f64],
  ) {
    #[cfg(all(unix, target_arch = "x86_64"))]
    // SAFETY: native code belongs to this decoded typed plan. It checks every
    // dynamic index against the supplied slice's actual length; both exclusive
    // borrows outlive the call, which invokes no VM callbacks or allocation.
    unsafe {
      self.native.call_typed_view(frame, elements);
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
    let native = NativeCode::typed_view(&plan).ok()?;
    let compiled = Rc::new(CompiledTypedView {
      _owner: Arc::clone(owner),
      plan,
      native,
    });
    self.typed_views.insert(key, Rc::clone(&compiled));
    Some(compiled)
  }
}
