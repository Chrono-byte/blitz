// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::value::ProxyData;
use std::cell::RefMut;

/// Contains references to global intrinsic values.
pub(crate) struct GlobalIntrinsicValues<'a> {
  /// The Reflect object.
  pub reflect_obj: &'a Rc<RefCell<ObjectData>>,
  /// The Object constructor value.
  pub object_ctor_val: &'a Value,
  /// The Function constructor value.
  pub function_ctor_val: &'a Value,
  /// The Array constructor value.
  pub array_ctor_val: &'a Value,
  /// The RegExp constructor value.
  pub regexp_ctor_val: &'a Value,
  /// The Promise constructor value.
  pub promise_ctor_val: &'a Value,
  /// The Date constructor value.
  pub date_ctor_val: &'a Value,
  /// The Error constructor value.
  pub error_ctor_val: &'a Value,
  /// The AggregateError constructor value.
  pub aggregate_error_ctor_val: &'a Value,
  /// The Map constructor value.
  pub map_ctor_val: &'a Value,
  /// The Set constructor value.
  pub set_ctor_val: &'a Value,
  /// The ArrayBuffer constructor value.
  pub arraybuffer_ctor_val: &'a Value,
  /// The SharedArrayBuffer constructor value.
  pub sharedarraybuffer_ctor_val: &'a Value,
  /// The DataView constructor value.
  pub dataview_ctor_val: &'a Value,
  /// The Uint8Array constructor value.
  pub uint8array_ctor_val: &'a Value,
  /// The Int8Array constructor value.
  pub int8array_ctor_val: &'a Value,
  /// The Uint16Array constructor value.
  pub uint16array_ctor_val: &'a Value,
  /// The Int16Array constructor value.
  pub int16array_ctor_val: &'a Value,
  /// The Uint32Array constructor value.
  pub uint32array_ctor_val: &'a Value,
  /// The Int32Array constructor value.
  pub int32array_ctor_val: &'a Value,
  /// The Float32Array constructor value.
  pub float32array_ctor_val: &'a Value,
  /// The Float64Array constructor value.
  pub float64array_ctor_val: &'a Value,
  /// The Uint8ClampedArray constructor value.
  pub uint8clampedarray_ctor_val: &'a Value,
  /// The BigInt64Array constructor value.
  pub bigint64array_ctor_val: &'a Value,
  /// The BigUint64Array constructor value.
  pub biguint64array_ctor_val: &'a Value,
  /// The EvalError constructor value.
  pub eval_error_ctor_val: &'a Value,
  /// The RangeError constructor value.
  pub range_error_ctor_val: &'a Value,
  /// The ReferenceError constructor value.
  pub reference_error_ctor_val: &'a Value,
  /// The SyntaxError constructor value.
  pub syntax_error_ctor_val: &'a Value,
  /// The TypeError constructor value.
  pub type_error_ctor_val: &'a Value,
  /// The URIError constructor value.
  pub uri_error_ctor_val: &'a Value,
  /// The Math object.
  pub math: &'a Rc<RefCell<ObjectData>>,
  /// The JSON object.
  pub json_obj: &'a Rc<RefCell<ObjectData>>,
  /// The Atomics object.
  pub atomics_obj: &'a Value,
  /// The ShadowRealm constructor value.
  pub shadowrealm_ctor_val: &'a Value,
}

