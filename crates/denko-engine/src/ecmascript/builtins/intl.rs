// SPDX-License-Identifier: MPL-2.0
//! Minimal ECMA-402 intrinsic graph and locale-list semantics.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::ecmascript::semantics::{builtin_object, set_builtin_length, set_builtin_name};
use crate::runtime_utils::{array_from_values, js_number_to_string, to_length};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::{RuntimeError, Vm};

fn tagged_object(
  prototype: Value,
  tag: &str,
) -> Rc<RefCell<ObjectData>> {
  let object = Rc::new(RefCell::new(ObjectData::with_prototype(prototype)));
  object.borrow_mut().properties.insert(
    PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String(tag.into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  object
}

fn canonicalize_locale(
  vm: &mut Vm,
  locale: &js_string::JsString,
) -> Result<js_string::JsString, RuntimeError> {
  let locale = locale.as_str_lossy();
  let subtags = locale.split('-').collect::<Vec<_>>();
  if subtags.is_empty()
    || subtags.iter().any(|subtag| subtag.is_empty())
    || !subtags[0]
      .bytes()
      .all(|byte| byte.is_ascii_alphabetic())
    || !(2..=8).contains(&subtags[0].len())
  {
    return Err(vm.throw_range_error("Invalid language tag"));
  }
  let mut seen = HashSet::new();
  for subtag in &subtags {
    if subtag.len() > 8
      || !subtag
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric())
    {
      return Err(vm.throw_range_error("Invalid language tag"));
    }
    if !seen.insert(subtag.to_ascii_lowercase()) {
      return Err(vm.throw_range_error("Invalid language tag"));
    }
  }

  let mut canonical = Vec::with_capacity(subtags.len());
  canonical.push(subtags[0].to_ascii_lowercase());
  for subtag in subtags.into_iter().skip(1) {
    if subtag.len() == 2
      && subtag
        .bytes()
        .all(|byte| byte.is_ascii_alphabetic())
    {
      canonical.push(subtag.to_ascii_uppercase());
    } else if subtag.len() == 4
      && subtag
        .bytes()
        .all(|byte| byte.is_ascii_alphabetic())
    {
      let mut chars = subtag.chars();
      let first = chars
        .next()
        .expect("four-character script subtag");
      canonical.push(format!(
        "{}{}",
        first.to_ascii_uppercase(),
        chars.as_str().to_ascii_lowercase()
      ));
    } else {
      canonical.push(subtag.to_ascii_lowercase());
    }
  }
  Ok(canonical.join("-").into())
}

fn canonicalize_locale_list(
  vm: &mut Vm,
  locales: Value,
) -> Result<Value, RuntimeError> {
  if matches!(locales, Value::Undefined) {
    return Ok(array_from_values(Vec::new()));
  }
  let mut values = Vec::new();
  if let Value::String(locale) = locales {
    values.push(Value::String(canonicalize_locale(vm, &locale)?));
  } else {
    if matches!(locales, Value::Null) {
      return Err(vm.throw_type_error("Cannot convert null to a locale list"));
    }
    let length = vm.get_property_host(&locales, &PropertyKey::String("length".into()))?;
    let length = to_length(vm, &length)?;
    for index in 0..length {
      let key = PropertyKey::String(index.to_string().into());
      if !vm.has_property_host(&locales, &key)? {
        continue;
      }
      let locale = vm.get_property_host(&locales, &key)?;
      if !matches!(locale, Value::String(_) | Value::Object(_)) {
        return Err(vm.throw_type_error("Locale list element must be a string or object"));
      }
      let locale = vm.to_string(&locale)?;
      let locale = canonicalize_locale(vm, &locale)?;
      if !values
        .iter()
        .any(|value| matches!(value, Value::String(existing) if existing == &locale))
      {
        values.push(Value::String(locale));
      }
    }
  }
  Ok(array_from_values(values))
}

fn part(
  vm: &Vm,
  kind: &str,
  value: String,
) -> Value {
  let mut object = ObjectData::with_prototype(vm.realm().object_prototype());
  object.properties.insert(
    PropertyKey::String("type".into()),
    PropertyDescriptor::ordinary_data(Value::String(kind.into())),
  );
  object.properties.insert(
    PropertyKey::String("value".into()),
    PropertyDescriptor::ordinary_data(Value::String(value.into())),
  );
  Value::Object(Rc::new(RefCell::new(object)))
}

fn install_constructor(
  intl: &Rc<RefCell<ObjectData>>,
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &'static str,
) -> (Value, Rc<RefCell<ObjectData>>) {
  let prototype = tagged_object(
    Value::Object(Rc::clone(object_prototype)),
    &format!("Intl.{name}"),
  );
  let properties = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_prototype),
  ))));
  properties.borrow_mut().properties.insert(
    PropertyKey::prototype(),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&prototype))),
  );
  let instance_prototype = Rc::clone(&prototype);
  let constructor = Value::BuiltinObject(
    Rc::new(move |_vm, _args| {
      let mut instance = ObjectData::with_prototype(Value::Object(Rc::clone(&instance_prototype)));
      instance.set_internal_slot("[[IntlObject]]", Value::String(name.into()));
      Ok(Value::Object(Rc::new(RefCell::new(instance))))
    }),
    properties,
  );
  set_builtin_name(&constructor, name);
  set_builtin_length(&constructor, 0.0);
  prototype.borrow_mut().properties.insert(
    PropertyKey::String("constructor".into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  intl.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  (constructor, prototype)
}

pub(crate) fn create_intl_intrinsic(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> Value {
  let intl = tagged_object(Value::Object(Rc::clone(object_prototype)), "Intl");
  let get_canonical_locales = builtin_object(
    Rc::new(|vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      canonicalize_locale_list(
        vm,
        args
          .get(offset)
          .cloned()
          .unwrap_or(Value::Undefined),
      )
    }),
    function_prototype,
  );
  set_builtin_name(&get_canonical_locales, "getCanonicalLocales");
  set_builtin_length(&get_canonical_locales, 1.0);
  intl.borrow_mut().properties.insert(
    PropertyKey::String("getCanonicalLocales".into()),
    PropertyDescriptor::builtin_data(get_canonical_locales),
  );

  let (collator, collator_prototype) =
    install_constructor(&intl, object_prototype, function_prototype, "Collator");
  let (date_time_format, date_time_format_prototype) = install_constructor(
    &intl,
    object_prototype,
    function_prototype,
    "DateTimeFormat",
  );
  let (number_format, number_format_prototype) =
    install_constructor(&intl, object_prototype, function_prototype, "NumberFormat");
  let (plural_rules, plural_rules_prototype) =
    install_constructor(&intl, object_prototype, function_prototype, "PluralRules");

  let make_resolved_options = || {
    let method = builtin_object(
      Rc::new(|vm, _args| {
        Ok(Value::Object(Rc::new(RefCell::new(
          ObjectData::with_prototype(vm.realm().object_prototype()),
        ))))
      }),
      function_prototype,
    );
    set_builtin_name(&method, "resolvedOptions");
    set_builtin_length(&method, 0.0);
    method
  };
  for prototype in [
    &collator_prototype,
    &date_time_format_prototype,
    &number_format_prototype,
  ] {
    prototype.borrow_mut().properties.insert(
      PropertyKey::String("resolvedOptions".into()),
      PropertyDescriptor::builtin_data(make_resolved_options()),
    );
  }

  let make_supported_locales = || {
    let method = builtin_object(
      Rc::new(|vm, args| {
        let offset = usize::from(vm.builtin_this_arg_prepended());
        canonicalize_locale_list(
          vm,
          args
            .get(offset)
            .cloned()
            .unwrap_or(Value::Undefined),
        )
      }),
      function_prototype,
    );
    set_builtin_name(&method, "supportedLocalesOf");
    set_builtin_length(&method, 1.0);
    method
  };
  for constructor in [&collator, &date_time_format, &number_format] {
    if let Value::BuiltinObject(_, properties) = constructor {
      properties.borrow_mut().properties.insert(
        PropertyKey::String("supportedLocalesOf".into()),
        PropertyDescriptor::builtin_data(make_supported_locales()),
      );
    }
  }

  let make_bound_function_getter = |intl_name: &'static str, property_name: &'static str| {
    let function_prototype = Rc::clone(function_prototype);
    let bound_function_prototype = Rc::clone(&function_prototype);
    let getter = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let Value::Object(object) = &receiver else {
          return Err(vm.throw_type_error("Intl method called on incompatible receiver"));
        };
        if !matches!(object.borrow().internal_slot("[[IntlObject]]"), Some(Value::String(name)) if name == intl_name)
        {
          return Err(vm.throw_type_error("Intl method called on incompatible receiver"));
        }
        let bound = builtin_object(
          Rc::new(move |vm, args| {
            let offset = usize::from(vm.builtin_this_arg_prepended());
            if property_name == "compare" {
              let left = vm.to_string(args.get(offset).unwrap_or(&Value::Undefined))?;
              let right = vm.to_string(args.get(offset + 1).unwrap_or(&Value::Undefined))?;
              Ok(Value::Number(match left.cmp(&right) {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
              }))
            } else {
              let value = args.get(offset).unwrap_or(&Value::Undefined);
              Ok(Value::String(
                js_number_to_string(vm.to_number_value(value)?).into(),
              ))
            }
          }),
          &bound_function_prototype,
        );
        set_builtin_name(&bound, "");
        set_builtin_length(&bound, if property_name == "compare" { 2.0 } else { 1.0 });
        Ok(bound)
      }),
      &function_prototype,
    );
    set_builtin_name(&getter, &format!("get {property_name}"));
    set_builtin_length(&getter, 0.0);
    getter
  };
  for (prototype, intl_name, property_name) in [
    (&collator_prototype, "Collator", "compare"),
    (&date_time_format_prototype, "DateTimeFormat", "format"),
  ] {
    prototype.borrow_mut().properties.insert(
      PropertyKey::String(property_name.into()),
      PropertyDescriptor::Accessor {
        get: Some(make_bound_function_getter(intl_name, property_name)),
        set: None,
        enumerable: false,
        configurable: true,
      },
    );
  }

  let date_format_to_parts = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let Value::Object(object) = &receiver else {
        return Err(
          vm.throw_type_error("Intl.DateTimeFormat method called on incompatible receiver"),
        );
      };
      if !matches!(object.borrow().internal_slot("[[IntlObject]]"), Some(Value::String(name)) if name == "DateTimeFormat")
      {
        return Err(
          vm.throw_type_error("Intl.DateTimeFormat method called on incompatible receiver"),
        );
      }
      let number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      Ok(array_from_values(vec![part(
        vm,
        "literal",
        js_number_to_string(number),
      )]))
    }),
    function_prototype,
  );
  set_builtin_name(&date_format_to_parts, "formatToParts");
  set_builtin_length(&date_format_to_parts, 1.0);
  date_time_format_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("formatToParts".into()),
      PropertyDescriptor::builtin_data(date_format_to_parts),
    );

  let number_format_getter = builtin_object(
    Rc::new({
      let function_prototype = Rc::clone(function_prototype);
      move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let Value::Object(object) = &receiver else {
          return Err(
            vm.throw_type_error("Intl.NumberFormat format getter called on incompatible receiver"),
          );
        };
        if !matches!(object.borrow().internal_slot("[[IntlObject]]"), Some(Value::String(name)) if name == "NumberFormat")
        {
          return Err(
            vm.throw_type_error("Intl.NumberFormat format getter called on incompatible receiver"),
          );
        }
        let format = builtin_object(
          Rc::new(|vm, args| {
            let value = args
              .get(1)
              .or_else(|| args.first())
              .unwrap_or(&Value::Undefined);
            Ok(Value::String(
              js_number_to_string(vm.to_number_value(value)?).into(),
            ))
          }),
          &function_prototype,
        );
        set_builtin_name(&format, "");
        set_builtin_length(&format, 1.0);
        Ok(format)
      }
    }),
    function_prototype,
  );
  set_builtin_name(&number_format_getter, "get format");
  set_builtin_length(&number_format_getter, 0.0);
  number_format_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("format".into()),
      PropertyDescriptor::Accessor {
        get: Some(number_format_getter),
        set: None,
        enumerable: false,
        configurable: true,
      },
    );

  let number_format_to_parts = builtin_object(
    Rc::new(|vm, args| {
      let number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let kind = if number.is_nan() {
        "nan"
      } else if number.is_infinite() {
        "infinity"
      } else {
        "integer"
      };
      Ok(array_from_values(vec![part(
        vm,
        kind,
        js_number_to_string(number),
      )]))
    }),
    function_prototype,
  );
  set_builtin_name(&number_format_to_parts, "formatToParts");
  set_builtin_length(&number_format_to_parts, 1.0);
  number_format_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("formatToParts".into()),
      PropertyDescriptor::builtin_data(number_format_to_parts),
    );

  let resolved_options = builtin_object(
    Rc::new(|vm, _args| {
      Ok(Value::Object(Rc::new(RefCell::new(
        ObjectData::with_prototype(vm.realm().object_prototype()),
      ))))
    }),
    function_prototype,
  );
  set_builtin_name(&resolved_options, "resolvedOptions");
  set_builtin_length(&resolved_options, 0.0);
  plural_rules_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("resolvedOptions".into()),
      PropertyDescriptor::builtin_data(resolved_options),
    );
  let supported_locales = builtin_object(
    Rc::new(|vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      canonicalize_locale_list(
        vm,
        args
          .get(offset)
          .cloned()
          .unwrap_or(Value::Undefined),
      )
    }),
    function_prototype,
  );
  set_builtin_name(&supported_locales, "supportedLocalesOf");
  set_builtin_length(&supported_locales, 1.0);
  if let Value::BuiltinObject(_, properties) = &plural_rules {
    properties.borrow_mut().properties.insert(
      PropertyKey::String("supportedLocalesOf".into()),
      PropertyDescriptor::builtin_data(supported_locales),
    );
  }

  Value::Object(intl)
}
