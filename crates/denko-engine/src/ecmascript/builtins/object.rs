// SPDX-License-Identifier: MPL-2.0
//! Object constructor, static methods, and Object.prototype methods.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ecmascript::semantics::{descriptor_value_from_descriptor, property_key_to_value};
use crate::runtime_utils::{
  array_from_values, builtin_object, collect_iterable_values, set_builtin_length, set_builtin_name,
  static_builtin_arg_offset, to_length, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::{RuntimeError, Vm};

/// The preallocated Object constructor and prototype intrinsic values.
pub(crate) struct ObjectIntrinsics {
  pub(crate) constructor: Value,
}

/// Installs Object constructor behavior, static methods, and Object.prototype methods.
///
/// The foundation installer allocates the cyclic Object/Function graph first; this
/// function only populates those preallocated objects.
#[allow(clippy::too_many_lines)]
pub(crate) fn install_object_intrinsics(
  constructor_properties: &Rc<RefCell<ObjectData>>,
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> ObjectIntrinsics {
  use PropertyKey as PKey;

  let constructor_properties_for_constructor = Rc::clone(constructor_properties);
  let constructor_function: BuiltinFn = Rc::new(move |vm, args| {
    let value_idx = if args.len() > 1 { 1 } else { 0 };
    let value = args
      .get(value_idx)
      .cloned()
      .unwrap_or(Value::Undefined);
    match value {
      Value::Null | Value::Undefined => {
        let proto = constructor_properties_for_constructor
          .borrow()
          .properties
          .get(&PKey::String("prototype".to_string().into()))
          .and_then(|descriptor| match descriptor {
            PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            _ => None,
          })
          .unwrap_or(Value::Null);
        let mut instance = ObjectData::new();
        instance.set_prototype_value(proto);
        Ok(Value::Object(Rc::new(RefCell::new(instance))))
      }
      _ => vm.to_object_value(value),
    }
  });
  let constructor = Value::BuiltinObject(
    Rc::clone(&constructor_function),
    Rc::clone(constructor_properties),
  );

  object_prototype.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );

  let proto_getter = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      if matches!(receiver, Value::Undefined | Value::Null) {
        return Err(vm.throw_type_error("Object.prototype.__proto__ called on nullish value"));
      }
      let object = vm.to_object_value(receiver)?;
      vm.get_prototype_of_value(&object)
    }),
    function_prototype,
  );
  set_builtin_name(&proto_getter, "get __proto__");
  set_builtin_length(&proto_getter, 0.0);
  let proto_setter = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      if matches!(receiver, Value::Undefined | Value::Null) {
        return Err(vm.throw_type_error("Object.prototype.__proto__ called on nullish value"));
      }
      let prototype = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !matches!(prototype, Value::Null) && !prototype.is_object() {
        return Ok(Value::Undefined);
      }
      if !receiver.is_object() {
        return Ok(Value::Undefined);
      }
      if !vm.set_prototype_of_value(&receiver, &prototype)? {
        return Err(vm.throw_type_error("Object prototype mutation was rejected"));
      }
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_name(&proto_setter, "set __proto__");
  set_builtin_length(&proto_setter, 1.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("__proto__".to_string().into()),
    PropertyDescriptor::Accessor {
      get: Some(proto_getter),
      set: Some(proto_setter),
      enumerable: false,
      configurable: true,
    },
  );

  let object_to_string = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Null);
      if matches!(this, Value::Undefined) {
        return Ok(Value::String("[object Undefined]".to_string().into()));
      }
      if matches!(this, Value::Null) {
        return Ok(Value::String("[object Null]".to_string().into()));
      }
      let tag = vm.get_property_host(&this, &PKey::Symbol(SymbolId::well_known_to_string_tag()))?;
      if let Value::String(tag) = tag {
        return Ok(Value::String(format!("[object {tag}]").into()));
      }
      if let Value::Object(obj) = &this {
        let object = obj.borrow();
        if object
          .internal_slot("[[ArgumentsData]]")
          .is_some()
        {
          return Ok(Value::String("[object Arguments]".into()));
        }
        if object.internal_slot("[[BooleanData]]").is_some() {
          return Ok(Value::String("[object Boolean]".into()));
        }
        if object.internal_slot("[[NumberData]]").is_some() {
          return Ok(Value::String("[object Number]".into()));
        }
      }
      if let Value::Object(obj) = &this
        && obj
          .borrow()
          .internal_slot("[[SymbolData]]")
          .is_some()
      {
        return Ok(Value::String("[object Symbol]".into()));
      }
      if let Value::Object(obj) = &this {
        let object = obj.borrow();
        if let Some(value) = object.internal_slot("[[PrimitiveValue]]") {
          return Ok(Value::String(
            format!("[object {}]", value.object_tag()).into(),
          ));
        }
        if let Some(PropertyDescriptor::Data { value, .. }) = object
          .properties
          .get(&PKey::String("[[PrimitiveValue]]".to_string().into()))
        {
          return Ok(Value::String(
            format!("[object {}]", value.object_tag()).into(),
          ));
        }
      }
      Ok(Value::String(
        format!("[object {}]", this.object_tag()).into(),
      ))
    }),
    function_prototype,
  );
  set_builtin_length(&object_to_string, 0.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(object_to_string),
  );
  object_prototype.borrow_mut().properties.insert(
    PKey::String("join".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let sep = match args.get(1) {
          Some(Value::Undefined) | None => js_string::JsString::from(","),
          Some(v) => vm.to_string(v)?,
        };
        let mut out = js_string::JsStringBuilder::new();
        let mut first = true;
        let len =
          match vm.get_property_host(&this, &PropertyKey::String("length".to_string().into())) {
            Ok(v) => to_length(vm, &v).unwrap_or(0),
            Err(_) => 0usize,
          };
        for i in 0..len {
          if !first {
            out.push_string(&sep);
          }
          first = false;
          let key = PropertyKey::String(i.to_string().into());
          let item = vm
            .get_property_host(&this, &key)
            .unwrap_or(Value::Undefined);
          if !matches!(item, Value::Undefined | Value::Null) {
            out.push_string(&vm.to_string(&item)?);
          }
        }
        Ok(Value::String(out.finish()))
      }),
      function_prototype,
    )),
  );
  object_prototype.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        // Builtins invoked as methods receive the receiver as args[0].
        // Return the receiver (this) per standard Object.prototype.valueOf.
        let res = args.first().cloned().unwrap_or(Value::Null);
        if crate::trace::enabled() {
          crate::trace::value_string(
            "[trace] Object.prototype.valueOf receiver",
            &args.first().cloned().unwrap_or(Value::Null),
          );
          crate::trace::value_string("[trace] Object.prototype.valueOf result", &res);
        }
        Ok(res)
      }),
      function_prototype,
    )),
  );
  let object_to_locale_string = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let obj = vm.to_object_value(this)?;
      let to_string = vm.get_property_host(&obj, &PKey::String("toString".to_string().into()))?;
      vm.call_value_with_this(&to_string, &obj, &[])
    }),
    function_prototype,
  );
  set_builtin_length(&object_to_locale_string, 0.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("toLocaleString".to_string().into()),
    PropertyDescriptor::builtin_data(object_to_locale_string),
  );
  let has_own_property = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
      let key = vm.to_property_key(&key_value)?;
      let has_own = vm.get_own_property_value(&this, &key)?.is_some();
      Ok(Value::Bool(has_own))
    }),
    function_prototype,
  );
  set_builtin_length(&has_own_property, 1.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("hasOwnProperty".to_string().into()),
    PropertyDescriptor::builtin_data(has_own_property),
  );
  let property_is_enumerable = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
      let key = vm.to_property_key(&key_value)?;
      let enumerable = vm
        .get_own_property_value(&this, &key)?
        .is_some_and(|desc| match desc {
          PropertyDescriptor::Data { enumerable, .. }
          | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
        });
      Ok(Value::Bool(enumerable))
    }),
    function_prototype,
  );
  set_builtin_length(&property_is_enumerable, 1.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("propertyIsEnumerable".to_string().into()),
    PropertyDescriptor::builtin_data(property_is_enumerable),
  );
  let is_prototype_of = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let value = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(vm.is_prototype_of(&this, &value)?))
    }),
    function_prototype,
  );
  set_builtin_length(&is_prototype_of, 1.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("isPrototypeOf".to_string().into()),
    PropertyDescriptor::builtin_data(is_prototype_of),
  );
  let define_getter = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let name = args.get(1).cloned().unwrap_or(Value::Undefined);
      let getter = args.get(2).cloned().unwrap_or(Value::Undefined);
      if !matches!(getter, Value::Undefined) && !Vm::is_callable_value(&getter) {
        return Err(vm.throw_type_error("Getter must be callable"));
      }
      let this = vm.to_object_value(this)?;
      let key = vm.to_property_key(&name)?;
      let mut desc = ObjectData::new();
      desc.properties.insert(
        PKey::String("get".to_string().into()),
        PropertyDescriptor::ordinary_data(getter),
      );
      desc.properties.insert(
        PKey::String("set".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Undefined),
      );
      desc.properties.insert(
        PKey::String("enumerable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      desc.properties.insert(
        PKey::String("configurable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      let desc = Value::Object(Rc::new(RefCell::new(desc)));
      if !vm.define_property_value(&this, &property_key_to_value(&key), &desc)? {
        return Err(vm.throw_type_error("__defineGetter__ failed"));
      }
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_length(&define_getter, 2.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("__defineGetter__".to_string().into()),
    PropertyDescriptor::builtin_data(define_getter),
  );
  let define_setter = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let name = args.get(1).cloned().unwrap_or(Value::Undefined);
      let setter = args.get(2).cloned().unwrap_or(Value::Undefined);
      if !matches!(setter, Value::Undefined) && !Vm::is_callable_value(&setter) {
        return Err(vm.throw_type_error("Setter must be callable"));
      }
      let this = vm.to_object_value(this)?;
      let key = vm.to_property_key(&name)?;
      let mut desc = ObjectData::new();
      desc.properties.insert(
        PKey::String("get".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Undefined),
      );
      desc.properties.insert(
        PKey::String("set".to_string().into()),
        PropertyDescriptor::ordinary_data(setter),
      );
      desc.properties.insert(
        PKey::String("enumerable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      desc.properties.insert(
        PKey::String("configurable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      let desc = Value::Object(Rc::new(RefCell::new(desc)));
      if !vm.define_property_value(&this, &property_key_to_value(&key), &desc)? {
        return Err(vm.throw_type_error("__defineSetter__ failed"));
      }
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_length(&define_setter, 2.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("__defineSetter__".to_string().into()),
    PropertyDescriptor::builtin_data(define_setter),
  );
  let lookup_getter = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let name = args.get(1).cloned().unwrap_or(Value::Undefined);
      let this = vm.to_object_value(this)?;
      let key = vm.to_property_key(&name)?;
      if let Some(desc) = vm.get_own_property_value(&this, &key)?
        && let PropertyDescriptor::Accessor { get, .. } = desc
      {
        return Ok(get.unwrap_or(Value::Undefined));
      }
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_length(&lookup_getter, 1.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("__lookupGetter__".to_string().into()),
    PropertyDescriptor::builtin_data(lookup_getter),
  );
  let lookup_setter = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let name = args.get(1).cloned().unwrap_or(Value::Undefined);
      let this = vm.to_object_value(this)?;
      let key = vm.to_property_key(&name)?;
      if let Some(desc) = vm.get_own_property_value(&this, &key)?
        && let PropertyDescriptor::Accessor { set, .. } = desc
      {
        return Ok(set.unwrap_or(Value::Undefined));
      }
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_length(&lookup_setter, 1.0);
  object_prototype.borrow_mut().properties.insert(
    PKey::String("__lookupSetter__".to_string().into()),
    PropertyDescriptor::builtin_data(lookup_setter),
  );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("create".to_string().into()),
      PropertyDescriptor::builtin_data({
        let object_prototype = Rc::clone(object_prototype);
        let builtin = builtin_object(
          Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let proto = args.get(off).cloned().unwrap_or(Value::Undefined);
            let obj = match proto {
              Value::Null => ObjectData::with_prototype(Value::Null),
              Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _) => ObjectData::with_prototype(proto),
              _ => {
                return Err(RuntimeError::from(
                  "Object.create: prototype must be object or null",
                ));
              }
            };
            let created = Value::Object(Rc::new(RefCell::new(obj)));
            if let Some(props) = args.get(off + 1)
              && !matches!(props, Value::Undefined)
            {
              for key in vm.own_enumerable_keys_value(props)? {
                let descriptor = vm.get_property_host(props, &key)?;
                let key_value = match &key {
                  PropertyKey::String(s) => Value::String(s.clone()),
                  PropertyKey::Symbol(sym) => Value::Symbol(sym.clone(), None),
                };
                let ok = vm.define_property_value(&created, &key_value, &descriptor)?;
                if !ok {
                  return Err(RuntimeError::from("Object.create: define property failed"));
                }
              }
            }
            let _ = &object_prototype;
            Ok(created)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 2.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("assign".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let target = vm.to_object_value(target)?;
            for source in args.iter().skip(off + 1).cloned() {
              if matches!(source, Value::Undefined | Value::Null) {
                continue;
              }
              let source_obj = vm.to_object_value(source)?;
              for key in vm.own_enumerable_keys_value(&source_obj)? {
                let value = vm.get_property_host(&source_obj, &key)?;
                let key_value = property_key_to_value(&key);
                vm.set_property_host(&target, &key_value, value)?;
              }
            }
            Ok(target)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 2.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(PKey::String("is".to_string().into()), {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let a = args.get(off).cloned().unwrap_or(Value::Undefined);
          let b = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          Ok(Value::Bool(crate::vm::same_value(&a, &b)))
        }),
        function_prototype,
      );
      set_builtin_length(&builtin, 2.0);
      PropertyDescriptor::builtin_data(builtin)
    });
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("hasOwn".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let key_value = args
              .get(off + 1)
              .cloned()
              .unwrap_or(Value::Undefined);
            let key = vm.to_property_key(&key_value)?;
            let has = vm
              .get_own_property_value(&target, &key)?
              .is_some();
            Ok(Value::Bool(has))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 2.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("getPrototypeOf".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            if matches!(target, Value::Null | Value::Undefined) {
              return Err(vm.throw_type_error("Object.getPrototypeOf called on null/undefined"));
            }
            let target = vm.to_object_value(target)?;
            vm.get_prototype_of_value(&target)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("getOwnPropertyDescriptor".to_string().into()),
      PropertyDescriptor::builtin_data({
        let object_prototype = Rc::clone(object_prototype);
        let builtin = builtin_object(
          Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let key_value = args
              .get(off + 1)
              .cloned()
              .unwrap_or(Value::Undefined);
            let key = vm.to_property_key(&key_value)?;
            let Some(desc) = vm.get_own_property_value(&target, &key)? else {
              return Ok(Value::Undefined);
            };
            Ok(descriptor_value_from_descriptor(&object_prototype, &desc))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 2.0);
        builtin
      }),
    );

  constructor_properties
    .borrow_mut()
    .properties
    .insert(PKey::String("getOwnPropertySymbols".to_string().into()), {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = vm.to_object_value(args.get(off).cloned().unwrap_or(Value::Undefined))?;
          let symbols = vm
            .own_property_keys_value(&target)?
            .into_iter()
            .filter_map(|key| {
              if let PropertyKey::Symbol(id) = key {
                Some(Some(Value::Symbol(id, None)))
              } else {
                None
              }
            })
            .collect();
          Ok(vm.new_array(symbols))
        }),
        function_prototype,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    });

  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("getOwnPropertyDescriptors".to_string().into()),
      {
        let object_prototype = Rc::clone(object_prototype);
        let builtin = builtin_object(
          Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let mut result = ObjectData::new();
            for key in vm.own_property_keys_value(&target)? {
              if let Some(desc) = vm.get_own_property_value(&target, &key)? {
                let descriptor_value = descriptor_value_from_descriptor(&object_prototype, &desc);
                result.properties.insert(
                  key.clone(),
                  PropertyDescriptor::ordinary_data(descriptor_value),
                );
              }
            }
            Ok(Value::Object(Rc::new(RefCell::new(result))))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        PropertyDescriptor::builtin_data(builtin)
      },
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("getOwnPropertyNames".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = vm.to_object_value(args.get(off).cloned().unwrap_or(Value::Undefined))?;
            let names = vm
              .own_property_keys_value(&target)?
              .into_iter()
              .filter_map(|key| {
                if let PropertyKey::String(name) = key {
                  Some(Some(Value::String(name)))
                } else {
                  None
                }
              })
              .collect();
            Ok(vm.new_array(names))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("keys".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let target = vm.to_object_value(target)?;
            let keys = vm
              .own_enumerable_keys_value(&target)?
              .into_iter()
              .filter_map(|key| match key {
                PropertyKey::String(s) => Some(Value::String(s)),
                _ => None,
              })
              .map(Some)
              .collect();
            Ok(vm.new_array(keys))
          }),
          function_prototype,
        );
        set_builtin_name(&builtin, "keys");
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("values".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let target = vm.to_object_value(target)?;
            let mut values = Vec::new();
            for key in vm.own_property_keys_value(&target)? {
              if !matches!(key, PropertyKey::String(_)) {
                continue;
              }
              let enumerable = vm
                .get_own_property_value(&target, &key)?
                .is_some_and(|descriptor| match descriptor {
                  PropertyDescriptor::Data { enumerable, .. }
                  | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
                });
              if enumerable {
                values.push(vm.get_property_host(&target, &key)?);
              }
            }
            Ok(vm.new_array(values.into_iter().map(Some).collect()))
          }),
          function_prototype,
        );
        set_builtin_name(&builtin, "values");
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("entries".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let target = vm.to_object_value(target)?;
            let mut entries = Vec::new();
            for key in vm.own_property_keys_value(&target)? {
              if !matches!(key, PropertyKey::String(_)) {
                continue;
              }
              let enumerable = vm
                .get_own_property_value(&target, &key)?
                .is_some_and(|descriptor| match descriptor {
                  PropertyDescriptor::Data { enumerable, .. }
                  | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
                });
              if enumerable {
                let key_value = property_key_to_value(&key);
                let value = vm.get_property_host(&target, &key)?;
                entries.push(vm.new_array(vec![Some(key_value), Some(value)]));
              }
            }
            Ok(vm.new_array(entries.into_iter().map(Some).collect()))
          }),
          function_prototype,
        );
        set_builtin_name(&builtin, "entries");
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(PKey::String("defineProperty".to_string().into()), {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          if args.len() < off + 3 {
            return Ok(Value::Undefined);
          }
          let target = args[off].clone();
          if crate::trace::enabled() {
            eprintln!(
              "[trace] Object.defineProperty target={:?} key={:?} desc={:?}",
              target,
              args.get(off + 1),
              args.get(off + 2)
            );
          }
          let ok = vm.define_property_value(&target, &args[off + 1], &args[off + 2])?;
          if !ok {
            return Err(vm.throw_type_error("Object.defineProperty failed"));
          }
          Ok(target)
        }),
        function_prototype,
      );
      set_builtin_length(&builtin, 3.0);
      PropertyDescriptor::builtin_data(builtin)
    });

  constructor_properties
    .borrow_mut()
    .properties
    .insert(PKey::String("defineProperties".to_string().into()), {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let props = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          if !matches!(
            props,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
          ) {
            return Err(vm.throw_type_error("Object.defineProperties called on non-object"));
          }
          for key in vm.own_property_keys_value(&props)? {
            let Some(property) = vm.get_own_property_value(&props, &key)? else {
              continue;
            };
            let enumerable = match property {
              PropertyDescriptor::Data { enumerable, .. }
              | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
            };
            if !enumerable {
              continue;
            }
            let descriptor = vm.get_property_host(&props, &key)?;
            let key_value = property_key_to_value(&key);
            let ok = vm.define_property_value(&target, &key_value, &descriptor)?;
            if !ok {
              return Err(vm.throw_type_error("Object.defineProperties failed"));
            }
          }
          Ok(target)
        }),
        function_prototype,
      );
      set_builtin_length(&builtin, 2.0);
      PropertyDescriptor::builtin_data(builtin)
    });
  constructor_properties
    .borrow_mut()
    .properties
    .insert(PKey::String("isExtensible".to_string().into()), {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          if !target.is_object() {
            return Ok(Value::Bool(false));
          }
          Ok(Value::Bool(vm.is_extensible_value(&target)?))
        }),
        function_prototype,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    });
  constructor_properties
    .borrow_mut()
    .properties
    .insert(PKey::String("preventExtensions".to_string().into()), {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          if !target.is_object() {
            return Ok(target);
          }
          if !vm.prevent_extensions_value(&target)? {
            return Err(vm.throw_type_error("Object.preventExtensions failed"));
          }
          Ok(target)
        }),
        function_prototype,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    });
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("setPrototypeOf".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let proto = args
              .get(off + 1)
              .cloned()
              .unwrap_or(Value::Undefined);
            if !matches!(proto, Value::Null) && !proto.is_object() {
              return Err(
                vm.throw_type_error("Object.setPrototypeOf prototype must be object or null"),
              );
            }
            if matches!(target, Value::Null | Value::Undefined) {
              return Err(vm.throw_type_error("Object.setPrototypeOf called on null/undefined"));
            }
            if !target.is_object() {
              return Ok(target);
            }
            if !vm.set_prototype_of_value(&target, &proto)? {
              return Err(
                vm.throw_type_error("Object.setPrototypeOf called on non-extensible object"),
              );
            }
            Ok(target)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 2.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("freeze".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            if !target.is_object() {
              return Ok(target);
            }
            if !vm.set_integrity_level(&target, true)? {
              return Err(vm.throw_type_error("Object.freeze failed"));
            }
            Ok(target)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("seal".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            if !target.is_object() {
              return Ok(target);
            }
            if !vm.set_integrity_level(&target, false)? {
              return Err(vm.throw_type_error("Object.seal failed"));
            }
            Ok(target)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("isSealed".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            if !target.is_object() {
              return Ok(Value::Bool(true));
            }
            Ok(Value::Bool(vm.test_integrity_level(&target, false)?))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("isFrozen".to_string().into()),
      PropertyDescriptor::builtin_data({
        let builtin = builtin_object(
          Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            if !target.is_object() {
              return Ok(Value::Bool(true));
            }
            Ok(Value::Bool(vm.test_integrity_level(&target, true)?))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("fromEntries".to_string().into()),
      PropertyDescriptor::builtin_data({
        let object_proto_for_from_entries = Rc::clone(object_prototype);
        let builtin = builtin_object(
          Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let iterable = args.get(off).cloned().unwrap_or(Value::Undefined);
            let entries = collect_iterable_values(vm, &iterable)?;
            let result = Value::Object(Rc::new(RefCell::new(ObjectData::with_prototype(
              Value::Object(Rc::clone(&object_proto_for_from_entries)),
            ))));
            for entry in entries {
              let entry_obj = vm.to_object_value(entry)?;
              let key = vm.get_property_host(&entry_obj, &PKey::String("0".to_string().into()))?;
              let value =
                vm.get_property_host(&entry_obj, &PKey::String("1".to_string().into()))?;
              let prop_key = vm.to_property_key(&key)?;
              let key_value = property_key_to_value(&prop_key);
              vm.set_property_host(&result, &key_value, value)?;
            }
            Ok(result)
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 1.0);
        builtin
      }),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("groupBy".to_string().into()),
      PropertyDescriptor::builtin_data({
        let object_proto_for_group_by = Rc::clone(object_prototype);
        let builtin = builtin_object(
          Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "Object");
            let iterable = args.get(off).cloned().unwrap_or(Value::Undefined);
            let callback = args
              .get(off + 1)
              .cloned()
              .unwrap_or(Value::Undefined);
            if !Vm::is_callable_value(&callback) {
              return Err(vm.throw_type_error("Object.groupBy callback is not callable"));
            }
            let items = collect_iterable_values(vm, &iterable)?;
            #[expect(
              clippy::mutable_key_type,
              reason = "PropertyKey's interior mutability is limited to JsString's derived UTF-8 cache"
            )]
            let mut groups: HashMap<PropertyKey, Vec<Value>> = HashMap::new();
            for (index, item) in items.into_iter().enumerate() {
              let key = vm.call_value_with_this(
                &callback,
                &Value::Undefined,
                &[item.clone(), Value::Number(usize_to_f64(index))],
              )?;
              let prop_key = vm.to_property_key(&key)?;
              groups.entry(prop_key).or_default().push(item);
            }
            let mut result = ObjectData::new();
            result.set_prototype_value(Value::Null);
            for (key, group) in groups {
              result.properties.insert(
                key,
                PropertyDescriptor::ordinary_data(array_from_values(group)),
              );
            }
            let _ = &object_proto_for_group_by;
            Ok(Value::Object(Rc::new(RefCell::new(result))))
          }),
          function_prototype,
        );
        set_builtin_length(&builtin, 2.0);
        builtin
      }),
    );

  ObjectIntrinsics { constructor }
}
