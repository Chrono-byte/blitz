use crate::errors::RuntimeError;
use crate::gc;
use crate::mutex_compat::MutexBorrowExt;
use crate::runtime_utils::{array_length_descriptor, usize_to_f64};
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::{ensure_slot, get_property};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

impl Vm {
  pub(super) fn build_array(&mut self) -> Result<(), RuntimeError> {
    let n = self.read_byte()? as usize;
    let mut elems: Vec<Option<Value>> = Vec::with_capacity(n);
    for _ in 0..n {
      let value = self.pop()?;
      elems.push(match value {
        Value::Uninitialized => None,
        other => Some(other),
      });
    }
    elems.reverse();

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(elems)));
    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string()),
      array_length_descriptor(n),
    );

    if let Some(array_val) = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Array".to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(proto_val) =
        get_property(&array_val, &PropertyKey::String("prototype".to_string()), 0)
      && !matches!(proto_val, Value::Null)
    {
      props.borrow_mut().set_prototype_value(proto_val);
    }

    self
      .stack
      .push(Value::Array(Rc::clone(&arr_rc), Rc::clone(&props)));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] build_array: created Array arr_ptr={:p} props_ptr={:p} ip={}",
        Rc::as_ptr(&arr_rc),
        Rc::as_ptr(&props),
        self.ip.saturating_sub(1)
      );
    }
    Ok(())
  }

  pub(super) fn build_object(&mut self) -> Result<(), RuntimeError> {
    let n = self.read_byte()? as usize;
    let mut entries: Vec<(PropertyKey, PropertyDescriptor)> = Vec::with_capacity(n);
    for _ in 0..n {
      let value = self.pop()?;
      let key_val = self.pop()?;
      let key = self.to_property_key(&key_val)?;
      entries.push((key, PropertyDescriptor::data(value)));
    }
    let mut map = indexmap::IndexMap::with_capacity(n);
    for (key, desc) in entries.into_iter().rev() {
      map.insert(key, desc);
    }
    let mut data = ObjectData::new();
    data.properties = map;
    if let Some(object_val) = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Object".to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(proto_val) = get_property(
        &object_val,
        &PropertyKey::String("prototype".to_string()),
        0,
      )
      && !matches!(proto_val, Value::Null)
    {
      data.set_prototype_value(proto_val);
    }

    let obj_rc = gc::track_object(Rc::new(RefCell::new(data)));
    self.stack.push(Value::Object(Rc::clone(&obj_rc)));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] build_object: created Object obj_ptr={:p} ip={}",
        Rc::as_ptr(&obj_rc),
        self.ip.saturating_sub(1)
      );
    }
    Ok(())
  }

  /// Creates a closure value from a function index.
  #[allow(clippy::too_many_lines)]
  pub(super) fn make_closure(&mut self) -> Result<(), RuntimeError> {
    let idx = self.read_byte()? as usize;
    let proto = self
      .chunk
      .functions
      .get(idx)
      .cloned()
      .ok_or("Bad function index")?;
    let has_prototype_property = proto.has_prototype_property;

    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    if let Some(function_val) = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Function".to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(function_proto) = get_property(
        &function_val,
        &PropertyKey::String("prototype".to_string()),
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
          .current_closure_env
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
        ensure_slot(&mut self.locals, slot);
        self
          .locals
          .get(slot)
          .cloned()
          .ok_or("Bad upvalue local slot")?
      } else {
        let env = self
          .current_closure_env
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

    let closure = Value::Closure {
      proto: Rc::clone(&closure_proto),
      env: gc::track_env(Rc::new(Mutex::new(env_cells))),
      lexical_this: if closure_proto.captures_this {
        Some(Box::new(self.this_value.clone()))
      } else {
        None
      },
      props: Rc::clone(&props),
    };
    props
      .borrow_mut()
      .set_captured_lexical_env(Some(Rc::clone(&self.lexical_env)));
    if closure_proto.is_generator {
      props
        .borrow_mut()
        .set_prototype_value(self.realm.generator_function_prototype());
    }

    if closure_proto.captures_this
      && let Some(home_object) = self.current_method_home_object()
    {
      props.borrow_mut().properties.insert(
        PropertyKey::String("[[HomeObject]]".to_string()),
        PropertyDescriptor::data(home_object),
      );
    }

    // Set the `name` property
    props.borrow_mut().properties.insert(
      PropertyKey::String("name".to_string()),
      PropertyDescriptor::Data {
        value: Value::String(closure_proto.name.clone()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

    // Set the `length` property
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string()),
      PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(closure_proto.arity)),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

    if has_prototype_property {
      let proto_obj = if closure_proto.is_generator {
        self.realm.generator_prototype()
      } else {
        let object_proto = self
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

      if let Value::Object(proto_obj_map) = &proto_obj {
        proto_obj_map.borrow_mut().properties.insert(
          PropertyKey::String("constructor".to_string()),
          PropertyDescriptor::Data {
            value: closure.clone(),
            writable: true,
            enumerable: false,
            configurable: true,
          },
        );
      }

      props.borrow_mut().properties.insert(
        PropertyKey::String("prototype".to_string()),
        PropertyDescriptor::Data {
          value: proto_obj,
          writable: true,
          enumerable: false,
          configurable: false,
        },
      );
    }

    self.stack.push(closure.clone());
    if crate::trace::enabled() {
      if let Value::Closure { props, .. } = &closure {
        eprintln!(
          "[trace] make_closure: created Closure props_ptr={:p} proto_idx={} proto_chunk_ptr={:p} ip={}",
          Rc::as_ptr(props),
          idx,
          Rc::as_ptr(&closure_proto.chunk),
          self.ip.saturating_sub(1)
        );
      }
      if has_prototype_property
        && let Value::Closure { props, .. } = &closure
        && let Some(PropertyDescriptor::Data { value, .. }) = props
          .borrow()
          .properties
          .get(&PropertyKey::String("prototype".to_string()))
        && let Value::Object(proto_obj_map) = value
      {
        eprintln!(
          "[trace] make_closure: created proto_obj props_ptr={:p}",
          Rc::as_ptr(proto_obj_map)
        );
      }
    }
    Ok(())
  }
}
