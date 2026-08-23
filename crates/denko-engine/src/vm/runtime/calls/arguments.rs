// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::FunctionProto;
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::{usize_to_decimal_string, usize_to_f64};
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::runtime::property::get_property;
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
  pub(in crate::vm::runtime) fn flatten_spread_values(
    &mut self,
    values: Value,
    mask: Value,
  ) -> Result<Vec<Value>, RuntimeError> {
    let values_vec = match values {
      Value::Array(arr, _) => arr.borrow().clone_elements(),
      _ => Vec::new(),
    };
    let mask_vec = match mask {
      Value::Array(arr, _) => arr.borrow().clone_elements(),
      _ => Vec::new(),
    };

    let mut flat: Vec<Value> = Vec::new();
    for (i, value) in values_vec.into_iter().enumerate() {
      let spread = mask_vec
        .get(i)
        .and_then(|v| v.as_ref())
        .is_some_and(|v| self.to_boolean(v));
      if !spread {
        if let Some(v) = value {
          flat.push(v);
        }
        continue;
      }

      if let Some(iterable) = value {
        let iterator_method = self.get_property_host(
          &iterable,
          &PropertyKey::Symbol(SymbolId::well_known_iterator()),
        )?;

        if !Self::is_callable_value(&iterator_method) {
          return Err(self.throw_type_error("Spread value is not iterable"));
        }

        let iter_obj = self.call_value_with_this(&iterator_method, &iterable, &[])?;

        loop {
          let next_fn =
            self.get_property_host(&iter_obj, &PropertyKey::String("next".to_string().into()))?;
          let result = self.call_value_with_this(&next_fn, &iter_obj, &[])?;
          let done_val =
            self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
          if self.to_boolean(&done_val) {
            break;
          }
          let val =
            self.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?;
          flat.push(val);
        }
      }
    }

    Ok(flat)
  }

  pub(in crate::vm::runtime) fn prepare_call_args(
    &self,
    args: Vec<Value>,
    proto: &FunctionProto,
  ) -> Result<Vec<Value>, RuntimeError> {
    if !proto.has_rest {
      return Ok(args);
    }

    let ar = proto.arity;
    let fixed_count = ar.saturating_sub(1);
    let mut fixed = Vec::with_capacity(ar);

    for i in 0..fixed_count {
      fixed.push(args.get(i).cloned().unwrap_or(Value::Undefined));
    }

    let rest_slice: Vec<Option<Value>> = if args.len() > fixed_count {
      args[fixed_count..]
        .iter()
        .map(|v| Some(v.clone()))
        .collect()
    } else {
      Vec::new()
    };

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(rest_slice)));
    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
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
      let proto_for_lookup = props.borrow().get_prototype();
      if let Ok(for_each_val) = get_property(
        &proto_for_lookup,
        &PropertyKey::String("forEach".to_string().into()),
        0,
      ) && !matches!(for_each_val, Value::Undefined)
      {
        props.borrow_mut().properties.insert(
          PropertyKey::String("forEach".to_string().into()),
          PropertyDescriptor::ordinary_data(for_each_val),
        );
      }
    }

    fixed.push(Value::Array(arr_rc, props));
    if crate::trace::enabled() {
      // Log allocation site for rest-array used as arguments so we can
      // correlate a later callee pointing at the same props pointer.
      if let Value::Array(_arr, props_rc) = fixed
        .last()
        .cloned()
        .expect("fixed must not be empty after push")
      {
        eprintln!(
          "[trace] prepare_call_args: created rest-Array props_ptr={:p} proto='{}'",
          Rc::as_ptr(&props_rc),
          proto.name
        );
      }
    }
    Ok(fixed)
  }

  /// Builds an arguments object for a function call, mapping arguments to local slots.
  pub(in crate::vm::runtime) fn build_mapped_arguments_object(
    &mut self,
    proto: &FunctionProto,
    callee: Value,
    args_for_arguments: &[Option<Value>],
  ) -> Value {
    let obj_rc: Rc<RefCell<ObjectData>> =
      gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    obj_rc
      .borrow_mut()
      .set_internal_slot("[[ArgumentsData]]", Value::Bool(true));
    let arg_count = args_for_arguments.len();
    let mapped_count = proto
      .arity
      .min(self.execution.active.locals.len());
    for idx in 0..arg_count {
      let key = PropertyKey::String(usize_to_decimal_string(idx).into());
      if idx < mapped_count {
        obj_rc.borrow_mut().properties.insert(
          key,
          PropertyDescriptor::Data {
            value: args_for_arguments
              .get(idx)
              .cloned()
              .flatten()
              .unwrap_or(Value::Undefined),
            writable: true,
            enumerable: true,
            configurable: true,
          },
        );
      } else {
        obj_rc.borrow_mut().properties.insert(
          key,
          PropertyDescriptor::ordinary_data(
            args_for_arguments
              .get(idx)
              .cloned()
              .flatten()
              .unwrap_or(Value::Undefined),
          ),
        );
      }
    }
    obj_rc.borrow_mut().properties.insert(
      PropertyKey::String("callee".to_string().into()),
      PropertyDescriptor::Data {
        value: callee,
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
    obj_rc.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(arg_count)),
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
    let object_proto = self.realm_state.realm.object_prototype();
    if !matches!(object_proto, Value::Null) {
      obj_rc
        .borrow_mut()
        .set_prototype_value(object_proto);
    }
    self.install_arguments_iterator_property(&obj_rc);
    Value::Object(obj_rc)
  }

  pub(in crate::vm::runtime) fn install_arguments_iterator_property(
    &mut self,
    obj_rc: &Rc<RefCell<ObjectData>>,
  ) {
    let array_proto = self.realm_state.realm.array_prototype();
    let iterator_key = PropertyKey::Symbol(SymbolId::well_known_iterator());
    let Ok(iterator) = self.get_property_host(&array_proto, &iterator_key) else {
      return;
    };
    obj_rc.borrow_mut().properties.insert(
      iterator_key,
      PropertyDescriptor::Data {
        value: iterator,
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
  }
}
