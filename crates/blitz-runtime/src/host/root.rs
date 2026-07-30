// SPDX-License-Identifier: MPL-2.0
//! Runtime-owned Effect root supervision.

use std::cell::RefCell;
use std::rc::Rc;

use denko::errors::RuntimeError;
use denko::value::Value;
use denko::{HostRoot, Vm};

use super::event_loop::{ResourceHandle, ResourceRegistry};

/// Lifecycle of the one Effect root owned by a runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootState {
  /// No root has been reserved.
  NotStarted,
  /// The facade reserved the root and attached its upstream fiber.
  Running,
  /// The host requested interruption and is waiting for finalizers.
  InterruptRequested,
  /// The upstream fiber reported its complete Exit.
  RootCompleted,
  /// Root finalization and reporting are being completed.
  DrainingFinalizers,
  /// The root is fully settled and no longer keeps the runtime alive.
  Exited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootId {
  generation: u64,
}

impl RootId {
  pub(super) const fn from_generation(generation: u64) -> Self {
    Self { generation }
  }

  pub const fn generation(self) -> u64 {
    self.generation
  }
}

struct RootInner {
  state: RootState,
  generation: u64,
  interrupt: Option<HostRoot>,
  resource: Option<ResourceHandle>,
  exit_status: Option<i32>,
}

/// VM-thread supervisor for the runtime's one Effect root.
#[derive(Clone)]
pub struct RootSupervisor {
  inner: Rc<RefCell<RootInner>>,
  resources: ResourceRegistry,
}

impl RootSupervisor {
  pub fn new(resources: ResourceRegistry) -> Self {
    Self {
      inner: Rc::new(RefCell::new(RootInner {
        state: RootState::NotStarted,
        generation: 0,
        interrupt: None,
        resource: None,
        exit_status: None,
      })),
      resources,
    }
  }

  pub fn reserve(&self) -> Result<RootId, RuntimeError> {
    let mut inner = self.inner.borrow_mut();
    if inner.state != RootState::NotStarted {
      return Err(RuntimeError::from(
        "blitz:runtime runMain already has a root",
      ));
    }
    inner.state = RootState::Running;
    inner.resource = Some(self.resources.register("effect-root", true));
    Ok(RootId {
      generation: inner.generation,
    })
  }

  pub fn attach(
    &self,
    vm: &mut Vm,
    id: RootId,
    interrupt: Value,
  ) -> Result<(), RuntimeError> {
    if !interrupt.is_callable() {
      return Err(RuntimeError::from(
        "runtime root interrupt callback must be callable",
      ));
    }
    let mut inner = self.inner.borrow_mut();
    if inner.generation != id.generation || inner.state != RootState::Running {
      return Err(RuntimeError::from("runtime root identifier is stale"));
    }
    if inner.interrupt.is_some() {
      return Err(RuntimeError::from(
        "runtime root interrupt callback is already attached",
      ));
    }
    inner.interrupt = Some(vm.create_host_root(interrupt));
    Ok(())
  }

  pub fn request_interrupt(
    &self,
    vm: &mut Vm,
    status: i32,
  ) -> Result<bool, RuntimeError> {
    let interrupt = {
      let mut inner = self.inner.borrow_mut();
      if inner.state != RootState::Running {
        return Ok(false);
      }
      inner.state = RootState::InterruptRequested;
      inner.exit_status = Some(status);
      inner
        .interrupt
        .and_then(|root| vm.host_root(root))
    };
    if let Some(interrupt) = interrupt {
      vm.call_value_with_this(&interrupt, &Value::Undefined, &[])?;
    }
    Ok(true)
  }

  pub fn complete(
    &self,
    vm: &mut Vm,
    id: RootId,
    status: i32,
  ) -> Result<bool, RuntimeError> {
    let (interrupt, resource) = {
      let mut inner = self.inner.borrow_mut();
      if inner.generation != id.generation
        || !matches!(
          inner.state,
          RootState::Running | RootState::InterruptRequested
        )
      {
        return Ok(false);
      }
      inner.state = RootState::RootCompleted;
      if inner.exit_status.is_none() {
        inner.exit_status = Some(status);
      }
      inner.state = RootState::DrainingFinalizers;
      (inner.interrupt.take(), inner.resource.take())
    };
    if let Some(interrupt) = interrupt {
      vm.release_host_root(interrupt)?;
    }
    if let Some(resource) = resource {
      resource.close();
    }
    self.inner.borrow_mut().state = RootState::Exited;
    Ok(true)
  }

  pub fn fail_reservation(
    &self,
    vm: &mut Vm,
    id: RootId,
  ) -> Result<(), RuntimeError> {
    let _completed = self.complete(vm, id, 1)?;
    Ok(())
  }

  pub fn exit_status(&self) -> Option<i32> {
    self.inner.borrow().exit_status
  }
}
