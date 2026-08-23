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
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
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
  let keys = if is_array_value(vm, &value)? {
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

fn quote_json_string(value: &js_string::JsString) -> String {
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
        output.push(char::from_u32(u32::from(unit)).expect("ASCII is a Unicode scalar"));
      }
      unit => output.push_str(&format!("\\u{unit:04x}")),
    }
  }
  output.push('"');
  output
}

fn boxed_primitive(value: &Value) -> Option<Value> {
  let data = match value {
    Value::Object(data)
    | Value::Array(_, data)
    | Value::Closure { props: data, .. }
    | Value::BuiltinObject(_, data) => data,
    _ => return None,
  };
  if let Some(value) = data
    .borrow()
    .internal_slot("[[BooleanData]]")
    .cloned()
  {
    return Some(value);
  }
  if let Some(value) = data
    .borrow()
    .internal_slot("[[PrimitiveValue]]")
    .cloned()
  {
    return matches!(
      value,
      Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::BigInt(_)
    )
    .then_some(value);
  }
  let descriptor = data
    .borrow()
    .properties
    .get(&PropertyKey::String("[[PrimitiveValue]]".into()))
    .cloned()?;
  let PropertyDescriptor::Data { value, .. } = descriptor else {
    return None;
  };
  matches!(
    value,
    Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::BigInt(_)
  )
  .then_some(value)
}

struct StringifyState {
  replacer: Option<Value>,
  property_list: Option<Vec<js_string::JsString>>,
  gap: String,
  indent: String,
  stack: HashSet<usize>,
}

impl StringifyState {
  fn serialize_property(
    &mut self,
    vm: &mut Vm,
    holder: &Value,
    key: &js_string::JsString,
  ) -> Result<Option<String>, RuntimeError> {
    let mut value = vm.get_property_host(holder, &PropertyKey::String(key.clone()))?;
    if value.is_object() || matches!(value, Value::BigInt(_)) {
      let to_json =
        vm.get_property_host(&value, &PropertyKey::String("toJSON".to_string().into()))?;
      if Vm::is_callable_value(&to_json) {
        value = vm.call_value_with_this(&to_json, &value, &[Value::String(key.clone())])?;
      }
    }
    if let Some(replacer) = self.replacer.clone() {
      value = vm.call_value_with_this(&replacer, holder, &[Value::String(key.clone()), value])?;
    }
    if let Some(primitive) = boxed_primitive(&value) {
      value = primitive;
    }
    if Vm::is_callable_value(&value) {
      return Ok(None);
    }
    self.serialize_value(vm, &value)
  }