/// Installs global ECMAScript bindings and intrinsic objects.
pub(crate) fn install_global_bindings(
  globals: &Rc<RefCell<ObjectData>>,
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
  values: &GlobalIntrinsicValues<'_>,
) {
  use PropertyKey as PKey;

  let mut g: RefMut<'_, ObjectData> = globals.borrow_mut();
  builtins::text_and_symbols::install_string_boolean_symbol_builtins(
    &mut g,
    object_proto_map,
    function_proto_map,
  );
  builtins::primitives::install_bigint_and_number_builtins(
    &mut g,
    object_proto_map,
    function_proto_map,
  );

  g.properties.insert(
    PKey::String("globalThis".to_string()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(globals)),
      writable: true,
      enumerable: false,
      configurable: true,
    },
  );
  g.properties
    .entry(PKey::String("NaN".to_string()))
    .or_insert(PropertyDescriptor::Data {
      value: Value::Number(f64::NAN),
      writable: false,
      enumerable: false,
      configurable: false,
    });
  g.properties
    .entry(PKey::String("Infinity".to_string()))
    .or_insert(PropertyDescriptor::Data {
      value: Value::Number(f64::INFINITY),
      writable: false,
      enumerable: false,
      configurable: false,
    });
  g.properties
    .entry(PKey::String("undefined".to_string()))
    .or_insert(PropertyDescriptor::Data {
      value: Value::Undefined,
      writable: false,
      enumerable: false,
      configurable: false,
    });
  builtins::globals::install_global_function_builtins(&mut g);
  g.properties.insert(
    PKey::String("Reflect".to_string()),
    PropertyDescriptor::builtin_data(Value::Object(Rc::clone(values.reflect_obj))),
  );

  for (name, value) in [
    ("Function", values.function_ctor_val),
    ("Object", values.object_ctor_val),
    ("Array", values.array_ctor_val),
    ("RegExp", values.regexp_ctor_val),
    ("Promise", values.promise_ctor_val),
    ("Date", values.date_ctor_val),
    ("ArrayBuffer", values.arraybuffer_ctor_val),
    ("SharedArrayBuffer", values.sharedarraybuffer_ctor_val),
    ("DataView", values.dataview_ctor_val),
    ("Uint8Array", values.uint8array_ctor_val),
    ("Int8Array", values.int8array_ctor_val),
    ("Uint16Array", values.uint16array_ctor_val),
    ("Int16Array", values.int16array_ctor_val),
    ("Uint32Array", values.uint32array_ctor_val),
    ("Int32Array", values.int32array_ctor_val),
    ("Float32Array", values.float32array_ctor_val),
    ("Float64Array", values.float64array_ctor_val),
    ("Uint8ClampedArray", values.uint8clampedarray_ctor_val),
    ("BigInt64Array", values.bigint64array_ctor_val),
    ("BigUint64Array", values.biguint64array_ctor_val),
    ("Error", values.error_ctor_val),
    ("AggregateError", values.aggregate_error_ctor_val),
    ("EvalError", values.eval_error_ctor_val),
    ("RangeError", values.range_error_ctor_val),
    ("ReferenceError", values.reference_error_ctor_val),
    ("SyntaxError", values.syntax_error_ctor_val),
    ("TypeError", values.type_error_ctor_val),
    ("URIError", values.uri_error_ctor_val),
    ("Atomics", values.atomics_obj),
    ("ShadowRealm", values.shadowrealm_ctor_val),
    ("Map", values.map_ctor_val),
    ("Set", values.set_ctor_val),
  ] {
    g.properties.insert(
      PKey::String(name.to_string()),
      PropertyDescriptor::builtin_data(value.clone()),
    );
  }

  set_builtin_name(values.object_ctor_val, "Object");
  set_builtin_name(values.function_ctor_val, "Function");
  set_builtin_name(values.array_ctor_val, "Array");
  set_builtin_name(values.regexp_ctor_val, "RegExp");
  set_builtin_name(values.promise_ctor_val, "Promise");
  set_builtin_name(values.date_ctor_val, "Date");
  set_builtin_name(values.error_ctor_val, "Error");
  set_builtin_name(values.aggregate_error_ctor_val, "AggregateError");
  set_builtin_name(values.map_ctor_val, "Map");
  set_builtin_name(values.set_ctor_val, "Set");

  g.properties.insert(
    PKey::String("Math".to_string()),
    PropertyDescriptor::builtin_data(Value::Object(Rc::clone(values.math))),
  );
  g.properties.insert(
    PKey::String("JSON".to_string()),
    PropertyDescriptor::builtin_data(Value::Object(Rc::clone(values.json_obj))),
  );

  let make_placeholder_ctor = |name: &'static str, returns_target_on_construct: bool| -> Value {
    let proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
      Rc::clone(object_proto_map),
    ))));
    let ctor_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
      Rc::clone(function_proto_map),
    ))));
    ctor_map.borrow_mut().properties.insert(
      PKey::String("prototype".to_string()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&proto_map))),
    );
    let name_owned = name.to_string();
    let ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
      if vm.current_new_target().is_some() {
        if returns_target_on_construct && name_owned == "Proxy" {
          return Ok(args.get(1).cloned().unwrap_or(Value::Undefined));
        }
        return Ok(args.first().cloned().unwrap_or(Value::Undefined));
      }
      if returns_target_on_construct && name_owned == "Proxy" {
        return Ok(args.first().cloned().unwrap_or(Value::Undefined));
      }
      Ok(Value::Undefined)
    });
    let ctor_val = Value::BuiltinObject(ctor_fn, Rc::clone(&ctor_map));
    ctor_map.borrow_mut().properties.insert(
      PKey::String("name".to_string()),
      PropertyDescriptor::Data {
        value: Value::String(name.to_string()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
    proto_map.borrow_mut().properties.insert(
      PKey::String("constructor".to_string()),
      PropertyDescriptor::builtin_data(ctor_val.clone()),
    );
    ctor_val
  };

  let weakref_ctor_val = make_placeholder_ctor("WeakRef", false);
  let finalization_registry_ctor_val = make_placeholder_ctor("FinalizationRegistry", false);
  let temporal_namespace = Value::Object(Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(object_proto_map)),
  ))));
  g.properties.insert(
    PKey::String("WeakRef".to_string()),
    PropertyDescriptor::builtin_data(weakref_ctor_val),
  );
  g.properties.insert(
    PKey::String("FinalizationRegistry".to_string()),
    PropertyDescriptor::builtin_data(finalization_registry_ctor_val),
  );
  g.properties.insert(
    PKey::String("Temporal".to_string()),
    PropertyDescriptor::builtin_data(temporal_namespace),
  );

  let weakmap_ctor_val = make_placeholder_ctor("WeakMap", false);
  let weakset_ctor_val = make_placeholder_ctor("WeakSet", false);
  let proxy_object_proto = Rc::clone(object_proto_map);
  let proxy_function_proto = Rc::clone(function_proto_map);
  let proxy_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&proxy_object_proto),
  ))));
  let proxy_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&proxy_function_proto)),
  )));
  proxy_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string()),
    PropertyDescriptor::builtin_prototype(Value::Object(proxy_proto_map)),
  );
  let proxy_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let off = static_builtin_arg_offset(vm, args, "Proxy");
    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
    let handler = args
      .get(off + 1)
      .cloned()
      .unwrap_or(Value::Undefined);
    if !target.is_object() {
      return Err(vm.throw_type_error("Proxy target must be an object"));
    }
    if !handler.is_object() {
      return Err(vm.throw_type_error("Proxy handler must be an object"));
    }
    let proxy = crate::gc::track_proxy(Rc::new(RefCell::new(ProxyData::new(target, handler))));
    Ok(Value::Proxy(proxy))
  });
  let proxy_ctor_val = Value::BuiltinObject(Rc::clone(&proxy_ctor_fn), Rc::clone(&proxy_ctor_map));
  proxy_ctor_map.borrow_mut().properties.insert(
    PKey::String("name".to_string()),
    PropertyDescriptor::builtin_metadata(Value::String("Proxy".to_string())),
  );
  proxy_ctor_map.borrow_mut().properties.insert(
    PKey::String("revocable".to_string()),
    PropertyDescriptor::builtin_data({
      let proxy_function_proto_for_builtin = Rc::clone(&proxy_function_proto);
      let proxy_function_proto_for_revoke = Rc::clone(&proxy_function_proto);
      let builtin = builtin_object(
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Proxy");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let handler = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          if !target.is_object() {
            return Err(vm.throw_type_error("Proxy target must be an object"));
          }
          if !handler.is_object() {
            return Err(vm.throw_type_error("Proxy handler must be an object"));
          }
          let proxy =
            crate::gc::track_proxy(Rc::new(RefCell::new(ProxyData::new(target, handler))));
          let revoke_state = Rc::clone(&proxy);
          let revoke_fn = builtin_object(
            Rc::new(move |_vm, _args| {
              revoke_state.borrow_mut().revoke();
              Ok(Value::Undefined)
            }),
            &proxy_function_proto_for_revoke,
          );
          set_builtin_name(&revoke_fn, "revoke");
          set_builtin_length(&revoke_fn, 0.0);
          let result = Rc::new(RefCell::new(ObjectData::new()));
          result.borrow_mut().properties.insert(
            PKey::String("proxy".to_string()),
            PropertyDescriptor::ordinary_data(Value::Proxy(proxy)),
          );
          result.borrow_mut().properties.insert(
            PKey::String("revoke".to_string()),
            PropertyDescriptor::ordinary_data(revoke_fn),
          );
          Ok(Value::Object(result))
        }),
        &proxy_function_proto_for_builtin,
      );
      set_builtin_name(&builtin, "revocable");
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  g.properties.insert(
    PKey::String("WeakMap".to_string()),
    PropertyDescriptor::builtin_data(weakmap_ctor_val),
  );
  g.properties.insert(
    PKey::String("WeakSet".to_string()),
    PropertyDescriptor::builtin_data(weakset_ctor_val),
  );
  g.properties.insert(
    PKey::String("Proxy".to_string()),
    PropertyDescriptor::builtin_data(proxy_ctor_val),
  );

  let insert_math_builtin = |name: &'static str, length: f64, func: BuiltinFn| -> Value {
    let func_val = builtin_object(func, function_proto_map);
    set_builtin_name(&func_val, name);
    set_builtin_length(&func_val, length);
    func_val
  };
  g.properties.insert(
    PKey::String("isNaN".to_string()),
    PropertyDescriptor::builtin_data(insert_math_builtin(
      "isNaN",
      1.0,
      Rc::new(|vm, args| {
        let v = args.first().cloned().unwrap_or(Value::Undefined);
        let n = vm.to_number_value(&v)?;
        Ok(Value::Bool(n.is_nan()))
      }),
    )),
  );
  g.properties.insert(
    PKey::String("isFinite".to_string()),
    PropertyDescriptor::builtin_data(insert_math_builtin(
      "isFinite",
      1.0,
      Rc::new(|vm, args| {
        let v = args.first().cloned().unwrap_or(Value::Undefined);
        let n = vm.to_number_value(&v)?;
        Ok(Value::Bool(n.is_finite()))
      }),
    )),
  );
}
