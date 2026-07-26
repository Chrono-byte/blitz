// SPDX-License-Identifier: MPL-2.0
//! JSON namespace object.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::ecmascript::semantics::{builtin_object, js_number_to_string};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::{RuntimeError, Vm};

fn value_identity(value: &Value) -> Option<usize> {
  match value {
    Value::Object(object) => Some(Rc::as_ptr(object) as usize),
    Value::Array(array, _) => Some(Rc::as_ptr(array) as usize),
    Value::Proxy(proxy) => Some(Rc::as_ptr(proxy) as usize),
    Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      Some(Rc::as_ptr(props) as usize)
    }
    Value::Promise(state) => Some(Rc::as_ptr(state) as usize),
    _ => None,
  }
}

fn stringify_value(
  vm: &mut Vm,
  value: &Value,
  seen: &mut HashSet<usize>,
) -> Result<Option<String>, RuntimeError> {
  let result = match value {
    Value::Uninitialized
    | Value::Undefined
    | Value::Builtin(_)
    | Value::BuiltinObject(_, _)
    | Value::Closure { .. } => None,
    Value::Null => Some("null".to_owned()),
    Value::Bool(value) => Some(if *value { "true" } else { "false" }.to_owned()),
    Value::Number(value) => Some(if value.is_finite() {
      js_number_to_string(*value)
    } else {
      "null".to_owned()
    }),
    Value::String(value) => Some({
      let mut output = String::with_capacity(value.len() + 2);
      output.push('"');
      for unit in value.code_units() {
        match unit {
          0x0022 => output.push_str("\\\""),
          0x005C => output.push_str("\\\\"),
          0x000A => output.push_str("\\n"),
          0x000D => output.push_str("\\r"),
          0x0009 => output.push_str("\\t"),
          0x0008 => output.push_str("\\b"),
          0x000C => output.push_str("\\f"),
          0x0020..=0x007E => {
            output.push(char::from_u32(u32::from(unit)).expect("ASCII is a Unicode scalar"))
          }
          unit => output.push_str(&format!("\\u{unit:04x}")),
        }
      }
      output.push('"');
      output
    }),
    Value::BigInt(_) => return Err(vm.throw_type_error("Do not know how to serialize a BigInt")),
    Value::Symbol(_, _) => None,
    Value::Array(elements, _) => {
      let identity = value_identity(value).expect("array has identity");
      if !seen.insert(identity) {
        return Err(vm.throw_type_error("Converting circular structure to JSON"));
      }
      let mut parts = Vec::with_capacity(elements.borrow().len());
      for slot in elements.borrow().iter() {
        match slot {
          Some(item) => match stringify_value(vm, item, seen)? {
            Some(json) => parts.push(json),
            None => parts.push("null".to_owned()),
          },
          None => parts.push("null".to_owned()),
        }
      }
      seen.remove(&identity);
      Some(format!("[{}]", parts.join(",")))
    }
    Value::Object(_) | Value::Proxy(_) | Value::Promise(_) => {
      let identity = value_identity(value).expect("object-like value has identity");
      if !seen.insert(identity) {
        return Err(vm.throw_type_error("Converting circular structure to JSON"));
      }
      let mut parts = Vec::new();
      for key in vm.own_enumerable_keys_value(value)? {
        let PropertyKey::String(name) = key else {
          continue;
        };
        let key = stringify_value(vm, &Value::String(name.clone().into()), seen)?
          .expect("strings stringify");
        let property = vm.get_property_host(value, &PropertyKey::String(name))?;
        if let Some(serialized) = stringify_value(vm, &property, seen)? {
          parts.push(format!("{key}:{serialized}"));
        }
      }
      seen.remove(&identity);
      Some(format!("{{{}}}", parts.join(",")))
    }
  };
  Ok(result)
}

/// Creates the JSON namespace object.
pub(crate) fn create_json_object(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> Rc<RefCell<ObjectData>> {
  let object = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  object.borrow_mut().properties.insert(
    PropertyKey::String("stringify".into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let offset = usize::from(args.len() > 1);
        let value = args
          .get(offset)
          .cloned()
          .unwrap_or(Value::Undefined);
        let mut seen = HashSet::new();
        Ok(
          stringify_value(vm, &value, &mut seen)?
            .map_or(Value::Undefined, |output| Value::String(output.into())),
        )
      }),
      function_prototype,
    )),
  );
  object
}
