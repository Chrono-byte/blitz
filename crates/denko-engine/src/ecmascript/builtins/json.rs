// SPDX-License-Identifier: MPL-2.0
//! JSON namespace object.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use super::array::is_array_value;
use super::json_parse::{self, ParsedJson, ParsedValue};
use crate::ecmascript::semantics::{builtin_object, js_number_to_string};
use crate::gc;
use crate::runtime_utils::{set_builtin_length, set_builtin_name, syntax_error, to_length};
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
    Value::Promise(state, _) => Some(Rc::as_ptr(state) as usize),
    _ => None,
  }
}

enum ParseChildren {
  Primitive,
  Array(Vec<ParseRecord>),
  Object(Vec<(js_string::JsString, ParseRecord)>),
}

struct ParseRecord {
  value: Value,
  children: ParseChildren,
  source: Option<js_string::JsString>,
}

fn build_parse_record(
  vm: &Vm,
  source: &js_string::JsString,
  parsed: ParsedJson,
) -> ParseRecord {
  let primitive_source = || {
    source
      .slice(parsed.span.clone())
      .expect("Chumsky produced a span within the JSON source")
  };
  match parsed.value {
    ParsedValue::Null => ParseRecord {
      value: Value::Null,
      children: ParseChildren::Primitive,
      source: Some(primitive_source()),
    },
    ParsedValue::Bool(value) => ParseRecord {
      value: Value::Bool(value),
      children: ParseChildren::Primitive,
      source: Some(primitive_source()),
    },
    ParsedValue::Number(value) => ParseRecord {
      value: Value::Number(value),
      children: ParseChildren::Primitive,
      source: Some(primitive_source()),
    },
    ParsedValue::String(value) => ParseRecord {
      value: Value::String(value),
      children: ParseChildren::Primitive,
      source: Some(primitive_source()),
    },
    ParsedValue::Array(values) => {
      let children = values
        .into_iter()
        .map(|value| build_parse_record(vm, source, value))
        .collect::<Vec<_>>();
      let value = vm.new_array(
        children
          .iter()
          .map(|child| Some(child.value.clone()))
          .collect(),
      );
      ParseRecord {
        value,
        children: ParseChildren::Array(children),
        source: None,
      }
    }
    ParsedValue::Object(properties) => {
      let children = properties
        .into_iter()
        .map(|(key, value)| (key, build_parse_record(vm, source, value)))
        .collect::<Vec<_>>();
      let mut object = ObjectData::with_prototype(vm.realm().object_prototype());
      for (key, child) in &children {
        object.properties.insert(
          PropertyKey::String(key.clone()),
          PropertyDescriptor::ordinary_data(child.value.clone()),
        );
      }
      ParseRecord {
        value: Value::Object(gc::track_object(Rc::new(RefCell::new(object)))),
        children: ParseChildren::Object(children),
        source: None,
      }
    }
  }
}

fn child_record<'a>(
  record: Option<&'a ParseRecord>,
  key: &PropertyKey,
) -> Option<&'a ParseRecord> {
  let PropertyKey::String(key) = key else {
    return None;
  };
  match &record?.children {
    ParseChildren::Array(children) => key
      .parse::<usize>()
      .ok()
      .and_then(|index| children.get(index)),
    ParseChildren::Object(children) => children
      .iter()
      .rev()
      .find(|(name, _)| name == key)
      .map(|(_, child)| child),
    ParseChildren::Primitive => None,
  }
}

fn reviver_context(
  vm: &Vm,
  source: Option<&js_string::JsString>,
) -> Value {
  let mut context = ObjectData::with_prototype(vm.realm().object_prototype());
  if let Some(source) = source {
    context.properties.insert(
      PropertyKey::String("source".into()),
      PropertyDescriptor::ordinary_data(Value::String(source.clone())),
    );
  }
  Value::Object(gc::track_object(Rc::new(RefCell::new(context))))
}

