// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::same_value;

#[allow(clippy::type_complexity)]
fn typed_view_details(
  value: &Value
) -> Option<(
  Rc<RefCell<ObjectData>>,
  Value,
  Rc<RefCell<ArrayData>>,
  String,
  usize,
  usize,
  Value,
)> {
  let details = byte_view_details(value)?;
  Some((
    details.object,
    details.buffer,
    details.data,
    details.kind,
    details.length,
    details.byte_offset,
    details.prototype,
  ))
}

fn new_typed_array_view(
  prototype: Value,
  buffer: Value,
  data: Rc<RefCell<ArrayData>>,
  kind: &str,
  length: usize,
  byte_offset: usize,
) -> Value {
  new_byte_view(prototype, buffer, data, kind, length, byte_offset)
}

fn new_byte_buffer(
  prototype: &Rc<RefCell<ObjectData>>,
  length: usize,
) -> (Value, Rc<RefCell<ArrayData>>) {
  new_array_buffer(Value::Object(Rc::clone(prototype)), length, false)
}

fn typed_array_relative_index(
  vm: &mut Vm,
  value: &Value,
  length: usize,
) -> Result<usize, RuntimeError> {
  let number = vm.to_number_value(value)?;
  if number.is_nan() {
    return Ok(0);
  }
  if number == f64::NEG_INFINITY {
    return Ok(0);
  }
  if number == f64::INFINITY {
    return Ok(length);
  }
  if number < 0.0 {
    return Ok(length.saturating_sub((-number).trunc() as usize));
  }
  Ok(number.trunc().min(length as f64) as usize)
}

fn regexp_matcher(
  vm: &mut Vm,
  value: &Value,
) -> Result<regex_js::Regex, RuntimeError> {
  let Value::Object(object) = value else {
    return Err(vm.throw_type_error("RegExp method called on incompatible receiver"));
  };
  object
    .borrow()
    .regexp_matcher
    .clone()
    .ok_or_else(|| vm.throw_type_error("RegExp object has no [[RegExpMatcher]]"))
}

fn is_regexp(
  vm: &mut Vm,
  value: &Value,
) -> Result<bool, RuntimeError> {
  let Value::Object(object) = value else {
    return Ok(false);
  };
  let matcher = vm.get_property_host(value, &PropertyKey::Symbol(SymbolId::well_known_match()))?;
  if !matches!(matcher, Value::Undefined) {
    return Ok(vm.to_boolean(&matcher));
  }
  Ok(object.borrow().regexp_matcher.is_some())
}

fn regexp_builtin_exec(
  vm: &mut Vm,
  this: &Value,
  input: Value,
) -> Result<Value, RuntimeError> {
  let matcher = regexp_matcher(vm, this)?;
  let subject = vm.to_string(&input)?;
  let subject_utf16 = subject.clone();
  let global = matcher.flags().global();
  let sticky = matcher.flags().sticky();
  let start = if global || sticky {
    let last_index =
      vm.get_property_host(this, &PropertyKey::String("lastIndex".to_string().into()))?;
    let number = vm.to_number_value(&last_index)?;
    if number.is_nan() || number <= 0.0 {
      0
    } else if number.is_finite() {
      number.trunc().to_usize().unwrap_or(usize::MAX)
    } else {
      usize::MAX
    }
  } else {
    0
  };

  let Some(matched) = matcher.find(&subject_utf16, start) else {
    if global || sticky {
      drop(vm.set_property_host(
        this,
        &Value::String("lastIndex".to_string().into()),
        Value::Number(0.0),
      )?);
    }
    return Ok(Value::Null);
  };

  if global || sticky {
    drop(vm.set_property_host(
      this,
      &Value::String("lastIndex".to_string().into()),
      Value::Number(usize_to_f64(matched.range().end)),
    )?);
  }

  let entries = matched
    .captures()
    .iter()
    .map(|capture| {
      capture
        .as_ref()
        .map_or(Value::Undefined, |capture| {
          let value = subject_utf16
            .slice(capture.range())
            .expect("matcher capture is within its input");
          Value::String(value)
        })
    })
    .collect::<Vec<_>>();
  let result = array_from_values(entries);
  drop(vm.set_property_host(
    &result,
    &Value::String("index".to_string().into()),
    Value::Number(usize_to_f64(matched.range().start)),
  )?);
  drop(vm.set_property_host(
    &result,
    &Value::String("input".to_string().into()),
    Value::String(subject),
  )?);

  let named_groups = if matcher
    .capture_names()
    .iter()
    .skip(1)
    .any(Option::is_some)
  {
    let mut groups = ObjectData::with_prototype(Value::Null);
    for (index, name) in matcher.capture_names().iter().enumerate().skip(1) {
      let Some(name) = name else {
        continue;
      };
      let value = matched
        .capture(index)
        .map_or(Value::Undefined, |capture| {
          Value::String(
            subject_utf16
              .slice(capture.range())
              .expect("named capture is within the input"),
          )
        });
      groups.properties.insert(
        PropertyKey::String(name.clone()),
        PropertyDescriptor::ordinary_data(value),
      );
    }
    Value::Object(Rc::new(RefCell::new(groups)))
  } else {
    Value::Undefined
  };
  drop(vm.set_property_host(
    &result,
    &Value::String("groups".to_string().into()),
    named_groups,
  )?);

  if matcher.flags().has_indices() {
    let indices = matched
      .captures()
      .iter()
      .map(|capture| {
        capture
          .as_ref()
          .map_or(Value::Undefined, |capture| {
            let range = capture.range();
            array_from_values(vec![
              Value::Number(usize_to_f64(range.start)),
              Value::Number(usize_to_f64(range.end)),
            ])
          })
      })
      .collect::<Vec<_>>();
    let indices = array_from_values(indices);
    let named_indices = if matcher
      .capture_names()
      .iter()
      .skip(1)
      .any(Option::is_some)
    {
      let mut groups = ObjectData::with_prototype(Value::Null);
      for (index, name) in matcher.capture_names().iter().enumerate().skip(1) {
        let Some(name) = name else {
          continue;
        };
        let value = matched
          .capture(index)
          .map_or(Value::Undefined, |capture| {
            let range = capture.range();
            array_from_values(vec![
              Value::Number(usize_to_f64(range.start)),
              Value::Number(usize_to_f64(range.end)),
            ])
          });
        groups.properties.insert(
          PropertyKey::String(name.clone()),
          PropertyDescriptor::ordinary_data(value),
        );
      }
      Value::Object(Rc::new(RefCell::new(groups)))
    } else {
      Value::Undefined
    };
    drop(vm.set_property_host(
      &indices,
      &Value::String("groups".to_string().into()),
      named_indices,
    )?);
    drop(vm.set_property_host(
      &result,
      &Value::String("indices".to_string().into()),
      indices,
    )?);
  }
  Ok(result)
}

fn regexp_exec(
  vm: &mut Vm,
  this: &Value,
  subject: &js_string::JsString,
) -> Result<Value, RuntimeError> {
  let exec = vm.get_property_host(this, &PropertyKey::String("exec".into()))?;
  if Vm::is_callable_value(&exec) {
    let result = vm.call_value_with_this(&exec, this, &[Value::String(subject.clone())])?;
    if matches!(result, Value::Null) || result.is_object() {
      return Ok(result);
    }
    return Err(vm.throw_type_error("RegExp exec must return an object or null"));
  }
  regexp_builtin_exec(vm, this, Value::String(subject.clone()))
}

fn regexp_builtin_match(
  vm: &mut Vm,
  this: &Value,
  subject: js_string::JsString,
) -> Result<Value, RuntimeError> {
  let global = vm.get_property_host(this, &PropertyKey::String("global".into()))?;
  let global = vm.to_boolean(&global);
  if !global {
    return regexp_exec(vm, this, &subject);
  }
  let unicode = vm.get_property_host(this, &PropertyKey::String("unicode".into()))?;
  let unicode = vm.to_boolean(&unicode);
  drop(vm.set_property_host(this, &Value::String("lastIndex".into()), Value::Number(0.0))?);
  let mut matches = Vec::new();
  loop {
    let result = regexp_exec(vm, this, &subject)?;
    if matches!(result, Value::Null) {
      break;
    }
    let matched = vm.get_property_host(&result, &PropertyKey::String("0".into()))?;
    let matched = vm.to_string(&matched)?;
    matches.push(Value::String(matched.clone()));
    if matched.is_empty() {
      let last_index = vm.get_property_host(this, &PropertyKey::String("lastIndex".into()))?;
      let last_index = to_length(vm, &last_index)?;
      let next = subject.advance_string_index(last_index, unicode);
      drop(vm.set_property_host(
        this,
        &Value::String("lastIndex".into()),
        Value::Number(usize_to_f64(next)),
      )?);
    }
  }
  if matches.is_empty() {
    Ok(Value::Null)
  } else {
    Ok(array_from_values(matches))
  }
}

fn regexp_builtin_search(
  vm: &mut Vm,
  this: &Value,
  subject: js_string::JsString,
) -> Result<Value, RuntimeError> {
  let previous_last_index = vm.get_property_host(this, &PropertyKey::String("lastIndex".into()))?;
  if !same_value(&previous_last_index, &Value::Number(0.0)) {
    drop(vm.set_property_host(this, &Value::String("lastIndex".into()), Value::Number(0.0))?);
  }
  let result = regexp_exec(vm, this, &subject);
  let current_last_index = vm.get_property_host(this, &PropertyKey::String("lastIndex".into()))?;
  if !same_value(&current_last_index, &previous_last_index) {
    drop(vm.set_property_host(
      this,
      &Value::String("lastIndex".into()),
      previous_last_index,
    )?);
  }
  let result = result?;
  if matches!(result, Value::Null) {
    return Ok(Value::Number(-1.0));
  }
  vm.get_property_host(&result, &PropertyKey::String("index".into()))
}

fn regexp_builtin_replace(
  vm: &mut Vm,
  this: &Value,
  subject: js_string::JsString,
  replacement: Value,
) -> Result<Value, RuntimeError> {
  struct ReplacementMatch {
    matched: js_string::JsString,
    captures: Vec<Option<js_string::JsString>>,
    groups: Value,
    start: usize,
    end: usize,
  }

  let global = vm.get_property_host(this, &PropertyKey::String("global".into()))?;
  let global = vm.to_boolean(&global);
  let unicode = if global {
    let unicode = vm.get_property_host(this, &PropertyKey::String("unicode".into()))?;
    vm.to_boolean(&unicode)
  } else {
    false
  };
  let functional_replace = Vm::is_callable_value(&replacement);
  let replacement_string = if functional_replace {
    None
  } else {
    Some(vm.to_string(&replacement)?)
  };
  let mut matches = Vec::new();

  if global {
    drop(vm.set_property_host(
      this,
      &Value::String("lastIndex".to_string().into()),
      Value::Number(0.0),
    )?);
  }

  loop {
    let result = regexp_exec(vm, this, &subject)?;
    if matches!(result, Value::Null) {
      break;
    }
    let matched = vm.get_property_host(&result, &PropertyKey::String("0".into()))?;
    let matched = vm.to_string(&matched)?;
    let position = vm.get_property_host(&result, &PropertyKey::String("index".into()))?;
    let position = vm.to_number_value(&position)?;
    let start = if position.is_nan() || position <= 0.0 {
      0
    } else if position.is_finite() {
      position
        .trunc()
        .to_usize()
        .unwrap_or(subject.len())
        .min(subject.len())
    } else {
      subject.len()
    };
    let end = start
      .saturating_add(matched.len())
      .min(subject.len());
    let capture_length = vm.get_property_host(&result, &PropertyKey::String("length".into()))?;
    let capture_length = to_length(vm, &capture_length)?.saturating_sub(1);
    let mut captures = Vec::with_capacity(capture_length);
    for index in 1..=capture_length {
      let capture =
        vm.get_property_host(&result, &PropertyKey::String(index.to_string().into()))?;
      captures.push(if matches!(capture, Value::Undefined) {
        None
      } else {
        Some(vm.to_string(&capture)?)
      });
    }
    let groups = vm.get_property_host(&result, &PropertyKey::String("groups".into()))?;
    matches.push(ReplacementMatch {
      matched: matched.clone(),
      captures,
      groups,
      start,
      end,
    });
    if !global {
      break;
    }
    if matched.is_empty() {
      let last_index = vm.get_property_host(this, &PropertyKey::String("lastIndex".into()))?;
      let last_index = to_length(vm, &last_index)?;
      let next = subject.advance_string_index(last_index, unicode);
      drop(vm.set_property_host(
        this,
        &Value::String("lastIndex".into()),
        Value::Number(usize_to_f64(next)),
      )?);
    }
  }

  let mut output = js_string::JsStringBuilder::with_capacity(subject.len());
  let mut next_source_position = 0;
  for matched in matches {
    if matched.start < next_source_position {
      continue;
    }
    output.push_string(
      &subject
        .slice(next_source_position..matched.start)
        .expect("match prefix is within the input"),
    );
    let replacement_text = if functional_replace {
      let mut call_args = vec![Value::String(matched.matched.clone())];
      call_args.extend(matched.captures.iter().map(|capture| {
        capture
          .as_ref()
          .map_or(Value::Undefined, |capture| Value::String(capture.clone()))
      }));
      call_args.push(Value::Number(usize_to_f64(matched.start)));
      call_args.push(Value::String(subject.clone()));
      if !matches!(matched.groups, Value::Undefined) {
        call_args.push(matched.groups.clone());
      }
      let result = vm.call_value_with_this(&replacement, &Value::Undefined, &call_args)?;
      vm.to_string(&result)?
    } else {
      let replacement = replacement_string
        .as_ref()
        .expect("non-callable replacement was converted before matching");
      let named_captures = if matches!(matched.groups, Value::Undefined) {
        None
      } else {
        let mut names = Vec::new();
        let units = replacement.as_slice();
        let mut cursor = 0;
        while cursor + 2 < units.len() {
          if units[cursor] == u16::from(b'$')
            && units[cursor + 1] == u16::from(b'<')
            && let Some(relative_end) = units[cursor + 2..]
              .iter()
              .position(|unit| *unit == u16::from(b'>'))
          {
            let end = cursor + 2 + relative_end;
            let name = js_string::JsString::from_utf16(units[cursor + 2..end].to_vec());
            if !names
              .iter()
              .any(|(candidate, _)| candidate == &name)
            {
              let value =
                vm.get_property_host(&matched.groups, &PropertyKey::String(name.clone()))?;
              names.push((
                name,
                if matches!(value, Value::Undefined) {
                  None
                } else {
                  Some(vm.to_string(&value)?)
                },
              ));
            }
            cursor = end + 1;
            continue;
          }
          cursor += 1;
        }
        Some(names)
      };
      get_substitution(
        &matched.matched,
        &subject,
        matched.start,
        matched.end,
        &matched.captures,
        named_captures.as_deref(),
        replacement,
      )
    };
    output.push_string(&replacement_text);
    next_source_position = matched.end;
  }
  output.push_string(
    &subject
      .slice(next_source_position..subject.len())
      .expect("replacement suffix is within the input"),
  );
  Ok(Value::String(output.finish()))
}

