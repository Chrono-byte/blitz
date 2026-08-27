// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::collections::{HashMap, HashSet};

fn mark_realm_callables(
  value: &Value,
  realm_global: &Value,
  visited: &mut HashSet<u64>,
) {
  let object = match value {
    Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      props
        .borrow_mut()
        .set_internal_slot("[[RealmGlobals]]", realm_global.clone());
      Rc::clone(props)
    }
    Value::Object(object) | Value::Array(_, object) | Value::Promise(_, object) => {
      Rc::clone(object)
    }
    Value::Proxy(proxy) => {
      let proxy = proxy.borrow();
      mark_realm_callables(&proxy.target, realm_global, visited);
      if let Some(handler) = &proxy.handler {
        mark_realm_callables(handler, realm_global, visited);
      }
      return;
    }
    _ => return,
  };
  let (identity, prototype, descriptors) = {
    let object = object.borrow();
    (
      object.identity,
      object.get_prototype(),
      object
        .properties
        .values()
        .cloned()
        .collect::<Vec<_>>(),
    )
  };
  if !visited.insert(identity) {
    return;
  }
  mark_realm_callables(&prototype, realm_global, visited);
  for descriptor in descriptors {
    match descriptor {
      PropertyDescriptor::Data { value, .. } => {
        mark_realm_callables(&value, realm_global, visited);
      }
      PropertyDescriptor::Accessor { get, set, .. } => {
        if let Some(getter) = get {
          mark_realm_callables(&getter, realm_global, visited);
        }
        if let Some(setter) = set {
          mark_realm_callables(&setter, realm_global, visited);
        }
      }
    }
  }
}

