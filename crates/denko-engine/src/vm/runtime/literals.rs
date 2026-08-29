// SPDX-License-Identifier: MPL-2.0
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::{array_length_descriptor, usize_to_f64};
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::ensure_slot;
use crate::vm::runtime::property::get_property;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

impl Vm {
  pub(super) fn build_array_value(
    &mut self,
    values: Vec<Value>,
  ) -> Result<Value, RuntimeError> {
    let n = values.len();
    let elems: Vec<Option<Value>> = values
      .into_iter()
      .map(|value| match value {
        Value::Uninitialized => None,
        other => Some(other),
      })
      .collect();

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(elems)));
    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      array_length_descriptor(n),
    );

    if let Some(array_val) = self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Array".to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(proto_val) = get_property(
        &array_val,
        &PropertyKey::String("prototype".to_string().into()),
        0,
      )
      && !matches!(proto_val, Value::Null)
    {
      props.borrow_mut().set_prototype_value(proto_val);
    }

    let array = Value::Array(Rc::clone(&arr_rc), Rc::clone(&props));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] build_array: created Array arr_ptr={:p} props_ptr={:p} pc={}",
        Rc::as_ptr(&arr_rc),
        Rc::as_ptr(&props),
        self.execution.active.pc.saturating_sub(1)
      );
    }
    Ok(array)
  }

  pub(super) fn build_object_value(
    &mut self,
    values: Vec<Value>,
  ) -> Result<Value, RuntimeError> {
    if !values.len().is_multiple_of(2) {
      return Err(RuntimeError::from(
        "object literal operand count is not even",
      ));
    }
    let n = values.len() / 2;
    let mut entries: Vec<(PropertyKey, PropertyDescriptor)> = Vec::with_capacity(n);
    for pair in values.chunks_exact(2) {
      let key_val = pair[0].clone();
      let value = pair[1].clone();
      let key = self.to_property_key(&key_val)?;
      entries.push((key, PropertyDescriptor::ordinary_data(value)));
    }
    let mut map = indexmap::IndexMap::with_capacity(n);
    for (key, desc) in entries {
      map.insert(key, desc);
    }
    let mut data = ObjectData::new();
    data.properties = map;
    if let Some(object_val) = self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Object".to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(proto_val) = get_property(
        &object_val,
        &PropertyKey::String("prototype".to_string().into()),
        0,
      )
      && !matches!(proto_val, Value::Null)
    {
      data.set_prototype_value(proto_val);
    }

    let obj_rc = gc::track_object(Rc::new(RefCell::new(data)));
    let object = Value::Object(Rc::clone(&obj_rc));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] build_object: created Object obj_ptr={:p} pc={}",
        Rc::as_ptr(&obj_rc),
        self.execution.active.pc.saturating_sub(1)
      );
    }
    Ok(object)
  }

  /// Creates a closure without coupling the result to compiler emission state.
  #[allow(clippy::too_many_lines)]
  pub(super) fn make_closure_value(
    &mut self,
    idx: usize,
  ) -> Result<Value, RuntimeError> {
    let proto = self
      .execution
      .active
      .code
      .functions
      .get(idx)
      .cloned()
      .ok_or("Bad function index")?;
    let has_prototype_property = proto.has_prototype_property;

    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    if let Some(function_val) = self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Function".to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(function_proto) = get_property(
        &function_val,
        &PropertyKey::String("prototype".to_string().into()),
        0,
      )
      && !matches!(function_proto, Value::Null)
    {
      props
        .borrow_mut()
        .set_prototype_value(function_proto);
    }

    let closure_proto = Rc::new(proto);
    let mut env_cells = Vec::with_capacity(closure_proto.upvalues.len());
    if crate::trace::enabled() {
      eprintln!(
        "[trace] make_closure proto='{}' upvalues={} current_env_len={}",
        closure_proto.name,
        closure_proto.upvalues.len(),
        self
          .execution
          .active
          .closure_env
          .as_ref()
          .map_or(0, |env| env.borrow().len())
      );
      for (i, upvalue) in closure_proto.upvalues.iter().enumerate() {
        eprintln!(
          "[trace]   upvalue[{}] local={} index={}",
          i, upvalue.is_local, upvalue.index
        );
      }
    }
    for upvalue in &closure_proto.upvalues {
      let cell = if upvalue.is_local {
        let slot = upvalue.index as usize;
        ensure_slot(&mut self.execution.active.locals, slot);
        self
          .execution
          .active
          .locals
          .get(slot)
          .cloned()
          .ok_or("Bad upvalue local slot")?
      } else {
        let env = self
          .execution
          .active
          .closure_env
          .as_ref()
          .ok_or("Bad enclosing closure env")?;
        let env_guard = env.borrow();
        env_guard
          .get(upvalue.index as usize)
          .cloned()
          .ok_or_else(|| {
            if crate::trace::enabled() {
              eprintln!(
                "[diag] make_closure bad enclosing index={} env_len={} proto='{}'",
                upvalue.index,
                env_guard.len(),
                closure_proto.name
              );
              for (i, spec) in closure_proto.upvalues.iter().enumerate() {
                eprintln!(
                  "[diag]   upvalue[{}] local={} index={}",
                  i, spec.is_local, spec.index
                );
              }
            }
            "Bad enclosing upvalue index"
          })?
      };
      env_cells.push(cell);
    }

    if closure_proto.captures_this {
      props.borrow_mut().set_internal_slot(
        "[[LexicalNewTarget]]",
        self
          .current_new_target()
          .unwrap_or(Value::Undefined),
      );
    }
    let closure = Value::Closure {
      proto: Rc::clone(&closure_proto),
      env: self
        .memory
        .allocate_closure_environment(env_cells),
      lexical_this: if closure_proto.captures_this {
        Some(Box::new(self.execution.active.this_value.clone()))
      } else {
        None
      },
      props: Rc::clone(&props),
    };
    props
      .borrow_mut()
      .set_captured_lexical_env(Some(Rc::clone(&self.execution.active.lexical_env)));
    props.borrow_mut().set_internal_slot(
      "[[RealmGlobals]]",
      Value::Object(Rc::clone(&self.realm_state.globals)),
    );
    if closure_proto.is_generator {
      props.borrow_mut().set_prototype_value(
        self
          .realm_state
          .realm
          .generator_function_prototype(),
      );
    } else if closure_proto.is_async {
      props
        .borrow_mut()
        .set_prototype_value(self.realm_state.realm.async_function_prototype());
    }

    if closure_proto.captures_this
      && let Some(home_object) = self.current_method_home_object()
    {
      props.borrow_mut().properties.insert(
        PropertyKey::String("[[HomeObject]]".to_string().into()),
        PropertyDescriptor::ordinary_data(home_object),
      );
    }

    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(closure_proto.length)),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

    props.borrow_mut().properties.insert(
      PropertyKey::String("name".to_string().into()),
      PropertyDescriptor::Data {
        value: Value::String(
          if matches!(
            closure_proto.name.as_str(),
            "<anonymous>" | "<arrow>" | "<method>"
          ) {
            "".into()
          } else {
            closure_proto.name.clone().into()
          },
        ),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

    if has_prototype_property {
      let proto_obj = if closure_proto.is_generator {
        Value::Object(gc::track_object(Rc::new(RefCell::new(
          ObjectData::with_prototype(self.realm_state.realm.generator_prototype()),
        ))))
      } else {
        let object_proto = self
          .realm_state
          .realm
          .intrinsics
          .object_prototype
          .clone()
          .unwrap_or(Value::Null);
        let proto_obj_map: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(
          ObjectData::with_prototype(object_proto),
        )));
        Value::Object(proto_obj_map)
      };

      if !closure_proto.is_generator
        && let Value::Object(proto_obj_map) = &proto_obj
      {
        proto_obj_map.borrow_mut().properties.insert(
          PropertyKey::String("constructor".to_string().into()),
          PropertyDescriptor::Data {
            value: closure.clone(),
            writable: true,
            enumerable: false,
            configurable: true,
          },
        );
      }

      props.borrow_mut().properties.insert(
        PropertyKey::String("prototype".to_string().into()),
        PropertyDescriptor::Data {
          value: proto_obj,
          writable: true,
          enumerable: false,
          configurable: false,
        },
      );
    }

    if crate::trace::enabled() {
      if let Value::Closure { props, .. } = &closure {
        eprintln!(
          "[trace] make_closure: created Closure props_ptr={:p} proto_idx={} proto_chunk_ptr={:p} pc={}",
          Rc::as_ptr(props),
          idx,
          Arc::as_ptr(&closure_proto.code),
          self.execution.active.pc.saturating_sub(1)
        );
      }
      if has_prototype_property
        && let Value::Closure { props, .. } = &closure
        && let Some(PropertyDescriptor::Data { value, .. }) = props
          .borrow()
          .properties
          .get(&PropertyKey::String("prototype".to_string().into()))
        && let Value::Object(proto_obj_map) = value
      {
        eprintln!(
          "[trace] make_closure: created proto_obj props_ptr={:p}",
          Rc::as_ptr(proto_obj_map)
        );
      }
    }
    Ok(closure)
  }
}