  fn serialize_value(
    &mut self,
    vm: &mut Vm,
    value: &Value,
  ) -> Result<Option<String>, RuntimeError> {
    let is_array = is_array_value(vm, value)?;
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
      Value::String(value) => Some(quote_json_string(value)),
      Value::BigInt(_) => return Err(vm.throw_type_error("Do not know how to serialize a BigInt")),
      Value::Symbol(_, _) => None,
      value if is_array => {
        let identity = value_identity(value).expect("array has identity");
        if !self.stack.insert(identity) {
          return Err(vm.throw_type_error("Converting circular structure to JSON"));
        }
        let previous_indent = self.indent.clone();
        self.indent.push_str(&self.gap);
        let length = vm.get_property_host(value, &PropertyKey::String("length".into()))?;
        let length = to_length(vm, &length)?;
        let mut parts = Vec::with_capacity(length);
        for index in 0..length {
          match self.serialize_property(vm, value, &index.to_string().into())? {
            Some(json) => parts.push(json),
            None => parts.push("null".to_owned()),
          }
        }
        let result = if parts.is_empty() {
          "[]".to_owned()
        } else if self.gap.is_empty() {
          format!("[{}]", parts.join(","))
        } else {
          format!(
            "[\n{}{}\n{}]",
            self.indent,
            parts.join(&format!(",\n{}", self.indent)),
            previous_indent
          )
        };
        self.stack.remove(&identity);
        self.indent = previous_indent;
        Some(result)
      }
      Value::Object(_) | Value::Proxy(_) | Value::Promise(..) => {
        let identity = value_identity(value).expect("object-like value has identity");
        if !self.stack.insert(identity) {
          return Err(vm.throw_type_error("Converting circular structure to JSON"));
        }
        let previous_indent = self.indent.clone();
        self.indent.push_str(&self.gap);
        let mut parts = Vec::new();
        let keys = if let Some(property_list) = &self.property_list {
          property_list.clone()
        } else {
          vm.own_enumerable_keys_value(value)?
            .into_iter()
            .filter_map(|key| match key {
              PropertyKey::String(name) => Some(name),
              PropertyKey::Symbol(_) => None,
            })
            .collect()
        };
        for name in keys {
          if let Some(serialized) = self.serialize_property(vm, value, &name)? {
            let separator = if self.gap.is_empty() { ":" } else { ": " };
            parts.push(format!(
              "{}{separator}{serialized}",
              quote_json_string(&name)
            ));
          }
        }
        let result = if parts.is_empty() {
          "{}".to_owned()
        } else if self.gap.is_empty() {
          format!("{{{}}}", parts.join(","))
        } else {
          format!(
            "{{\n{}{}\n{}}}",
            self.indent,
            parts.join(&format!(",\n{}", self.indent)),
            previous_indent
          )
        };
        self.stack.remove(&identity);
        self.indent = previous_indent;
        Some(result)
      }
      Value::Array(..) => unreachable!("arrays handled by IsArray"),
    };
    Ok(result)
  }
}

fn replacer_property_list(
  vm: &mut Vm,
  replacer: &Value,
) -> Result<Option<Vec<js_string::JsString>>, RuntimeError> {
  if !is_array_value(vm, replacer)? {
    return Ok(None);
  }
  let length = vm.get_property_host(replacer, &PropertyKey::String("length".into()))?;
  let length = to_length(vm, &length)?;
  let mut list = Vec::new();
  for index in 0..length {
    let mut item =
      vm.get_property_host(replacer, &PropertyKey::String(index.to_string().into()))?;
    if let Some(primitive) = boxed_primitive(&item) {
      item = primitive;
    }
    let key = match item {
      Value::String(value) => Some(value),
      Value::Number(value) => Some(js_number_to_string(value).into()),
      _ => None,
    };
    if let Some(key) = key
      && !list.contains(&key)
    {
      list.push(key);
    }
  }
  Ok(Some(list))
}

fn stringify_gap(
  _vm: &mut Vm,
  mut space: Value,
) -> Result<String, RuntimeError> {
  if let Some(primitive) = boxed_primitive(&space) {
    space = primitive;
  }
  match space {
    Value::Number(number) => {
      let width = if number.is_nan() || number <= 0.0 {
        0
      } else {
        number.floor().min(10.0) as usize
      };
      Ok(" ".repeat(width))
    }
    Value::String(value) => Ok(
      value
        .slice(0..value.len().min(10))
        .expect("space slice is in bounds")
        .to_string(),
    ),
    Value::Object(_) => {
      // A forged primitive slot is not a boxed String/Number.
      Ok(String::new())
    }
    _ => Ok(String::new()),
  }
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
    PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("JSON".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  let parse = builtin_object(
    Rc::new(|vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
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
      let offset = usize::from(vm.builtin_this_arg_prepended());
      let value = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let replacer = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let space = args
        .get(offset + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property_list = replacer_property_list(vm, &replacer)?;
      let mut state = StringifyState {
        replacer: Vm::is_callable_value(&replacer).then_some(replacer),
        property_list,
        gap: stringify_gap(vm, space)?,
        indent: String::new(),
        stack: HashSet::new(),
      };
      let mut root = ObjectData::with_prototype(vm.realm().object_prototype());
      root.properties.insert(
        PropertyKey::String("".into()),
        PropertyDescriptor::ordinary_data(value),
      );
      let root = Value::Object(gc::track_object(Rc::new(RefCell::new(root))));
      Ok(
        state
          .serialize_property(vm, &root, &"".into())?
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
