// SPDX-License-Identifier: MPL-2.0
//! Array iterator prototype and iterator factory methods.

use super::*;

pub(super) struct ArrayIteratorIntrinsics {
  pub(super) prototype: Rc<RefCell<ObjectData>>,
  pub(super) values: Value,
  pub(super) keys: Value,
  pub(super) entries: Value,
}

pub(super) fn install(
  array_proto_map: &Rc<RefCell<ObjectData>>,
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> ArrayIteratorIntrinsics {
  use PropertyKey as PKey;

  let array_iterator_proto: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  array_iterator_proto
    .borrow_mut()
    .properties
    .insert(
      PKey::Symbol(SymbolId::well_known_iterator()),
      PropertyDescriptor::builtin_data(builtin_object(
        Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
        &function_proto_map,
      )),
    );
  array_iterator_proto
    .borrow_mut()
    .properties
    .insert(
      PKey::String("next".to_string().into()),
      PropertyDescriptor::builtin_data(builtin_object(
        Rc::new(|vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let iter_obj = match this {
            Value::Object(obj) => obj,
            _ => {
              let result = Rc::new(RefCell::new(ObjectData::new()));
              result.borrow_mut().properties.insert(
                PKey::String("done".to_string().into()),
                PropertyDescriptor::ordinary_data(Value::Bool(true)),
              );
              result.borrow_mut().properties.insert(
                PKey::String("value".to_string().into()),
                PropertyDescriptor::ordinary_data(Value::Undefined),
              );
              return Ok(Value::Object(result));
            }
          };

          let iterated = iter_obj
            .borrow()
            .properties
            .get(&PKey::String("[[IteratedObject]]".to_string().into()))
            .and_then(|d| match d {
              PropertyDescriptor::Data { value, .. } => Some(value.clone()),
              _ => None,
            })
            .unwrap_or(Value::Undefined);
          let index = iter_obj
            .borrow()
            .properties
            .get(&PKey::String(
              "[[ArrayIteratorNextIndex]]".to_string().into(),
            ))
            .and_then(|d| match d {
              PropertyDescriptor::Data { value, .. } => Some(value.clone()),
              _ => None,
            })
            .and_then(|v| match v {
              Value::Number(n) => Some(n.max(0.0) as usize),
              _ => None,
            })
            .unwrap_or(0);

          let len = vm
            .get_property_host(&iterated, &PKey::String("length".to_string().into()))
            .ok()
            .and_then(|v| match v {
              Value::Number(n) => Some(n.max(0.0) as usize),
              _ => None,
            })
            .unwrap_or(0);

          let kind = iter_obj
            .borrow()
            .properties
            .get(&PKey::String("[[ArrayIteratorKind]]".to_string().into()))
            .and_then(|d| match d {
              PropertyDescriptor::Data { value, .. } => Some(value.clone()),
              _ => None,
            })
            .unwrap_or(Value::String("value".to_string().into()));

          let result = Rc::new(RefCell::new(ObjectData::new()));
          if index >= len {
            result.borrow_mut().properties.insert(
              PKey::String("done".to_string().into()),
              PropertyDescriptor::ordinary_data(Value::Bool(true)),
            );
            result.borrow_mut().properties.insert(
              PKey::String("value".to_string().into()),
              PropertyDescriptor::ordinary_data(Value::Undefined),
            );
          } else {
            let element =
              vm.get_property_host(&iterated, &PKey::String(index.to_string().into()))?;
            let value = match kind {
              Value::String(ref s) if s == "key" => Value::Number(usize_to_f64(index)),
              Value::String(ref s) if s == "key+value" => {
                array_from_values(vec![Value::Number(usize_to_f64(index)), element])
              }
              _ => element,
            };
            iter_obj.borrow_mut().properties.insert(
              PKey::String("[[ArrayIteratorNextIndex]]".to_string().into()),
              PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(index + 1))),
            );
            result.borrow_mut().properties.insert(
              PKey::String("done".to_string().into()),
              PropertyDescriptor::ordinary_data(Value::Bool(false)),
            );
            result.borrow_mut().properties.insert(
              PKey::String("value".to_string().into()),
              PropertyDescriptor::ordinary_data(value),
            );
          }
          Ok(Value::Object(result))
        }),
        &function_proto_map,
      )),
    );
  let make_array_iterator_builtin = |kind: &'static str| {
    builtin_object(
      Rc::new({
        let array_iterator_proto = Rc::clone(&array_iterator_proto);
        move |_vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let iter = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
            Rc::clone(&array_iterator_proto),
          ))));
          iter.borrow_mut().properties.insert(
            PKey::String("[[IteratedObject]]".to_string().into()),
            PropertyDescriptor::ordinary_data(this),
          );
          iter.borrow_mut().properties.insert(
            PKey::String("[[ArrayIteratorNextIndex]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(0.0)),
          );
          iter.borrow_mut().properties.insert(
            PKey::String("[[ArrayIteratorKind]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::String(kind.to_string().into())),
          );
          Ok(Value::Object(iter))
        }
      }),
      &function_proto_map,
    )
  };
  let array_values_builtin = make_array_iterator_builtin("value");
  let array_keys_builtin = make_array_iterator_builtin("key");
  let array_entries_builtin = make_array_iterator_builtin("key+value");
  set_builtin_name(&array_values_builtin, "values");
  set_builtin_name(&array_keys_builtin, "keys");
  set_builtin_name(&array_entries_builtin, "entries");
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("values".to_string().into()),
    PropertyDescriptor::builtin_data(array_values_builtin.clone()),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("keys".to_string().into()),
    PropertyDescriptor::builtin_data(array_keys_builtin.clone()),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("entries".to_string().into()),
    PropertyDescriptor::builtin_data(array_entries_builtin.clone()),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::builtin_data(array_values_builtin.clone()),
  );

  ArrayIteratorIntrinsics {
    prototype: array_iterator_proto,
    values: array_values_builtin,
    keys: array_keys_builtin,
    entries: array_entries_builtin,
  }
}