fn revive_property(
  vm: &mut Vm,
  holder: &Value,
  name: PropertyKey,
  reviver: &Value,
  record: Option<&ParseRecord>,
) -> Result<Value, RuntimeError> {
  let value = vm.get_property_host(holder, &name)?;
  let record = record.filter(|record| crate::same_value(&value, &record.value));
  let keys = if is_array_value(&value) {
    let length = vm.get_property_host(&value, &PropertyKey::String("length".into()))?;
    let length = to_length(vm, &length)?;
    (0..length)
      .map(|index| PropertyKey::String(index.to_string().into()))
      .collect()
  } else if value.is_object() {
    vm.own_enumerable_keys_value(&value)?
  } else {
    Vec::new()
  };

  for key in keys {
    let revived = revive_property(vm, &value, key.clone(), reviver, child_record(record, &key))?;
    if matches!(revived, Value::Undefined) {
      vm.delete_property_host(&value, &key)?;
    } else {
      vm.create_data_property_host(&value, key, revived)?;
    }
  }

  let context = reviver_context(
    vm,
    record.and_then(|record| match record.children {
      ParseChildren::Primitive => record.source.as_ref(),
      _ => None,
    }),
  );
  let name = match name {
    PropertyKey::String(name) => Value::String(name),
    PropertyKey::Symbol(_) => unreachable!("JSON properties cannot be symbols"),
  };
  vm.call_value_with_this(reviver, holder, &[name, value, context])
}

fn parse_value(
  vm: &mut Vm,
  text: Value,
  reviver: Value,
) -> Result<Value, RuntimeError> {
  let source = vm.to_string(&text)?;
  let parsed =
    json_parse::parse(&source).map_err(|error| syntax_error(vm, format!("JSON.parse: {error}")))?;
  let record = build_parse_record(vm, &source, parsed);
  if !Vm::is_callable_value(&reviver) {
    return Ok(record.value);
  }

  let mut root = ObjectData::with_prototype(vm.realm().object_prototype());
  root.properties.insert(
    PropertyKey::String("".into()),
    PropertyDescriptor::ordinary_data(record.value.clone()),
  );
  let root = Value::Object(gc::track_object(Rc::new(RefCell::new(root))));
  revive_property(
    vm,
    &root,
    PropertyKey::String("".into()),
    &reviver,
    Some(&record),
  )
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
      let length = elements.borrow().len();
      let mut parts = Vec::with_capacity(length);
      for index in 0..length {
        let item = vm.get_property_host(value, &PropertyKey::String(index.to_string().into()))?;
        match stringify_value(vm, &item, seen)? {
          Some(json) => parts.push(json),
          None => parts.push("null".to_owned()),
        }
      }
      seen.remove(&identity);
      Some(format!("[{}]", parts.join(",")))
    }
    Value::Object(_) | Value::Proxy(_) | Value::Promise(..) => {
      let identity = value_identity(value).expect("object-like value has identity");
      if !seen.insert(identity) {
        return Err(vm.throw_type_error("Converting circular structure to JSON"));
      }
      let mut parts = Vec::new();
      for key in vm.own_enumerable_keys_value(value)? {
        let PropertyKey::String(name) = key else {
          continue;
        };
        let key =
          stringify_value(vm, &Value::String(name.clone()), seen)?.expect("strings stringify");
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
  let parse = builtin_object(
    Rc::new(|vm, args| {
      let offset = usize::from(args.len() > 1);
      let text = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let reviver = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      parse_value(vm, text, reviver)
    }),
    function_prototype,
  );
  set_builtin_name(&parse, "parse");
  set_builtin_length(&parse, 2.0);
  object.borrow_mut().properties.insert(
    PropertyKey::String("parse".into()),
    PropertyDescriptor::builtin_data(parse),
  );

  let stringify = builtin_object(
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
  );
  set_builtin_name(&stringify, "stringify");
  set_builtin_length(&stringify, 3.0);
  object.borrow_mut().properties.insert(
    PropertyKey::String("stringify".into()),
    PropertyDescriptor::builtin_data(stringify),
  );
  object
}
