// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::value::ProxyData;
use std::cell::RefMut;

pub(crate) struct PrimitiveIntrinsics {
  pub(crate) text: builtins::text_and_symbols::TextAndSymbolIntrinsics,
  pub(crate) numeric: builtins::primitives::NumericIntrinsics,
}

pub(crate) struct GlobalIntrinsicValues<'a> {
  pub reflect_obj: &'a Rc<RefCell<ObjectData>>,
  pub object_ctor_val: &'a Value,
  pub function_ctor_val: &'a Value,
  pub array_ctor_val: &'a Value,
  pub regexp_ctor_val: &'a Value,
  pub promise_ctor_val: &'a Value,
  pub date_ctor_val: &'a Value,
  pub error_ctor_val: &'a Value,
  pub aggregate_error_ctor_val: &'a Value,
  pub map_ctor_val: &'a Value,
  pub weak_map_ctor_val: &'a Value,
  pub weak_set_ctor_val: &'a Value,
  pub weak_ref_ctor_val: &'a Value,
  pub finalization_registry_ctor_val: &'a Value,
  pub set_ctor_val: &'a Value,
  pub arraybuffer_ctor_val: &'a Value,
  pub sharedarraybuffer_ctor_val: &'a Value,
  pub dataview_ctor_val: &'a Value,
  pub uint8array_ctor_val: &'a Value,
  pub int8array_ctor_val: &'a Value,
  pub uint16array_ctor_val: &'a Value,
  pub int16array_ctor_val: &'a Value,
  pub uint32array_ctor_val: &'a Value,
  pub int32array_ctor_val: &'a Value,
  pub float32array_ctor_val: &'a Value,
  pub float64array_ctor_val: &'a Value,
  pub uint8clampedarray_ctor_val: &'a Value,
  pub bigint64array_ctor_val: &'a Value,
  pub biguint64array_ctor_val: &'a Value,
  pub eval_error_ctor_val: &'a Value,
  pub range_error_ctor_val: &'a Value,
  pub reference_error_ctor_val: &'a Value,
  pub syntax_error_ctor_val: &'a Value,
  pub type_error_ctor_val: &'a Value,
  pub uri_error_ctor_val: &'a Value,
  pub math: &'a Rc<RefCell<ObjectData>>,
  pub json_obj: &'a Rc<RefCell<ObjectData>>,
  pub atomics_obj: &'a Value,
  pub shadowrealm_ctor_val: &'a Value,
}

pub(crate) fn install_global_bindings(
  globals: &Rc<RefCell<ObjectData>>,
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
  values: &GlobalIntrinsicValues<'_>,
) -> PrimitiveIntrinsics {
  use PropertyKey as PKey;

  let mut g: RefMut<'_, ObjectData> = globals.borrow_mut();
  let text = builtins::text_and_symbols::install_string_boolean_symbol_builtins(
    &mut g,
    object_proto_map,
    function_proto_map,
  );
  let numeric = builtins::primitives::install_bigint_and_number_builtins(
    &mut g,
    object_proto_map,
    function_proto_map,
  );

  g.properties.insert(
    PKey::String("globalThis".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(globals)),
      writable: true,
      enumerable: false,
      configurable: true,
    },
  );
  g.properties
    .entry(PKey::String("NaN".to_string().into()))
    .or_insert(PropertyDescriptor::Data {
      value: Value::Number(f64::NAN),
      writable: false,
      enumerable: false,
      configurable: false,
    });
  g.properties
    .entry(PKey::String("Infinity".to_string().into()))
    .or_insert(PropertyDescriptor::Data {
      value: Value::Number(f64::INFINITY),
      writable: false,
      enumerable: false,
      configurable: false,
    });
  g.properties
    .entry(PKey::String("undefined".to_string().into()))
    .or_insert(PropertyDescriptor::Data {
      value: Value::Undefined,
      writable: false,
      enumerable: false,
      configurable: false,
    });
  builtins::globals::install_global_function_builtins(&mut g, function_proto_map);
  g.properties.insert(
    PKey::String("Reflect".to_string().into()),
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
    ("WeakMap", values.weak_map_ctor_val),
    ("WeakSet", values.weak_set_ctor_val),
    ("WeakRef", values.weak_ref_ctor_val),
    (
      "FinalizationRegistry",
      values.finalization_registry_ctor_val,
    ),
    ("Set", values.set_ctor_val),
  ] {
    g.properties.insert(
      PKey::String(name.to_string().into()),
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
  set_builtin_name(values.weak_map_ctor_val, "WeakMap");
  set_builtin_name(values.weak_set_ctor_val, "WeakSet");
  set_builtin_name(values.weak_ref_ctor_val, "WeakRef");
  set_builtin_name(
    values.finalization_registry_ctor_val,
    "FinalizationRegistry",
  );
  set_builtin_name(values.set_ctor_val, "Set");

  g.properties.insert(
    PKey::String("Math".to_string().into()),
    PropertyDescriptor::builtin_data(Value::Object(Rc::clone(values.math))),
  );
  g.properties.insert(
    PKey::String("JSON".to_string().into()),
    PropertyDescriptor::builtin_data(Value::Object(Rc::clone(values.json_obj))),
  );

  let proxy_function_proto = Rc::clone(function_proto_map);
  let proxy_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&proxy_function_proto)),
  )));
  proxy_ctor_map
    .borrow_mut()
    .set_internal_slot("[[Constructable]]", Value::Bool(true));
  let proxy_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Proxy constructor requires 'new'"));
    }
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
    PKey::String("length".to_string().into()),
    PropertyDescriptor::builtin_metadata(Value::Number(2.0)),
  );
  proxy_ctor_map.borrow_mut().properties.insert(
    PKey::String("name".to_string().into()),
    PropertyDescriptor::builtin_metadata(Value::String("Proxy".to_string().into())),
  );
  proxy_ctor_map.borrow_mut().properties.insert(
    PKey::String("revocable".to_string().into()),
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
          set_builtin_name(&revoke_fn, "");
          set_builtin_length(&revoke_fn, 0.0);
          let result = Rc::new(RefCell::new(ObjectData::new()));
          result.borrow_mut().properties.insert(
            PKey::String("proxy".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Proxy(proxy)),
          );
          result.borrow_mut().properties.insert(
            PKey::String("revoke".to_string().into()),
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
    PKey::String("Proxy".to_string().into()),
    PropertyDescriptor::builtin_data(proxy_ctor_val),
  );

  let insert_math_builtin = |name: &'static str, length: f64, func: BuiltinFn| -> Value {
    let func_val = builtin_object(func, function_proto_map);
    set_builtin_name(&func_val, name);
    set_builtin_length(&func_val, length);
    func_val
  };
  g.properties.insert(
    PKey::String("isNaN".to_string().into()),
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
    PKey::String("isFinite".to_string().into()),
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

  PrimitiveIntrinsics { text, numeric }
}
