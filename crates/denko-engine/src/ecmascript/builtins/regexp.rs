// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::ecmascript::semantics::get_substitution;
use crate::runtime_utils::{
  array_from_values, builtin_object, set_builtin_length, set_builtin_name,
  static_builtin_arg_offset, strict_equal, syntax_error, to_length, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::{RuntimeError, Vm};

pub(crate) struct RegExpIntrinsics {
  pub(crate) constructor: Value,
  pub(crate) prototype: Value,
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

fn regexp_find(
  vm: &mut Vm,
  matcher: &regex_js::Regex,
  input: &js_string::JsString,
  start: usize,
) -> Result<Option<regex_js::Match>, RuntimeError> {
  matcher
    .try_find(input, start)
    .map_err(|error| vm.throw_range_error(&error.to_string()))
}

pub(crate) fn is_regexp(
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

  let Some(matched) = regexp_find(vm, &matcher, &subject_utf16, start)? else {
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
  let flags = vm.get_property_host(this, &PropertyKey::String("flags".into()))?;
  let flags = vm.to_string(&flags)?;
  let global = flags
    .code_units()
    .any(|unit| unit == u16::from(b'g'));
  let unicode = flags
    .code_units()
    .any(|unit| unit == u16::from(b'u') || unit == u16::from(b'v'));
  if !global {
    return regexp_exec(vm, this, &subject);
  }
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
  if !crate::same_value(&previous_last_index, &Value::Number(0.0)) {
    drop(vm.set_property_host(this, &Value::String("lastIndex".into()), Value::Number(0.0))?);
  }
  let result = regexp_exec(vm, this, &subject)?;
  let current_last_index = vm.get_property_host(this, &PropertyKey::String("lastIndex".into()))?;
  if !crate::same_value(&current_last_index, &previous_last_index) {
    drop(vm.set_property_host(
      this,
      &Value::String("lastIndex".into()),
      previous_last_index,
    )?);
  }
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

  let flags = vm.get_property_host(this, &PropertyKey::String("flags".into()))?;
  let flags = vm.to_string(&flags)?;
  let global = flags
    .code_units()
    .any(|unit| unit == u16::from(b'g'));
  let unicode = flags
    .code_units()
    .any(|unit| unit == u16::from(b'u') || unit == u16::from(b'v'));
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
  limit: Value,
) -> Result<Value, RuntimeError> {
  if !this.is_object() {
    return Err(vm.throw_type_error("RegExp.prototype[Symbol.split] requires an object"));
  }

  let default_constructor = vm
    .get_global("RegExp")
    .ok_or_else(|| vm.throw_type_error("RegExp constructor is unavailable"))?;
  let constructor = vm.get_property_host(this, &PropertyKey::String("constructor".into()))?;
  let constructor = match constructor {
    Value::Undefined => default_constructor,
    constructor if constructor.is_object() => {
      let species = vm.get_property_host(
        &constructor,
        &PropertyKey::Symbol(SymbolId::well_known_species()),
      )?;
      match species {
        Value::Undefined | Value::Null => default_constructor,
        species => species,
      }
    }
    _ => return Err(vm.throw_type_error("RegExp constructor property is not an object")),
  };
  if !constructor.is_constructable() {
    return Err(vm.throw_type_error("RegExp species is not a constructor"));
  }

  let flags = vm.get_property_host(this, &PropertyKey::String("flags".into()))?;
  let flags = vm.to_string(&flags)?;
  let unicode_matching = flags.as_str_lossy().contains('u');
  let new_flags = if flags.as_str_lossy().contains('y') {
    flags
  } else {
    let mut builder = js_string::JsStringBuilder::with_capacity(flags.len() + 1);
    builder.push_string(&flags);
    builder.push_str("y");
    builder.finish()
  };
  let splitter = vm.construct_value(&constructor, &[this.clone(), Value::String(new_flags)])?;
  let limit = if matches!(limit, Value::Undefined) {
    u32::MAX
  } else {
    vm.to_uint32_value(&limit)?
  };

  if limit == 0 {
    return Ok(array_from_values(Vec::new()));
  }
  let size = subject.len();
  let mut output = Vec::new();

  if size == 0 {
    return if !matches!(regexp_exec(vm, &splitter, &subject)?, Value::Null) {
      Ok(array_from_values(output))
    } else {
      Ok(array_from_values(vec![Value::String(subject)]))
    };
  }

  let limit = usize::try_from(limit).unwrap_or(usize::MAX);
  let mut previous_end = 0;
  let mut position = 0;
  while position < size {
    drop(vm.set_property_host(
      &splitter,
      &Value::String("lastIndex".into()),
      Value::Number(usize_to_f64(position)),
    )?);
    let matched = regexp_exec(vm, &splitter, &subject)?;
    if matches!(matched, Value::Null) {
      position = subject.advance_string_index(position, unicode_matching);
      continue;
    }
    let end = vm.get_property_host(&splitter, &PropertyKey::String("lastIndex".into()))?;
    let end = to_length(vm, &end)?.min(size);
    if end == previous_end {
      position = subject.advance_string_index(position, unicode_matching);
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
    let capture_count = vm.get_property_host(&matched, &PropertyKey::String("length".into()))?;
    let capture_count = to_length(vm, &capture_count)?;
    for capture_index in 1..capture_count {
      output.push(vm.get_property_host(
        &matched,
        &PropertyKey::String(capture_index.to_string().into()),
      )?);
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

fn regexp_to_string(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let source = vm.get_property_host(&this, &PropertyKey::String("source".to_string().into()))?;
  let flags = vm.get_property_host(&this, &PropertyKey::String("flags".to_string().into()))?;
  let source = vm.to_string(&source)?;
  let flags = vm.to_string(&flags)?;
  let mut result = js_string::JsStringBuilder::with_capacity(source.len() + flags.len() + 2);
  result.push_str("/");
  result.push_string(&source);
  result.push_str("/");
  result.push_string(&flags);
  Ok(Value::String(result.finish()))
}

fn regexp_exec_method(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let input = args.get(1).cloned().unwrap_or(Value::Undefined);
  regexp_builtin_exec(vm, &this, input)
}

fn regexp_test(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let input = args.get(1).cloned().unwrap_or(Value::Undefined);
  let exec = vm.get_property_host(&this, &PropertyKey::String("exec".to_string().into()))?;
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
}

fn regexp_compile(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let Value::Object(object) = &this else {
    return Err(vm.throw_type_error("RegExp.prototype.compile called on incompatible receiver"));
  };
  if object.borrow().regexp_matcher.is_none() {
    return Err(vm.throw_type_error("RegExp object has no [[RegExpMatcher]]"));
  }

  let pattern = args.get(1).cloned().unwrap_or(Value::Undefined);
  let flags = args.get(2).cloned().unwrap_or(Value::Undefined);
  let (source, flags) = if let Value::Object(pattern_object) = &pattern
    && let Some(matcher) = pattern_object.borrow().regexp_matcher.clone()
  {
    if !matches!(flags, Value::Undefined) {
      return Err(vm.throw_type_error("flags must be undefined when compiling a RegExp"));
    }
    (matcher.source().clone(), matcher.flags().to_string())
  } else {
    let source = if matches!(pattern, Value::Undefined) {
      js_string::JsString::empty()
    } else {
      vm.to_string(&pattern)?
    };
    let flags = if matches!(flags, Value::Undefined) {
      String::new()
    } else {
      vm.to_string(&flags)?.to_string_lossy()
    };
    (source, flags)
  };

  let matcher = regex_js::Regex::from_js(&source, &flags)
    .map_err(|error| syntax_error(vm, format!("Invalid regular expression: {error}")))?;
  object.borrow_mut().regexp_matcher = Some(matcher);
  drop(vm.set_property_host(
    &this,
    &Value::String("lastIndex".into()),
    Value::Number(0.0),
  )?);
  Ok(this)
}

fn regexp_match(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
  regexp_builtin_match(vm, &this, subject)
}

fn regexp_search(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
  regexp_builtin_search(vm, &this, subject)
}

fn regexp_replace(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
  let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
  regexp_builtin_replace(vm, &this, subject, replacement)
}

fn regexp_split(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
  let limit = args.get(2).cloned().unwrap_or(Value::Undefined);
  regexp_builtin_split(vm, &this, subject, limit)
}

fn regexp_match_all(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  const MATCHER: &str = "[[RegExpStringIteratorMatcher]]";
  const STRING: &str = "[[RegExpStringIteratorString]]";
  const GLOBAL: &str = "[[RegExpStringIteratorGlobal]]";
  const UNICODE: &str = "[[RegExpStringIteratorUnicode]]";
  const DONE: &str = "[[RegExpStringIteratorDone]]";

  let this = args.first().cloned().unwrap_or(Value::Undefined);
  if !this.is_object() {
    return Err(vm.throw_type_error("RegExp.prototype[Symbol.matchAll] requires an object"));
  }
  let subject = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
  let flags = vm.get_property_host(&this, &PropertyKey::String("flags".into()))?;
  let flags = vm.to_string(&flags)?;
  let constructor = vm.get_property_host(&this, &PropertyKey::String("constructor".into()))?;
  let default_constructor = vm
    .get_global("RegExp")
    .ok_or_else(|| vm.throw_type_error("RegExp constructor is unavailable"))?;
  let species = if matches!(constructor, Value::Undefined) {
    default_constructor
  } else {
    if !constructor.is_object() {
      return Err(vm.throw_type_error("RegExp constructor property is not an object"));
    }
    let species = vm.get_property_host(
      &constructor,
      &PropertyKey::Symbol(SymbolId::well_known_species()),
    )?;
    if matches!(species, Value::Undefined | Value::Null) {
      default_constructor
    } else {
      species
    }
  };
  if !species.is_constructable() {
    return Err(vm.throw_type_error("RegExp species is not a constructor"));
  }
  let matcher = vm.construct_value(&species, &[this.clone(), Value::String(flags.clone())])?;
  let last_index = vm.get_property_host(&this, &PropertyKey::String("lastIndex".into()))?;
  let last_index = to_length(vm, &last_index)?;
  drop(vm.set_property_host(
    &matcher,
    &Value::String("lastIndex".into()),
    Value::Number(usize_to_f64(last_index)),
  )?);
  let global = flags
    .code_units()
    .any(|unit| unit == u16::from(b'g'));
  let unicode = flags
    .code_units()
    .any(|unit| unit == u16::from(b'u') || unit == u16::from(b'v'));

  let mut iterator = ObjectData::with_prototype(vm.realm().object_prototype());
  iterator.set_internal_slot(MATCHER, matcher);
  iterator.set_internal_slot(STRING, Value::String(subject));
  iterator.set_internal_slot(GLOBAL, Value::Bool(global));
  iterator.set_internal_slot(UNICODE, Value::Bool(unicode));
  iterator.set_internal_slot(DONE, Value::Bool(false));
  let iterator = Value::Object(Rc::new(RefCell::new(iterator)));
  let next = Value::Builtin(Rc::new(move |vm, args| {
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let Value::Object(data) = &receiver else {
      return Err(
        vm.throw_type_error("RegExp String Iterator next called on incompatible receiver"),
      );
    };
    let (matcher, subject, global, unicode, done) = {
      let data = data.borrow();
      let matcher = data.internal_slot(MATCHER).cloned();
      let subject = data.internal_slot(STRING).cloned();
      let global = data.internal_slot(GLOBAL).cloned();
      let unicode = data.internal_slot(UNICODE).cloned();
      let done = data.internal_slot(DONE).cloned();
      (matcher, subject, global, unicode, done)
    };
    let (
      Some(matcher),
      Some(Value::String(subject)),
      Some(Value::Bool(global)),
      Some(Value::Bool(unicode)),
      Some(Value::Bool(done)),
    ) = (matcher, subject, global, unicode, done)
    else {
      return Err(
        vm.throw_type_error("RegExp String Iterator next called on incompatible receiver"),
      );
    };
    if done {
      return Ok(iterator_result(vm, Value::Undefined, true));
    }
    let result = regexp_exec(vm, &matcher, &subject)?;
    if matches!(result, Value::Null) {
      data
        .borrow_mut()
        .set_internal_slot(DONE, Value::Bool(true));
      return Ok(iterator_result(vm, Value::Undefined, true));
    }
    if !global {
      data
        .borrow_mut()
        .set_internal_slot(DONE, Value::Bool(true));
    } else {
      let matched = vm.get_property_host(&result, &PropertyKey::String("0".into()))?;
      if vm.to_string(&matched)?.is_empty() {
        let last_index =
          vm.get_property_host(&matcher, &PropertyKey::String("lastIndex".into()))?;
        let last_index = to_length(vm, &last_index)?;
        let next_index = subject.advance_string_index(last_index, unicode);
        drop(vm.set_property_host(
          &matcher,
          &Value::String("lastIndex".into()),
          Value::Number(usize_to_f64(next_index)),
        )?);
      }
    }
    Ok(iterator_result(vm, result, false))
  }));
  if let Value::Object(data) = &iterator {
    data.borrow_mut().properties.insert(
      PropertyKey::String("next".into()),
      PropertyDescriptor::builtin_data(next),
    );
    data.borrow_mut().properties.insert(
      PropertyKey::Symbol(SymbolId::well_known_iterator()),
      PropertyDescriptor::builtin_data(Value::Builtin(Rc::new(|_vm, args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
      }))),
    );
    data.borrow_mut().properties.insert(
      PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
      PropertyDescriptor::Data {
        value: Value::String("RegExp String Iterator".into()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
  }
  Ok(iterator)
}

fn iterator_result(
  vm: &Vm,
  value: Value,
  done: bool,
) -> Value {
  let mut result = ObjectData::with_prototype(vm.realm().object_prototype());
  result.properties.insert(
    PropertyKey::String("value".into()),
    PropertyDescriptor::ordinary_data(value),
  );
  result.properties.insert(
    PropertyKey::String("done".into()),
    PropertyDescriptor::ordinary_data(Value::Bool(done)),
  );
  Value::Object(Rc::new(RefCell::new(result)))
}

fn accessor(
  target: &Rc<RefCell<ObjectData>>,
  key: PropertyKey,
  get: Value,
) {
  target.borrow_mut().properties.insert(
    key,
    PropertyDescriptor::Accessor {
      get: Some(get),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );
}

#[allow(clippy::too_many_lines)]
pub(crate) fn create_regexp_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> RegExpIntrinsics {
  use crate::ecmascript::semantics::intrinsics::IntrinsicInstaller;
  use PropertyKey as PKey;

  let prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  let constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_prototype),
  ))));
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&prototype))),
    );

  let prototype_for_constructor = Rc::clone(&prototype);
  let constructor_function: BuiltinFn = Rc::new(move |vm, args| {
    let constructing = vm.pending_constructor_new_target().is_some();
    let offset = if constructing {
      1
    } else {
      static_builtin_arg_offset(vm, args, "RegExp")
    };
    let pattern_arg = args
      .get(offset)
      .cloned()
      .unwrap_or(Value::Undefined);
    let flags_arg = args
      .get(offset + 1)
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
    let (pattern, flags) = if pattern_is_regexp {
      let source = vm.get_property_host(&pattern_arg, &PKey::String("source".into()))?;
      let pattern = vm.to_string(&source)?;
      let flags = if matches!(flags_arg, Value::Undefined) {
        vm.get_property_host(&pattern_arg, &PKey::String("flags".into()))?
      } else {
        flags_arg
      };
      (pattern, vm.to_string(&flags)?)
    } else {
      let pattern = match pattern_arg {
        Value::Undefined => js_string::JsString::from("(?:)"),
        value => vm.to_string(&value)?,
      };
      let flags = match flags_arg {
        Value::Undefined => js_string::JsString::empty(),
        value => vm.to_string(&value)?,
      };
      (pattern, flags)
    };

    let matcher = regex_js::Regex::from_js(&pattern, flags.as_str_lossy())
      .map_err(|error| syntax_error(vm, format!("Invalid regular expression: {error}")))?;
    let mut instance = if constructing {
      args.first().cloned().unwrap_or(Value::Undefined)
    } else {
      let mut data = ObjectData::new();
      data.set_prototype_value(Value::Object(Rc::clone(&prototype_for_constructor)));
      Value::Object(Rc::new(RefCell::new(data)))
    };

    let Value::Object(object) = &mut instance else {
      return Err(RuntimeError::from(
        "RegExp constructor failed to create an object",
      ));
    };
    {
      let mut object = object.borrow_mut();
      let next_prototype = if constructing {
        object.get_prototype()
      } else {
        Value::Object(Rc::clone(&prototype_for_constructor))
      };
      object.set_prototype_value(next_prototype);
      object.regexp_matcher = Some(matcher);
      object.properties.insert(
        PKey::String("lastIndex".to_string().into()),
        PropertyDescriptor::Data {
          value: Value::Number(0.0),
          writable: true,
          enumerable: false,
          configurable: false,
        },
      );
    }
    Ok(instance)
  });
  let constructor = Value::BuiltinObject(constructor_function, constructor_properties);
  set_builtin_name(&constructor, "RegExp");
  set_builtin_length(&constructor, 2.0);
  let species_getter = builtin_object(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    function_prototype,
  );
  set_builtin_name(&species_getter, "get [Symbol.species]");
  set_builtin_length(&species_getter, 0.0);
  if let Value::BuiltinObject(_, properties) = &constructor {
    accessor(
      properties,
      PKey::Symbol(SymbolId::well_known_species()),
      species_getter,
    );
  }
  prototype.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );

  let prototype_for_source = Rc::clone(&prototype);
  let source_getter = builtin_object(
    Rc::new(move |vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      if strict_equal(&this, &Value::Object(Rc::clone(&prototype_for_source))) {
        return Ok(Value::String("(?:)".into()));
      }
      Ok(Value::String(regexp_matcher(vm, &this)?.escaped_source()))
    }),
    function_prototype,
  );
  set_builtin_name(&source_getter, "get source");
  set_builtin_length(&source_getter, 0.0);
  accessor(&prototype, PKey::String("source".into()), source_getter);

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
    let prototype_for_getter = Rc::clone(&prototype);
    let getter = builtin_object(
      Rc::new(move |vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        if strict_equal(&this, &Value::Object(Rc::clone(&prototype_for_getter))) {
          return Ok(Value::Undefined);
        }
        Ok(Value::Bool(read_flag(regexp_matcher(vm, &this)?.flags())))
      }),
      function_prototype,
    );
    set_builtin_name(&getter, &format!("get {name}"));
    set_builtin_length(&getter, 0.0);
    accessor(&prototype, PKey::String(name.into()), getter);
  }

  let flags_getter = builtin_object(
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
    function_prototype,
  );
  set_builtin_name(&flags_getter, "get flags");
  set_builtin_length(&flags_getter, 0.0);
  accessor(&prototype, PKey::String("flags".into()), flags_getter);

  let methods = IntrinsicInstaller::new(&prototype, function_prototype);
  methods.method("toString", 0, regexp_to_string);
  methods.method("exec", 1, regexp_exec_method);
  methods.method("test", 1, regexp_test);
  methods.method("compile", 2, regexp_compile);
  methods.named_method(
    PKey::Symbol(SymbolId::well_known_match()),
    "[Symbol.match]",
    1,
    regexp_match,
  );
  methods.named_method(
    PKey::Symbol(SymbolId::well_known_match_all()),
    "[Symbol.matchAll]",
    1,
    regexp_match_all,
  );
  methods.named_method(
    PKey::Symbol(SymbolId::well_known_search()),
    "[Symbol.search]",
    1,
    regexp_search,
  );
  methods.named_method(
    PKey::Symbol(SymbolId::well_known_replace()),
    "[Symbol.replace]",
    2,
    regexp_replace,
  );
  methods.named_method(
    PKey::Symbol(SymbolId::well_known_split()),
    "[Symbol.split]",
    2,
    regexp_split,
  );

  RegExpIntrinsics {
    constructor,
    prototype: Value::Object(prototype),
  }
}
