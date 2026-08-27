// SPDX-License-Identifier: MPL-2.0
//! Text and symbol builtin functions and prototype methods

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_string::{JsString, JsStringBuilder};
use num_traits::ToPrimitive;
use unicode_normalization::UnicodeNormalization;

use crate::runtime_utils::{
  array_from_values, builtin_object, require_object_coercible, set_builtin_length,
  set_builtin_name, static_builtin_arg_offset, this_boolean_value, this_string_value, to_length,
  usize_to_decimal_string, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

thread_local! {
  /// Agent-wide symbol registry used by `Symbol.for` and `Symbol.keyFor`.
  static SYMBOL_REGISTRY: RefCell<HashMap<JsString, SymbolId>> = RefCell::new(HashMap::new());
}

/// Return whether `id` belongs to the agent-wide `Symbol.for` registry.
pub(crate) fn is_registered_symbol(id: &SymbolId) -> bool {
  SYMBOL_REGISTRY.with(|registry| {
    registry
      .borrow()
      .values()
      .any(|registered| registered == id)
  })
}

pub(crate) struct TextAndSymbolIntrinsics {
  pub(crate) string_constructor: Value,
  pub(crate) string_prototype: Value,
  pub(crate) string_iterator_prototype: Value,
  pub(crate) boolean_constructor: Value,
  pub(crate) boolean_prototype: Value,
  pub(crate) symbol_constructor: Value,
  pub(crate) symbol_prototype: Value,
}

fn utf16_encode_code_point(cp: u32) -> JsString {
  if cp <= 0xFFFF {
    return JsString::from_utf16(vec![cp as u16]);
  }
  let mut builder = JsStringBuilder::new();
  let _ = builder.push_code_point(cp);
  builder.finish()
}

/// Normalize valid Unicode subsequences while keeping isolated UTF-16
/// surrogates as exact code units. Rust strings cannot represent those
/// surrogates, but ECMAScript strings can.
fn normalize_string(
  value: &JsString,
  form: &str,
) -> JsString {
  fn normalize_segment(
    segment: &[u16],
    form: &str,
    output: &mut JsStringBuilder,
  ) {
    if segment.is_empty() {
      return;
    }
    let segment = String::from_utf16(segment)
      .expect("normalization segments contain only valid Unicode scalar values");
    match form {
      "NFC" => {
        for character in segment.nfc() {
          let _ = output.push_code_point(character.into());
        }
      }
      "NFD" => {
        for character in segment.nfd() {
          let _ = output.push_code_point(character.into());
        }
      }
      "NFKC" => {
        for character in segment.nfkc() {
          let _ = output.push_code_point(character.into());
        }
      }
      "NFKD" => {
        for character in segment.nfkd() {
          let _ = output.push_code_point(character.into());
        }
      }
      _ => unreachable!("String.prototype.normalize validates its form"),
    }
  }

  let mut output = JsStringBuilder::with_capacity(value.len());
  let mut segment_start = 0;
  let mut index = 0;
  while let Some(point) = value.code_point_at(index, true) {
    if char::from_u32(point.value()).is_none() {
      normalize_segment(&value.as_slice()[segment_start..index], form, &mut output);
      output.push_code_unit(point.value() as u16);
      index += point.width();
      segment_start = index;
    } else {
      index += point.width();
    }
  }
  normalize_segment(&value.as_slice()[segment_start..], form, &mut output);
  output.finish()
}

fn string_constructor_to_string(
  vm: &mut Vm,
  value: &Value,
) -> Result<JsString, crate::errors::RuntimeError> {
  match value {
    Value::Symbol(_, desc) if vm.pending_constructor_new_target().is_some() => {
      Err(vm.throw_type_error("Cannot convert a Symbol value to a string"))
    }
    Value::Symbol(_, desc) => Ok(match desc {
      Some(d) => format!("Symbol({})", d).into(),
      None => "Symbol()".into(),
    }),
    Value::String(_)
    | Value::Number(_)
    | Value::Bool(_)
    | Value::Null
    | Value::Undefined
    | Value::BigInt(_) => vm.to_string(value),
    _ => {
      let prim = vm.to_primitive(value, Some("string"))?;
      if matches!(
        prim,
        Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::Builtin(_)
          | Value::BuiltinObject(_, _)
          | Value::Promise(..)
      ) {
        return Err(vm.throw_type_error("Cannot convert object to string"));
      }
      vm.to_string(&prim)
    }
  }
}

/// Installs the String, Boolean, and Symbol builtins into the globals object.
pub(crate) fn install_string_boolean_symbol_builtins(
  globals: &mut ObjectData,
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> TextAndSymbolIntrinsics {
  use PropertyKey as PKey;

  let string_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let string_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  string_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&string_proto_map))),
  );

  let boolean_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  boolean_proto_map
    .borrow_mut()
    .set_internal_slot("[[BooleanData]]", Value::Bool(false));
  let boolean_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
  ));
  boolean_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(&boolean_proto_map)),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );

  let symbol_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let symbol_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.pending_constructor_new_target().is_some() {
      return Err(vm.throw_type_error("Symbol is not a constructor"));
    }
    let value_idx = 0;
    let desc = match args.get(value_idx) {
      Some(Value::Undefined) | None => None,
      Some(Value::String(s)) => Some(s.clone()),
      Some(v) => Some(vm.to_string(v)?),
    };
    Ok(Value::Symbol(SymbolId::new(), desc))
  });
  let symbol_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  symbol_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&symbol_proto_map))),
  );
  symbol_ctor_map.borrow_mut().properties.insert(
    PKey::String("for".to_string().into()),
    PropertyDescriptor::builtin_data({
      let symbol_for = builtin_object(
        Rc::new(|vm, args| {
          let offset = static_builtin_arg_offset(vm, args, "Symbol");
          let key = vm.to_string(args.get(offset).unwrap_or(&Value::Undefined))?;
          let id = SYMBOL_REGISTRY.with(|registry| {
            registry
              .borrow_mut()
              .entry(key.clone())
              .or_insert_with(SymbolId::new)
              .clone()
          });
          Ok(Value::Symbol(id, Some(key)))
        }),
        function_proto_map,
      );
      set_builtin_name(&symbol_for, "for");
      set_builtin_length(&symbol_for, 1.0);
      symbol_for
    }),
  );
  symbol_ctor_map.borrow_mut().properties.insert(
    PKey::String("keyFor".to_string().into()),
    PropertyDescriptor::builtin_data({
      let key_for = builtin_object(
        Rc::new(|vm, args| {
          let offset = static_builtin_arg_offset(vm, args, "Symbol");
          let Some(Value::Symbol(id, _)) = args.get(offset) else {
            return Err(vm.throw_type_error("Symbol.keyFor requires a symbol"));
          };
          Ok(SYMBOL_REGISTRY.with(|registry| {
            registry
              .borrow()
              .iter()
              .find_map(|(key, registered)| (registered == id).then(|| Value::String(key.clone())))
              .unwrap_or(Value::Undefined)
          }))
        }),
        function_proto_map,
      );
      set_builtin_name(&key_for, "keyFor");
      set_builtin_length(&key_for, 1.0);
      key_for
    }),
  );
  let symbol_ctor_val =
    Value::BuiltinObject(Rc::clone(&symbol_ctor_fn), Rc::clone(&symbol_ctor_map));
  set_builtin_length(&symbol_ctor_val, 0.0);
  symbol_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(symbol_ctor_val.clone()),
  );
  symbol_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Symbol".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  let symbol_to_string = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      match this {
        Value::Symbol(_, desc) => Ok(Value::String(match desc {
          Some(d) => format!("Symbol({})", d).into(),
          None => "Symbol()".to_string().into(),
        })),
        Value::Object(obj) => {
          let borrowed = obj.borrow();
          if let Some(Value::Symbol(_, desc)) = borrowed.internal_slot("[[SymbolData]]") {
            Ok(Value::String(match desc {
              Some(d) => format!("Symbol({})", d).into(),
              None => "Symbol()".to_string().into(),
            }))
          } else {
            Err(vm.throw_type_error("Symbol.prototype.toString called on incompatible receiver"))
          }
        }
        _ => Err(vm.throw_type_error("Symbol.prototype.toString called on incompatible receiver")),
      }
    }),
    function_proto_map,
  );
  set_builtin_name(&symbol_to_string, "toString");
  set_builtin_length(&symbol_to_string, 0.0);
  symbol_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(symbol_to_string),
  );
  let symbol_value_of = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      match this {
        Value::Symbol(_, _) => Ok(this.clone()),
        Value::Object(obj) => {
          let borrowed = obj.borrow();
          if let Some(Value::Symbol(id, desc)) = borrowed.internal_slot("[[SymbolData]]") {
            return Ok(Value::Symbol(id.clone(), desc.clone()));
          }
          Err(vm.throw_type_error("Symbol.prototype.valueOf called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("Symbol.prototype.valueOf called on incompatible receiver")),
      }
    }),
    function_proto_map,
  );
  set_builtin_name(&symbol_value_of, "valueOf");
  set_builtin_length(&symbol_value_of, 0.0);
  symbol_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string().into()),
    PropertyDescriptor::builtin_data(symbol_value_of),
  );
  let symbol_to_primitive = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().unwrap_or(&Value::Undefined);
      match this {
        Value::Symbol(_, _) => Ok(this.clone()),
        Value::Object(object) => object
          .borrow()
          .internal_slot("[[SymbolData]]")
          .and_then(|value| match value {
            Value::Symbol(id, description) => Some(Value::Symbol(id.clone(), description.clone())),
            _ => None,
          })
          .ok_or_else(|| {
            vm.throw_type_error(
              "Symbol.prototype[Symbol.toPrimitive] called on incompatible receiver",
            )
          }),
        _ => Err(vm.throw_type_error(
          "Symbol.prototype[Symbol.toPrimitive] called on incompatible receiver",
        )),
      }
    }),
    function_proto_map,
  );
  set_builtin_name(&symbol_to_primitive, "[Symbol.toPrimitive]");
  set_builtin_length(&symbol_to_primitive, 1.0);
  symbol_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_primitive()),
    PropertyDescriptor::Data {
      value: symbol_to_primitive,
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  let symbol_description = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().unwrap_or(&Value::Undefined);
      match this {
        Value::Symbol(_, description) => Ok(
          description
            .clone()
            .map_or(Value::Undefined, Value::String),
        ),
        Value::Object(object) => object
          .borrow()
          .internal_slot("[[SymbolData]]")
          .and_then(|value| match value {
            Value::Symbol(_, description) => Some(
              description
                .clone()
                .map_or(Value::Undefined, Value::String),
            ),
            _ => None,
          })
          .ok_or_else(|| {
            vm.throw_type_error("Symbol.prototype.description called on incompatible receiver")
          }),
        _ => {
          Err(vm.throw_type_error("Symbol.prototype.description called on incompatible receiver"))
        }
      }
    }),
    function_proto_map,
  );
  set_builtin_name(&symbol_description, "get description");
  set_builtin_length(&symbol_description, 0.0);
  symbol_proto_map.borrow_mut().properties.insert(
    PKey::String("description".to_string().into()),
    PropertyDescriptor::Accessor {
      get: Some(symbol_description),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );
  for (name, sym) in [
    (
      "iterator",
      Value::Symbol(
        SymbolId::well_known_iterator(),
        Some("Symbol.iterator".to_string().into()),
      ),
    ),
    (
      "asyncIterator",
      Value::Symbol(
        SymbolId::well_known_async_iterator(),
        Some("Symbol.asyncIterator".to_string().into()),
      ),
    ),
    (
      "toPrimitive",
      Value::Symbol(
        SymbolId::well_known_to_primitive(),
        Some("Symbol.toPrimitive".to_string().into()),
      ),
    ),
    (
      "toStringTag",
      Value::Symbol(
        SymbolId::well_known_to_string_tag(),
        Some("Symbol.toStringTag".to_string().into()),
      ),
    ),
    (
      "species",
      Value::Symbol(
        SymbolId::well_known_species(),
        Some("Symbol.species".to_string().into()),
      ),
    ),
    (
      "hasInstance",
      Value::Symbol(
        SymbolId::well_known_has_instance(),
        Some("Symbol.hasInstance".to_string().into()),
      ),
    ),
    (
      "isConcatSpreadable",
      Value::Symbol(
        SymbolId::well_known_is_concat_spreadable(),
        Some("Symbol.isConcatSpreadable".to_string().into()),
      ),
    ),
    (
      "match",
      Value::Symbol(
        SymbolId::well_known_match(),
        Some("Symbol.match".to_string().into()),
      ),
    ),
    (
      "matchAll",
      Value::Symbol(
        SymbolId::well_known_match_all(),
        Some("Symbol.matchAll".to_string().into()),
      ),
    ),
    (
      "replace",
      Value::Symbol(
        SymbolId::well_known_replace(),
        Some("Symbol.replace".to_string().into()),
      ),
    ),
    (
      "search",
      Value::Symbol(
        SymbolId::well_known_search(),
        Some("Symbol.search".to_string().into()),
      ),
    ),
    (
      "split",
      Value::Symbol(
        SymbolId::well_known_split(),
        Some("Symbol.split".to_string().into()),
      ),
    ),
    (
      "unscopables",
      Value::Symbol(
        SymbolId::well_known_unscopables(),
        Some("Symbol.unscopables".to_string().into()),
      ),
    ),
  ] {
    symbol_ctor_map.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::immutable_data(sym),
    );
  }

  let string_proto_for_ctor = Rc::clone(&string_proto_map);
  let string_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let constructing = vm.pending_constructor_new_target().is_some();
    let value_idx = if constructing || args.len() > 1 { 1 } else { 0 };
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let s = match args.get(value_idx) {
      None => JsString::empty(),
      Some(v) => string_constructor_to_string(vm, v)?,
    };
    if constructing && let Value::Object(obj_rc) = &receiver {
      {
        let mut obj = obj_rc.borrow_mut();
        obj.set_prototype_value(Value::Object(Rc::clone(&string_proto_for_ctor)));
        obj.set_internal_slot("[[PrimitiveValue]]", Value::String(s.clone()));
        obj.properties.insert(
          PKey::String("length".to_string().into()),
          PropertyDescriptor::Data {
            value: Value::Number(usize_to_f64(s.len())),
            writable: false,
            enumerable: false,
            configurable: false,
          },
        );
        for idx in 0..s.len() {
          obj.properties.insert(
            PKey::String(usize_to_decimal_string(idx).into()),
            PropertyDescriptor::Data {
              value: Value::String(
                s.slice(idx..idx + 1)
                  .expect("string index is within bounds"),
              ),
              writable: false,
              enumerable: true,
              configurable: false,
            },
          );
        }
      }
      return Ok(receiver);
    }
    Ok(Value::String(s))
  });
  let string_ctor_val =
    Value::BuiltinObject(Rc::clone(&string_ctor_fn), Rc::clone(&string_ctor_map));
  set_builtin_length(&string_ctor_val, 1.0);
  set_builtin_name(&string_ctor_val, "String");
  string_ctor_map.borrow_mut().properties.insert(
    PKey::String("fromCharCode".to_string().into()),
    PropertyDescriptor::builtin_data({
      let from_char_code = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "String");
          let mut out = JsStringBuilder::new();
          for arg in args.iter().skip(off) {
            let n = vm.to_number_value(arg)?;
            let int = if n.is_nan() || !n.is_finite() {
              0u32
            } else {
              n.trunc() as i64 as u32
            };
            let unit = (int & 0xFFFF) as u16;
            out.push_code_unit(unit);
          }
          Ok(Value::String(out.finish()))
        }),
        function_proto_map,
      );
      set_builtin_name(&from_char_code, "fromCharCode");
      set_builtin_length(&from_char_code, 1.0);
      from_char_code
    }),
  );
  string_ctor_map.borrow_mut().properties.insert(
    PKey::String("fromCodePoint".to_string().into()),
    PropertyDescriptor::builtin_data({
      let from_code_point = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "String");
          let mut out = JsStringBuilder::new();
          for arg in args.iter().skip(off) {
            let n = vm.to_number_value(arg)?;
            if !n.is_finite() || n.fract() != 0.0 || !(0.0..=0x10FFFF as f64).contains(&n) {
              return Err(vm.throw_range_error("Invalid code point"));
            }
            out.push_string(&utf16_encode_code_point(n as u32));
          }
          Ok(Value::String(out.finish()))
        }),
        function_proto_map,
      );
      set_builtin_name(&from_code_point, "fromCodePoint");
      set_builtin_length(&from_code_point, 1.0);
      from_code_point
    }),
  );
  string_ctor_map.borrow_mut().properties.insert(
    PKey::String("raw".to_string().into()),
    PropertyDescriptor::builtin_data({
      let raw_fn = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "String");
          let template = args.get(off).cloned().unwrap_or(Value::Undefined);
          let template_obj = vm.to_object_value(template)?;
          let raw = vm.get_property_host(&template_obj, &PKey::String("raw".to_string().into()))?;
          let raw_obj =
            if matches!(raw, Value::Undefined) && matches!(template_obj, Value::Array(..)) {
              template_obj.clone()
            } else {
              vm.to_object_value(raw)?
            };
          let len_value =
            vm.get_property_host(&raw_obj, &PKey::String("length".to_string().into()))?;
          let literal_count = to_length(vm, &len_value)?;
          if literal_count == 0 {
            return Ok(Value::String(String::new().into()));
          }
          let substitutions = args.get(off + 1..).unwrap_or(&[]);
          let mut out = JsStringBuilder::new();
          for i in 0..literal_count {
            let next =
              vm.get_property_host(&raw_obj, &PKey::String(usize_to_decimal_string(i).into()))?;
            out.push_string(&vm.to_string(&next)?);
            if i + 1 == literal_count {
              break;
            }
            if let Some(subst) = substitutions.get(i) {
              out.push_string(&vm.to_string(subst)?);
            }
          }
          Ok(Value::String(out.finish()))
        }),
        function_proto_map,
      );
      set_builtin_name(&raw_fn, "raw");
      set_builtin_length(&raw_fn, 1.0);
      raw_fn
    }),
  );
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(string_ctor_val.clone()),
  );
  let string_value_of = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      Ok(Value::String(this_string_value(vm, this)?))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_value_of, "valueOf");
  set_builtin_length(&string_value_of, 0.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string().into()),
    PropertyDescriptor::builtin_data(string_value_of),
  );
  let string_to_string = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      Ok(Value::String(this_string_value(vm, this)?))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_to_string, "toString");
  set_builtin_length(&string_to_string, 0.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(string_to_string),
  );
  const ITERATED_STRING: &str = "[[IteratedString]]";
  const STRING_ITERATOR_NEXT_INDEX: &str = "[[StringIteratorNextIndex]]";
  let string_iterator_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let string_iterator_next = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let Value::Object(iterator) = &receiver else {
        return Err(
          vm.throw_type_error("String Iterator.prototype.next called on incompatible receiver"),
        );
      };
      let (iterated, index) = {
        let iterator = iterator.borrow();
        let Some(iterated) = iterator.internal_slot(ITERATED_STRING).cloned() else {
          return Err(
            vm.throw_type_error("String Iterator.prototype.next called on incompatible receiver"),
          );
        };
        let index = iterator
          .internal_slot(STRING_ITERATOR_NEXT_INDEX)
          .and_then(|value| match value {
            Value::Number(index) => index.trunc().to_usize(),
            _ => None,
          })
          .unwrap_or(0);
        (iterated, index)
      };

      let iterator_result = |value, done| {
        let mut result = ObjectData::with_prototype(vm.realm().object_prototype());
        result.properties.insert(
          PKey::String("value".into()),
          PropertyDescriptor::ordinary_data(value),
        );
        result.properties.insert(
          PKey::String("done".into()),
          PropertyDescriptor::ordinary_data(Value::Bool(done)),
        );
        Value::Object(Rc::new(RefCell::new(result)))
      };

      let Value::String(string) = iterated else {
        return Ok(iterator_result(Value::Undefined, true));
      };
      let Some(point) = string.code_point_at(index, true) else {
        iterator
          .borrow_mut()
          .set_internal_slot(ITERATED_STRING, Value::Undefined);
        return Ok(iterator_result(Value::Undefined, true));
      };
      let next_index = index + point.width();
      iterator.borrow_mut().set_internal_slot(
        STRING_ITERATOR_NEXT_INDEX,
        Value::Number(usize_to_f64(next_index)),
      );
      Ok(iterator_result(
        Value::String(
          string
            .slice(index..next_index)
            .expect("string iterator code point is within bounds"),
        ),
        false,
      ))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_iterator_next, "next");
  set_builtin_length(&string_iterator_next, 0.0);
  string_iterator_proto
    .borrow_mut()
    .properties
    .insert(
      PKey::String("next".into()),
      PropertyDescriptor::builtin_data(string_iterator_next),
    );
  string_iterator_proto
    .borrow_mut()
    .properties
    .insert(
      PKey::Symbol(SymbolId::well_known_to_string_tag()),
      PropertyDescriptor::Data {
        value: Value::String("String Iterator".into()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
  let string_iterator_proto_for_instances = Rc::clone(&string_iterator_proto);
  let string_iterator = builtin_object(
    Rc::new(move |vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let iterator = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&string_iterator_proto_for_instances),
      ))));
      iterator
        .borrow_mut()
        .set_internal_slot(ITERATED_STRING, Value::String(string));
      iterator
        .borrow_mut()
        .set_internal_slot(STRING_ITERATOR_NEXT_INDEX, Value::Number(0.0));
      Ok(Value::Object(iterator))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_iterator, "[Symbol.iterator]");
  set_builtin_length(&string_iterator, 0.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::builtin_data(string_iterator),
  );
  let string_index_of = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let haystack = vm.to_string(receiver)?;
      let needle_undefined = Value::Undefined;
      let needle_value = args.get(1).unwrap_or(&needle_undefined);
      let needle = match needle_value {
        Value::String(s) => s.clone(),
        _ => vm.to_string(needle_value)?,
      };
      let length = haystack.len();
      let start = if let Some(pos) = args.get(2) {
        let n = vm.to_number_value(pos)?;
        if n.is_nan() || n <= 0.0 {
          0usize
        } else {
          n.floor().to_usize().unwrap_or(length).min(length)
        }
      } else {
        0usize
      };
      if needle.is_empty() {
        return Ok(Value::Number(usize_to_f64(start)));
      }
      if let Some(index) = haystack.find(&needle, start) {
        return Ok(Value::Number(usize_to_f64(index)));
      }
      Ok(Value::Number(-1.0))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_index_of, "indexOf");
  set_builtin_length(&string_index_of, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("indexOf".to_string().into()),
    PropertyDescriptor::builtin_data(string_index_of),
  );

  let string_last_index_of = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let haystack = vm.to_string(receiver)?;
      let needle = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      let length = haystack.len();
      let position = match args.get(2) {
        None => length,
        Some(value) => {
          let number = vm.to_number_value(value)?;
          if number.is_nan() || number == f64::INFINITY {
            length
          } else if number <= 0.0 {
            0
          } else if number >= usize_to_f64(length) {
            length
          } else {
            number.trunc().to_usize().unwrap_or(length)
          }
        }
      };
      if needle.is_empty() {
        return Ok(Value::Number(usize_to_f64(position)));
      }
      for index in (0..=position).rev() {
        let end = index.saturating_add(needle.len());
        if end <= length
          && haystack
            .slice(index..end)
            .expect("lastIndexOf bounds were checked")
            == needle
        {
          return Ok(Value::Number(usize_to_f64(index)));
        }
      }
      Ok(Value::Number(-1.0))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_last_index_of, "lastIndexOf");
  set_builtin_length(&string_last_index_of, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("lastIndexOf".to_string().into()),
    PropertyDescriptor::builtin_data(string_last_index_of),
  );

  let string_starts_with = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let value = vm.to_string(receiver)?;
      let search = args.get(1).unwrap_or(&undefined);
      if search.is_object() {
        let matcher = vm.get_property_host(search, &PKey::Symbol(SymbolId::well_known_match()))?;
        if !matches!(matcher, Value::Undefined) && vm.to_boolean(&matcher) {
          return Err(vm.throw_type_error(
            "First argument to String.prototype.startsWith must not be a RegExp",
          ));
        }
      }
      let search = vm.to_string(search)?;
      let length = value.len();
      let position = match args.get(2) {
        Some(position) => {
          let number = vm.to_number_value(position)?;
          if number.is_nan() || number == f64::NEG_INFINITY || number <= 0.0 {
            0
          } else if number == f64::INFINITY {
            length
          } else {
            number
              .floor()
              .to_usize()
              .unwrap_or(length)
              .min(length)
          }
        }
        None => 0,
      };
      let suffix = value
        .slice(position..value.len())
        .expect("position was clamped to string length");
      Ok(Value::Bool(suffix.starts_with(&search)))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_starts_with, "startsWith");
  set_builtin_length(&string_starts_with, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("startsWith".to_string().into()),
    PropertyDescriptor::builtin_data(string_starts_with),
  );

  for (name, ends_with) in [("includes", false), ("endsWith", true)] {
    let method = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().unwrap_or(&Value::Undefined);
        require_object_coercible(vm, receiver)?;
        let string = vm.to_string(receiver)?;
        let search_value = args.get(1).unwrap_or(&Value::Undefined);
        if crate::ecmascript::builtins::regexp::is_regexp(vm, search_value)? {
          return Err(vm.throw_type_error(&format!(
            "First argument to String.prototype.{name} must not be a RegExp"
          )));
        }
        let search = vm.to_string(search_value)?;
        let number = match args.get(2) {
          Some(value) => vm.to_number_value(value)?,
          None if ends_with => string.len().to_f64().unwrap_or(f64::INFINITY),
          None => 0.0,
        };
        let position = if number.is_nan() || number <= 0.0 {
          0
        } else if number == f64::INFINITY {
          string.len()
        } else {
          number
            .trunc()
            .to_usize()
            .unwrap_or(string.len())
            .min(string.len())
        };
        if ends_with {
          if search.len() > position {
            return Ok(Value::Bool(false));
          }
          let start = position - search.len();
          return Ok(Value::Bool(
            string
              .slice(start..position)
              .expect("endsWith bounds are valid")
              == search,
          ));
        }
        Ok(Value::Bool(string.find(&search, position).is_some()))
      }),
      function_proto_map,
    );
    set_builtin_name(&method, name);
    set_builtin_length(&method, 1.0);
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::builtin_data(method),
    );
  }

  let string_char_at = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let index = if number.is_nan() {
        Some(0)
      } else if number < 0.0 || !number.is_finite() {
        None
      } else {
        number.trunc().to_usize()
      };
      Ok(
        index
          .and_then(|index| string.code_unit_at(index))
          .map_or_else(
            || Value::String(JsString::empty()),
            |unit| Value::String(JsString::from_utf16(vec![unit])),
          ),
      )
    }),
    function_proto_map,
  );
  set_builtin_name(&string_char_at, "charAt");
  set_builtin_length(&string_char_at, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("charAt".into()),
    PropertyDescriptor::builtin_data(string_char_at),
  );

  let string_at = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      if number.is_infinite() {
        return Ok(Value::Undefined);
      }
      let relative = if number.is_nan() {
        0
      } else {
        number.trunc() as isize
      };
      let index = if relative < 0 {
        string.len().to_isize().unwrap_or(isize::MAX) + relative
      } else {
        relative
      };
      let Ok(index) = usize::try_from(index) else {
        return Ok(Value::Undefined);
      };
      Ok(
        string
          .code_unit_at(index)
          .map_or(Value::Undefined, |unit| {
            Value::String(JsString::from_utf16(vec![unit]))
          }),
      )
    }),
    function_proto_map,
  );
  set_builtin_name(&string_at, "at");
  set_builtin_length(&string_at, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("at".into()),
    PropertyDescriptor::builtin_data(string_at),
  );

  let string_concat = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let mut output = JsStringBuilder::new();
      output.push_string(&vm.to_string(receiver)?);
      for value in args.iter().skip(1) {
        output.push_string(&vm.to_string(value)?);
      }
      Ok(Value::String(output.finish()))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_concat, "concat");
  set_builtin_length(&string_concat, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("concat".into()),
    PropertyDescriptor::builtin_data(string_concat),
  );

  let string_normalize = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let form = match args.get(1) {
        None | Some(Value::Undefined) => JsString::from("NFC"),
        Some(value) => vm.to_string(value)?,
      };
      match form.as_slice() {
        [0x004E, 0x0046, 0x0043]
        | [0x004E, 0x0046, 0x0044]
        | [0x004E, 0x0046, 0x004B, 0x0043]
        | [0x004E, 0x0046, 0x004B, 0x0044] => {
          let form = form.as_str_lossy();
          Ok(Value::String(normalize_string(&string, form)))
        }
        _ => Err(vm.throw_range_error("The normalization form must be NFC, NFD, NFKC, or NFKD")),
      }
    }),
    function_proto_map,
  );
  set_builtin_name(&string_normalize, "normalize");
  set_builtin_length(&string_normalize, 0.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("normalize".into()),
    PropertyDescriptor::builtin_data(string_normalize),
  );

  let string_substring = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let clamp = |number: f64, length: usize| {
        if number.is_nan() || number <= 0.0 {
          0
        } else if number == f64::INFINITY {
          length
        } else {
          number
            .trunc()
            .to_usize()
            .unwrap_or(length)
            .min(length)
        }
      };
      let start = clamp(
        vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?,
        string.len(),
      );
      let end = match args.get(2) {
        None | Some(Value::Undefined) => string.len(),
        Some(value) => clamp(vm.to_number_value(value)?, string.len()),
      };
      let (from, to) = if start <= end {
        (start, end)
      } else {
        (end, start)
      };
      Ok(Value::String(
        string
          .slice(from..to)
          .expect("substring bounds were clamped"),
      ))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_substring, "substring");
  set_builtin_length(&string_substring, 2.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("substring".into()),
    PropertyDescriptor::builtin_data(string_substring),
  );

  let string_substr = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let length = string.len();
      let start_number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let start_integer = if start_number.is_nan() {
        0.0
      } else {
        start_number.trunc()
      };
      let start = if start_integer < 0.0 {
        (length as f64 + start_integer).max(0.0) as usize
      } else {
        start_integer.min(length as f64) as usize
      };
      let count = match args.get(2) {
        None | Some(Value::Undefined) => length - start,
        Some(value) => {
          let number = vm.to_number_value(value)?;
          let integer = if number.is_nan() { 0.0 } else { number.trunc() };
          integer.max(0.0).min((length - start) as f64) as usize
        }
      };
      Ok(Value::String(
        string
          .slice(start..start + count)
          .expect("substr bounds were clamped"),
      ))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_substr, "substr");
  set_builtin_length(&string_substr, 2.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("substr".into()),
    PropertyDescriptor::builtin_data(string_substr),
  );

  for (name, open, close, argument_name) in [
    ("anchor", "<a name=\"", "</a>", Some("name")),
    ("big", "<big>", "</big>", None),
    ("blink", "<blink>", "</blink>", None),
    ("bold", "<b>", "</b>", None),
    ("fixed", "<tt>", "</tt>", None),
    ("fontcolor", "<font color=\"", "</font>", Some("color")),
    ("fontsize", "<font size=\"", "</font>", Some("size")),
    ("italics", "<i>", "</i>", None),
    ("link", "<a href=\"", "</a>", Some("href")),
    ("small", "<small>", "</small>", None),
    ("strike", "<strike>", "</strike>", None),
    ("sub", "<sub>", "</sub>", None),
    ("sup", "<sup>", "</sup>", None),
  ] {
    let method = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().unwrap_or(&Value::Undefined);
        require_object_coercible(vm, receiver)?;
        let string = vm.to_string(receiver)?;
        let mut result = js_string::JsStringBuilder::new();
        result.push_str(open);
        if argument_name.is_some() {
          let argument = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
          for unit in argument.code_units() {
            if unit == u16::from(b'"') {
              result.push_str("&quot;");
            } else {
              result.push_code_unit(unit);
            }
          }
          result.push_str("\">");
        }
        result.push_string(&string);
        result.push_str(close);
        Ok(Value::String(result.finish()))
      }),
      function_proto_map,
    );
    set_builtin_name(&method, name);
    set_builtin_length(&method, f64::from(argument_name.is_some()));
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::builtin_data(method),
    );
  }

  let string_repeat = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      if number < 0.0 || number == f64::INFINITY {
        return Err(vm.throw_range_error("Invalid count value"));
      }
      let count = if number.is_nan() {
        0
      } else {
        number.trunc().to_usize().unwrap_or(usize::MAX)
      };
      string
        .len()
        .checked_mul(count)
        .ok_or_else(|| vm.throw_range_error("Invalid string length"))?;
      let mut output = JsStringBuilder::new();
      for _ in 0..count {
        output.push_string(&string);
      }
      Ok(Value::String(output.finish()))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_repeat, "repeat");
  set_builtin_length(&string_repeat, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("repeat".into()),
    PropertyDescriptor::builtin_data(string_repeat),
  );

  for (name, at_start) in [("padStart", true), ("padEnd", false)] {
    let pad = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().unwrap_or(&Value::Undefined);
        require_object_coercible(vm, receiver)?;
        let string = vm.to_string(receiver)?;
        let target = to_length(vm, args.get(1).unwrap_or(&Value::Undefined))?;
        if target <= string.len() {
          return Ok(Value::String(string));
        }
        let filler = match args.get(2) {
          None | Some(Value::Undefined) => JsString::from(" "),
          Some(value) => vm.to_string(value)?,
        };
        if filler.is_empty() {
          return Ok(Value::String(string));
        }
        let fill_length = target - string.len();
        let mut builder = JsStringBuilder::new();
        let mut built_length = 0;
        while built_length < fill_length {
          builder.push_string(&filler);
          built_length += filler.len();
        }
        let fill = builder
          .finish()
          .slice(0..fill_length)
          .expect("padding was built to the target length");
        Ok(Value::String(if at_start {
          fill.concat(&string)
        } else {
          string.concat(&fill)
        }))
      }),
      function_proto_map,
    );
    set_builtin_name(&pad, name);
    set_builtin_length(&pad, 1.0);
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::builtin_data(pad),
    );
  }

  let string_char_code_at = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let s = vm.to_string(receiver)?;
      let pos = match args.get(1) {
        Some(v) => vm.to_number_value(v)?,
        None => 0.0,
      };
      let index = if pos.is_nan() || pos == 0.0 {
        0usize
      } else if !pos.is_finite() || pos < 0.0 {
        return Ok(Value::Number(f64::NAN));
      } else {
        pos.trunc().to_usize().unwrap_or(0)
      };
      match s.code_unit_at(index) {
        Some(code) => Ok(Value::Number(f64::from(code))),
        None => Ok(Value::Number(f64::NAN)),
      }
    }),
    function_proto_map,
  );
  set_builtin_name(&string_char_code_at, "charCodeAt");
  set_builtin_length(&string_char_code_at, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("charCodeAt".to_string().into()),
    PropertyDescriptor::builtin_data(string_char_code_at),
  );

  let string_code_point_at = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let number = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      if number.is_infinite() || number < 0.0 {
        return Ok(Value::Undefined);
      }
      let index = if number.is_nan() {
        0
      } else {
        number.trunc().to_usize().unwrap_or(usize::MAX)
      };
      let Some(first) = string.code_unit_at(index) else {
        return Ok(Value::Undefined);
      };
      if (0xD800..=0xDBFF).contains(&first)
        && let Some(second) = string.code_unit_at(index + 1)
        && (0xDC00..=0xDFFF).contains(&second)
      {
        let point = 0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00);
        return Ok(Value::Number(f64::from(point)));
      }
      Ok(Value::Number(f64::from(first)))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_code_point_at, "codePointAt");
  set_builtin_length(&string_code_point_at, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("codePointAt".into()),
    PropertyDescriptor::builtin_data(string_code_point_at),
  );

  let string_slice = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let s = vm.to_string(receiver)?;
      let len = s.len().to_isize().unwrap_or(isize::MAX);
      let to_int = |vm: &mut Vm, v: &Value| -> Result<isize, crate::errors::RuntimeError> {
        let n = vm.to_number_value(v)?;
        if n.is_nan() || n == 0.0 {
          return Ok(0);
        }
        if n == f64::NEG_INFINITY {
          return Ok(isize::MIN);
        }
        if n == f64::INFINITY {
          return Ok(isize::MAX);
        }
        Ok(n.trunc().to_isize().unwrap_or(0))
      };
      let start_raw = match args.get(1) {
        Some(v) => to_int(vm, v)?,
        None => 0,
      };
      let from = if start_raw < 0 {
        len.saturating_add(start_raw).max(0)
      } else {
        start_raw.min(len)
      };
      let end_raw_opt = args
        .get(2)
        .filter(|v| !matches!(v, Value::Undefined));
      let end_raw = match end_raw_opt {
        Some(v) => to_int(vm, v)?,
        None => len,
      };
      let to = if end_raw < 0 {
        len.saturating_add(end_raw).max(0)
      } else {
        end_raw.min(len)
      };
      if to <= from {
        return Ok(Value::String(String::new().into()));
      }
      let value = s
        .slice(usize::try_from(from).unwrap_or(0)..usize::try_from(to).unwrap_or(0))
        .expect("slice bounds were clamped to string length");
      Ok(Value::String(value))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_slice, "slice");
  set_builtin_length(&string_slice, 2.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("slice".to_string().into()),
    PropertyDescriptor::builtin_data(string_slice),
  );

  let install_trim = |name: &str, trim_start: bool, trim_end: bool| {
    let trim = builtin_object(
      Rc::new(move |vm, args| {
        let undefined = Value::Undefined;
        let receiver = args.first().unwrap_or(&undefined);
        require_object_coercible(vm, receiver)?;
        let value = vm.to_string(receiver)?;
        let is_trimmable = |unit: u16| {
          matches!(
            unit,
            0x0009 | 0x000A | 0x000B | 0x000C | 0x000D | 0x0020 | 0x00A0 | 0x1680 | 0x2000
              ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
          )
        };
        let mut start = 0;
        let mut end = value.len();
        if trim_start {
          while start < end && is_trimmable(value.as_slice()[start]) {
            start += 1;
          }
        }
        if trim_end {
          while end > start && is_trimmable(value.as_slice()[end - 1]) {
            end -= 1;
          }
        }
        Ok(Value::String(
          value
            .slice(start..end)
            .expect("trim bounds are within the string"),
        ))
      }),
      function_proto_map,
    );
    set_builtin_name(&trim, name);
    set_builtin_length(&trim, 0.0);
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::builtin_data(trim),
    );
  };
  install_trim("trim", true, true);
  install_trim("trimStart", true, false);
  install_trim("trimEnd", false, true);

  let install_case_conversion = |name: &str, uppercase: bool| {
    let conversion = builtin_object(
      Rc::new(move |vm, args| {
        let undefined = Value::Undefined;
        let receiver = args.first().unwrap_or(&undefined);
        require_object_coercible(vm, receiver)?;
        let value = vm.to_string(receiver)?;
        let converted = if uppercase {
          value.to_uppercase()
        } else {
          value.to_lowercase()
        };
        Ok(Value::String(converted))
      }),
      function_proto_map,
    );
    set_builtin_name(&conversion, name);
    set_builtin_length(&conversion, 0.0);
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::builtin_data(conversion),
    );
  };
  install_case_conversion("toLowerCase", false);
  install_case_conversion("toUpperCase", true);
  // ECMA-402 `Intl` is not available in this engine, so the permitted
  // non-Intl fallback is the default Unicode case conversion and ignores
  // locale arguments.
  install_case_conversion("toLocaleLowerCase", false);
  install_case_conversion("toLocaleUpperCase", true);

  // Without ECMA-402, locale-sensitive collation is implementation-defined.
  // Keep the fallback exact over UTF-16 code units rather than lossy UTF-8.
  let string_locale_compare = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let string = vm.to_string(receiver)?;
      let that = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(match string.cmp(&that) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
      }))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_locale_compare, "localeCompare");
  set_builtin_length(&string_locale_compare, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("localeCompare".into()),
    PropertyDescriptor::builtin_data(string_locale_compare),
  );

  for (name, to_well_formed) in [("isWellFormed", false), ("toWellFormed", true)] {
    let method = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().unwrap_or(&Value::Undefined);
        require_object_coercible(vm, receiver)?;
        let string = vm.to_string(receiver)?;
        if to_well_formed {
          Ok(Value::String(string.to_well_formed()))
        } else {
          Ok(Value::Bool(string.is_well_formed()))
        }
      }),
      function_proto_map,
    );
    set_builtin_name(&method, name);
    set_builtin_length(&method, 0.0);
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::builtin_data(method),
    );
  }

  let install_regexp_dispatch = |name: &str, symbol: SymbolId| {
    let method = builtin_object(
      Rc::new(move |vm, args| {
        let undefined = Value::Undefined;
        let receiver = args.first().unwrap_or(&undefined);
        require_object_coercible(vm, receiver)?;
        let regexp = args.get(1).cloned().unwrap_or(Value::Undefined);
        if regexp.is_object() {
          let method = vm.get_property_host(&regexp, &PKey::Symbol(symbol.clone()))?;
          if !matches!(method, Value::Null | Value::Undefined) {
            if !Vm::is_callable_value(&method) {
              return Err(vm.throw_type_error("RegExp symbol method is not callable"));
            }
            return vm.call_value_with_this(&method, &regexp, std::slice::from_ref(receiver));
          }
        }
        let string = Value::String(vm.to_string(receiver)?);
        let constructor = vm
          .get_global("RegExp")
          .unwrap_or(Value::Undefined);
        let created = vm.call_value_with_this(&constructor, &Value::Undefined, &[regexp])?;
        let method = vm.get_property_host(&created, &PKey::Symbol(symbol.clone()))?;
        vm.call_value_with_this(&method, &created, std::slice::from_ref(&string))
      }),
      function_proto_map,
    );
    set_builtin_name(&method, name);
    set_builtin_length(&method, 1.0);
    string_proto_map.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::builtin_data(method),
    );
  };
  install_regexp_dispatch("match", SymbolId::well_known_match());
  install_regexp_dispatch("search", SymbolId::well_known_search());

  let string_replace = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let search = args.get(1).cloned().unwrap_or(Value::Undefined);
      let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
      if search.is_object() {
        let replacer =
          vm.get_property_host(&search, &PKey::Symbol(SymbolId::well_known_replace()))?;
        if !matches!(replacer, Value::Undefined | Value::Null) {
          if !Vm::is_callable_value(&replacer) {
            return Err(vm.throw_type_error("Symbol.replace method is not callable"));
          }
          return vm.call_value_with_this(&replacer, &search, &[receiver.clone(), replacement]);
        }
      }

      let subject = vm.to_string(receiver)?;
      let search = vm.to_string(&search)?;
      let Some(position) = subject.find(&search, 0) else {
        return Ok(Value::String(subject));
      };
      let end = position + search.len();
      let replacement = if Vm::is_callable_value(&replacement) {
        let result = vm.call_value_with_this(
          &replacement,
          &Value::Undefined,
          &[
            Value::String(search),
            Value::Number(usize_to_f64(position)),
            Value::String(subject.clone()),
          ],
        )?;
        vm.to_string(&result)?
      } else {
        let replacement = vm.to_string(&replacement)?;
        super::super::semantics::get_substitution(
          &search,
          &subject,
          position,
          end,
          &[],
          None,
          &replacement,
        )
      };
      let prefix = subject
        .slice(0..position)
        .expect("match start is within the subject");
      let suffix = subject
        .slice(end..subject.len())
        .expect("match end is within the subject");
      Ok(Value::String(prefix.concat(&replacement).concat(&suffix)))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_replace, "replace");
  set_builtin_length(&string_replace, 2.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("replace".to_string().into()),
    PropertyDescriptor::builtin_data(string_replace),
  );

  let string_replace_all = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let search = args.get(1).cloned().unwrap_or(Value::Undefined);
      let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
      if crate::ecmascript::builtins::regexp::is_regexp(vm, &search)? {
        let flags = vm.get_property_host(&search, &PKey::String("flags".into()))?;
        let flags = vm.to_string(&flags)?;
        if !flags
          .code_units()
          .any(|unit| unit == u16::from(b'g'))
        {
          return Err(
            vm.throw_type_error("String.prototype.replaceAll called with a non-global RegExp"),
          );
        }
      }
      if search.is_object() {
        let replacer =
          vm.get_property_host(&search, &PKey::Symbol(SymbolId::well_known_replace()))?;
        if !matches!(replacer, Value::Undefined | Value::Null) {
          if !Vm::is_callable_value(&replacer) {
            return Err(vm.throw_type_error("Symbol.replace method is not callable"));
          }
          return vm.call_value_with_this(&replacer, &search, &[receiver.clone(), replacement]);
        }
      }
      let subject = vm.to_string(receiver)?;
      let search = vm.to_string(&search)?;
      let functional = Vm::is_callable_value(&replacement);
      let replacement_string = if functional {
        None
      } else {
        Some(vm.to_string(&replacement)?)
      };
      let mut output = JsStringBuilder::new();
      let mut end_of_previous = 0;
      let mut search_position = 0;
      let advance = search.len().max(1);
      while let Some(position) = subject.find(&search, search_position) {
        output.push_string(
          &subject
            .slice(end_of_previous..position)
            .expect("replaceAll prefix is within the subject"),
        );
        let replacement_value = if functional {
          let result = vm.call_value_with_this(
            &replacement,
            &Value::Undefined,
            &[
              Value::String(search.clone()),
              Value::Number(usize_to_f64(position)),
              Value::String(subject.clone()),
            ],
          )?;
          vm.to_string(&result)?
        } else {
          super::super::semantics::get_substitution(
            &search,
            &subject,
            position,
            position + search.len(),
            &[],
            None,
            replacement_string
              .as_ref()
              .expect("non-functional replacement was converted"),
          )
        };
        output.push_string(&replacement_value);
        end_of_previous = position + search.len();
        search_position = position + advance;
        if search_position > subject.len() {
          break;
        }
      }
      output.push_string(
        &subject
          .slice(end_of_previous..subject.len())
          .expect("replaceAll tail is within the subject"),
      );
      Ok(Value::String(output.finish()))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_replace_all, "replaceAll");
  set_builtin_length(&string_replace_all, 2.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("replaceAll".into()),
    PropertyDescriptor::builtin_data(string_replace_all),
  );

  let string_match_all = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().unwrap_or(&Value::Undefined);
      require_object_coercible(vm, receiver)?;
      let regexp = args.get(1).cloned().unwrap_or(Value::Undefined);
      if crate::ecmascript::builtins::regexp::is_regexp(vm, &regexp)? {
        let flags = vm.get_property_host(&regexp, &PKey::String("flags".into()))?;
        let flags = vm.to_string(&flags)?;
        if !flags
          .code_units()
          .any(|unit| unit == u16::from(b'g'))
        {
          return Err(
            vm.throw_type_error("String.prototype.matchAll called with a non-global RegExp"),
          );
        }
      }
      if regexp.is_object() {
        let matcher =
          vm.get_property_host(&regexp, &PKey::Symbol(SymbolId::well_known_match_all()))?;
        if !matches!(matcher, Value::Undefined | Value::Null) {
          if !Vm::is_callable_value(&matcher) {
            return Err(vm.throw_type_error("Symbol.matchAll method is not callable"));
          }
          return vm.call_value_with_this(&matcher, &regexp, std::slice::from_ref(receiver));
        }
      }
      let constructor = vm
        .get_global("RegExp")
        .unwrap_or(Value::Undefined);
      let created = vm.call_value_with_this(
        &constructor,
        &Value::Undefined,
        &[regexp, Value::String("g".into())],
      )?;
      let matcher =
        vm.get_property_host(&created, &PKey::Symbol(SymbolId::well_known_match_all()))?;
      vm.call_value_with_this(&matcher, &created, std::slice::from_ref(receiver))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_match_all, "matchAll");
  set_builtin_length(&string_match_all, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("matchAll".into()),
    PropertyDescriptor::builtin_data(string_match_all),
  );

  let string_split = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let receiver = args.first().unwrap_or(&undefined);
      require_object_coercible(vm, receiver)?;
      let separator = args.get(1).unwrap_or(&Value::Undefined);
      let limit_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
      if separator.is_object() {
        let splitter =
          vm.get_property_host(separator, &PKey::Symbol(SymbolId::well_known_split()))?;
        if !matches!(splitter, Value::Null | Value::Undefined) {
          if !Vm::is_callable_value(&splitter) {
            return Err(vm.throw_type_error("Symbol.split method is not callable"));
          }
          return vm.call_value_with_this(&splitter, separator, &[receiver.clone(), limit_arg]);
        }
      }
      let s = vm.to_string(receiver)?;
      let limit = if matches!(limit_arg, Value::Undefined) {
        usize::try_from(u32::MAX).unwrap_or(usize::MAX)
      } else {
        usize::try_from(vm.to_uint32_value(&limit_arg)?).unwrap_or(usize::MAX)
      };
      if limit == 0 {
        return Ok(array_from_values(Vec::new()));
      }

      let separator = match separator {
        Value::Undefined => None,
        Value::String(value) => Some(value.clone()),
        other => Some(vm.to_string(other)?),
      };
      let parts: Vec<Value> = if let Some(separator) = separator {
        if separator.is_empty() {
          (0..s.len())
            .map(|index| {
              Value::String(
                s.slice(index..index + 1)
                  .expect("string index is within bounds"),
              )
            })
            .collect()
        } else {
          let mut parts = Vec::new();
          let mut start = 0;
          while let Some(index) = s.find(&separator, start) {
            parts.push(Value::String(
              s.slice(start..index)
                .expect("split range is within bounds"),
            ));
            start = index + separator.len();
          }
          parts.push(Value::String(
            s.slice(start..s.len())
              .expect("split tail is within bounds"),
          ));
          parts
        }
      } else {
        vec![Value::String(s)]
      };
      let parts = parts.into_iter().take(limit).collect();

      Ok(array_from_values(parts))
    }),
    function_proto_map,
  );
  set_builtin_name(&string_split, "split");
  set_builtin_length(&string_split, 1.0);
  string_proto_map.borrow_mut().properties.insert(
    PKey::String("split".to_string().into()),
    PropertyDescriptor::builtin_data(string_split),
  );

  let boolean_ctor_fn: BuiltinFn = Rc::new(|vm, args| {
    let value_idx = if vm.pending_constructor_new_target().is_some() || args.len() > 1 {
      1
    } else {
      0
    };
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args
      .get(value_idx)
      .cloned()
      .unwrap_or(Value::Undefined);
    let prim = Value::Bool(vm.to_boolean(&value));
    if vm.pending_constructor_new_target().is_some()
      && let Value::Object(obj) = &receiver
    {
      obj
        .borrow_mut()
        .set_internal_slot("[[BooleanData]]", prim);
      return Ok(receiver);
    }
    Ok(prim)
  });
  let boolean_ctor_val = Value::BuiltinObject(boolean_ctor_fn, boolean_ctor_map);
  set_builtin_length(&boolean_ctor_val, 1.0);
  set_builtin_name(&boolean_ctor_val, "Boolean");
  boolean_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(boolean_ctor_val.clone()),
  );
  let boolean_value_of = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      Ok(Value::Bool(this_boolean_value(vm, this)?))
    }),
    function_proto_map,
  );
  set_builtin_name(&boolean_value_of, "valueOf");
  set_builtin_length(&boolean_value_of, 0.0);
  boolean_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string().into()),
    PropertyDescriptor::builtin_data(boolean_value_of),
  );
  let boolean_to_string = builtin_object(
    Rc::new(|vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      let b = this_boolean_value(vm, this)?;
      Ok(Value::String(
        if b { "true" } else { "false" }
          .to_string()
          .into(),
      ))
    }),
    function_proto_map,
  );
  set_builtin_name(&boolean_to_string, "toString");
  set_builtin_length(&boolean_to_string, 0.0);
  boolean_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(boolean_to_string),
  );

  set_builtin_name(&string_ctor_val, "String");
  set_builtin_name(&symbol_ctor_val, "Symbol");
  globals.properties.insert(
    PKey::String("String".to_string().into()),
    PropertyDescriptor::builtin_data(string_ctor_val.clone()),
  );
  globals.properties.insert(
    PKey::String("Boolean".to_string().into()),
    PropertyDescriptor::builtin_data(boolean_ctor_val.clone()),
  );
  globals.properties.insert(
    PKey::String("Symbol".to_string().into()),
    PropertyDescriptor::builtin_data(symbol_ctor_val.clone()),
  );

  TextAndSymbolIntrinsics {
    string_constructor: string_ctor_val,
    string_prototype: Value::Object(string_proto_map),
    string_iterator_prototype: Value::Object(string_iterator_proto),
    boolean_constructor: boolean_ctor_val,
    boolean_prototype: Value::Object(boolean_proto_map),
    symbol_constructor: symbol_ctor_val,
    symbol_prototype: Value::Object(symbol_proto_map),
  }
}