/// Builds the ECMAScript intrinsics and installs the available global bindings.
pub(crate) fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) -> Intrinsics {
  globals.borrow_mut().properties.reserve(64);
  let foundation = create_foundation();
  let object_proto_map = foundation.object_prototype;
  let function_proto_map = foundation.function_prototype;
  let function_ctor_map = foundation.function_constructor_properties;
  let object_ctor_map = foundation.object_constructor_properties;
  let generator_proto_map = foundation.generator_prototype;
  let generator_function_proto_map = foundation.generator_function_prototype;
  let async_function_proto_map = foundation.async_function_prototype;
  let throw_type_error_val = foundation.throw_type_error;

  let array_intrinsics =
    builtins::array::install_array_intrinsics(&object_proto_map, &function_proto_map);
  let array_ctor_val = array_intrinsics.constructor;
  let array_proto_map = array_intrinsics.prototype;
  let array_iterator_proto = array_intrinsics.iterator_prototype;
  let array_values_builtin = array_intrinsics.values;
  let array_keys_builtin = array_intrinsics.keys;
  let array_entries_builtin = array_intrinsics.entries;
  let array_to_string_builtin = array_proto_map
    .borrow()
    .properties
    .get(&PropertyKey::String("toString".into()))
    .and_then(|descriptor| match descriptor {
      PropertyDescriptor::Data { value, .. } => Some(value.clone()),
      _ => None,
    })
    .expect("Array.prototype.toString is installed");

  let function_intrinsics = builtins::function::create_function_intrinsics(
    globals,
    &function_ctor_map,
    &function_proto_map,
  );
  let function_ctor_val = function_intrinsics.constructor;
  let _generator_function_ctor_val = builtins::function::create_generator_function_constructor(
    globals,
    &function_ctor_val,
    &generator_function_proto_map,
  );
  let _async_function_ctor_val = builtins::function::create_async_function_constructor(
    globals,
    &function_ctor_val,
    &async_function_proto_map,
  );

  // 27.2 Promise Objects
  let (promise_proto_map, _promise_ctor_map, promise_ctor_val) =
    builtins::promise::create_promise_intrinsics(&object_proto_map, &function_proto_map);

  let reflect_intrinsics =
    builtins::reflect::create_reflect_intrinsics(&object_proto_map, &function_proto_map);
  let reflect_obj = reflect_intrinsics.object;
  let intl_obj = builtins::intl::create_intl_intrinsic(&object_proto_map, &function_proto_map);

  let date_intrinsics =
    builtins::date::create_date_intrinsics(&object_proto_map, &function_proto_map);
  let date_ctor_val = date_intrinsics.constructor;
  let date_proto_val = date_intrinsics.prototype;

  let binary_data = builtins::binary_data::install_binary_data_intrinsics(
    &object_proto_map,
    &function_proto_map,
    array_values_builtin,
    array_keys_builtin,
    array_entries_builtin,
    array_to_string_builtin,
  );
  let arraybuffer_ctor_val = binary_data.arraybuffer_ctor_val;
  let arraybuffer_proto_map = binary_data.arraybuffer_proto_map;
  let sharedarraybuffer_ctor_val = binary_data.sharedarraybuffer_ctor_val;
  let sharedarraybuffer_proto_map = binary_data.sharedarraybuffer_proto_map;
  let dataview_ctor_val = binary_data.dataview_ctor_val;
  let dataview_proto_map = binary_data.dataview_proto_map;
  let typedarray_ctor_val = binary_data.typedarray_ctor_val;
  let uint8array_ctor_val = binary_data.uint8array_ctor_val;
  let int8array_ctor_val = binary_data.int8array_ctor_val;
  let uint16array_ctor_val = binary_data.uint16array_ctor_val;
  let int16array_ctor_val = binary_data.int16array_ctor_val;
  let uint32array_ctor_val = binary_data.uint32array_ctor_val;
  let int32array_ctor_val = binary_data.int32array_ctor_val;
  let float32array_ctor_val = binary_data.float32array_ctor_val;
  let float64array_ctor_val = binary_data.float64array_ctor_val;
  let bigint64array_ctor_val = binary_data.bigint64array_ctor_val;
  let biguint64array_ctor_val = binary_data.biguint64array_ctor_val;
  let uint8clampedarray_ctor_val = binary_data.uint8clampedarray_ctor_val;
  let typedarray_proto_map = binary_data.typedarray_proto_map;

  let collection_intrinsics =
    builtins::collections::install_map_set_constructors(&object_proto_map, &function_proto_map);
  let weak_reference_intrinsics = builtins::weak_references::create_weak_reference_intrinsics(
    &object_proto_map,
    &function_proto_map,
  );
  let map_ctor_val = collection_intrinsics.map_constructor.clone();
  let weak_map_ctor_val = collection_intrinsics.weak_map_constructor.clone();
  let weak_set_ctor_val = collection_intrinsics.weak_set_constructor.clone();
  let weak_ref_ctor_val = weak_reference_intrinsics
    .weak_ref_constructor
    .clone();
  let finalization_registry_ctor_val = weak_reference_intrinsics
    .finalization_registry_constructor
    .clone();
  let set_ctor_val = collection_intrinsics.set_constructor.clone();
  let iterator_proto = collection_intrinsics.iterator_prototype.clone();
  array_iterator_proto
    .borrow_mut()
    .set_prototype_value(iterator_proto.clone());
  generator_proto_map
    .borrow_mut()
    .set_prototype_value(iterator_proto.clone());
  let map_iterator_proto = collection_intrinsics
    .map_iterator_prototype
    .clone();
  let set_iterator_proto = collection_intrinsics
    .set_iterator_prototype
    .clone();
  let object_intrinsics = builtins::object::install_object_intrinsics(
    &object_ctor_map,
    &object_proto_map,
    &function_proto_map,
  );
  let object_ctor_val = object_intrinsics.constructor;

  let regexp_intrinsics =
    builtins::regexp::create_regexp_intrinsics(&object_proto_map, &function_proto_map);
  let regexp_ctor_val = regexp_intrinsics.constructor;
  let regexp_proto_val = regexp_intrinsics.prototype;

  let error_intrinsics =
    builtins::errors::create_error_intrinsics(&object_proto_map, &function_proto_map);
  let error_ctor_val = error_intrinsics.error_constructor;
  let aggregate_error_ctor_val = error_intrinsics.aggregate_error_constructor;
  let eval_error_ctor_val = error_intrinsics.eval_error_constructor;
  let range_error_ctor_val = error_intrinsics.range_error_constructor;
  let reference_error_ctor_val = error_intrinsics.reference_error_constructor;
  let syntax_error_ctor_val = error_intrinsics.syntax_error_constructor;
  let type_error_ctor_val = error_intrinsics.type_error_constructor;
  let uri_error_ctor_val = error_intrinsics.uri_error_constructor;
  let error_proto_val = error_intrinsics.error_prototype;

  let math = builtins::math::create_math_object(&object_proto_map, &function_proto_map);

  let shadowrealm_ctor_val =
    shadowrealm::build_shadowrealm_constructor(&object_proto_map, &function_proto_map);

  let json_obj = builtins::json::create_json_object(&object_proto_map, &function_proto_map);

  let atomics_obj = atomics::build_atomics_object(&object_proto_map, &function_proto_map);

  let primitive_intrinsics = install_global_bindings(
    globals,
    &object_proto_map,
    &function_proto_map,
    &GlobalIntrinsicValues {
      reflect_obj: &reflect_obj,
      object_ctor_val: &object_ctor_val,
      function_ctor_val: &function_ctor_val,
      array_ctor_val: &array_ctor_val,
      regexp_ctor_val: &regexp_ctor_val,
      promise_ctor_val: &promise_ctor_val,
      date_ctor_val: &date_ctor_val,
      error_ctor_val: &error_ctor_val,
      aggregate_error_ctor_val: &aggregate_error_ctor_val,
      map_ctor_val: &map_ctor_val,
      weak_map_ctor_val: &weak_map_ctor_val,
      weak_set_ctor_val: &weak_set_ctor_val,
      weak_ref_ctor_val: &weak_ref_ctor_val,
      finalization_registry_ctor_val: &finalization_registry_ctor_val,
      set_ctor_val: &set_ctor_val,
      arraybuffer_ctor_val: &arraybuffer_ctor_val,
      sharedarraybuffer_ctor_val: &sharedarraybuffer_ctor_val,
      dataview_ctor_val: &dataview_ctor_val,
      uint8array_ctor_val: &uint8array_ctor_val,
      int8array_ctor_val: &int8array_ctor_val,
      uint16array_ctor_val: &uint16array_ctor_val,
      int16array_ctor_val: &int16array_ctor_val,
      uint32array_ctor_val: &uint32array_ctor_val,
      int32array_ctor_val: &int32array_ctor_val,
      float32array_ctor_val: &float32array_ctor_val,
      float64array_ctor_val: &float64array_ctor_val,
      uint8clampedarray_ctor_val: &uint8clampedarray_ctor_val,
      bigint64array_ctor_val: &bigint64array_ctor_val,
      biguint64array_ctor_val: &biguint64array_ctor_val,
      eval_error_ctor_val: &eval_error_ctor_val,
      range_error_ctor_val: &range_error_ctor_val,
      reference_error_ctor_val: &reference_error_ctor_val,
      syntax_error_ctor_val: &syntax_error_ctor_val,
      type_error_ctor_val: &type_error_ctor_val,
      uri_error_ctor_val: &uri_error_ctor_val,
      math: &math,
      json_obj: &json_obj,
      atomics_obj: &atomics_obj,
      shadowrealm_ctor_val: &shadowrealm_ctor_val,
    },
  );
  globals.borrow_mut().properties.insert(
    PropertyKey::String("Intl".into()),
    PropertyDescriptor::builtin_data(intl_obj),
  );
  let realm_global = Value::Object(Rc::clone(globals));
  mark_realm_callables(&realm_global, &realm_global, &mut HashSet::new());

  let text_intrinsics = primitive_intrinsics.text;
  let numeric_intrinsics = primitive_intrinsics.numeric;
  if let Value::Object(string_iterator_proto) = &text_intrinsics.string_iterator_prototype {
    string_iterator_proto
      .borrow_mut()
      .set_prototype_value(iterator_proto.clone());
  }
  let typed_array_constructors = vec![
    uint8array_ctor_val,
    int8array_ctor_val,
    uint16array_ctor_val,
    int16array_ctor_val,
    uint32array_ctor_val,
    int32array_ctor_val,
    float32array_ctor_val,
    float64array_ctor_val,
    uint8clampedarray_ctor_val,
    bigint64array_ctor_val,
    biguint64array_ctor_val,
  ];

  let mut intrinsics = Intrinsics {
    object_constructor: Some(object_ctor_val),
    array_constructor: Some(array_ctor_val),
    function_constructor: Some(function_ctor_val),
    string_constructor: Some(text_intrinsics.string_constructor),
    symbol_constructor: Some(text_intrinsics.symbol_constructor),
    boolean_constructor: Some(text_intrinsics.boolean_constructor),
    bigint_constructor: Some(numeric_intrinsics.bigint_constructor),
    number_constructor: Some(numeric_intrinsics.number_constructor),
    error_constructor: Some(error_ctor_val),
    aggregate_error_constructor: Some(aggregate_error_ctor_val),
    type_error_constructor: Some(type_error_ctor_val),
    range_error_constructor: Some(range_error_ctor_val),
    reference_error_constructor: Some(reference_error_ctor_val),
    syntax_error_constructor: Some(syntax_error_ctor_val),
    eval_error_constructor: Some(eval_error_ctor_val),
    uri_error_constructor: Some(uri_error_ctor_val),
    promise_constructor: Some(promise_ctor_val),
    date_constructor: Some(date_ctor_val),
    regexp_constructor: Some(regexp_ctor_val),
    map_constructor: Some(map_ctor_val),
    weak_map_constructor: Some(weak_map_ctor_val),
    weak_set_constructor: Some(weak_set_ctor_val),
    weak_ref_constructor: Some(weak_ref_ctor_val),
    finalization_registry_constructor: Some(finalization_registry_ctor_val),
    set_constructor: Some(set_ctor_val),
    arraybuffer_constructor: Some(arraybuffer_ctor_val),
    shared_array_buffer_constructor: Some(sharedarraybuffer_ctor_val),
    data_view_constructor: Some(dataview_ctor_val),
    typed_array_constructor: Some(typedarray_ctor_val),
    object_prototype: Some(Value::Object(object_proto_map)),
    array_prototype: Some(Value::Object(array_proto_map)),
    function_prototype: Some(Value::Object(function_proto_map)),
    string_prototype: Some(text_intrinsics.string_prototype),
    symbol_prototype: Some(text_intrinsics.symbol_prototype),
    boolean_prototype: Some(text_intrinsics.boolean_prototype),
    bigint_prototype: Some(numeric_intrinsics.bigint_prototype),
    number_prototype: Some(numeric_intrinsics.number_prototype),
    error_prototype: Some(error_proto_val),
    promise_prototype: Some(Value::Object(promise_proto_map)),
    date_prototype: Some(date_proto_val),
    regexp_prototype: Some(regexp_proto_val),
    map_prototype: Some(collection_intrinsics.map_prototype),
    weak_map_prototype: Some(collection_intrinsics.weak_map_prototype),
    set_prototype: Some(collection_intrinsics.set_prototype),
    weak_set_prototype: Some(collection_intrinsics.weak_set_prototype),
    weak_ref_prototype: Some(weak_reference_intrinsics.weak_ref_prototype),
    finalization_registry_prototype: Some(
      weak_reference_intrinsics.finalization_registry_prototype,
    ),
    typed_array_prototype: Some(Value::Object(typedarray_proto_map)),
    arraybuffer_prototype: Some(Value::Object(arraybuffer_proto_map)),
    shared_array_buffer_prototype: Some(Value::Object(sharedarraybuffer_proto_map)),
    data_view_prototype: Some(Value::Object(dataview_proto_map)),
    iterator_prototype: Some(iterator_proto),
    async_iterator_prototype: None,
    array_iterator_prototype: Some(Value::Object(array_iterator_proto)),
    map_iterator_prototype: Some(map_iterator_proto),
    set_iterator_prototype: Some(set_iterator_proto),
    string_iterator_prototype: Some(text_intrinsics.string_iterator_prototype),
    regexp_string_iterator_prototype: None,
    generator_prototype: Some(Value::Object(generator_proto_map)),
    generator_function_prototype: Some(Value::Object(generator_function_proto_map)),
    async_function_prototype: Some(Value::Object(async_function_proto_map)),
    async_generator_prototype: None,
    async_from_sync_iterator_prototype: None,
    throw_type_error: Some(throw_type_error_val),
    typed_array_constructors,
    hidden_builtins: HashMap::new(),
  };

  let intrinsic_eval = globals
    .borrow()
    .properties
    .get(&PropertyKey::String("eval".to_string().into()))
    .and_then(|descriptor| match descriptor {
      PropertyDescriptor::Data { value, .. } => Some(value.clone()),
      PropertyDescriptor::Accessor { .. } => None,
    })
    .unwrap_or(Value::Undefined);
  install_hidden_builtins(&mut intrinsics, intrinsic_eval);

  intrinsics
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::same_value;

  fn data_property(
    object: &Rc<RefCell<ObjectData>>,
    key: &str,
  ) -> Value {
    object
      .borrow()
      .properties
      .get(&PropertyKey::String(key.into()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        PropertyDescriptor::Accessor { .. } => None,
      })
      .unwrap_or_else(|| panic!("missing data property {key}"))
  }

  fn object_data(value: &Value) -> &Rc<RefCell<ObjectData>> {
    match value {
      Value::Object(data)
      | Value::Array(_, data)
      | Value::BuiltinObject(_, data)
      | Value::Closure { props: data, .. } => data,
      _ => panic!("expected object value"),
    }
  }

  #[test]
  fn globals_and_intrinsic_table_share_constructor_identity() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let intrinsics = install_minimal_kernel_constructors(&globals);

    for (name, intrinsic) in [
      ("Object", intrinsics.object_constructor.as_ref()),
      ("Function", intrinsics.function_constructor.as_ref()),
      ("Array", intrinsics.array_constructor.as_ref()),
      ("Promise", intrinsics.promise_constructor.as_ref()),
      ("RegExp", intrinsics.regexp_constructor.as_ref()),
      ("Map", intrinsics.map_constructor.as_ref()),
      ("WeakMap", intrinsics.weak_map_constructor.as_ref()),
      ("WeakSet", intrinsics.weak_set_constructor.as_ref()),
      ("Set", intrinsics.set_constructor.as_ref()),
      ("ArrayBuffer", intrinsics.arraybuffer_constructor.as_ref()),
      ("DataView", intrinsics.data_view_constructor.as_ref()),
    ] {
      let intrinsic = intrinsic.unwrap_or_else(|| panic!("missing {name} intrinsic"));
      assert!(
        same_value(&data_property(&globals, name), intrinsic),
        "global {name} is not the canonical intrinsic"
      );
    }
  }

  #[test]
  fn constructor_and_prototype_links_are_canonical() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let intrinsics = install_minimal_kernel_constructors(&globals);

    for (name, constructor, prototype) in [
      (
        "Object",
        intrinsics.object_constructor.as_ref(),
        intrinsics.object_prototype.as_ref(),
      ),
      (
        "Function",
        intrinsics.function_constructor.as_ref(),
        intrinsics.function_prototype.as_ref(),
      ),
      (
        "Array",
        intrinsics.array_constructor.as_ref(),
        intrinsics.array_prototype.as_ref(),
      ),
      (
        "RegExp",
        intrinsics.regexp_constructor.as_ref(),
        intrinsics.regexp_prototype.as_ref(),
      ),
      (
        "Error",
        intrinsics.error_constructor.as_ref(),
        intrinsics.error_prototype.as_ref(),
      ),
      (
        "WeakMap",
        intrinsics.weak_map_constructor.as_ref(),
        intrinsics.weak_map_prototype.as_ref(),
      ),
      (
        "WeakSet",
        intrinsics.weak_set_constructor.as_ref(),
        intrinsics.weak_set_prototype.as_ref(),
      ),
    ] {
      let constructor = constructor.unwrap_or_else(|| panic!("missing {name} constructor"));
      let prototype = prototype.unwrap_or_else(|| panic!("missing {name} prototype"));
      assert!(
        same_value(
          &data_property(object_data(constructor), "prototype"),
          prototype
        ),
        "{name}.prototype is not canonical"
      );
      assert!(
        same_value(
          &data_property(object_data(prototype), "constructor"),
          constructor
        ),
        "{name}.prototype.constructor is not canonical"
      );
    }
  }

  #[test]
  fn foundational_prototype_chains_remain_intact() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let intrinsics = install_minimal_kernel_constructors(&globals);
    let object_prototype = intrinsics.object_prototype.as_ref().unwrap();
    let function_prototype = intrinsics.function_prototype.as_ref().unwrap();

    assert!(same_value(
      &object_data(function_prototype)
        .borrow()
        .get_prototype(),
      object_prototype
    ));

    for constructor in [
      intrinsics.object_constructor.as_ref(),
      intrinsics.function_constructor.as_ref(),
      intrinsics.array_constructor.as_ref(),
      intrinsics.regexp_constructor.as_ref(),
    ] {
      let constructor = constructor.unwrap();
      assert!(same_value(
        &object_data(constructor).borrow().get_prototype(),
        function_prototype
      ));
    }
  }
}