fn regexp_builtin_split(
  vm: &mut Vm,
  this: &Value,
  subject: js_string::JsString,
  limit: u32,
) -> Result<Value, RuntimeError> {
  if limit == 0 {
    return Ok(array_from_values(Vec::new()));
  }
  let matcher = regexp_matcher(vm, this)?;
  let unicode = matcher.flags().unicode();
  let size = subject.len();
  let mut output = Vec::new();

  if size == 0 {
    return if matcher.find(&subject, 0).is_some() {
      Ok(array_from_values(output))
    } else {
      Ok(array_from_values(vec![Value::String(subject)]))
    };
  }

  let limit = usize::try_from(limit).unwrap_or(usize::MAX);
  let mut previous_end = 0;
  let mut position = 0;
  while position < size {
    let Some(matched) = matcher.find(&subject, position) else {
      break;
    };
    let range = matched.range();
    if range.start != position {
      position = subject.advance_string_index(position, unicode);
      continue;
    }
    let end = range.end.min(size);
    if end == previous_end {
      position = subject.advance_string_index(position, unicode);
      continue;
    }

    output.push(Value::String(
      subject
        .slice(previous_end..position)
        .expect("split prefix is within the input"),
    ));
    if output.len() == limit {
      return Ok(array_from_values(output));
    }
    for capture in matched.captures().iter().skip(1) {
      output.push(
        capture
          .as_ref()
          .map_or(Value::Undefined, |capture| {
            Value::String(
              subject
                .slice(capture.range())
                .expect("split capture is within the input"),
            )
          }),
      );
      if output.len() == limit {
        return Ok(array_from_values(output));
      }
    }
    previous_end = end;
    position = end;
  }

  output.push(Value::String(
    subject
      .slice(previous_end..size)
      .expect("split suffix is within the input"),
  ));
  output.truncate(limit);
  Ok(array_from_values(output))
}

