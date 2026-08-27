// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Vm {
  pub(super) fn try_direct_baseline_call(
    &mut self,
    call_site: usize,
    proto: &Rc<crate::bytecode::FunctionProto>,
    this_value: &Value,
    arguments: &[Value],
  ) -> Result<Option<Value>, RuntimeError> {
    let cached = self
      .caches
      .active_direct_calls
      .borrow()
      .get(call_site)
      .and_then(Clone::clone)
      .filter(|entry| Rc::ptr_eq(&entry.expected_proto, proto));
    if let Some(entry) = cached {
      let result = entry
        .target
        .call(crate::vm::jit::DirectCallInputs { arguments });
      if matches!(
        entry.target.leaf(),
        crate::vm::jit::DirectLeaf::NumericBinary(_)
      ) {
        self
          .profiling
          .jit
          .record_optimizing_call(result.is_ok());
      }
      if let Ok(result) = result {
        self.profiling.jit_stats.baseline_entries = self
          .profiling
          .jit_stats
          .baseline_entries
          .saturating_add(1);
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        return Ok(Some(result));
      }
    }
    let chunk = &proto.chunk;
    let parameter_count = proto.arity;
    let function = self.function_id_for_chunk(chunk);
    if arguments.len() >= 2
      && matches!(arguments[0], Value::Number(_))
      && matches!(arguments[1], Value::Number(_))
      && let Some(target) = self
        .profiling
        .jit
        .optimizing_numeric_binary_target(function, chunk, parameter_count)
        .map_err(RuntimeError::from)?
    {
      if let Some(slot) = self
        .caches
        .active_direct_calls
        .borrow_mut()
        .get_mut(call_site)
      {
        *slot = Some(crate::vm::state::DirectCallSite {
          expected_proto: Rc::clone(proto),
          target: target.clone(),
        });
      }
      let result = target.call(crate::vm::jit::DirectCallInputs { arguments });
      self
        .profiling
        .jit
        .record_optimizing_call(result.is_ok());
      if let Ok(result) = result {
        self.profiling.jit_stats.baseline_entries = self
          .profiling
          .jit_stats
          .baseline_entries
          .saturating_add(1);
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        return Ok(Some(result));
      }
    }
    let profile = self
      .profiling
      .functions
      .entry(function)
      .or_default();
    profile.parameter_count = parameter_count;
    profile.record_call();
    self.process_baseline_request(function);
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)))
    {
      return Ok(None);
    }
    if let Some(target) = self.profiling.jit.direct_call_target(function) {
      if let Some(slot) = self
        .caches
        .active_direct_calls
        .borrow_mut()
        .get_mut(call_site)
      {
        *slot = Some(crate::vm::state::DirectCallSite {
          expected_proto: Rc::clone(proto),
          target: target.clone(),
        });
      }
      let result = target
        .call(crate::vm::jit::DirectCallInputs { arguments })
        .map_err(RuntimeError::from)?;
      self.profiling.jit_stats.baseline_entries = self
        .profiling
        .jit_stats
        .baseline_entries
        .saturating_add(1);
      self.profiling.jit_stats.baseline_returns = self
        .profiling
        .jit_stats
        .baseline_returns
        .saturating_add(1);
      return Ok(Some(result));
    }
    let mut registers: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
    registers[1] = this_value.clone();
    let mut result = Value::Undefined;
    let mut frame = crate::vm::jit::native::JitFrame {
      vm: self as *mut Vm,
      function_id: 0,
      vm_pc: 0,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
      arguments: arguments.as_ptr(),
      argument_count: arguments.len().min(parameter_count),
      locals: std::ptr::null_mut(),
      local_count: 0,
    };
    let Some(exit) = self.profiling.jit.execute(function, &mut frame) else {
      return Ok(None);
    };
    if crate::vm::jit::native::decode_exit(exit).map_err(RuntimeError::from)?
      != crate::vm::jit::native::JitExit::Return
    {
      return Ok(None);
    }
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    self.profiling.jit_stats.baseline_returns = self
      .profiling
      .jit_stats
      .baseline_returns
      .saturating_add(1);
    Ok(Some(result))
  }

  pub(super) fn try_cached_numeric_call(
    &mut self,
    call_site: usize,
    callee: &Value,
    arguments: &[Value],
  ) -> Result<Option<Value>, RuntimeError> {
    let Value::Closure { proto, .. } = callee else {
      return Ok(None);
    };
    let result = {
      let direct_calls = self.caches.active_direct_calls.borrow();
      let Some(entry) = direct_calls
        .get(call_site)
        .and_then(Option::as_ref)
        .filter(|entry| Rc::ptr_eq(&entry.expected_proto, proto))
        .filter(|entry| {
          matches!(
            entry.target.leaf(),
            crate::vm::jit::DirectLeaf::NumericBinary(_)
          )
        })
      else {
        return Ok(None);
      };
      entry
        .target
        .call(crate::vm::jit::DirectCallInputs { arguments })
    };
    self
      .profiling
      .jit
      .record_optimizing_call(result.is_ok());
    Ok(result.ok())
  }

  pub(super) fn try_run_nested_baseline(
    &mut self,
    registers: &mut [Value; 16],
    record_call: bool,
  ) -> Result<Option<Value>, RuntimeError> {
    let function = self.active_function_id();
    if record_call {
      self
        .profiling
        .functions
        .entry(function)
        .or_default()
        .record_call();
      self.process_baseline_request(function);
    }
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)))
    {
      return Ok(None);
    }
    let mut result = Value::Undefined;
    let locals = (&mut self.execution.active.locals as *mut Vec<SharedValue>).cast();
    let local_count = self.execution.active.locals.len();
    let mut frame = crate::vm::jit::native::JitFrame {
      vm: self as *mut Vm,
      function_id: 0,
      vm_pc: self.execution.active.ip,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals,
      local_count,
    };
    let Some(exit) = self.profiling.jit.execute(function, &mut frame) else {
      return Ok(None);
    };
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    match crate::vm::jit::native::decode_exit(exit).map_err(RuntimeError::from)? {
      crate::vm::jit::native::JitExit::Return => {
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        Ok(Some(result))
      }
      crate::vm::jit::native::JitExit::ResumeInterpreter
      | crate::vm::jit::native::JitExit::Invalidated => {
        self.profiling.jit_stats.interpreter_resumes = self
          .profiling
          .jit_stats
          .interpreter_resumes
          .saturating_add(1);
        self.execution.active.ip = frame.vm_pc;
        Ok(None)
      }
      crate::vm::jit::native::JitExit::NextBlock => {
        Err(RuntimeError::from("unresolved nested baseline block exit"))
      }
      crate::vm::jit::native::JitExit::Exception => {
        Err(RuntimeError::from("baseline JIT reported an exception"))
      }
    }
  }

  fn process_baseline_request(
    &mut self,
    function: crate::vm::jit::FunctionId,
  ) {
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| profile.baseline == crate::vm::jit::BaselineState::Requested)
    {
      return;
    }
    self
      .profiling
      .functions
      .entry(function)
      .or_default()
      .baseline = crate::vm::jit::BaselineState::Compiling;
    let chunk = self
      .profiling
      .chunk_ids
      .iter()
      .find_map(|(identity, id)| {
        (*id == function)
          .then(|| self.profiling.chunk_refs.get(identity).cloned())
          .flatten()
      });
    let parameter_count = self
      .profiling
      .functions
      .get(&function)
      .map_or(0, |profile| profile.parameter_count);
    let state = chunk
      .as_deref()
      .and_then(|chunk| {
        self
          .profiling
          .jit
          .install_const_prefix(function, chunk, parameter_count)
          .ok()
      })
      .map_or(
        crate::vm::jit::BaselineState::Disabled,
        crate::vm::jit::BaselineState::Ready,
      );
    self
      .profiling
      .functions
      .entry(function)
      .or_default()
      .baseline = state;
  }

  pub(super) fn process_baseline_requests(&mut self) {
    let requested: Vec<_> = self
      .profiling
      .functions
      .iter()
      .filter_map(|(function, profile)| {
        (profile.baseline == crate::vm::jit::BaselineState::Requested).then_some(*function)
      })
      .collect();
    for function in requested {
      self.process_baseline_request(function);
    }
  }

  pub(super) fn try_run_baseline(
    &mut self,
    state: &mut RiscExecutionState,
  ) -> Result<Option<RiscRunOutcome>, RuntimeError> {
    let function = self.active_function_id();
    let baseline_ready = self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)));
    if !baseline_ready {
      return Ok(None);
    }
    let mut result = Value::Undefined;
    let locals = (&mut self.execution.active.locals as *mut Vec<SharedValue>).cast();
    let local_count = self.execution.active.locals.len();
    let mut frame = crate::vm::jit::native::JitFrame {
      vm: self as *mut Vm,
      function_id: 0,
      vm_pc: self.execution.active.ip,
      registers: state.registers.as_mut_ptr(),
      register_count: state.registers.len(),
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals,
      local_count,
    };
    let Some(exit) = self.profiling.jit.execute(function, &mut frame) else {
      return Ok(None);
    };
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    match crate::vm::jit::native::decode_exit(exit).map_err(RuntimeError::from)? {
      crate::vm::jit::native::JitExit::Return => {
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        Ok(Some(RiscRunOutcome::Return(result)))
      }
      crate::vm::jit::native::JitExit::ResumeInterpreter => {
        self.profiling.jit_stats.interpreter_resumes = self
          .profiling
          .jit_stats
          .interpreter_resumes
          .saturating_add(1);
        self.execution.active.ip = frame.vm_pc;
        Ok(None)
      }
      crate::vm::jit::native::JitExit::Invalidated => Ok(None),
      crate::vm::jit::native::JitExit::NextBlock => {
        Err(RuntimeError::from("unresolved baseline block exit"))
      }
      crate::vm::jit::native::JitExit::Exception => {
        Err(RuntimeError::from("baseline JIT reported an exception"))
      }
    }
  }

  fn function_id_for_chunk(
    &mut self,
    chunk: &std::sync::Arc<crate::bytecode::Chunk>,
  ) -> crate::vm::jit::FunctionId {
    let chunk_identity = std::sync::Arc::as_ptr(chunk) as usize;
    if let Some(id) = self
      .profiling
      .chunk_ids
      .get(&chunk_identity)
      .copied()
    {
      return id;
    }
    self.profiling.next_function_id = self.profiling.next_function_id.saturating_add(1);
    let id = crate::vm::jit::FunctionId::new(self.profiling.next_function_id);
    self
      .profiling
      .chunk_refs
      .insert(chunk_identity, std::sync::Arc::clone(chunk));
    self
      .profiling
      .chunk_ids
      .insert(chunk_identity, id);
    id
  }

  pub(super) fn active_function_id(&mut self) -> crate::vm::jit::FunctionId {
    let identity = Arc::as_ptr(&self.execution.active.chunk) as usize;
    if self.profiling.active_function_chunk == identity
      && let Some(function) = self.profiling.active_function_id
    {
      return function;
    }
    let chunk = std::sync::Arc::clone(&self.execution.active.chunk);
    let function = self.function_id_for_chunk(&chunk);
    self.profiling.active_function_chunk = identity;
    self.profiling.active_function_id = Some(function);
    function
  }

  pub(super) fn record_jit_backedge(
    &mut self,
    pc: usize,
  ) {
    let chunk_identity = Arc::as_ptr(&self.execution.active.chunk) as usize;
    if self.profiling.last_hot_chunk == chunk_identity
      || self
        .profiling
        .hot_chunks
        .contains(&chunk_identity)
    {
      self.profiling.last_hot_chunk = chunk_identity;
      return;
    }
    let function_id = self.active_function_id();
    let profile = self
      .profiling
      .functions
      .entry(function_id)
      .or_default();
    profile.record_backedge(pc);
    if matches!(profile.baseline, crate::vm::jit::BaselineState::Requested) {
      self.profiling.hot_chunks.insert(chunk_identity);
      self.profiling.last_hot_chunk = chunk_identity;
    }
  }

  pub(super) fn site_action(&mut self) -> SiteAction {
    let identity = Arc::as_ptr(&self.execution.active.chunk) as usize;
    if self.caches.active_quickened_chunk != identity {
      let sites = if let Some(plan) = self.caches.quickened.get(&identity) {
        self.caches.active_direct_calls = Rc::clone(&plan.direct_calls);
        self.caches.active_regions = Arc::clone(&plan.regions);
        Arc::clone(&plan.sites)
      } else {
        let chunk = Arc::clone(&self.execution.active.chunk);
        let (sites, regions) = build_quickened_sites(&chunk, self.caches.native_regions_enabled);
        let sites = Arc::<[SiteAction]>::from(sites);
        let regions = Arc::<[crate::vm::region::NativeRegion]>::from(regions);
        let direct_calls = Rc::new(RefCell::new(vec![None; chunk.code.len()]));
        self.caches.quickened.insert(
          identity,
          QuickenedPlan {
            _chunk: chunk,
            sites: Arc::clone(&sites),
            regions: Arc::clone(&regions),
            direct_calls: Rc::clone(&direct_calls),
          },
        );
        self.caches.active_regions = regions;
        self.caches.active_direct_calls = direct_calls;
        sites
      };
      self.caches.active_quickened_chunk = identity;
      self.caches.active_quickened_sites = sites;
    }
    self
      .caches
      .active_quickened_sites
      .get(self.execution.active.ip)
      .copied()
      .unwrap_or(SiteAction::Interpret)
  }
}
