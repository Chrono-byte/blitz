// SPDX-License-Identifier: MPL-2.0
//! Minimal WHATWG URL host object used by runtime HTTP APIs.

use std::cell::RefCell;
use std::rc::Rc;

use ::url::Url;
use denko::Vm;
use denko::errors::RuntimeError;
use denko::gc;
use denko::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

use super::host_constructor;

pub(super) fn install(vm: &mut Vm) {
  let prototype = object(vm.realm().object_prototype());
  let instance_prototype = prototype.clone();
  let constructor: BuiltinFn = Rc::new(move |vm, args| {
    let input = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("URL requires an input"))?;
    let input = vm.to_string(input)?;
    let parsed = match args.get(2) {
      None | Some(Value::Undefined) => Url::parse(input.as_str_lossy()),
      Some(base) => {
        let base_string = vm.to_string(base)?;
        let base = Url::parse(base_string.as_str_lossy())
          .map_err(|error| RuntimeError::from(format!("invalid base URL: {error}")))?;
        base.join(input.as_str_lossy())
      }
    }
    .map_err(|error| RuntimeError::from(format!("invalid URL: {error}")))?;

    Ok(url_object(instance_prototype.clone(), &parsed))
  });
  let constructor = host_constructor(vm, "URL", 1, prototype, constructor);
  vm.realm().set_global("URL", constructor);
}

fn url_object(
  prototype: Value,
  parsed: &Url,
) -> Value {
  let value = object(prototype);
  let host = parsed.host_str().unwrap_or_default();
  let port = parsed
    .port()
    .map_or_else(String::new, |port| port.to_string());
  let authority = if port.is_empty() {
    host.to_string()
  } else {
    format!("{host}:{port}")
  };
  let search = parsed
    .query()
    .map_or_else(String::new, |query| format!("?{query}"));
  let hash = parsed
    .fragment()
    .map_or_else(String::new, |fragment| format!("#{fragment}"));

  for (name, property) in [
    ("href", parsed.as_str().to_string()),
    ("origin", parsed.origin().ascii_serialization()),
    ("protocol", format!("{}:", parsed.scheme())),
    ("username", parsed.username().to_string()),
    (
      "password",
      parsed
        .password()
        .map_or_else(String::new, str::to_string),
    ),
    ("host", authority),
    ("hostname", host.to_string()),
    ("port", port),
    ("pathname", parsed.path().to_string()),
    ("search", search),
    ("hash", hash),
  ] {
    set(&value, name, Value::String(property.into()));
  }
  value
}

fn object(prototype: Value) -> Value {
  Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(prototype),
  ))))
}

fn set(
  target: &Value,
  name: &str,
  value: Value,
) {
  if let Value::Object(object) = target {
    object.borrow_mut().properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::ordinary_data(value),
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn exposes_url_components() {
    let parsed =
      Url::parse("https://user:pass@example.com:8443/a/b?q=one#fragment").expect("valid URL");
    let value = url_object(Value::Null, &parsed);
    let Value::Object(properties) = value else {
      panic!("URL value should be an object");
    };
    let properties = properties.borrow();
    let string_property = |name: &str| {
      let Some(PropertyDescriptor::Data {
        value: Value::String(value),
        ..
      }) = properties
        .properties
        .get(&PropertyKey::String(name.to_string().into()))
      else {
        panic!("{name} should be a string data property");
      };
      value.clone()
    };
    assert_eq!(
      string_property("href"),
      "https://user:pass@example.com:8443/a/b?q=one#fragment"
    );
    assert_eq!(string_property("origin"), "https://example.com:8443");
    assert_eq!(string_property("protocol"), "https:");
    assert_eq!(string_property("username"), "user");
    assert_eq!(string_property("password"), "pass");
    assert_eq!(string_property("host"), "example.com:8443");
    assert_eq!(string_property("hostname"), "example.com");
    assert_eq!(string_property("port"), "8443");
    assert_eq!(string_property("pathname"), "/a/b");
    assert_eq!(string_property("search"), "?q=one");
    assert_eq!(string_property("hash"), "#fragment");
  }
}
