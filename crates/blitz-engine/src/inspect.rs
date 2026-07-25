// SPDX-License-Identifier: MPL-2.0
//! Side-effect-free formatting for host-facing runtime value inspection.
//!
//! This module is intentionally separate from ECMAScript `ToString`. Inspecting
//! a value must not invoke user code, getters, or proxy traps.

use std::collections::HashSet;
use std::rc::Rc;

use crate::value::{PromiseState, PropertyDescriptor, PropertyKey, Value};

/// Controls bounded host-side value inspection.
#[derive(Clone, Copy, Debug)]
pub struct InspectOptions {
  /// Maximum number of container levels to expand, including the root value.
  pub depth: usize,
  /// Maximum number of entries rendered from each object or array.
  pub max_entries: usize,
  /// Whether containers use indented, multi-line output.
  pub multiline: bool,
}

impl Default for InspectOptions {
  fn default() -> Self {
    Self {
      depth: 3,
      max_entries: 20,
      multiline: true,
    }
  }
}

/// Inspect a runtime value using the interactive REPL defaults.
pub fn inspect(value: &Value) -> String {
  inspect_with_options(value, InspectOptions::default())
}

/// Inspect a runtime value using explicit presentation limits.
pub fn inspect_with_options(
  value: &Value,
  options: InspectOptions,
) -> String {
  let mut inspector = Inspector {
    options,
    ancestors: HashSet::new(),
  };
  inspector.render(value, options.depth, 0)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ContainerId {
  Object(usize),
  Array(usize),
  Proxy(usize),
  Promise(usize),
}

struct Inspector {
  options: InspectOptions,
  ancestors: HashSet<ContainerId>,
}

impl Inspector {
  fn render(
    &mut self,
    value: &Value,
    depth: usize,
    indentation: usize,
  ) -> String {
    match value {
      Value::Uninitialized => "<uninitialized>".to_string(),
      Value::Undefined => "undefined".to_string(),
      Value::Number(number) => format_number(*number),
      Value::String(string) => format!("{string:?}"),
      Value::Bool(boolean) => boolean.to_string(),
      Value::BigInt(integer) => format!("{integer}n"),
      Value::Symbol(_, description) => match description {
        Some(description) => format!("Symbol({description})"),
        None => "Symbol()".to_string(),
      },
      Value::Null => "null".to_string(),
      Value::Object(object) => {
        let id = ContainerId::Object(Rc::as_ptr(object) as usize);
        if !self.ancestors.insert(id) {
          return "[Circular]".to_string();
        }
        let properties = object
          .borrow()
          .properties
          .iter()
          .filter(|(_, descriptor)| descriptor_is_enumerable(descriptor))
          .map(|(key, descriptor)| (key.clone(), descriptor.clone()))
          .collect::<Vec<_>>();
        let rendered = self.render_object(&properties, depth, indentation);
        self.ancestors.remove(&id);
        rendered
      }
      Value::Array(elements, properties) => {
        let id = ContainerId::Array(Rc::as_ptr(elements) as usize);
        if !self.ancestors.insert(id) {
          return "[Circular]".to_string();
        }
        let elements = elements.borrow().clone_elements();
        let properties = properties
          .borrow()
          .properties
          .iter()
          .filter(|(_, descriptor)| descriptor_is_enumerable(descriptor))
          .map(|(key, descriptor)| (key.clone(), descriptor.clone()))
          .collect::<Vec<_>>();
        let rendered = self.render_array(&elements, &properties, depth, indentation);
        self.ancestors.remove(&id);
        rendered
      }
      Value::Proxy(proxy) => {
        let id = ContainerId::Proxy(Rc::as_ptr(proxy) as usize);
        if !self.ancestors.insert(id) {
          return "[Circular]".to_string();
        }
        let target = {
          let proxy = proxy.borrow();
          proxy
            .handler
            .as_ref()
            .map(|_| proxy.target.clone())
        };
        let rendered = match target {
          Some(target) if depth > 0 => format!(
            "Proxy({})",
            self.render(&target, depth.saturating_sub(1), indentation)
          ),
          Some(_) => "Proxy(…)".to_string(),
          None => "[Revoked Proxy]".to_string(),
        };
        self.ancestors.remove(&id);
        rendered
      }
      Value::Closure { proto, .. } => format!("[Function: {}]", proto.name),
      Value::Builtin(_) => "[Native Function]".to_string(),
      Value::BuiltinObject(_, properties) => {
        let name = properties
          .borrow()
          .properties
          .get(&PropertyKey::String("name".to_string()))
          .and_then(|descriptor| match descriptor {
            PropertyDescriptor::Data {
              value: Value::String(name),
              ..
            } if !name.is_empty() => Some(name.clone()),
            _ => None,
          });
        name.map_or_else(
          || "[Native Function]".to_string(),
          |name| format!("[Function: {name}]"),
        )
      }
      Value::Promise(state) => {
        let id = ContainerId::Promise(Rc::as_ptr(state) as usize);
        if !self.ancestors.insert(id) {
          return "[Circular]".to_string();
        }
        let state = state.borrow().clone();
        let rendered = match state {
          PromiseState::Pending { .. } => "Promise { <pending> }".to_string(),
          PromiseState::Fulfilled(value) if depth > 0 => format!(
            "Promise {{ <fulfilled>: {} }}",
            self.render(&value, depth.saturating_sub(1), indentation)
          ),
          PromiseState::Fulfilled(_) => "Promise { <fulfilled>: … }".to_string(),
          PromiseState::Rejected { reason, .. } if depth > 0 => format!(
            "Promise {{ <rejected>: {} }}",
            self.render(&reason, depth.saturating_sub(1), indentation)
          ),
          PromiseState::Rejected { .. } => "Promise { <rejected>: … }".to_string(),
        };
        self.ancestors.remove(&id);
        rendered
      }
    }
  }

  fn render_object(
    &mut self,
    properties: &[(PropertyKey, PropertyDescriptor)],
    depth: usize,
    indentation: usize,
  ) -> String {
    if properties.is_empty() {
      return "{}".to_string();
    }
    if depth == 0 {
      return "{ … }".to_string();
    }

    let visible = properties.len().min(self.options.max_entries);
    let mut entries = properties[..visible]
      .iter()
      .map(|(key, descriptor)| {
        format!(
          "{}: {}",
          format_property_key(key),
          self.render_descriptor(descriptor, depth - 1, indentation + 1)
        )
      })
      .collect::<Vec<_>>();
    if properties.len() > visible {
      entries.push(format!("… {} more", properties.len() - visible));
    }
    self.container("{", "}", entries, indentation)
  }

  fn render_array(
    &mut self,
    elements: &[Option<Value>],
    properties: &[(PropertyKey, PropertyDescriptor)],
    depth: usize,
    indentation: usize,
  ) -> String {
    let total = elements.len().saturating_add(properties.len());
    if total == 0 {
      return "[]".to_string();
    }
    if depth == 0 {
      return "[ … ]".to_string();
    }

    let mut entries = Vec::new();
    for element in elements.iter().take(self.options.max_entries) {
      entries.push(match element {
        Some(value) => self.render(value, depth - 1, indentation + 1),
        None => "<empty>".to_string(),
      });
    }
    let remaining_capacity = self
      .options
      .max_entries
      .saturating_sub(entries.len());
    entries.extend(
      properties
        .iter()
        .take(remaining_capacity)
        .map(|(key, descriptor)| {
          format!(
            "{}: {}",
            format_property_key(key),
            self.render_descriptor(descriptor, depth - 1, indentation + 1)
          )
        }),
    );
    if total > entries.len() {
      entries.push(format!("… {} more", total - entries.len()));
    }
    self.container("[", "]", entries, indentation)
  }

  fn render_descriptor(
    &mut self,
    descriptor: &PropertyDescriptor,
    depth: usize,
    indentation: usize,
  ) -> String {
    match descriptor {
      PropertyDescriptor::Data { value, .. } => self.render(value, depth, indentation),
      PropertyDescriptor::Accessor { get, set, .. } => match (get.is_some(), set.is_some()) {
        (true, true) => "[Getter/Setter]".to_string(),
        (true, false) => "[Getter]".to_string(),
        (false, true) => "[Setter]".to_string(),
        (false, false) => "[Accessor]".to_string(),
      },
    }
  }

  fn container(
    &self,
    open: &str,
    close: &str,
    entries: Vec<String>,
    indentation: usize,
  ) -> String {
    if !self.options.multiline {
      return format!("{open} {} {close}", entries.join(", "));
    }

    let entry_indent = "  ".repeat(indentation + 1);
    let closing_indent = "  ".repeat(indentation);
    let entries = entries
      .into_iter()
      .map(|entry| indent_entry(&entry, &entry_indent))
      .collect::<Vec<_>>()
      .join(",\n");
    format!("{open}\n{entries}\n{closing_indent}{close}")
  }
}

fn descriptor_is_enumerable(descriptor: &PropertyDescriptor) -> bool {
  match descriptor {
    PropertyDescriptor::Data { enumerable, .. }
    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
  }
}

fn format_number(number: f64) -> String {
  if number.is_nan() {
    "NaN".to_string()
  } else if number == f64::INFINITY {
    "Infinity".to_string()
  } else if number == f64::NEG_INFINITY {
    "-Infinity".to_string()
  } else if number == 0.0 && number.is_sign_negative() {
    "-0".to_string()
  } else {
    number.to_string()
  }
}

fn format_property_key(key: &PropertyKey) -> String {
  match key {
    PropertyKey::String(key) if is_identifier(key) => key.clone(),
    PropertyKey::String(key) => format!("{key:?}"),
    PropertyKey::Symbol(symbol) => format!("[{symbol:?}]"),
  }
}

fn is_identifier(value: &str) -> bool {
  let mut chars = value.chars();
  let Some(first) = chars.next() else {
    return false;
  };
  (first == '_' || first == '$' || first.is_alphabetic())
    && chars.all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

fn indent_entry(
  value: &str,
  indentation: &str,
) -> String {
  let mut lines = value.lines();
  let Some(first) = lines.next() else {
    return indentation.to_string();
  };
  std::iter::once(format!("{indentation}{first}"))
    .chain(lines.map(str::to_string))
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::rc::Rc;

  use super::*;
  use crate::value::{ArrayData, ObjectData};

  fn object(properties: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut data = ObjectData::new();
    for (key, value) in properties {
      data.properties.insert(
        PropertyKey::String(key.to_string()),
        PropertyDescriptor::data(value),
      );
    }
    Value::Object(Rc::new(RefCell::new(data)))
  }

  fn compact_options() -> InspectOptions {
    InspectOptions {
      multiline: false,
      ..InspectOptions::default()
    }
  }

  #[test]
  fn inspects_nested_objects_and_arrays() {
    let array = Value::Array(
      Rc::new(RefCell::new(ArrayData::new(vec![
        Some(Value::String("one".to_string())),
        None,
      ]))),
      Rc::new(RefCell::new(ObjectData::new())),
    );
    let value = object([("answer", Value::Number(42.0)), ("items", array)]);

    assert_eq!(
      inspect_with_options(&value, compact_options()),
      r#"{ answer: 42, items: [ "one", <empty> ] }"#
    );
  }

  #[test]
  fn detects_circular_objects() {
    let data = Rc::new(RefCell::new(ObjectData::new()));
    data.borrow_mut().properties.insert(
      PropertyKey::String("self".to_string()),
      PropertyDescriptor::data(Value::Object(Rc::clone(&data))),
    );

    assert_eq!(
      inspect_with_options(&Value::Object(data), compact_options()),
      "{ self: [Circular] }"
    );
  }

  #[test]
  fn does_not_invoke_accessors_and_omits_non_enumerable_properties() {
    let mut data = ObjectData::new();
    data.properties.insert(
      PropertyKey::String("value".to_string()),
      PropertyDescriptor::Accessor {
        get: Some(Value::Builtin(Rc::new(|_, _| Ok(Value::Number(1.0))))),
        set: None,
        enumerable: true,
        configurable: true,
      },
    );
    data.properties.insert(
      PropertyKey::String("hidden".to_string()),
      PropertyDescriptor::Data {
        value: Value::Bool(true),
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );

    assert_eq!(
      inspect_with_options(
        &Value::Object(Rc::new(RefCell::new(data))),
        compact_options()
      ),
      "{ value: [Getter] }"
    );
  }

  #[test]
  fn respects_depth_and_entry_limits() {
    let value = object([
      ("nested", object([("answer", Value::Number(42.0))])),
      ("second", Value::Bool(true)),
      ("third", Value::Null),
    ]);
    let options = InspectOptions {
      depth: 1,
      max_entries: 2,
      multiline: false,
    };

    assert_eq!(
      inspect_with_options(&value, options),
      "{ nested: { … }, second: true, … 1 more }"
    );
  }

  #[test]
  fn indents_nested_multiline_values() {
    let value = object([("nested", object([("answer", Value::Number(42.0))]))]);

    assert_eq!(inspect(&value), "{\n  nested: {\n    answer: 42\n  }\n}");
  }
}