/// Builds the ECMAScript intrinsics and installs the available global bindings.
pub(crate) fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) -> Intrinsics {
  use PropertyKey as PKey;

  let object_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
  let function_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let function_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  function_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&function_proto_map))),
  );

  let generator_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let generator_function_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  generator_function_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&generator_proto_map))),
    );

  let throw_type_error_val = builtin_object(
    Rc::new(|vm, _args| Err(vm.throw_type_error("ThrowTypeError"))),
    &function_proto_map,
  );

  let object_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_proto_map)),
  )));
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&object_proto_map))),
  );

  let array_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&object_proto_map)),
  )));
  let for_each_fn = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
      let this_for_call = this.clone();
      if let Value::Array(arr, _) = &this {
        // Snapshot initial length per ECMAScript semantics and avoid
        // holding a RefCell borrow across callback invocation.
        let len = arr.borrow().len();
        for i in 0..len {
          let item = arr.borrow().get(i).cloned().flatten();
          if let Some(item) = item {
            let idx = Value::Number(usize_to_f64(i));
            vm.call_value_with_this(
              &callback,
              &this_for_call,
              &[item, idx, this_for_call.clone()],
            )?;
          }
        }
      }
      Ok(Value::Undefined)
    }),
    &function_proto_map,
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("forEach".to_string().into()),
    PropertyDescriptor::builtin_data(for_each_fn),
  );
  let array_proto_for_map = Rc::clone(&array_proto_map);
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("map".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

        let len = array_like_length(vm, &receiver)?;
        if len > u32::MAX as usize {
          return Err(vm.throw_range_error("Invalid array length"));
        }
        let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          // Check whether the property exists, including inherited properties.
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            let mapped = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            out.push(Some(mapped));
          } else {
            // Preserve hole
            out.push(None);
          }
        }

        let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props
          .borrow_mut()
          .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_map)));
        props.borrow_mut().properties.insert(
          PKey::String("length".to_string().into()),
          array_length_descriptor(arr_rc.borrow().len()),
        );
        Ok(Value::Array(arr_rc, props))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("join".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let sep = match args.get(1) {
          Some(Value::Undefined) | None => js_string::JsString::from(","),
          Some(v) => vm.to_string(v)?,
        };
        match this {
          Value::Array(arr, _) => {
            let mut out = js_string::JsStringBuilder::new();
            let mut first = true;
            for item_slot in arr.borrow().iter() {
              if !first {
                out.push_string(&sep);
              }
              first = false;
              if let Some(item) = item_slot {
                match item {
                  Value::Undefined | Value::Null => {}
                  v => out.push_string(&vm.to_string(v)?),
                }
              }
            }
            Ok(Value::String(out.finish()))
          }
          _ => Ok(Value::String("".to_string().into())),
        }
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let join = vm.get_property_host(&this, &PKey::String("join".to_string().into()))?;
        vm.call_value_with_this(&join, &this, &[])
      }),
      &function_proto_map,
    )),
  );
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("sort".to_string().into()), {
      let sort_fn = builtin_object(
        Rc::new(|vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let compare_fn = args.get(1).cloned().unwrap_or(Value::Undefined);
          let compare_is_undefined = matches!(compare_fn, Value::Undefined);
          let compare_is_callable = compare_is_undefined
            || matches!(
              compare_fn,
              Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
            );
          if !compare_is_callable {
            return Err(vm.throw_type_error("Array.prototype.sort comparefn is not callable"));
          }

          let len = array_like_length(vm, &this)?;
          let (keys, snapshot) = collect_indexed_snapshot(vm, &this, len)?;
          let mut values: Vec<Value> = Vec::new();
          let mut undefined_count = 0usize;
          for idx in &keys {
            match snapshot
              .get(idx)
              .cloned()
              .unwrap_or(Value::Undefined)
            {
              Value::Undefined => undefined_count += 1,
              v => values.push(v),
            }
          }

          let compare_pair =
            |vm: &mut Vm, a: &Value, b: &Value| -> Result<Ordering, RuntimeError> {
              if compare_is_undefined {
                let sa = vm.to_string(a)?;
                let sb = vm.to_string(b)?;
                return Ok(sa.cmp(&sb));
              }
              let cmp =
                vm.call_value_with_this(&compare_fn, &Value::Undefined, &[a.clone(), b.clone()])?;
              let cmp_num = vm.to_number_value(&cmp)?;
              Ok(if cmp_num.is_nan() || cmp_num == 0.0 {
                Ordering::Equal
              } else if cmp_num < 0.0 {
                Ordering::Less
              } else {
                Ordering::Greater
              })
            };
          let n = values.len();
          for i in 0..n {
            for j in (i + 1)..n {
              if compare_pair(vm, &values[i], &values[j])? == Ordering::Greater {
                values.swap(i, j);
              }
            }
          }

          let cutoff = values.len() + undefined_count;
          let mut next_index = 0usize;
          for value in values {
            vm.set_property_host(&this, &Value::String(next_index.to_string().into()), value)?;
            next_index += 1;
          }
          for _ in 0..undefined_count {
            vm.set_property_host(
              &this,
              &Value::String(next_index.to_string().into()),
              Value::Undefined,
            )?;
            next_index += 1;
          }
          for idx in keys {
            if idx >= cutoff {
              let _ = vm.delete_property_host(&this, &PKey::String(idx.to_string().into()))?;
            }
          }
          Ok(this)
        }),
        &function_proto_map,
      );
      set_builtin_length(&sort_fn, 1.0);
      PropertyDescriptor::builtin_data(sort_fn)
    });
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toReversed".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
          for k in 0..len {
            let from = len - 1 - k;
            let key = PKey::String(from.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              out.push(Some(vm.get_property_host(&receiver, &key)?));
            } else {
              out.push(None);
            }
          }
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toSorted".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let compare_fn = args.get(1).cloned().unwrap_or(Value::Undefined);
          let has_compare = matches!(
            compare_fn,
            Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
          );
          let len = array_like_length(vm, &receiver)?;
          let mut values: Vec<Value> = Vec::new();
          let mut undefined_count = 0usize;
          let mut hole_count = 0usize;
          for i in 0..len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              let item = vm.get_property_host(&receiver, &key)?;
              match item {
                Value::Undefined => undefined_count += 1,
                v => values.push(v),
              }
            } else {
              hole_count += 1;
            }
          }
          let n = values.len();
          for i in 0..n {
            for j in (i + 1)..n {
              let swap = if has_compare {
                let cmp = vm.call_value_with_this(
                  &compare_fn,
                  &Value::Undefined,
                  &[values[i].clone(), values[j].clone()],
                )?;
                let cmp_num = vm.to_number_value(&cmp)?;
                cmp_num.is_finite() && cmp_num > 0.0
              } else {
                let ai = vm.to_string(&values[i])?;
                let aj = vm.to_string(&values[j])?;
                ai > aj
              };
              if swap {
                values.swap(i, j);
              }
            }
          }
          while undefined_count > 0 {
            values.push(Value::Undefined);
            undefined_count -= 1;
          }
          let mut out: Vec<Option<Value>> = values.into_iter().map(Some).collect();
          out.extend((0..hole_count).map(|_| None));
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("with".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new({
          let array_proto_map = Rc::clone(&array_proto_map);
          move |vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_like_length(vm, &receiver)?;
            let relative_index =
              to_integer_or_infinity(vm, args.get(1).unwrap_or(&Value::Undefined))?;
            let actual_index = if relative_index < 0 {
              len as isize + relative_index
            } else {
              relative_index
            };
            if actual_index < 0 || actual_index as usize >= len {
              return Err(vm.throw_range_error("Array.prototype.with index out of range"));
            }
            let actual_index = actual_index as usize;
            let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
            let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
            for i in 0..len {
              if i == actual_index {
                out.push(Some(replacement.clone()));
                continue;
              }
              let key = PKey::String(i.to_string().into());
              let value = if matches!(receiver, Value::Array(..))
                && own_property_descriptor(&receiver, &key).is_none()
              {
                vm.get_property_host(&Value::Object(Rc::clone(&array_proto_map)), &key)?
              } else {
                vm.get_property_host(&receiver, &key)?
              };
              out.push(Some(value));
            }
            let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
            let props = Rc::new(RefCell::new(ObjectData::new()));
            props
              .borrow_mut()
              .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
            props.borrow_mut().properties.insert(
              PKey::String("length".to_string().into()),
              array_length_descriptor(arr_rc.borrow().len()),
            );
            Ok(Value::Array(arr_rc, props))
          }
        }),
        &function_proto_map,
      );
      set_builtin_name(&builtin, "with");
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("push".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let (items, props) = match this {
          Value::Array(arr, props) => (arr, props),
          _ => return Ok(Value::Number(0.0)),
        };
        let mut vec = items.borrow_mut();
        for v in args.iter().skip(1).cloned() {
          vec.push(Some(v));
        }
        let new_len = usize_to_f64(vec.len());
        drop(vec);
        props.borrow_mut().properties.insert(
          PKey::String("length".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Number(new_len)),
        );
        Ok(Value::Number(new_len))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("splice".to_string().into()), {
      let splice_fn = builtin_object(
        Rc::new({
          let array_proto_map = Rc::clone(&array_proto_map);
          move |vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_like_length(vm, &this)?;
            let arg_count = args.len().saturating_sub(1);
            let start_raw = vm.to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?;
            let start = if start_raw < 0.0 {
              let back = start_raw.abs().trunc().to_usize().unwrap_or(len);
              len.saturating_sub(back)
            } else {
              start_raw
                .trunc()
                .to_usize()
                .unwrap_or(len)
                .min(len)
            };
            let actual_delete_count = if arg_count == 0 {
              0
            } else if arg_count == 1 {
              len.saturating_sub(start)
            } else {
              let delete_count = vm
                .to_number_value(
                  args
                    .get(2)
                    .expect("args.get(2) should exist when arg_count >= 2"),
                )?
                .max(0.0)
                .trunc()
                .to_usize()
                .unwrap_or(0);
              delete_count.min(len.saturating_sub(start))
            };
            if actual_delete_count > u32::MAX as usize {
              return Err(vm.throw_range_error("Invalid array length"));
            }
            let insert_items: Vec<Value> = args.iter().skip(3).cloned().collect();
            let insert_count = insert_items.len();
            let new_len = len
              .saturating_sub(actual_delete_count)
              .saturating_add(insert_count);
            if new_len > u32::MAX as usize {
              return Err(vm.throw_range_error("Invalid array length"));
            }
            if new_len > 9_007_199_254_740_991usize {
              return Err(vm.throw_type_error("Array length exceeds supported size"));
            }
            let (keys, snapshot) = collect_indexed_snapshot(vm, &this, len)?;
            let mut removed: Vec<Option<Value>> = Vec::with_capacity(actual_delete_count);
            for i in 0..actual_delete_count {
              removed.push(snapshot.get(&(start + i)).cloned());
            }
            let removed_arr = Rc::new(RefCell::new(ArrayData::from(removed)));
            let removed_props = Rc::new(RefCell::new(ObjectData::new()));
            removed_props
              .borrow_mut()
              .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
            removed_props.borrow_mut().properties.insert(
              PKey::String("length".to_string().into()),
              array_length_descriptor(removed_arr.borrow().len()),
            );

            let delta = insert_count as isize - actual_delete_count as isize;
            let mut written_targets: HashSet<usize> = HashSet::new();
            for idx in &keys {
              let target = if *idx < start {
                Some(*idx)
              } else if *idx >= start + actual_delete_count {
                idx.checked_add_signed(delta)
              } else {
                None
              };
              if let Some(target_idx) = target
                && let Some(value) = snapshot.get(idx).cloned()
              {
                vm.set_property_host(&this, &Value::String(target_idx.to_string().into()), value)?;
                written_targets.insert(target_idx);
              }
            }
            for (offset, value) in insert_items.into_iter().enumerate() {
              let idx = start + offset;
              vm.set_property_host(&this, &Value::String(idx.to_string().into()), value)?;
              written_targets.insert(idx);
            }
            for idx in keys {
              if idx >= start && !written_targets.contains(&idx) {
                let _ = vm.delete_property_host(&this, &PKey::String(idx.to_string().into()))?;
              }
            }
            if !array_length_is_writable(&this) && new_len != len {
              return Err(vm.throw_type_error("Array length is not writable"));
            }
            vm.set_property_host(
              &this,
              &Value::String("length".to_string().into()),
              Value::Number(usize_to_f64(new_len)),
            )?;
            Ok(Value::Array(removed_arr, removed_props))
          }
        }),
        &function_proto_map,
      );
      set_builtin_length(&splice_fn, 2.0);
      PropertyDescriptor::builtin_data(splice_fn)
    });
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("shift".to_string().into()), {
      let shift_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &this)?;
          if len == 0 {
            return Ok(Value::Undefined);
          }
          let result = vm.get_property_host(&this, &PKey::String("0".to_string().into()))?;
          let (keys, snapshot) = collect_indexed_snapshot(vm, &this, len)?;
          let source_keys: HashSet<usize> = keys.iter().copied().collect();
          let mut written_targets: HashSet<usize> = HashSet::new();

          for idx in keys.iter().copied() {
            if idx == 0 {
              continue;
            }
            if let Some(value) = snapshot.get(&idx).cloned() {
              vm.set_property_host(&this, &Value::String((idx - 1).to_string().into()), value)?;
              written_targets.insert(idx - 1);
            }
          }
          for idx in keys {
            if !source_keys.contains(&(idx + 1)) && !written_targets.contains(&idx) {
              let _ = vm.delete_property_host(&this, &PKey::String(idx.to_string().into()))?;
            }
          }
          if !array_length_is_writable(&this) {
            return Err(vm.throw_type_error("Array length is not writable"));
          }
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(len - 1)),
          )?;
          Ok(result)
        }),
        &function_proto_map,
      );
      set_builtin_length(&shift_fn, 0.0);
      PropertyDescriptor::builtin_data(shift_fn)
    });
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("unshift".to_string().into()), {
      let unshift_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &this)?;
          let insert_items: Vec<Value> = args.iter().skip(1).cloned().collect();
          let insert_count = insert_items.len();
          if insert_count == 0 {
            return Ok(Value::Number(usize_to_f64(len)));
          }
          let new_len = len
            .checked_add(insert_count)
            .ok_or_else(|| vm.throw_type_error("Array length exceeds supported size"))?;
          if new_len > 9_007_199_254_740_991usize {
            return Err(vm.throw_type_error("Array length exceeds supported size"));
          }
          let mut keys: Vec<usize> = vm
            .own_property_keys_value(&this)?
            .into_iter()
            .filter_map(|key| match key {
              PropertyKey::String(s) => s.parse::<usize>().ok(),
              PropertyKey::Symbol(_) => None,
            })
            .filter(|idx| *idx < len)
            .collect();
          keys.sort_unstable_by(|a, b| b.cmp(a));
          for idx in keys {
            let from_key = PKey::String(idx.to_string().into());
            let to_key = Value::String((idx + insert_count).to_string().into());
            let from_present = vm.has_property_host(&this, &from_key)?;
            if from_present {
              let value = vm.get_property_host(&this, &from_key)?;
              vm.set_property_host(&this, &to_key, value)?;
            } else {
              let _ = vm.delete_property_host(
                &this,
                &PKey::String((idx + insert_count).to_string().into()),
              )?;
            }
          }
          for (idx, value) in insert_items.into_iter().enumerate() {
            vm.set_property_host(&this, &Value::String(idx.to_string().into()), value)?;
          }
          if !array_length_is_writable(&this) {
            return Err(vm.throw_type_error("Array length is not writable"));
          }
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(new_len)),
          )?;
          Ok(Value::Number(usize_to_f64(new_len)))
        }),
        &function_proto_map,
      );
      set_builtin_length(&unshift_fn, 0.0);
      PropertyDescriptor::builtin_data(unshift_fn)
    });
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toSpliced".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let max_safe_len = 9_007_199_254_740_991u128;
          let len = if len == 0 {
            0usize
          } else {
            len.min(max_safe_len as usize)
          };
          let insert_count = args.len().saturating_sub(3);
          let start_raw = vm.to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?;
          let start = if start_raw < 0.0 {
            let back = start_raw.abs().trunc().to_usize().unwrap_or(len);
            len.saturating_sub(back)
          } else {
            start_raw
              .trunc()
              .to_usize()
              .unwrap_or(len)
              .min(len)
          };
          let delete_count = if let Some(dc) = args.get(2) {
            vm.to_number_value(dc)?
              .max(0.0)
              .trunc()
              .to_usize()
              .unwrap_or(0)
          } else {
            len.saturating_sub(start)
          };
          let actual_delete_count = delete_count.min(len.saturating_sub(start));
          let new_len = (len as u128)
            .checked_sub(actual_delete_count as u128)
            .and_then(|n| n.checked_add(insert_count as u128))
            .ok_or_else(|| vm.throw_type_error("Array length exceeds supported size"))?;
          if new_len > max_safe_len {
            return Err(vm.throw_type_error("Array length exceeds supported size"));
          }
          if new_len > 4_294_967_295u128 {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          let mut out: Vec<Option<Value>> = Vec::new();
          for i in 0..start {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              out.push(Some(vm.get_property_host(&receiver, &key)?));
            } else {
              out.push(None);
            }
          }
          out.extend(args.iter().skip(3).cloned().map(Some));
          for i in start.saturating_add(actual_delete_count)..len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              out.push(Some(vm.get_property_host(&receiver, &key)?));
            } else {
              out.push(None);
            }
          }
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("concat".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |_vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let mut out: Vec<Option<Value>> = Vec::new();
          let mut append = |value: Value| -> Result<(), RuntimeError> {
            match value {
              Value::Array(arr, _) => {
                out.extend(arr.borrow().iter().cloned());
              }
              other => out.push(Some(other)),
            }
            Ok(())
          };
          append(receiver)?;
          for arg in args.iter().skip(1).cloned() {
            append(arg)?;
          }

          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      &function_proto_map,
    )),
  );
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
  // Array.prototype.filter
  let array_proto_for_filter = Rc::clone(&array_proto_map);
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("filter".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

        let len = array_like_length(vm, &receiver)?;
        let mut out: Vec<Option<Value>> = Vec::new();

        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            let keep = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[
                item.clone(),
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
            if keep.is_truthy() {
              out.push(Some(item));
            }
          }
        }

        let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props
          .borrow_mut()
          .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_filter)));
        props.borrow_mut().properties.insert(
          PKey::String("length".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
        );
        Ok(Value::Array(arr_rc, props))
      }),
      &function_proto_map,
    )),
  );
  // Array.prototype.find
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("find".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

        let len = array_like_length(vm, &receiver)?;

        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            let found = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[
                item.clone(),
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
            if found.is_truthy() {
              return Ok(item);
            }
          }
        }
        Ok(Value::Undefined)
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("at".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let index = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
        if index.is_nan() {
          return Ok(Value::Undefined);
        }
        let idx = if index < 0.0 {
          let offset = index.trunc() as isize;
          len as isize + offset
        } else {
          index.trunc() as isize
        };
        if idx < 0 || idx as usize >= len {
          return Ok(Value::Undefined);
        }
        let key = PKey::String((idx as usize).to_string().into());
        if vm.has_property_host(&receiver, &key)? {
          Ok(vm.get_property_host(&receiver, &key)?)
        } else {
          Ok(Value::Undefined)
        }
      }),
      &function_proto_map,
    )),
  );
  let array_includes = builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let search = args.get(1).cloned().unwrap_or(Value::Undefined);
      let len = array_like_length(vm, &receiver)?;
      if len == 0 {
        return Ok(Value::Bool(false));
      }
      let from_index = args.get(2).cloned().unwrap_or(Value::Number(0.0));
      let from_index = vm.to_number_value(&from_index)?;
      let mut k = match from_index {
        n if n.is_nan() || n == f64::NEG_INFINITY => 0usize,
        n if n == f64::INFINITY => return Ok(Value::Bool(false)),
        n if n >= 0.0 => n.trunc().to_usize().unwrap_or(len),
        n => len.saturating_sub(n.abs().trunc().to_usize().unwrap_or(len)),
      };
      while k < len {
        let key = PKey::String(k.to_string().into());
        let element = vm.get_property_host(&receiver, &key)?;
        if same_value_zero(&element, &search) {
          return Ok(Value::Bool(true));
        }
        k += 1;
      }
      Ok(Value::Bool(false))
    }),
    &function_proto_map,
  );
  set_builtin_name(&array_includes, "includes");
  set_builtin_length(&array_includes, 1.0);
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("includes".to_string().into()),
    PropertyDescriptor::builtin_data(array_includes),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("indexOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let search = args.get(1).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let from_index = args.get(2).cloned().unwrap_or(Value::Number(0.0));
        let mut k = match vm.to_number_value(&from_index)? {
          n if n.is_nan() => 0isize,
          n if n >= 0.0 => n.trunc() as isize,
          n => len as isize + n.trunc() as isize,
        };
        if k < 0 {
          k = 0;
        }
        while (k as usize) < len {
          let key = PKey::String((k as usize).to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let element = vm.get_property_host(&receiver, &key)?;
            if strict_equal(&element, &search) {
              return Ok(Value::Number(usize_to_f64(k as usize)));
            }
          }
          k += 1;
        }
        Ok(Value::Number(-1.0))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("slice".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_for_slice = Rc::clone(&array_proto_map);
        move |vm, args| {
          fn is_array_value(value: &Value) -> bool {
            match value {
              Value::Array(..) => true,
              Value::Proxy(proxy) => {
                let target = {
                  let proxy = proxy.borrow();
                  if proxy.is_revoked() {
                    return false;
                  }
                  proxy.target.clone()
                };
                is_array_value(&target)
              }
              _ => false,
            }
          }

          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let start_raw = vm.to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?;
          let start = if start_raw.is_nan() {
            0usize
          } else if start_raw < 0.0 {
            let back = start_raw.trunc().abs().to_usize().unwrap_or(len);
            len.saturating_sub(back)
          } else {
            start_raw
              .trunc()
              .to_usize()
              .unwrap_or(len)
              .min(len)
          };
          let end = match args.get(2) {
            None | Some(Value::Undefined) => len,
            Some(end_arg) => {
              let end_raw = vm.to_number_value(end_arg)?;
              if end_raw.is_nan() {
                0usize
              } else if end_raw < 0.0 {
                let back = end_raw.trunc().abs().to_usize().unwrap_or(len);
                len.saturating_sub(back)
              } else {
                end_raw.trunc().to_usize().unwrap_or(len).min(len)
              }
            }
          };
          let end = end.max(start);
          let slice_len = end.saturating_sub(start);
          if slice_len > u32::MAX as usize {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          let array_ctor = vm.get_global("Array").unwrap_or(Value::Undefined);
          let species_ctor = if is_array_value(&receiver) {
            let ctor =
              vm.get_property_host(&receiver, &PKey::String("constructor".to_string().into()))?;
            match ctor {
              Value::Undefined | Value::Null => array_ctor,
              v if !matches!(
                v,
                Value::Object(..)
                  | Value::Array(..)
                  | Value::Closure { .. }
                  | Value::BuiltinObject(_, _)
                  | Value::Proxy(_)
              ) =>
              {
                return Err(vm.throw_type_error("constructor property is not an object"));
              }
              ctor_obj => {
                let species =
                  vm.get_property_host(&ctor_obj, &PKey::Symbol(SymbolId::well_known_species()))?;
                match species {
                  Value::Undefined | Value::Null => array_ctor,
                  _ if !species.is_constructable() => {
                    return Err(
                      vm.throw_type_error("constructor[Symbol.species] is not a constructor"),
                    );
                  }
                  _ => species,
                }
              }
            }
          } else {
            array_ctor
          };
          let result =
            vm.construct_value(&species_ctor, &[Value::Number(usize_to_f64(slice_len))])?;
          if let Ok(proto) =
            vm.get_property_host(&species_ctor, &PKey::String("prototype".to_string().into()))
            && matches!(
              proto,
              Value::Object(..)
                | Value::Array(..)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
            )
          {
            match &result {
              Value::Object(data) => {
                data.borrow_mut().set_prototype_value(proto);
              }
              Value::Array(_, props) => {
                props.borrow_mut().set_prototype_value(proto);
              }
              Value::Closure { props, .. } => {
                props.borrow_mut().set_prototype_value(proto);
              }
              Value::BuiltinObject(_, props) => {
                props.borrow_mut().set_prototype_value(proto);
              }
              _ => {}
            }
          };
          for i in start..end {
            let key = PKey::String(i.to_string().into());
            let out_key = Value::String((i - start).to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              let item = vm.get_property_host(&receiver, &key)?;
              vm.set_property_host(&result, &out_key, item)?;
            } else {
              let _ =
                vm.delete_property_host(&result, &PKey::String((i - start).to_string().into()))?;
            }
          }
          vm.set_property_host(
            &result,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(slice_len)),
          )?;
          if let Value::Array(_, props) = &result {
            props
              .borrow_mut()
              .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_slice)));
          }
          Ok(result)
        }
      }),
      &function_proto_map,
    )),
  );
  // Array.prototype.reduce
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("reduce".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);

        let len = array_like_length(vm, &receiver)?;

        let mut i = 0;
        let mut accumulator = if args.len() > 2 {
          args[2].clone()
        } else {
          let mut first = None;
          while i < len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              first = Some(vm.get_property_host(&receiver, &key)?);
              i += 1;
              break;
            }
            i += 1;
          }
          match first {
            Some(value) => value,
            None => {
              return Err(vm.throw_type_error("Reduce of empty array with no initial value"));
            }
          }
        };

        while i < len {
          let key = PKey::String(i.to_string().into());
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            accumulator = vm.call_value_with_this(
              &callback,
              &Value::Undefined,
              &[
                accumulator,
                item,
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
          }
          i += 1;
        }
        Ok(accumulator)
      }),
      &function_proto_map,
    )),
  );
  // Array.prototype.flat
  let array_proto_for_flat = Rc::clone(&array_proto_map);
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("flat".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let depth = if let Some(d) = args.get(1) {
          if matches!(d, Value::Undefined) {
            1
          } else {
            to_length(vm, d)?
          }
        } else {
          1
        };

        let len = array_like_length(vm, &receiver)?;
        let mut out: Vec<Option<Value>> = Vec::new();

        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            if depth > 0 {
              // Check if item is an array
              if matches!(item, Value::Array(_, _)) {
                let item_len_val =
                  vm.get_property_host(&item, &PKey::String("length".to_string().into()))?;
                let item_len = to_length(vm, &item_len_val)?;
                for j in 0..item_len {
                  let j_key = PKey::String(j.to_string().into());
                  let has_j = vm.has_property_host(&item, &j_key)?;
                  if has_j {
                    let sub_item = vm.get_property_host(&item, &j_key)?;
                    // Recursive flat for depth > 1 would need more work
                    out.push(Some(sub_item));
                  }
                }
              } else {
                out.push(Some(item));
              }
            } else {
              out.push(Some(item));
            }
          }
        }

        let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props
          .borrow_mut()
          .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_flat)));
        props.borrow_mut().properties.insert(
          PKey::String("length".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
        );
        Ok(Value::Array(arr_rc, props))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("copyWithin".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let to = clamp_relative_index(
          to_integer_or_infinity(vm, args.get(1).unwrap_or(&Value::Undefined))?,
          len,
        );
        let from = clamp_relative_index(
          to_integer_or_infinity(vm, args.get(2).unwrap_or(&Value::Undefined))?,
          len,
        );
        let final_ = if let Some(end_arg) = args.get(3) {
          clamp_relative_index(to_integer_or_infinity(vm, end_arg)?, len)
        } else {
          len
        };
        if let Value::Array(arr, _) = &receiver
          && arr
            .borrow_mut()
            .copy_within_dense_numbers(to, from, final_)
        {
          return Ok(receiver);
        }
        let count = final_
          .saturating_sub(from)
          .min(len.saturating_sub(to));
        let mut snapshot: Vec<Option<Value>> = Vec::with_capacity(count);
        for i in 0..count {
          let key = PKey::String((from + i).to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            snapshot.push(Some(vm.get_property_host(&receiver, &key)?));
          } else {
            snapshot.push(None);
          }
        }
        for (i, slot) in snapshot.into_iter().enumerate() {
          let key = PKey::String((to + i).to_string().into());
          let key_value = Value::String((to + i).to_string().into());
          if let Some(value) = slot {
            vm.set_property_host(&receiver, &key_value, value)?;
          } else {
            let _ = vm.delete_property_host(&receiver, &key)?;
          }
        }
        Ok(receiver)
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("every".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            let keep = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if !keep.is_truthy() {
              return Ok(Value::Bool(false));
            }
          }
        }
        Ok(Value::Bool(true))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("fill".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let fill_value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let start = clamp_relative_index(
          to_integer_or_infinity(vm, args.get(2).unwrap_or(&Value::Undefined))?,
          len,
        );
        let end = if let Some(end_arg) = args.get(3) {
          clamp_relative_index(to_integer_or_infinity(vm, end_arg)?, len)
        } else {
          len
        };
        if let Value::Array(arr, _) = &receiver
          && let Value::Number(n) = &fill_value
          && arr
            .borrow_mut()
            .fill_dense_numbers(start, end, *n)
        {
          return Ok(receiver);
        }
        for i in start..end {
          let key = Value::String(i.to_string().into());
          vm.set_property_host(&receiver, &key, fill_value.clone())?;
        }
        Ok(receiver)
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("findIndex".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            let found = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[
                item.clone(),
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
            if found.is_truthy() {
              return Ok(Value::Number(usize_to_f64(i)));
            }
          }
        }
        Ok(Value::Number(-1.0))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("findLast".to_string().into()),
    PropertyDescriptor::builtin_data({
      let find_last_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let receiver = vm.to_object_value(this)?;
          let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          if !Vm::is_callable_value(&callback) {
            return Err(vm.throw_type_error("Array.prototype.findLast callback is not callable"));
          }
          let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
          let mut i = len;
          while i > 0 {
            i -= 1;
            let key = PKey::String(i.to_string().into());
            let item = vm.get_property_host(&receiver, &key)?;
            let found = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[
                item.clone(),
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
            if found.is_truthy() {
              return Ok(item);
            }
          }
          Ok(Value::Undefined)
        }),
        &function_proto_map,
      );
      set_builtin_name(&find_last_fn, "findLast");
      set_builtin_length(&find_last_fn, 1.0);
      find_last_fn
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("findLastIndex".to_string().into()),
    PropertyDescriptor::builtin_data({
      let find_last_index_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let receiver = vm.to_object_value(this)?;
          let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          if !Vm::is_callable_value(&callback) {
            return Err(
              vm.throw_type_error("Array.prototype.findLastIndex callback is not callable"),
            );
          }
          let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
          let mut i = len;
          while i > 0 {
            i -= 1;
            let key = PKey::String(i.to_string().into());
            let item = vm.get_property_host(&receiver, &key)?;
            let found = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if found.is_truthy() {
              return Ok(Value::Number(usize_to_f64(i)));
            }
          }
          Ok(Value::Number(-1.0))
        }),
        &function_proto_map,
      );
      set_builtin_name(&find_last_index_fn, "findLastIndex");
      set_builtin_length(&find_last_index_fn, 1.0);
      find_last_index_fn
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("flatMap".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
          let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let mut out: Vec<Option<Value>> = Vec::new();
          for i in 0..len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              let item = vm.get_property_host(&receiver, &key)?;
              let mapped = vm.call_value_with_this(
                &callback,
                &this_arg,
                &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
              )?;
              match mapped {
                Value::Array(arr, _) => {
                  for value in arr.borrow().iter().flatten() {
                    out.push(Some(value.clone()));
                  }
                }
                other => out.push(Some(other)),
              }
            }
          }
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("pop".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        if len == 0 {
          return Ok(Value::Undefined);
        }
        if !array_length_is_writable(&receiver) {
          return Err(vm.throw_type_error("Array length is not writable"));
        }
        let index = len - 1;
        let key = PKey::String(index.to_string().into());
        let result = if vm.has_property_host(&receiver, &key)? {
          vm.get_property_host(&receiver, &key)?
        } else {
          Value::Undefined
        };
        let _ = vm.delete_property_host(&receiver, &key)?;
        vm.set_property_host(
          &receiver,
          &Value::String("length".to_string().into()),
          Value::Number(usize_to_f64(index)),
        )?;
        Ok(result)
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("reduceRight".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let mut i = len;
        let mut accumulator = if args.len() > 2 {
          args[2].clone()
        } else {
          let mut found = None;
          while i > 0 {
            i -= 1;
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              found = Some(vm.get_property_host(&receiver, &key)?);
              break;
            }
          }
          match found {
            Some(value) => value,
            None => {
              return Err(vm.throw_type_error("Reduce of empty array with no initial value"));
            }
          }
        };

        while i > 0 {
          i -= 1;
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            accumulator = vm.call_value_with_this(
              &callback,
              &Value::Undefined,
              &[
                accumulator,
                item,
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
          }
        }
        Ok(accumulator)
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("reverse".to_string().into()),
    PropertyDescriptor::builtin_data({
      let reverse_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let receiver = vm.to_object_value(this)?;
          let len = array_like_length(vm, &receiver)?;
          let half = len / 2;
          for lower in 0..half {
            let upper = len - 1 - lower;
            let lower_key = PKey::String(lower.to_string().into());
            let upper_key = PKey::String(upper.to_string().into());
            let lower_key_value = Value::String(lower.to_string().into());
            let upper_key_value = Value::String(upper.to_string().into());
            let lower_exists = vm.has_property_host(&receiver, &lower_key)?;
            let upper_exists = vm.has_property_host(&receiver, &upper_key)?;
            let lower_value = if lower_exists {
              Some(vm.get_property_host(&receiver, &lower_key)?)
            } else {
              None
            };
            let upper_value = if upper_exists {
              Some(vm.get_property_host(&receiver, &upper_key)?)
            } else {
              None
            };
            let lower_still_exists = lower_exists && vm.has_property_host(&receiver, &lower_key)?;
            let upper_still_exists = upper_exists && vm.has_property_host(&receiver, &upper_key)?;

            match (
              lower_still_exists
                .then_some(lower_value)
                .flatten(),
              upper_still_exists
                .then_some(upper_value)
                .flatten(),
            ) {
              (Some(low), Some(high)) => {
                vm.set_property_host(&receiver, &lower_key_value, high)?;
                vm.set_property_host(&receiver, &upper_key_value, low)?;
              }
              (Some(low), None) => {
                let _ = vm.delete_property_host(&receiver, &lower_key)?;
                if let Value::Array(arr, props) = &receiver {
                  // Keep the dense store consistent with a length mutation
                  // observed while reading the opposite property.
                  if lower < arr.borrow().len() {
                    arr.borrow_mut()[lower] = None;
                  }
                  props
                    .borrow_mut()
                    .properties
                    .shift_remove(&lower_key);
                }
                vm.set_property_host(&receiver, &upper_key_value, low)?;
              }
              (None, Some(high)) => {
                let _ = vm.delete_property_host(&receiver, &upper_key)?;
                if let Value::Array(arr, props) = &receiver {
                  if upper < arr.borrow().len() {
                    arr.borrow_mut()[upper] = None;
                  }
                  props
                    .borrow_mut()
                    .properties
                    .shift_remove(&upper_key);
                }
                vm.set_property_host(&receiver, &lower_key_value, high)?;
              }
              (None, None) => {}
            }
          }
          Ok(receiver)
        }),
        &function_proto_map,
      );
      set_builtin_name(&reverse_fn, "reverse");
      set_builtin_length(&reverse_fn, 0.0);
      reverse_fn
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("some".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            let keep = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if keep.is_truthy() {
              return Ok(Value::Bool(true));
            }
          }
        }
        Ok(Value::Bool(false))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toLocaleString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let locale_args = args.get(1..).unwrap_or(&[]);
        let mut out = js_string::JsStringBuilder::new();
        for i in 0..len {
          if i > 0 {
            out.push_code_unit(u16::from(b','));
          }
          let key = PKey::String(i.to_string().into());
          if !vm.has_property_host(&receiver, &key)? {
            continue;
          }
          let item = vm.get_property_host(&receiver, &key)?;
          match item {
            Value::Undefined | Value::Null => {}
            _ => {
              let to_locale =
                vm.get_property_host(&item, &PKey::String("toLocaleString".to_string().into()))?;
              if Vm::is_callable_value(&to_locale) {
                let rendered = vm.call_value_with_this(&to_locale, &item, locale_args)?;
                out.push_string(&vm.to_string(&rendered)?);
              } else {
                out.push_string(&vm.to_string(&item)?);
              }
            }
          }
        }
        Ok(Value::String(out.finish()))
      }),
      &function_proto_map,
    )),
  );
  let array_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_proto_map)),
  )));
  array_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&array_proto_map))),
  );

  let error_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&object_proto_map)),
  )));
  let error_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_proto_map)),
  )));
  error_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&error_proto_map))),
  );

  let aggregate_error_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&error_proto_map))),
  ));
  let aggregate_error_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  aggregate_error_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&aggregate_error_proto_map))),
    );

  let function_ctor_globals = Rc::clone(globals);
  let function_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let off = if vm.current_new_target().is_some() {
      1
    } else {
      0
    };
    let user_args = args.get(off..).unwrap_or(&[]);
    let body_src = if let Some(last) = user_args.last() {
      vm.to_string(last)?
    } else {
      js_string::JsString::empty()
    };
    let param_count = user_args.len().saturating_sub(1);
    let mut params = Vec::with_capacity(param_count);
    for idx in 0..param_count {
      params.push(vm.to_string(&user_args[idx])?);
    }
    let source = crate::js!(
      format(script),
      "(function __tsblitz_dynamic({}) {{\n{}\n}})",
      params
        .iter()
        .map(js_string::JsString::as_str_lossy)
        .collect::<Vec<_>>()
        .join(","),
      body_src
    );
    let chunk = crate::typecheck::with_strict_override(None, || {
      let session =
        Parser::parse_source(&source).map_err(|e| syntax_error(vm, format!("parse: {}", e)))?;
      let prog = session.program();
      compiler::compile_oxc_repl(prog, &source)
        .map_err(|e| syntax_error(vm, format!("compile: {}", e)))
    })?;
    let mut child = vm.fork_execution();
    let result = child.run(chunk)?;
    match &result {
      Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        props.borrow_mut().properties.insert(
          PKey::String("[[RealmGlobals]]".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&function_ctor_globals))),
        );
      }
      _ => {}
    }
    Ok(result)
  });
  let function_ctor_val = Value::BuiltinObject(function_ctor_fn, function_ctor_map);
  let function_call_val = builtin_object(
    Rc::new(|vm, args| {
      let callee = args.first().cloned().unwrap_or(Value::Undefined);
      let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
      let rest = args.get(2..).unwrap_or(&[]);
      vm.call_value_with_this(&callee, &this_arg, rest)
    }),
    &function_proto_map,
  );
  function_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(function_ctor_val.clone()),
  );
  function_proto_map.borrow_mut().properties.insert(
    PKey::String("call".to_string().into()),
    PropertyDescriptor::builtin_data(function_call_val),
  );
  function_proto_map.borrow_mut().properties.insert(
    PKey::String("apply".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let callee = args.first().cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callee) {
          return Err(vm.throw_type_error("Function.prototype.apply called on non-callable"));
        }

        let call_args = match args.get(2) {
          None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
          Some(arguments_list) => create_list_from_array_like(vm, arguments_list)?,
        };
        vm.call_value_with_this(&callee, &this_arg, &call_args)
      }),
      &function_proto_map,
    )),
  );
  function_proto_map.borrow_mut().properties.insert(
    PKey::String("bind".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let function_proto_map = Rc::clone(&function_proto_map);
        move |_vm, args| {
          let target = args.first().cloned().unwrap_or(Value::Undefined);
          let bound_this = args.get(1).cloned().unwrap_or(Value::Undefined);
          let bound_args: Vec<Value> = args.iter().skip(2).cloned().collect();
          if crate::trace::enabled() {
            eprintln!(
              "[trace] bind create target={:?} bound_this={:?} bound_args={:?}",
              target, bound_this, bound_args
            );
          }
          let bound_target = target.clone();
          let bound_this_arg = bound_this.clone();
          let prefix_args = bound_args.clone();
          let bound_fn = builtin_object(
            Rc::new(move |vm, call_args| {
              if crate::trace::enabled() {
                eprintln!(
                  "[trace] bound fn call target={:?} this={:?} args={:?}",
                  bound_target, bound_this_arg, call_args
                );
              }
              let mut combined = prefix_args.clone();
              if vm.builtin_this_arg_prepended() {
                combined.extend(call_args.iter().skip(1).cloned());
              } else {
                combined.extend(call_args.iter().cloned());
              }
              vm.call_value_with_this(&bound_target, &bound_this_arg, &combined)
            }),
            &function_proto_map,
          );
          if let Value::BuiltinObject(_, props) = &bound_fn {
            props.borrow_mut().properties.insert(
              PKey::String("[[BoundTargetFunction]]".to_string().into()),
              PropertyDescriptor::ordinary_data(target),
            );
            props.borrow_mut().properties.insert(
              PKey::String("[[BoundThis]]".to_string().into()),
              PropertyDescriptor::ordinary_data(bound_this),
            );
            props.borrow_mut().properties.insert(
              PKey::String("[[BoundArguments]]".to_string().into()),
              PropertyDescriptor::ordinary_data(array_from_values(bound_args)),
            );
          }
          Ok(bound_fn)
        }
      }),
      &function_proto_map,
    )),
  );

  function_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let source = if Vm::is_callable_value(&this) {
          vm.function_to_string(&this)?
        } else {
          // Be forgiving for harness message formatting. Test262's
          // assert helpers stringify a few function-like prototype
          // objects, and this runtime previously surfaced those as a
          // primitive-string throw. Returning an object-tag string
          // keeps error reporting stable without affecting callable
          // functions.
          format!("[object {}]", this.object_tag())
        };
        Ok(Value::String(source.into()))
      }),
      &function_proto_map,
    )),
  );

  let object_ctor_props = Rc::clone(&object_ctor_map);
  let object_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let value_idx = if args.len() > 1 { 1 } else { 0 };
    let value = args
      .get(value_idx)
      .cloned()
      .unwrap_or(Value::Undefined);
    match value {
      Value::Null | Value::Undefined => {
        let proto = object_ctor_props
          .borrow()
          .properties
          .get(&PKey::String("prototype".to_string().into()))
          .and_then(|d| match d {
            PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            _ => None,
          })
          .unwrap_or(Value::Null);
        let mut inst = ObjectData::new();
        inst.set_prototype_value(proto);
        vm.clear_property_get_ic_cache();
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
      }
      _ => vm.to_object_value(value),
    }
  });
  let object_ctor_val =
    Value::BuiltinObject(Rc::clone(&object_ctor_fn), Rc::clone(&object_ctor_map));

  object_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(object_ctor_val.clone()),
  );

  // 27.2 Promise Objects
  let (_promise_proto_map, _promise_ctor_map, promise_ctor_val) =
    builtins::promise::create_promise_intrinsics(&object_proto_map, &function_proto_map);

  // Reflect object and methods
  let reflect_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&object_proto_map),
  ))));
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("apply".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let this_arg = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let arguments_list = args
          .get(off + 2)
          .cloned()
          .unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&target) {
          return Err(vm.throw_type_error("Reflect.apply: target is not callable"));
        }
        let call_args = create_list_from_array_like(vm, &arguments_list)?;
        vm.call_value_with_this(&target, &this_arg, &call_args)
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("construct".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let arg_list = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let new_target = args
          .get(off + 2)
          .cloned()
          .unwrap_or_else(|| target.clone());

        let ctor = if matches!(new_target, Value::Undefined) {
          &target
        } else {
          &new_target
        };

        if !target.is_constructable() {
          return Err(vm.throw_type_error("Reflect.construct: target is not a constructor"));
        }
        if !ctor.is_constructable() {
          return Err(vm.throw_type_error("Reflect.construct: newTarget is not a constructor"));
        }

        let args_vec: Vec<Value> = match arg_list {
          Value::Array(arr, _) => arr
            .borrow()
            .iter()
            .filter_map(Clone::clone)
            .collect(),
          Value::Undefined | Value::Null => Vec::new(),
          _ => Vec::new(),
        };

        let result = vm.construct_value_with_new_target(&target, &args_vec, new_target.clone())?;
        if matches!(new_target, Value::Undefined) {
          return Ok(result);
        }

        let new_target_proto = vm
          .get_property_host(
            &new_target,
            &PropertyKey::String("prototype".to_string().into()),
          )
          .unwrap_or(Value::Undefined);
        let mut desired_proto = match new_target_proto {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _) => Some(new_target_proto),
          _ => None,
        };

        if desired_proto.is_none() {
          let ctor_name = vm
            .get_property_host(&target, &PropertyKey::String("name".to_string().into()))
            .ok()
            .and_then(|v| match v {
              Value::String(s) => Some(s),
              _ => None,
            })
            .or_else(|| {
              if vm
                .realm()
                .intrinsics
                .string_constructor
                .as_ref()
                .is_some_and(|v| strict_equal(v, &target))
              {
                Some("String".to_string().into())
              } else if vm
                .realm()
                .intrinsics
                .array_constructor
                .as_ref()
                .is_some_and(|v| strict_equal(v, &target))
              {
                Some("Array".to_string().into())
              } else if vm
                .realm()
                .intrinsics
                .object_constructor
                .as_ref()
                .is_some_and(|v| strict_equal(v, &target))
              {
                Some("Object".to_string().into())
              } else if vm
                .realm()
                .intrinsics
                .promise_constructor
                .as_ref()
                .is_some_and(|v| strict_equal(v, &target))
              {
                Some("Promise".to_string().into())
              } else {
                None
              }
            });
          let realm_globals = vm
            .get_property_host(
              &new_target,
              &PropertyKey::String("[[RealmGlobals]]".to_string().into()),
            )
            .ok();
          if let (Some(name), Some(Value::Object(globals))) = (ctor_name, realm_globals) {
            let ctor_in_realm = globals
              .borrow()
              .properties
              .get(&PropertyKey::String(name))
              .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
              });
            if let Some(ctor_in_realm) = ctor_in_realm {
              let proto = vm
                .get_property_host(
                  &ctor_in_realm,
                  &PropertyKey::String("prototype".to_string().into()),
                )
                .unwrap_or(Value::Undefined);
              if matches!(
                proto,
                Value::Object(_)
                  | Value::Array(..)
                  | Value::Closure { .. }
                  | Value::BuiltinObject(_, _)
              ) {
                desired_proto = Some(proto);
              }
            }
          }
        }

        if let Some(proto_value) = desired_proto
          && let Value::Object(obj) = &result
        {
          obj.borrow_mut().set_prototype_value(proto_value);
        }

        Ok(result)
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("ownKeys".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let target = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !target.is_object() {
          return Err(vm.throw_type_error("Reflect.ownKeys called on non-object"));
        }
        let keys = vm
          .own_property_keys_value(&target)?
          .into_iter()
          .map(|key| property_key_to_value(&key))
          .collect::<Vec<_>>();
        Ok(array_from_values(keys))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("deleteProperty".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let key = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let key = vm.to_property_key(&key)?;
        Ok(Value::Bool(vm.delete_property_host(&target, &key)?))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("defineProperty".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let key = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let desc = args
          .get(off + 2)
          .cloned()
          .unwrap_or(Value::Undefined);
        let ok = vm.define_property_value(&target, &key, &desc)?;
        Ok(Value::Bool(ok))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("get".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let key = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let _receiver = args
          .get(off + 2)
          .cloned()
          .unwrap_or_else(|| target.clone());
        let key = vm.to_property_key(&key)?;
        vm.get_property_host(&target, &key)
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("getOwnPropertyDescriptor".to_string().into()),
    PropertyDescriptor::builtin_data({
      let object_proto_map = Rc::clone(&object_proto_map);
      builtin_object(
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Reflect");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let key = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          let key = vm.to_property_key(&key)?;
          let Some(desc) = vm.get_own_property_value(&target, &key)? else {
            return Ok(Value::Undefined);
          };
          Ok(descriptor_value_from_descriptor(&object_proto_map, &desc))
        }),
        &function_proto_map,
      )
    }),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("getPrototypeOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        if !target.is_object() {
          return Err(vm.throw_type_error("Reflect.getPrototypeOf called on non-object"));
        }
        vm.get_prototype_of_value(&target)
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("has".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let key = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let key = vm.to_property_key(&key)?;
        Ok(Value::Bool(vm.has_property_host(&target, &key)?))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("isExtensible".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        if !target.is_object() {
          return Err(vm.throw_type_error("Reflect.isExtensible called on non-object"));
        }
        Ok(Value::Bool(vm.is_extensible_value(&target)?))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("preventExtensions".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        if !target.is_object() {
          return Err(vm.throw_type_error("Reflect.preventExtensions called on non-object"));
        }
        Ok(Value::Bool(vm.prevent_extensions_value(&target)?))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("setPrototypeOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let proto = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        if !target.is_object() {
          return Err(vm.throw_type_error("Reflect.setPrototypeOf called on non-object"));
        }
        if !matches!(proto, Value::Null) && !proto.is_object() {
          return Err(
            vm.throw_type_error("Reflect.setPrototypeOf prototype must be object or null"),
          );
        }
        Ok(Value::Bool(vm.set_prototype_of_value(&target, &proto)?))
      }),
      &function_proto_map,
    )),
  );
  reflect_obj.borrow_mut().properties.insert(
    PKey::String("set".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Reflect");
        let target = args.get(off).cloned().unwrap_or(Value::Undefined);
        let key = args
          .get(off + 1)
          .cloned()
          .unwrap_or(Value::Undefined);
        let value = args
          .get(off + 2)
          .cloned()
          .unwrap_or(Value::Undefined);
        let receiver = args
          .get(off + 3)
          .cloned()
          .unwrap_or_else(|| target.clone());
        let key = property_key_to_value(&vm.to_property_key(&key)?);
        Ok(Value::Bool(vm.reflect_set_property_host(
          &target,
          &key,
          value,
          Some(receiver),
        )?))
      }),
      &function_proto_map,
    )),
  );

  // Date constructor

  let date_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&object_proto_map)),
  )));
  let date_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_proto_map)),
  )));
  let date_ctor_fn: BuiltinFn = {
    let date_proto_map_rc = Rc::clone(&date_proto_map);
    Rc::new(move |_vm, args| {
      use std::time::{SystemTime, UNIX_EPOCH};

      // Helper to convert value to number, returning NaN if not a number
      fn to_num(v: &Value) -> f64 {
        match v {
          Value::Number(n) => *n,
          Value::Undefined => f64::NAN,
          Value::Null => 0.0,
          Value::Bool(b) => {
            if *b {
              1.0
            } else {
              0.0
            }
          }
          Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
          _ => f64::NAN,
        }
      }

      // Simplified Unix timestamp calculation for date components
      fn date_to_timestamp(
        year: i32,
        month: i32,
        day: i32,
        hour: i32,
        min: i32,
        sec: i32,
        ms: i32,
      ) -> f64 {
        // month is 1-indexed (1=Jan, 12=Dec)
        // Simplified - assumes proleptic Gregorian calendar
        let is_leap = |y: i32| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;

        let mut total_days = 0i64;

        // Add days for complete years before this year
        for y in 1970..year {
          total_days += if is_leap(y) { 366 } else { 365 };
        }

        // Add days for complete months in this year
        let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for m in 1..month {
          let mut m_days = days_in_month[m.min(12).to_usize().unwrap_or(12)] as i64;
          if m == 2 && is_leap(year) {
            m_days += 1;
          }
          total_days += m_days;
        }

        // Add days in this month (day is 1-indexed)
        total_days += i64::from(day - 1);

        // Convert to milliseconds
        let total_secs =
          total_days * 86400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
        total_secs.to_f64().unwrap_or(f64::NAN) * 1000.0 + f64::from(ms)
      }

      let timestamp = match args.len() {
        1 => {
          // Only receiver, no user arguments: current time
          match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_millis().to_f64().unwrap_or(f64::NAN),
            Err(_) => 0.0,
          }
        }
        2 => {
          // Receiver + single argument: numeric timestamp or numeric string
          if let Some(arg) = args.get(1) {
            to_num(arg)
          } else {
            0.0
          }
        }
        _ => {
          // Multiple user arguments: year, month [, day, hour, minute, second, millisecond]
          let year = to_num(args.get(1).unwrap_or(&Value::Undefined))
            .to_i32()
            .unwrap_or(0);
          let month = to_num(args.get(2).unwrap_or(&Value::Undefined))
            .to_i32()
            .unwrap_or(0); // 0-indexed
          let day = to_num(args.get(3).unwrap_or(&Value::Number(1.0)))
            .to_i32()
            .unwrap_or(1);
          let hour = to_num(args.get(4).unwrap_or(&Value::Number(0.0)))
            .to_i32()
            .unwrap_or(0);
          let min = to_num(args.get(5).unwrap_or(&Value::Number(0.0)))
            .to_i32()
            .unwrap_or(0);
          let sec = to_num(args.get(6).unwrap_or(&Value::Number(0.0)))
            .to_i32()
            .unwrap_or(0);
          let ms = to_num(args.get(7).unwrap_or(&Value::Number(0.0)))
            .to_i32()
            .unwrap_or(0);

          date_to_timestamp(year, month + 1, day, hour, min, sec, ms)
        }
      };

      let mut date_obj_data =
        ObjectData::with_prototype(Value::Object(Rc::clone(&date_proto_map_rc)));
      date_obj_data.properties.insert(
        PropertyKey::String("[[PrimitiveValue]]".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(timestamp)),
      );
      let date_obj = Rc::new(RefCell::new(date_obj_data));
      Ok(Value::Object(date_obj))
    })
  };
  let date_ctor_val = Value::BuiltinObject(Rc::clone(&date_ctor_fn), Rc::clone(&date_ctor_map));
  date_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&date_proto_map))),
  );
  date_ctor_map.borrow_mut().properties.insert(
    PKey::String("now".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, _args| {
        let millis = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map_or(f64::NAN, |duration| duration.as_secs_f64() * 1_000.0);
        Ok(Value::Number(millis))
      }),
      &function_proto_map,
    )),
  );
  date_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(date_ctor_val.clone()),
  );
  date_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let res = if let Value::Object(obj) = this
          && let Some(PropertyDescriptor::Data { value, .. }) = obj.borrow().properties.get(
            &PropertyKey::String("[[PrimitiveValue]]".to_string().into()),
          ) {
          value.clone()
        } else {
          Value::Number(f64::NAN)
        };
        Ok(res)
      }),
      &function_proto_map,
    )),
  );
  date_proto_map.borrow_mut().properties.insert(
    PKey::String("getTime".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        if let Value::Object(obj) = this
          && let Some(PropertyDescriptor::Data { value, .. }) = obj.borrow().properties.get(
            &PropertyKey::String("[[PrimitiveValue]]".to_string().into()),
          )
        {
          return Ok(value.clone());
        }
        Ok(Value::Number(f64::NAN))
      }),
      &function_proto_map,
    )),
  );
  date_proto_map.borrow_mut().properties.insert(
    PKey::String("getTimezoneOffset".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, _args| {
        // Return UTC offset in minutes (simplified - assuming UTC for now)
        Ok(Value::Number(0.0))
      }),
      &function_proto_map,
    )),
  );
  date_proto_map.borrow_mut().properties.insert(
    PKey::String("toISOString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        if let Value::Object(obj) = this
          && let Some(PropertyDescriptor::Data { value, .. }) = obj.borrow().properties.get(
            &PropertyKey::String("[[PrimitiveValue]]".to_string().into()),
          )
          && let Value::Number(n) = value
        {
          // Convert timestamp to ISO string (simplified)
          let millis = n.trunc().to_i64().unwrap_or_else(|| {
            if n.is_sign_negative() {
              i64::MIN
            } else {
              i64::MAX
            }
          });
          let secs = millis / 1000;
          let ms = millis % 1000;

          // Calculate date components (simplified)
          let year = 1970 + i32::try_from(secs / (365 * 24 * 3600)).unwrap_or(0);
          let day_of_year = i32::try_from((secs % (365 * 24 * 3600)) / (24 * 3600)).unwrap_or(0);
          let hour = i32::try_from((secs % (24 * 3600)) / 3600).unwrap_or(0);
          let minute = i32::try_from((secs % 3600) / 60).unwrap_or(0);
          let second = i32::try_from(secs % 60).unwrap_or(0);

          return Ok(Value::String(
            format!(
              "{:04}-01-{:02}T{:02}:{:02}:{:02}.{:03}Z",
              year,
              day_of_year.min(31).max(1),
              hour,
              minute,
              second,
              ms
            )
            .into(),
          ));
        }
        Ok(Value::String("Invalid Date".to_string().into()))
      }),
      &function_proto_map,
    )),
  );

  // ArrayBuffer constructor
  let arraybuffer_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let arraybuffer_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  let arraybuffer_ctor_fn: BuiltinFn = {
    let proto_map_rc = Rc::clone(&arraybuffer_proto_map);
    Rc::new(move |_vm, args| {
      // Read the requested buffer length; the length helper rejects non-finite
      // and oversized values.
      let instance_proto = _vm
        .current_new_target()
        .and_then(|new_target| {
          _vm
            .get_property_host(&new_target, &PKey::String("prototype".to_string().into()))
            .ok()
        })
        .and_then(|proto| match proto {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _) => Some(proto),
          _ => None,
        })
        .unwrap_or_else(|| Value::Object(Rc::clone(&proto_map_rc)));
      let length_value = args.get(1).cloned().unwrap_or(Value::Undefined);
      let length = to_length(_vm, &length_value)?;

      let (buffer, _) = new_array_buffer(instance_proto, length, false);
      Ok(buffer)
    })
  };
  let arraybuffer_ctor_val = Value::BuiltinObject(
    Rc::clone(&arraybuffer_ctor_fn),
    Rc::clone(&arraybuffer_ctor_map),
  );
  arraybuffer_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&arraybuffer_proto_map))),
    );
  arraybuffer_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(arraybuffer_ctor_val.clone()),
    );

  // SharedArrayBuffer constructor (minimal)
  let sharedarraybuffer_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let sharedarraybuffer_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  let sharedarraybuffer_ctor_fn: BuiltinFn = {
    let proto_map_rc = Rc::clone(&sharedarraybuffer_proto_map);
    Rc::new(move |vm, args| {
      let instance_proto = vm
        .current_new_target()
        .and_then(|new_target| {
          vm.get_property_host(&new_target, &PKey::String("prototype".to_string().into()))
            .ok()
        })
        .and_then(|proto| match proto {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _) => Some(proto),
          _ => None,
        })
        .unwrap_or_else(|| Value::Object(Rc::clone(&proto_map_rc)));
      let length_value = args.get(1).cloned().unwrap_or(Value::Undefined);
      let length = to_length(vm, &length_value)?;

      let (buffer, _) = new_array_buffer(instance_proto, length, true);
      Ok(buffer)
    })
  };
  let sharedarraybuffer_ctor_val = Value::BuiltinObject(
    Rc::clone(&sharedarraybuffer_ctor_fn),
    Rc::clone(&sharedarraybuffer_ctor_map),
  );
  sharedarraybuffer_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&sharedarraybuffer_proto_map))),
    );
  sharedarraybuffer_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(sharedarraybuffer_ctor_val.clone()),
    );

  // %TypedArray% intrinsic constructor and TypedArray constructors
  let typedarray_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let typedarray_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  typedarray_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&typedarray_proto_map))),
    );
  typedarray_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_metadata(Value::String("TypedArray".to_string().into())),
    );
  let typedarray_ctor_fn: BuiltinFn = Rc::new(move |_vm, _args| {
    Err(RuntimeError::from(
      "TypeError: TypedArray constructor is not directly callable",
    ))
  });
  let typedarray_ctor_val = Value::BuiltinObject(typedarray_ctor_fn, typedarray_ctor_map);
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(typedarray_ctor_val.clone()),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("values".to_string().into()),
      PropertyDescriptor::builtin_data(array_values_builtin.clone()),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("keys".to_string().into()),
      PropertyDescriptor::builtin_data(array_keys_builtin),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("entries".to_string().into()),
      PropertyDescriptor::builtin_data(array_entries_builtin),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::Symbol(SymbolId::well_known_iterator()),
      PropertyDescriptor::builtin_data(array_values_builtin),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("fill".to_string().into()),
      PropertyDescriptor::builtin_data(builtin_object(
        Rc::new(|vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let fill_value = args.get(1).cloned().unwrap_or(Value::Undefined);
          let len =
            match vm.get_property_host(&receiver, &PKey::String("length".to_string().into())) {
              Ok(Value::Number(n)) if n.is_finite() && n > 0.0 => n.trunc().to_usize().unwrap_or(0),
              _ => 0,
            };
          for i in 0..len {
            let key = Value::String(i.to_string().into());
            drop(vm.set_property_host(&receiver, &key, fill_value.clone())?);
          }
          Ok(receiver)
        }),
        &function_proto_map,
      )),
    );

  let subarray_builtin = {
    builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let (_, buffer, data, kind, length, byte_offset, prototype) = typed_view_details(&receiver)
          .filter(|(_, _, _, kind, _, _, _)| {
            matches!(
              kind.as_str(),
              "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
            )
          })
          .ok_or_else(|| {
            vm.throw_type_error("TypedArray.prototype.subarray called on incompatible receiver")
          })?;
        let begin_value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
        let begin = typed_array_relative_index(vm, &begin_value, length)?;
        let end_value = args
          .get(2)
          .cloned()
          .unwrap_or(Value::Number(usize_to_f64(length)));
        let end = typed_array_relative_index(vm, &end_value, length)?;
        let final_end = end.max(begin);
        let view = new_typed_array_view(
          prototype,
          buffer,
          data,
          &kind,
          final_end.saturating_sub(begin),
          byte_offset + begin,
        );
        Ok(view)
      }),
      &function_proto_map,
    )
  };
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("subarray".to_string().into()),
      PropertyDescriptor::builtin_data(subarray_builtin),
    );

  let slice_builtin = {
    let arraybuffer_proto_map = Rc::clone(&arraybuffer_proto_map);
    builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let (_, _, data, kind, length, byte_offset, prototype) = typed_view_details(&receiver)
          .filter(|(_, _, _, kind, _, _, _)| {
            matches!(
              kind.as_str(),
              "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
            )
          })
          .ok_or_else(|| {
            vm.throw_type_error("TypedArray.prototype.slice called on incompatible receiver")
          })?;
        let begin_value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
        let begin = typed_array_relative_index(vm, &begin_value, length)?;
        let end_value = args
          .get(2)
          .cloned()
          .unwrap_or(Value::Number(usize_to_f64(length)));
        let end = typed_array_relative_index(vm, &end_value, length)?;
        let copy_length = end.max(begin).saturating_sub(begin);
        let (buffer, new_data) = new_byte_buffer(&arraybuffer_proto_map, copy_length);
        for index in 0..copy_length {
          let value = data
            .borrow()
            .get(byte_offset + begin + index)
            .and_then(|slot| slot.clone())
            .unwrap_or(Value::Number(0.0));
          let byte = encode_byte(vm, &kind, &value)?;
          new_data.borrow_mut()[index] = Some(Value::Number(f64::from(byte)));
        }
        Ok(new_typed_array_view(
          prototype,
          buffer,
          new_data,
          &kind,
          copy_length,
          0,
        ))
      }),
      &function_proto_map,
    )
  };
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("slice".to_string().into()),
      PropertyDescriptor::builtin_data(slice_builtin),
    );

  let set_builtin = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let (_, _, _, _kind, target_length, _, _) = typed_view_details(&receiver)
        .filter(|(_, _, _, kind, _, _, _)| {
          matches!(
            kind.as_str(),
            "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
          )
        })
        .ok_or_else(|| {
          vm.throw_type_error("TypedArray.prototype.set called on incompatible receiver")
        })?;
      let source = args.get(1).cloned().unwrap_or(Value::Undefined);
      let offset_value = args.get(2).cloned().unwrap_or(Value::Number(0.0));
      let offset = to_index(vm, &offset_value)?;
      let source_length =
        vm.get_property_host(&source, &PKey::String("length".to_string().into()))?;
      let source_length = to_length(vm, &source_length)?;
      if offset.saturating_add(source_length) > target_length {
        return Err(vm.throw_range_error("Source is too large"));
      }
      let mut values = Vec::with_capacity(source_length);
      for index in 0..source_length {
        values.push(vm.get_property_host(
          &source,
          &PKey::String(usize_to_decimal_string(index).into()),
        )?);
      }
      for (index, value) in values.into_iter().enumerate() {
        vm.set_property_host(
          &receiver,
          &Value::String(usize_to_decimal_string(offset + index).into()),
          value,
        )?;
      }
      Ok(Value::Undefined)
    }),
    &function_proto_map,
  );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("set".to_string().into()),
      PropertyDescriptor::builtin_data(set_builtin),
    );

  macro_rules! create_typed_array_ctor {
    ($name:expr, $element_count:expr, $zero_value:expr, $shared:expr) => {{
      let proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(typedarray_proto_map.clone()),
      )));
      let ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        typedarray_ctor_val.clone(),
      )));
      let arraybuffer_proto_rc = Rc::clone(&arraybuffer_proto_map);
      let ctor_fn: BuiltinFn = {
        let proto_map_rc = proto_map.clone();
        let ctor_name = $name.to_string();
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, $name);
          let init = args.get(off).cloned().unwrap_or(Value::Undefined);
          let byte_offset_arg = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          let length_arg = args
            .get(off + 2)
            .cloned()
            .unwrap_or(Value::Undefined);
          let instance_proto = vm
            .current_new_target()
            .and_then(|new_target| {
              vm.get_property_host(&new_target, &PKey::String("prototype".to_string().into()))
                .ok()
            })
            .and_then(|proto| match proto {
              Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _) => Some(proto),
              _ => None,
            })
            .unwrap_or_else(|| Value::Object(proto_map_rc.clone()));
          let (length, initial_values, byte_offset, view_byte_length, mut buffer_slot): (
            usize,
            Vec<Value>,
            usize,
            usize,
            Value,
          ) = match &init {
            Value::Number(_) => {
              let length = to_length(vm, &init)?;
              let view_byte_length = length * $element_count;
              (
                length,
                Vec::<Value>::new(),
                0usize,
                view_byte_length,
                Value::Undefined,
              )
            }
            Value::String(s) => {
              let length = s.parse::<usize>().unwrap_or(0);
              let view_byte_length = length * $element_count;
              (
                length,
                Vec::<Value>::new(),
                0usize,
                view_byte_length,
                Value::Undefined,
              )
            }
            Value::Array(arr, _) => {
              let initial_values = arr
                .borrow()
                .iter()
                .map(|slot| slot.clone().unwrap_or(Value::Undefined))
                .collect::<Vec<Value>>();
              let length = initial_values.len();
              let view_byte_length = length * $element_count;
              (
                length,
                initial_values,
                0usize,
                view_byte_length,
                Value::Undefined,
              )
            }
            Value::Object(_) => {
              let buffer_value = init.clone();
              if is_array_buffer(&buffer_value) {
                let backing = array_buffer_backing(&buffer_value)
                  .ok_or_else(|| vm.throw_range_error("Invalid typed array length"))?;
                let buffer_byte_length = backing.borrow().len();
                let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
                  0
                } else {
                  to_index(vm, &byte_offset_arg)?
                };
                if !is_byte_offset_aligned(byte_offset, $element_count) {
                  return Err(vm.throw_range_error("Invalid typed array offset"));
                }
                let length_from_spec = if matches!(length_arg, Value::Undefined) {
                  if buffer_byte_length < byte_offset {
                    return Err(vm.throw_range_error("Invalid typed array length"));
                  }
                  (buffer_byte_length - byte_offset) / $element_count
                } else {
                  to_index(vm, &length_arg)?
                };
                let expected_byte_length = length_from_spec * $element_count;
                if byte_offset + expected_byte_length > buffer_byte_length {
                  return Err(vm.throw_range_error("Invalid typed array length"));
                }
                (
                  length_from_spec,
                  Vec::<Value>::new(),
                  byte_offset,
                  expected_byte_length,
                  buffer_value,
                )
              } else {
                let length_value = vm.get_property_host(
                  &buffer_value,
                  &PropertyKey::String("length".to_string().into()),
                )?;
                let length = to_length(vm, &length_value)?;
                let mut initial_values = Vec::with_capacity(length);
                for idx in 0..length {
                  initial_values.push(vm.get_property_host(
                    &buffer_value,
                    &PropertyKey::String(usize_to_decimal_string(idx).into()),
                  )?);
                }
                (
                  length,
                  initial_values,
                  0usize,
                  length * $element_count,
                  Value::Undefined,
                )
              }
            }
            _ => (0, Vec::<Value>::new(), 0usize, 0usize, Value::Undefined),
          };

          if $shared {
            let backing_data = if is_array_buffer(&buffer_slot) {
              array_buffer_backing(&buffer_slot)
                .ok_or_else(|| vm.throw_type_error("Invalid ArrayBuffer backing store"))?
            } else {
              let (buffer, data) = new_array_buffer(
                Value::Object(Rc::clone(&arraybuffer_proto_rc)),
                view_byte_length,
                false,
              );
              buffer_slot = buffer;
              data
            };
            for (idx, value) in initial_values.into_iter().enumerate() {
              let byte = encode_byte(vm, &ctor_name, &value)?;
              if let Some(slot) = backing_data
                .borrow_mut()
                .get_mut(byte_offset + idx)
              {
                *slot = Some(Value::Number(f64::from(byte)));
              }
            }
            return Ok(new_byte_view(
              instance_proto,
              buffer_slot,
              backing_data,
              &ctor_name,
              length,
              byte_offset,
            ));
          }
          let typed_data_value = Value::Array(
            Rc::new(RefCell::new(ArrayData::from(vec![
              Some($zero_value.clone());
              length
            ]))),
            Rc::new(RefCell::new(ObjectData::new())),
          );
          let mut typed_array_data = ObjectData::with_prototype(instance_proto);
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayData]]".to_string().into()),
            PropertyDescriptor::ordinary_data(typed_data_value),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayKind]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::String(ctor_name.clone().into())),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayBytesPerElement]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64($element_count))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayLength]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(length))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayByteLength]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(view_byte_length))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayByteOffset]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_offset))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayBuffer]]".to_string().into()),
            PropertyDescriptor::ordinary_data(buffer_slot.clone()),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("length".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(length))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("byteLength".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(view_byte_length))),
          );
          for (idx, value) in initial_values.into_iter().enumerate() {
            typed_array_data.properties.insert(
              PropertyKey::String(usize_to_decimal_string(idx).into()),
              PropertyDescriptor::ordinary_data(value),
            );
          }
          let typed_array_obj = Rc::new(RefCell::new(typed_array_data));
          Ok(Value::Object(typed_array_obj))
        })
      };
      let ctor_val = Value::BuiltinObject(ctor_fn.clone(), ctor_map.clone());
      ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string().into()),
        PropertyDescriptor::builtin_prototype(Value::Object(proto_map.clone())),
      );
      ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string().into()),
        PropertyDescriptor::builtin_metadata(Value::String($name.to_string().into())),
      );
      ctor_map.borrow_mut().properties.insert(
        PKey::String("BYTES_PER_ELEMENT".to_string().into()),
        PropertyDescriptor::immutable_data(Value::Number(usize_to_f64($element_count))),
      );
      proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string().into()),
        PropertyDescriptor::builtin_data(ctor_val.clone()),
      );
      (ctor_val, ctor_map)
    }};
  }

  let (uint8array_ctor_val, _uint8array_ctor_map) =
    create_typed_array_ctor!("Uint8Array", 1, Value::Number(0.0), true);
  let (int8array_ctor_val, _int8array_ctor_map) =
    create_typed_array_ctor!("Int8Array", 1, Value::Number(0.0), true);
  let (uint16array_ctor_val, _uint16array_ctor_map) =
    create_typed_array_ctor!("Uint16Array", 2, Value::Number(0.0), false);
  let (int16array_ctor_val, _int16array_ctor_map) =
    create_typed_array_ctor!("Int16Array", 2, Value::Number(0.0), false);
  let (uint32array_ctor_val, _uint32array_ctor_map) =
    create_typed_array_ctor!("Uint32Array", 4, Value::Number(0.0), false);
  let (int32array_ctor_val, _int32array_ctor_map) =
    create_typed_array_ctor!("Int32Array", 4, Value::Number(0.0), false);
  let (float32array_ctor_val, _float32array_ctor_map) =
    create_typed_array_ctor!("Float32Array", 4, Value::Number(0.0), false);
  let (float64array_ctor_val, _float64array_ctor_map) =
    create_typed_array_ctor!("Float64Array", 8, Value::Number(0.0), false);
  let (bigint64array_ctor_val, _bigint64array_ctor_map) = create_typed_array_ctor!(
    "BigInt64Array",
    8,
    Value::BigInt(BigIntValue::from(0u8)),
    false
  );
  let (biguint64array_ctor_val, _biguint64array_ctor_map) = create_typed_array_ctor!(
    "BigUint64Array",
    8,
    Value::BigInt(BigIntValue::from(0u8)),
    false
  );
  let (uint8clampedarray_ctor_val, _uint8clampedarray_ctor_map) =
    create_typed_array_ctor!("Uint8ClampedArray", 1, Value::Number(0.0), true);

  let dataview_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let dataview_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
  ));
  let dataview_ctor_fn: BuiltinFn = {
    let proto_map_rc = Rc::clone(&dataview_proto_map);
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "DataView");
      let buffer = args
        .get(off + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !is_array_buffer(&buffer) {
        return Err(
          vm.throw_type_error("DataView constructor requires ArrayBuffer or SharedArrayBuffer"),
        );
      }
      let buffer_byte_length = array_buffer_backing(&buffer)
        .ok_or_else(|| vm.throw_range_error("Invalid DataView length"))?
        .borrow()
        .len();
      let byte_offset_arg = args
        .get(off + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
        0
      } else {
        to_index(vm, &byte_offset_arg)?
      };
      let byte_length_arg = args
        .get(off + 3)
        .cloned()
        .unwrap_or(Value::Undefined);
      let byte_length = if matches!(byte_length_arg, Value::Undefined) {
        buffer_byte_length
          .checked_sub(byte_offset)
          .ok_or_else(|| vm.throw_range_error("Invalid DataView length"))?
      } else {
        to_index(vm, &byte_length_arg)?
      };
      if byte_offset + byte_length > buffer_byte_length {
        return Err(vm.throw_range_error("Invalid DataView length"));
      }
      let mut dataview_data = ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
      dataview_data.properties.insert(
        PropertyKey::String("[[DataViewBuffer]]".to_string().into()),
        PropertyDescriptor::ordinary_data(buffer.clone()),
      );
      dataview_data.properties.insert(
        PropertyKey::String("[[DataViewByteOffset]]".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_offset))),
      );
      dataview_data.properties.insert(
        PropertyKey::String("[[DataViewByteLength]]".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_length))),
      );
      dataview_data.properties.insert(
        PropertyKey::String("buffer".to_string().into()),
        PropertyDescriptor::ordinary_data(buffer),
      );
      dataview_data.properties.insert(
        PropertyKey::String("byteLength".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_length))),
      );
      dataview_data.properties.insert(
        PropertyKey::String("byteOffset".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_offset))),
      );
      let dataview_obj = Rc::new(RefCell::new(dataview_data));
      Ok(Value::Object(dataview_obj))
    })
  };
  let dataview_ctor_val =
    Value::BuiltinObject(Rc::clone(&dataview_ctor_fn), Rc::clone(&dataview_ctor_map));
  dataview_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&dataview_proto_map))),
  );
  dataview_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(dataview_ctor_val.clone()),
  );

  let (map_ctor_val, set_ctor_val, iterator_proto, map_iterator_proto, set_iterator_proto) =
    builtins::collections::install_map_set_constructors(&object_proto_map, &function_proto_map);
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
      if let Value::Object(obj) = &this
        && let Some(PropertyDescriptor::Data { value, .. }) = obj
          .borrow()
          .properties
          .get(&PKey::String("[[PrimitiveValue]]".to_string().into()))
      {
        return Ok(Value::String(
          format!("[object {}]", value.object_tag()).into(),
        ));
      }
      Ok(Value::String(
        format!("[object {}]", this.object_tag()).into(),
      ))
    }),
    &function_proto_map,
  );
  set_builtin_length(&object_to_string, 0.0);
  object_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(object_to_string),
  );
  object_proto_map.borrow_mut().properties.insert(
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
      &function_proto_map,
    )),
  );
  object_proto_map.borrow_mut().properties.insert(
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
      &function_proto_map,
    )),
  );
  let object_to_locale_string = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let obj = vm.to_object_value(this)?;
      let to_string = vm.get_property_host(&obj, &PKey::String("toString".to_string().into()))?;
      vm.call_value_with_this(&to_string, &obj, &[])
    }),
    &function_proto_map,
  );
  set_builtin_length(&object_to_locale_string, 0.0);
  object_proto_map.borrow_mut().properties.insert(
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
    &function_proto_map,
  );
  set_builtin_length(&has_own_property, 1.0);
  object_proto_map.borrow_mut().properties.insert(
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
    &function_proto_map,
  );
  set_builtin_length(&property_is_enumerable, 1.0);
  object_proto_map.borrow_mut().properties.insert(
    PKey::String("propertyIsEnumerable".to_string().into()),
    PropertyDescriptor::builtin_data(property_is_enumerable),
  );
  let is_prototype_of = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let value = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(vm.is_prototype_of(&this, &value)?))
    }),
    &function_proto_map,
  );
  set_builtin_length(&is_prototype_of, 1.0);
  object_proto_map.borrow_mut().properties.insert(
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
    &function_proto_map,
  );
  set_builtin_length(&define_getter, 2.0);
  object_proto_map.borrow_mut().properties.insert(
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
    &function_proto_map,
  );
  set_builtin_length(&define_setter, 2.0);
  object_proto_map.borrow_mut().properties.insert(
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
    &function_proto_map,
  );
  set_builtin_length(&lookup_getter, 1.0);
  object_proto_map.borrow_mut().properties.insert(
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
    &function_proto_map,
  );
  set_builtin_length(&lookup_setter, 1.0);
  object_proto_map.borrow_mut().properties.insert(
    PKey::String("__lookupSetter__".to_string().into()),
    PropertyDescriptor::builtin_data(lookup_setter),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("create".to_string().into()),
    PropertyDescriptor::builtin_data({
      let object_proto_map = Rc::clone(&object_proto_map);
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
                PropertyKey::String(s) => Value::String(s.clone().into()),
                PropertyKey::Symbol(sym) => Value::Symbol(sym.clone(), None),
              };
              let ok = vm.define_property_value(&created, &key_value, &descriptor)?;
              if !ok {
                return Err(RuntimeError::from("Object.create: define property failed"));
              }
            }
          }
          let _ = &object_proto_map;
          Ok(created)
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  object_ctor_map
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      PropertyDescriptor::builtin_data(builtin)
    });
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("getOwnPropertyDescriptor".to_string().into()),
    PropertyDescriptor::builtin_data({
      let object_proto_map = Rc::clone(&object_proto_map);
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
          Ok(descriptor_value_from_descriptor(&object_proto_map, &desc))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );

  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("getOwnPropertySymbols".to_string().into()),
    {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let target = args.get(1).cloned().unwrap_or(Value::Undefined);
          let symbols = vm
            .own_property_keys_value(&target)?
            .into_iter()
            .filter_map(|key| {
              if let PropertyKey::Symbol(id) = key {
                Some(Value::Symbol(id, None))
              } else {
                None
              }
            })
            .collect::<Vec<_>>();
          Ok(array_from_values(symbols))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    },
  );

  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("getOwnPropertyDescriptors".to_string().into()),
    {
      let object_proto_map = Rc::clone(&object_proto_map);
      let builtin = builtin_object(
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let mut result = ObjectData::new();
          for key in vm.own_property_keys_value(&target)? {
            if let Some(desc) = vm.get_own_property_value(&target, &key)? {
              let descriptor_value = descriptor_value_from_descriptor(&object_proto_map, &desc);
              result.properties.insert(
                key.clone(),
                PropertyDescriptor::ordinary_data(descriptor_value),
              );
            }
          }
          Ok(Value::Object(Rc::new(RefCell::new(result))))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    },
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("getOwnPropertyNames".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let mut names: Vec<String> = Vec::new();
          for key in vm.own_property_keys_value(&target)? {
            if let PropertyKey::String(name) = key {
              names.push(name.to_string());
            }
          }
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(
            names
              .into_iter()
              .map(|s| Some(Value::String(s.into())))
              .collect::<Vec<_>>(),
          )));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
          );
          Ok(Value::Array(arr_rc, props))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("keys".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let keys = vm
            .own_enumerable_keys_value(&target)?
            .into_iter()
            .filter_map(|key| match key {
              PropertyKey::String(s) => Some(Value::String(s.into())),
              _ => None,
            })
            .collect::<Vec<_>>();
          Ok(array_from_values(keys))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("values".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let values = vm
            .own_enumerable_keys_value(&target)?
            .into_iter()
            .map(|key| vm.get_property_host(&target, &key))
            .collect::<Result<Vec<_>, _>>()?;
          Ok(array_from_values(values))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("entries".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          let entries = vm
            .own_enumerable_keys_value(&target)?
            .into_iter()
            .map(|key| {
              let key_val = property_key_to_value(&key);
              let val = vm.get_property_host(&target, &key)?;
              Ok(array_from_values(vec![key_val, val]))
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
          Ok(array_from_values(entries))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("defineProperty".to_string().into()),
    {
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 3.0);
      PropertyDescriptor::builtin_data(builtin)
    },
  );

  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("defineProperties".to_string().into()),
    {
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
            let descriptor = vm.get_property_host(&props, &key)?;
            if matches!(descriptor, Value::Undefined) {
              continue;
            }
            let key_value = property_key_to_value(&key);
            let ok = vm.define_property_value(&target, &key_value, &descriptor)?;
            if !ok {
              return Err(vm.throw_type_error("Object.defineProperties failed"));
            }
          }
          Ok(target)
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      PropertyDescriptor::builtin_data(builtin)
    },
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("isExtensible".to_string().into()),
    {
      let builtin = builtin_object(
        Rc::new(|vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Object");
          let target = args.get(off).cloned().unwrap_or(Value::Undefined);
          if !target.is_object() {
            return Ok(Value::Bool(false));
          }
          Ok(Value::Bool(vm.is_extensible_value(&target)?))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    },
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("preventExtensions".to_string().into()),
    {
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      PropertyDescriptor::builtin_data(builtin)
    },
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("fromEntries".to_string().into()),
    PropertyDescriptor::builtin_data({
      let object_proto_for_from_entries = Rc::clone(&object_proto_map);
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
            let value = vm.get_property_host(&entry_obj, &PKey::String("1".to_string().into()))?;
            let prop_key = vm.to_property_key(&key)?;
            let key_value = property_key_to_value(&prop_key);
            vm.set_property_host(&result, &key_value, value)?;
          }
          Ok(result)
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  object_ctor_map.borrow_mut().properties.insert(
    PKey::String("groupBy".to_string().into()),
    PropertyDescriptor::builtin_data({
      let object_proto_for_group_by = Rc::clone(&object_proto_map);
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
        &function_proto_map,
      );
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );

  let array_ctor_val = builtins::array::install_array_constructor_and_late_prototype_methods(
    &array_ctor_map,
    &array_proto_map,
    &function_proto_map,
  );

  let regexp_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
  ));
  let regexp_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_proto_map)),
  )));
  regexp_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&regexp_proto_map))),
  );
  let regexp_proto_for_ctor = Rc::clone(&regexp_proto_map);
  let regexp_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let constructing = vm.current_new_target().is_some();
    let off = if constructing {
      1
    } else {
      static_builtin_arg_offset(vm, args, "RegExp")
    };
    let pattern_arg = args.get(off).cloned().unwrap_or(Value::Undefined);
    let flags_arg = args
      .get(off + 1)
      .cloned()
      .unwrap_or(Value::Undefined);

    let pattern_is_regexp = is_regexp(vm, &pattern_arg)?;
    if !constructing && matches!(flags_arg, Value::Undefined) && pattern_is_regexp {
      let pattern_constructor =
        vm.get_property_host(&pattern_arg, &PKey::String("constructor".into()))?;
      if vm
        .get_global("RegExp")
        .is_some_and(|constructor| strict_equal(&pattern_constructor, &constructor))
      {
        return Ok(pattern_arg);
      }
    }
    let (pattern, flags) = if matches!(flags_arg, Value::Undefined) && pattern_is_regexp {
      let source =
        vm.get_property_host(&pattern_arg, &PKey::String("source".to_string().into()))?;
      let flags = vm.get_property_host(&pattern_arg, &PKey::String("flags".to_string().into()))?;
      match (source, flags) {
        (Value::String(source), Value::String(flags)) => (source, flags),
        _ => (vm.to_string(&pattern_arg)?, js_string::JsString::empty()),
      }
    } else {
      let pattern = match pattern_arg {
        Value::Undefined => js_string::JsString::from("(?:)"),
        v => vm.to_string(&v)?,
      };
      let flags = match flags_arg {
        Value::Undefined => js_string::JsString::empty(),
        v => vm.to_string(&v)?,
      };
      (pattern, flags)
    };

    let matcher = regex_js::Regex::from_js(&pattern, flags.as_str_lossy())
      .map_err(|error| syntax_error(vm, format!("Invalid regular expression: {error}")))?;
    let mut inst = if constructing {
      args.first().cloned().unwrap_or(Value::Undefined)
    } else {
      let mut data = ObjectData::new();
      data.set_prototype_value(Value::Object(Rc::clone(&regexp_proto_for_ctor)));
      Value::Object(Rc::new(RefCell::new(data)))
    };

    let Value::Object(obj) = &mut inst else {
      return Err(RuntimeError::from(
        "RegExp constructor failed to create an object",
      ));
    };

    {
      let mut obj = obj.borrow_mut();
      let next_proto = if constructing {
        obj.get_prototype()
      } else {
        Value::Object(Rc::clone(&regexp_proto_for_ctor))
      };
      obj.set_prototype_value(next_proto);
      obj.regexp_matcher = Some(matcher);
      obj.properties.insert(
        PKey::String("lastIndex".to_string().into()),
        PropertyDescriptor::Data {
          value: Value::Number(0.0),
          writable: true,
          enumerable: false,
          configurable: false,
        },
      );
    }

    Ok(inst)
  });
  let regexp_ctor_val = Value::BuiltinObject(regexp_ctor_fn, regexp_ctor_map);
  set_builtin_name(&regexp_ctor_val, "RegExp");
  set_builtin_length(&regexp_ctor_val, 2.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(regexp_ctor_val.clone()),
  );
  let regexp_proto_for_source = Rc::clone(&regexp_proto_map);
  let regexp_source_getter = builtin_object(
    Rc::new(move |vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      if strict_equal(&this, &Value::Object(Rc::clone(&regexp_proto_for_source))) {
        return Ok(Value::String("(?:)".into()));
      }
      Ok(Value::String(regexp_matcher(vm, &this)?.escaped_source()))
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_source_getter, "get source");
  set_builtin_length(&regexp_source_getter, 0.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::String("source".into()),
    PropertyDescriptor::Accessor {
      get: Some(regexp_source_getter),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );

  for (name, read_flag) in [
    (
      "hasIndices",
      regex_js::Flags::has_indices as fn(regex_js::Flags) -> bool,
    ),
    ("global", regex_js::Flags::global),
    ("ignoreCase", regex_js::Flags::ignore_case),
    ("multiline", regex_js::Flags::multiline),
    ("dotAll", regex_js::Flags::dot_all),
    ("unicode", regex_js::Flags::unicode),
    ("sticky", regex_js::Flags::sticky),
  ] {
    let regexp_proto = Rc::clone(&regexp_proto_map);
    let getter = builtin_object(
      Rc::new(move |vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        if strict_equal(&this, &Value::Object(Rc::clone(&regexp_proto))) {
          return Ok(Value::Undefined);
        }
        Ok(Value::Bool(read_flag(regexp_matcher(vm, &this)?.flags())))
      }),
      &function_proto_map,
    );
    set_builtin_name(&getter, &format!("get {name}"));
    set_builtin_length(&getter, 0.0);
    regexp_proto_map.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::Accessor {
        get: Some(getter),
        set: None,
        enumerable: false,
        configurable: true,
      },
    );
  }
  let regexp_flags_getter = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      if !this.is_object() {
        return Err(vm.throw_type_error("RegExp flags getter called on incompatible receiver"));
      }
      let mut flags = js_string::JsStringBuilder::new();
      for (name, flag) in [
        ("hasIndices", "d"),
        ("global", "g"),
        ("ignoreCase", "i"),
        ("multiline", "m"),
        ("dotAll", "s"),
        ("unicode", "u"),
        ("unicodeSets", "v"),
        ("sticky", "y"),
      ] {
        let enabled = vm.get_property_host(&this, &PKey::String(name.into()))?;
        if vm.to_boolean(&enabled) {
          flags.push_str(flag);
        }
      }
      Ok(Value::String(flags.finish()))
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_flags_getter, "get flags");
  set_builtin_length(&regexp_flags_getter, 0.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::String("flags".into()),
    PropertyDescriptor::Accessor {
      get: Some(regexp_flags_getter),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );
  let regexp_to_string = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let source = vm.get_property_host(&this, &PKey::String("source".to_string().into()))?;
      let flags = vm.get_property_host(&this, &PKey::String("flags".to_string().into()))?;
      let src = vm.to_string(&source)?;
      let flg = vm.to_string(&flags)?;
      let mut result = js_string::JsStringBuilder::with_capacity(src.len() + flg.len() + 2);
      result.push_str("/");
      result.push_string(&src);
      result.push_str("/");
      result.push_string(&flg);
      Ok(Value::String(result.finish()))
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_to_string, "toString");
  set_builtin_length(&regexp_to_string, 0.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(regexp_to_string),
  );
  let regexp_exec = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let input = args.get(1).cloned().unwrap_or(Value::Undefined);
      regexp_builtin_exec(vm, &this, input)
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_exec, "exec");
  set_builtin_length(&regexp_exec, 1.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::String("exec".to_string().into()),
    PropertyDescriptor::builtin_data(regexp_exec),
  );
  let regexp_test = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let input = args.get(1).cloned().unwrap_or(Value::Undefined);
      let exec = vm.get_property_host(&this, &PKey::String("exec".to_string().into()))?;
      if !Vm::is_callable_value(&exec) {
        return Err(vm.throw_type_error("RegExp exec property is not callable"));
      }
      let result = vm.call_value_with_this(&exec, &this, &[input])?;
      if matches!(result, Value::Null) {
        Ok(Value::Bool(false))
      } else if result.is_object() {
        Ok(Value::Bool(true))
      } else {
        Err(vm.throw_type_error("RegExp exec must return an object or null"))
      }
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_test, "test");
  set_builtin_length(&regexp_test, 1.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::String("test".to_string().into()),
    PropertyDescriptor::builtin_data(regexp_test),
  );

  let regexp_match = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      regexp_builtin_match(vm, &this, subject)
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_match, "[Symbol.match]");
  set_builtin_length(&regexp_match, 1.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_match()),
    PropertyDescriptor::builtin_data(regexp_match),
  );

  let regexp_search = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      regexp_builtin_search(vm, &this, subject)
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_search, "[Symbol.search]");
  set_builtin_length(&regexp_search, 1.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_search()),
    PropertyDescriptor::builtin_data(regexp_search),
  );

  let regexp_replace = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
      regexp_builtin_replace(vm, &this, subject, replacement)
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_replace, "[Symbol.replace]");
  set_builtin_length(&regexp_replace, 2.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_replace()),
    PropertyDescriptor::builtin_data(regexp_replace),
  );
  let regexp_split = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      let limit = match args.get(2) {
        None | Some(Value::Undefined) => u32::MAX,
        Some(limit) => vm.to_uint32_value(limit)?,
      };
      regexp_builtin_split(vm, &this, subject, limit)
    }),
    &function_proto_map,
  );
  set_builtin_name(&regexp_split, "[Symbol.split]");
  set_builtin_length(&regexp_split, 2.0);
  regexp_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_split()),
    PropertyDescriptor::builtin_data(regexp_split),
  );

  let error_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let ctor = vm
      .current_new_target()
      .or_else(|| vm.get_global("Error"))
      .unwrap_or(Value::Undefined);
    let proto = vm
      .get_property_host(&ctor, &PKey::String("prototype".to_string().into()))
      .unwrap_or(Value::Null);
    let mut inst = ObjectData::new();
    inst.set_prototype_value(proto);
    let message = args.get(1).cloned();
    if let Some(message) = message.clone() {
      inst.properties.insert(
        PKey::String("message".to_string().into()),
        PropertyDescriptor::builtin_data(message),
      );
    }
    let stack = message.map_or_else(
      || Ok("Error".to_string()),
      |message| {
        vm.to_string(&message)
          .map(|message| format!("Error: {message}"))
      },
    )?;
    inst.properties.insert(
      PKey::String("stack".to_string().into()),
      PropertyDescriptor::builtin_data(Value::String(stack.into())),
    );
    inst.properties.insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(ctor),
    );
    Ok(Value::Object(Rc::new(RefCell::new(inst))))
  });
  let error_ctor_val = Value::BuiltinObject(Rc::clone(&error_ctor_fn), Rc::clone(&error_ctor_map));
  error_ctor_map.borrow_mut().properties.insert(
    PKey::String("name".to_string().into()),
    PropertyDescriptor::builtin_data(Value::String("Error".to_string().into())),
  );
  error_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(error_ctor_val.clone()),
  );
  error_proto_map.borrow_mut().properties.insert(
    PKey::String("name".to_string().into()),
    PropertyDescriptor::builtin_data(Value::String("Error".to_string().into())),
  );

  let aggregate_error_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let ctor = vm
      .current_new_target()
      .or_else(|| vm.get_global("AggregateError"))
      .unwrap_or(Value::Undefined);
    let proto = vm
      .get_property_host(&ctor, &PKey::String("prototype".to_string().into()))
      .unwrap_or(Value::Null);
    let mut inst = ObjectData::new();
    inst.set_prototype_value(proto);
    if let Some(message) = args.get(2).cloned() {
      inst.properties.insert(
        PKey::String("message".to_string().into()),
        PropertyDescriptor::builtin_data(message),
      );
    }
    inst.properties.insert(
      PKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_data(Value::String("AggregateError".to_string().into())),
    );
    inst.properties.insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(ctor),
    );
    Ok(Value::Object(Rc::new(RefCell::new(inst))))
  });
  let aggregate_error_ctor_val = Value::BuiltinObject(
    Rc::clone(&aggregate_error_ctor_fn),
    Rc::clone(&aggregate_error_ctor_map),
  );
  aggregate_error_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_data(Value::String("AggregateError".to_string().into())),
    );
  aggregate_error_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(aggregate_error_ctor_val.clone()),
    );
  aggregate_error_proto_map
    .borrow_mut()
    .properties
    .insert(
      PKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_data(Value::String("AggregateError".to_string().into())),
    );

  let _make_error_ctor = |name: &str, proto_map: &Rc<RefCell<ObjectData>>| {
    let ctor_props = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
      Rc::clone(&function_proto_map),
    ))));
    ctor_props.borrow_mut().properties.insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(proto_map))),
    );
    let ctor_name = name.to_string();
    let instance_name = name.to_string();
    let ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
      let ctor = vm
        .current_new_target()
        .or_else(|| vm.get_global(&ctor_name))
        .unwrap_or(Value::Undefined);
      let proto = vm
        .get_property_host(&ctor, &PKey::String("prototype".to_string().into()))
        .unwrap_or(Value::Null);
      let mut inst = ObjectData::new();
      inst.set_prototype_value(proto);
      if let Some(message) = args.get(1).cloned() {
        inst.properties.insert(
          PKey::String("message".to_string().into()),
          PropertyDescriptor::builtin_data(message),
        );
      }
      inst.properties.insert(
        PKey::String("name".to_string().into()),
        PropertyDescriptor::builtin_data(Value::String(instance_name.clone().into())),
      );
      Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let ctor_val = Value::BuiltinObject(ctor_fn, Rc::clone(&ctor_props));
    ctor_props.borrow_mut().properties.insert(
      PKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_data(Value::String(name.to_string().into())),
    );
    proto_map.borrow_mut().properties.insert(
      PKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(ctor_val.clone()),
    );
    proto_map.borrow_mut().properties.insert(
      PKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_data(Value::String(name.to_string().into())),
    );
    (name.to_string(), ctor_val)
  };

  let eval_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&error_proto_map),
  ))));
  let range_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&error_proto_map),
  ))));
  let reference_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&error_proto_map),
  ))));
  let syntax_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&error_proto_map),
  ))));
  let type_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&error_proto_map),
  ))));
  let uri_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    error_proto_map,
  ))));

  let (_eval_error_name, eval_error_ctor_val) =
    _make_error_ctor("EvalError", &eval_error_proto_map);
  let (_range_error_name, range_error_ctor_val) =
    _make_error_ctor("RangeError", &range_error_proto_map);
  let (_reference_error_name, reference_error_ctor_val) =
    _make_error_ctor("ReferenceError", &reference_error_proto_map);
  let (_syntax_error_name, syntax_error_ctor_val) =
    _make_error_ctor("SyntaxError", &syntax_error_proto_map);
  let (_type_error_name, type_error_ctor_val) =
    _make_error_ctor("TypeError", &type_error_proto_map);
  let (_uri_error_name, uri_error_ctor_val) = _make_error_ctor("URIError", &uri_error_proto_map);

  let math = Rc::new(RefCell::new(ObjectData::new()));
  let install_math = |name: &str, length: f64, function: BuiltinFn| {
    let builtin = builtin_object(function, &function_proto_map);
    set_builtin_name(&builtin, name);
    set_builtin_length(&builtin, length);
    math.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::builtin_data(builtin),
    );
  };
  install_math(
    "pow",
    2.0,
    Rc::new(|vm, args| {
      let x = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let y = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(number_exponentiate(x, y)))
    }),
  );
  let unary_math_operations: [(&str, fn(f64) -> f64); 3] = [
    ("abs", f64::abs),
    ("floor", f64::floor),
    ("round", |value: f64| (value + 0.5).floor()),
  ];
  for (name, operation) in unary_math_operations {
    install_math(
      name,
      1.0,
      Rc::new(move |vm, args| {
        let value = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
        Ok(Value::Number(operation(value)))
      }),
    );
  }
  install_math(
    "random",
    0.0,
    Rc::new(|_vm, _args| Ok(Value::Number(fastrand::f64()))),
  );
  install_math(
    "imul",
    2.0,
    Rc::new(|vm, args| {
      let left = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))? as i32;
      let right = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))? as i32;
      Ok(Value::Number(f64::from(left.wrapping_mul(right))))
    }),
  );
  let variadic_math_operations: [(&str, f64, fn(f64, f64) -> f64); 2] = [
    ("min", f64::INFINITY, f64::min),
    ("max", f64::NEG_INFINITY, f64::max),
  ];
  for (name, identity, operation) in variadic_math_operations {
    install_math(
      name,
      2.0,
      Rc::new(move |vm, args| {
        let mut result = identity;
        for value in args.iter().skip(1) {
          result = operation(result, vm.to_number_value(value)?);
        }
        Ok(Value::Number(result))
      }),
    );
  }
  for (name, value) in [
    ("E", std::f64::consts::E),
    ("LN10", std::f64::consts::LN_10),
    ("LN2", std::f64::consts::LN_2),
    ("LOG10E", std::f64::consts::LOG10_E),
    ("LOG2E", std::f64::consts::LOG2_E),
    ("PI", std::f64::consts::PI),
    ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ("SQRT2", std::f64::consts::SQRT_2),
  ] {
    math.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::Data {
        value: Value::Number(value),
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  }

  let reflect = Rc::new(RefCell::new(ObjectData::new()));
  reflect.borrow_mut().properties.insert(
    PKey::String("construct".to_string().into()),
    PropertyDescriptor::builtin_data(Value::BuiltinObject(
      Rc::new(|vm, args| {
        let target = args.get(1).cloned().unwrap_or(Value::Undefined);
        let arguments_list = args.get(2).cloned().unwrap_or(Value::Undefined);
        let new_target = args
          .get(3)
          .cloned()
          .unwrap_or_else(|| target.clone());

        if !target.is_constructable() {
          let err = vm.create_intrinsic_error("TypeError", "target is not a constructor")?;
          return Err(vm.throw_exception(err));
        }
        if !new_target.is_constructable() {
          let err = vm.create_intrinsic_error("TypeError", "newTarget is not a constructor")?;
          return Err(vm.throw_exception(err));
        }

        let mut args_vec = Vec::new();
        match &arguments_list {
          Value::Array(arr, _) => {
            for item in arr.borrow().iter().cloned() {
              if let Some(v) = item {
                args_vec.push(v);
              } else {
                args_vec.push(Value::Undefined);
              }
            }
          }
          other => {
            let len_val =
              vm.get_property_host(other, &PropertyKey::String("length".to_string().into()))?;
            let len = to_length(vm, &len_val)?;
            for idx in 0..len {
              let item = vm.get_property_host(
                other,
                &PropertyKey::String(usize_to_decimal_string(idx).into()),
              )?;
              args_vec.push(item);
            }
          }
        }

        vm.construct_value(&target, &args_vec)
      }),
      Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&function_proto_map),
      )))),
    )),
  );
  reflect.borrow_mut().properties.insert(
    PKey::String("ownKeys".to_string().into()),
    PropertyDescriptor::builtin_data(Value::BuiltinObject(
      Rc::new(|vm, args| {
        let target = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !target.is_object() {
          return Err(vm.throw_type_error("Reflect.ownKeys called on non-object"));
        }
        let keys = vm
          .own_property_keys_value(&target)?
          .into_iter()
          .map(|key| property_key_to_value(&key))
          .collect::<Vec<_>>();
        Ok(array_from_values(keys))
      }),
      Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&function_proto_map),
      )))),
    )),
  );

  let shadowrealm_ctor_val =
    shadowrealm::build_shadowrealm_constructor(&object_proto_map, &function_proto_map);

  let json_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&object_proto_map),
  ))));
  json_obj.borrow_mut().properties.insert(
    PKey::String("stringify".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        move |_vm, args| {
          let off = if args.len() > 1 { 1 } else { 0 };
          let value = args.get(off).cloned().unwrap_or(Value::Undefined);
          let mut seen = HashSet::new();
          Ok(match json_stringify_value(_vm, &value, &mut seen)? {
            Some(out) => Value::String(out.into()),
            None => Value::Undefined,
          })
        }
      }),
      &function_proto_map,
    )),
  );

  let atomics_obj = atomics::build_atomics_object(&object_proto_map, &function_proto_map);

  install_global_bindings(
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

  let get_ctor = |name: &str| -> Option<Value> {
    globals
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
  };

  let get_proto = |ctor: &Value| -> Option<Value> {
    match ctor {
      Value::BuiltinObject(_, props) => props
        .borrow()
        .properties
        .get(&PropertyKey::String("prototype".to_string().into()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        }),
      _ => None,
    }
  };

  let typed_array_names = [
    "Uint8Array",
    "Int8Array",
    "Uint16Array",
    "Int16Array",
    "Uint32Array",
    "Int32Array",
    "Float32Array",
    "Float64Array",
    "Uint8ClampedArray",
    "BigInt64Array",
    "BigUint64Array",
  ];

  let typed_array_constructors: Vec<Value> = typed_array_names
    .iter()
    .filter_map(|name| get_ctor(name))
    .collect();

  let mut intrinsics = Intrinsics {
    object_constructor: get_ctor("Object"),
    array_constructor: get_ctor("Array"),
    function_constructor: get_ctor("Function"),
    string_constructor: get_ctor("String"),
    symbol_constructor: get_ctor("Symbol"),
    boolean_constructor: get_ctor("Boolean"),
    bigint_constructor: get_ctor("BigInt"),
    number_constructor: get_ctor("Number"),
    error_constructor: get_ctor("Error"),
    aggregate_error_constructor: get_ctor("AggregateError"),
    type_error_constructor: get_ctor("TypeError"),
    range_error_constructor: get_ctor("RangeError"),
    reference_error_constructor: get_ctor("ReferenceError"),
    syntax_error_constructor: get_ctor("SyntaxError"),
    eval_error_constructor: get_ctor("EvalError"),
    uri_error_constructor: get_ctor("URIError"),
    promise_constructor: get_ctor("Promise"),
    date_constructor: get_ctor("Date"),
    regexp_constructor: get_ctor("RegExp"),
    map_constructor: get_ctor("Map"),
    set_constructor: get_ctor("Set"),
    arraybuffer_constructor: get_ctor("ArrayBuffer"),
    shared_array_buffer_constructor: get_ctor("SharedArrayBuffer"),
    data_view_constructor: get_ctor("DataView"),
    typed_array_constructor: get_ctor("TypedArray"),
    object_prototype: Some(Value::Object(object_proto_map)),
    array_prototype: Some(Value::Object(array_proto_map)),
    function_prototype: Some(Value::Object(function_proto_map)),
    string_prototype: get_ctor("String").and_then(|ctor| get_proto(&ctor)),
    symbol_prototype: get_ctor("Symbol").and_then(|ctor| get_proto(&ctor)),
    boolean_prototype: get_ctor("Boolean").and_then(|ctor| get_proto(&ctor)),
    bigint_prototype: get_ctor("BigInt").and_then(|ctor| get_proto(&ctor)),
    number_prototype: get_ctor("Number").and_then(|ctor| get_proto(&ctor)),
    error_prototype: get_ctor("Error").and_then(|ctor| get_proto(&ctor)),
    promise_prototype: get_ctor("Promise").and_then(|ctor| get_proto(&ctor)),
    date_prototype: get_ctor("Date").and_then(|ctor| get_proto(&ctor)),
    regexp_prototype: get_ctor("RegExp").and_then(|ctor| get_proto(&ctor)),
    map_prototype: get_ctor("Map").and_then(|ctor| get_proto(&ctor)),
    set_prototype: get_ctor("Set").and_then(|ctor| get_proto(&ctor)),
    typed_array_prototype: Some(Value::Object(typedarray_proto_map)),
    arraybuffer_prototype: get_ctor("ArrayBuffer").and_then(|ctor| get_proto(&ctor)),
    shared_array_buffer_prototype: get_ctor("SharedArrayBuffer").and_then(|ctor| get_proto(&ctor)),
    data_view_prototype: get_ctor("DataView").and_then(|ctor| get_proto(&ctor)),
    iterator_prototype: Some(iterator_proto),
    async_iterator_prototype: None,
    array_iterator_prototype: Some(Value::Object(array_iterator_proto)),
    map_iterator_prototype: Some(map_iterator_proto),
    set_iterator_prototype: Some(set_iterator_proto),
    string_iterator_prototype: None,
    regexp_string_iterator_prototype: None,
    generator_prototype: Some(Value::Object(generator_proto_map)),
    generator_function_prototype: Some(Value::Object(generator_function_proto_map)),
    async_generator_prototype: None,
    async_from_sync_iterator_prototype: None,
    throw_type_error: Some(throw_type_error_val),
    typed_array_constructors,
    hidden_builtins: HashMap::new(),
  };

  install_hidden_builtins(&mut intrinsics);

  intrinsics
}
