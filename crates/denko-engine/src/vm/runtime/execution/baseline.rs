// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Vm {
  pub(crate) fn try_host_baseline_call(
    &mut self,
    proto: &Rc<crate::bytecode::Function>,
    this_value: &Value,
    arguments: &[Value],
  ) -> Result<Option<Value>, RuntimeError> {
    let function = self.function_id_for_chunk(&proto.code);
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)))
    {
      return Ok(None);
    }
    let mut registers = RegisterState::call_registers(this_value.clone(), arguments);
    let mut locals = [];
    let vm = self as *mut Vm;
    let outcome = self
      .profiling
      .jit
      .invoke_baseline(
        function,
        crate::vm::jit::BaselineInvocation {
          vm,
          start_pc: 0,
          registers: &mut registers,
          arguments: &arguments[..arguments.len().min(proto.arity)],
          locals: &mut locals,
        },
      )
      .map_err(RuntimeError::from)?;
    let crate::vm::jit::BaselineOutcome::Returned(result) = outcome else {
      return Ok(None);
    };
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

  pub(super) fn try_direct_baseline_call(
    &mut self,
    call_site: usize,
    proto: &Rc<crate::bytecode::Function>,
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
          .record_optimizing_call(matches!(
            &result,
            Ok(crate::vm::jit::DirectCallOutcome::Returned(_))
          ));
      }
      if let Ok(crate::vm::jit::DirectCallOutcome::Returned(result)) = result {
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
    let code = &proto.code;
    let parameter_count = proto.arity;
    let function = self.function_id_for_chunk(code);
    if arguments.len() >= 2
      && matches!(arguments[0], Value::Number(_))
      && matches!(arguments[1], Value::Number(_))
      && let Some(target) = self
        .profiling
        .jit
        .optimizing_numeric_binary_target(function, code, parameter_count)
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
        .record_optimizing_call(matches!(
          &result,
          Ok(crate::vm::jit::DirectCallOutcome::Returned(_))
        ));
      if let Ok(crate::vm::jit::DirectCallOutcome::Returned(result)) = result {
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
      let outcome = target
        .call(crate::vm::jit::DirectCallInputs { arguments })
        .map_err(RuntimeError::from)?;
      let crate::vm::jit::DirectCallOutcome::Returned(result) = outcome else {
        return Ok(None);
      };
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
    let mut registers = RegisterState::call_registers(this_value.clone(), arguments);
    let mut locals = [];
    let vm = self as *mut Vm;
    let outcome = self
      .profiling
      .jit
      .invoke_baseline(
        function,
        crate::vm::jit::BaselineInvocation {
          vm,
          start_pc: 0,
          registers: &mut registers,
          arguments: &arguments[..arguments.len().min(parameter_count)],
          locals: &mut locals,
        },
      )
      .map_err(RuntimeError::from)?;
    let crate::vm::jit::BaselineOutcome::Returned(result) = outcome else {
      return Ok(None);
    };
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
      .record_optimizing_call(matches!(
        &result,
        Ok(crate::vm::jit::DirectCallOutcome::Returned(_))
      ));
    match result.map_err(RuntimeError::from)? {
      crate::vm::jit::DirectCallOutcome::Returned(value) => Ok(Some(value)),
      crate::vm::jit::DirectCallOutcome::Fallback => Ok(None),
    }
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
    let vm = self as *mut Vm;
    let outcome = self
      .profiling
      .jit
      .invoke_baseline(
        function,
        crate::vm::jit::BaselineInvocation {
          vm,
          start_pc: self.execution.active.pc,
          registers,
          arguments: &[],
          locals: &mut self.execution.active.locals,
        },
      )
      .map_err(RuntimeError::from)?;
    if outcome == crate::vm::jit::BaselineOutcome::Unavailable {
      return Ok(None);
    }
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    match outcome {
      crate::vm::jit::BaselineOutcome::Returned(result) => {
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        Ok(Some(result))
      }
      crate::vm::jit::BaselineOutcome::Interpret { resume_pc }
      | crate::vm::jit::BaselineOutcome::Invalidated { resume_pc } => {
        self.profiling.jit_stats.interpreter_resumes = self
          .profiling
          .jit_stats
          .interpreter_resumes
          .saturating_add(1);
        self.execution.active.pc = resume_pc;
        Ok(None)
      }
      crate::vm::jit::BaselineOutcome::Unavailable => Ok(None),
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
    let code = self
      .profiling
      .code_ids
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
    let state = code
      .as_deref()
      .and_then(|code| {
        self
          .profiling
          .jit
          .install_const_prefix(function, code, parameter_count)
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
    state: &mut RegisterState,
  ) -> Result<Option<RunOutcome>, RuntimeError> {
    let function = self.active_function_id();
    let baseline_ready = self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)));
    if !baseline_ready {
      return Ok(None);
    }
    let vm = self as *mut Vm;
    let outcome = self
      .profiling
      .jit
      .invoke_baseline(
        function,
        crate::vm::jit::BaselineInvocation {
          vm,
          start_pc: self.execution.active.pc,
          registers: &mut state.registers,
          arguments: &[],
          locals: &mut self.execution.active.locals,
        },
      )
      .map_err(RuntimeError::from)?;
    if outcome == crate::vm::jit::BaselineOutcome::Unavailable {
      return Ok(None);
    }
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    match outcome {
      crate::vm::jit::BaselineOutcome::Returned(result) => {
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        Ok(Some(RunOutcome::Return(result)))
      }
      crate::vm::jit::BaselineOutcome::Interpret { resume_pc } => {
        self.profiling.jit_stats.interpreter_resumes = self
          .profiling
          .jit_stats
          .interpreter_resumes
          .saturating_add(1);
        self.execution.active.pc = resume_pc;
        Ok(None)
      }
      crate::vm::jit::BaselineOutcome::Invalidated { resume_pc } => {
        self.execution.active.pc = resume_pc;
        Ok(None)
      }
      crate::vm::jit::BaselineOutcome::Unavailable => Ok(None),
    }
  }

  fn function_id_for_chunk(
    &mut self,
    code: &std::sync::Arc<crate::bytecode::Chunk>,
  ) -> crate::vm::jit::FunctionId {
    let code_identity = std::sync::Arc::as_ptr(code) as usize;
    if let Some(id) = self
      .profiling
      .code_ids
      .get(&code_identity)
      .copied()
    {
      return id;
    }
    self.profiling.next_function_id = self.profiling.next_function_id.saturating_add(1);
    let id = crate::vm::jit::FunctionId::new(self.profiling.next_function_id);
    self
      .profiling
      .chunk_refs
      .insert(code_identity, std::sync::Arc::clone(code));
    self.profiling.code_ids.insert(code_identity, id);
    id
  }

  pub(super) fn active_function_id(&mut self) -> crate::vm::jit::FunctionId {
    let identity = Arc::as_ptr(&self.execution.active.code) as usize;
    if self.profiling.active_function_identity == identity
      && let Some(function) = self.profiling.active_function_id
    {
      return function;
    }
    let code = std::sync::Arc::clone(&self.execution.active.code);
    let function = self.function_id_for_chunk(&code);
    self.profiling.active_function_identity = identity;
    self.profiling.active_function_id = Some(function);
    function
  }

  pub(super) fn record_jit_backedge(
    &mut self,
    pc: usize,
  ) {
    let code_identity = Arc::as_ptr(&self.execution.active.code) as usize;
    if self.profiling.last_hot_function == code_identity
      || self
        .profiling
        .hot_functions
        .contains(&code_identity)
    {
      self.profiling.last_hot_function = code_identity;
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
      self.profiling.hot_functions.insert(code_identity);
      self.profiling.last_hot_function = code_identity;
    }
  }

  pub(super) fn site_action(&mut self) -> SiteAction {
    let identity = Arc::as_ptr(&self.execution.active.code) as usize;
    if self.caches.active_quickened_chunk != identity {
      let sites = if let Some(plan) = self.caches.quickened.get(&identity) {
        self.caches.active_direct_calls = Rc::clone(&plan.direct_calls);
        self.caches.active_regions = Arc::clone(&plan.regions);
        Arc::clone(&plan.sites)
      } else {
        let code = Arc::clone(&self.execution.active.code);
        let (sites, regions) = build_quickened_sites(&code, self.caches.native_regions_enabled);
        let sites = Arc::<[SiteAction]>::from(sites);
        let regions = Arc::<[crate::vm::region::NativeRegion]>::from(regions);
        let direct_calls = Rc::new(RefCell::new(vec![None; code.words.len()]));
        self.caches.quickened.insert(
          identity,
          QuickenedPlan {
            _chunk: code,
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
      .get(self.execution.active.pc)
      .copied()
      .unwrap_or(SiteAction::Interpret)
  }
}
