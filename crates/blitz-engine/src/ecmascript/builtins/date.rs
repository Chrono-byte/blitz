// SPDX-License-Identifier: MPL-2.0
//! Date constructor and prototype methods.

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::ecmascript::semantics::builtin_object;
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

/// The Date constructor and its prototype object.
pub(crate) struct DateIntrinsics {
  pub(crate) constructor: Value,
  pub(crate) prototype: Value,
}

fn to_num(value: &Value) -> f64 {
  match value {
    Value::Number(number) => *number,
    Value::Undefined => f64::NAN,
    Value::Null => 0.0,
    Value::Bool(value) => f64::from(*value),
    Value::String(value) => value.parse::<f64>().unwrap_or(f64::NAN),
    _ => f64::NAN,
  }
}

fn date_to_timestamp(
  year: i32,
  month: i32,
  day: i32,
  hour: i32,
  min: i32,
  sec: i32,
  ms: i32,
) -> f64 {
  let is_leap = |year: i32| (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
  let mut total_days = 0_i64;
  for current_year in 1970..year {
    total_days += if is_leap(current_year) { 366 } else { 365 };
  }
  let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  for current_month in 1..month {
    let mut days = days_in_month[current_month.min(12).to_usize().unwrap_or(12)] as i64;
    if current_month == 2 && is_leap(year) {
      days += 1;
    }
    total_days += days;
  }
  total_days += i64::from(day - 1);
  let total_seconds =
    total_days * 86_400 + i64::from(hour) * 3_600 + i64::from(min) * 60 + i64::from(sec);
  total_seconds.to_f64().unwrap_or(f64::NAN) * 1_000.0 + f64::from(ms)
}

fn date_value(this: Value) -> Value {
  if let Value::Object(object) = this
    && let Some(PropertyDescriptor::Data { value, .. }) = object
      .borrow()
      .properties
      .get(&PropertyKey::String("[[PrimitiveValue]]".into()))
  {
    return value.clone();
  }
  Value::Number(f64::NAN)
}

/// Creates the Date constructor, static methods, and prototype methods.
pub(crate) fn create_date_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> DateIntrinsics {
  let prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  let constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_prototype),
  ))));
  let prototype_for_constructor = Rc::clone(&prototype);
  let constructor_function: BuiltinFn = Rc::new(move |_vm, args| {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = match args.len() {
      1 => match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().to_f64().unwrap_or(f64::NAN),
        Err(_) => 0.0,
      },
      2 => args.get(1).map_or(0.0, to_num),
      _ => {
        let year = to_num(args.get(1).unwrap_or(&Value::Undefined))
          .to_i32()
          .unwrap_or(0);
        let month = to_num(args.get(2).unwrap_or(&Value::Undefined))
          .to_i32()
          .unwrap_or(0);
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
    let mut object =
      ObjectData::with_prototype(Value::Object(Rc::clone(&prototype_for_constructor)));
    object.properties.insert(
      PropertyKey::String("[[PrimitiveValue]]".into()),
      PropertyDescriptor::ordinary_data(Value::Number(timestamp)),
    );
    Ok(Value::Object(Rc::new(RefCell::new(object))))
  });
  let constructor = Value::BuiltinObject(constructor_function, Rc::clone(&constructor_properties));
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("prototype".into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&prototype))),
    );
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("now".into()),
      PropertyDescriptor::builtin_data(builtin_object(
        Rc::new(|_vm, _args| {
          let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(f64::NAN, |duration| duration.as_secs_f64() * 1_000.0);
          Ok(Value::Number(millis))
        }),
        function_prototype,
      )),
    );
  prototype.borrow_mut().properties.insert(
    PropertyKey::String("constructor".into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  prototype.borrow_mut().properties.insert(
    PropertyKey::String("valueOf".into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        Ok(date_value(
          args.first().cloned().unwrap_or(Value::Undefined),
        ))
      }),
      function_prototype,
    )),
  );
  prototype.borrow_mut().properties.insert(
    PropertyKey::String("getTime".into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        Ok(date_value(
          args.first().cloned().unwrap_or(Value::Undefined),
        ))
      }),
      function_prototype,
    )),
  );
  prototype.borrow_mut().properties.insert(
    PropertyKey::String("getTimezoneOffset".into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, _args| Ok(Value::Number(0.0))),
      function_prototype,
    )),
  );
  prototype.borrow_mut().properties.insert(
    PropertyKey::String("toISOString".into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        let Value::Number(number) = date_value(args.first().cloned().unwrap_or(Value::Undefined))
        else {
          return Ok(Value::String("Invalid Date".into()));
        };
        let millis = number.trunc().to_i64().unwrap_or_else(|| {
          if number.is_sign_negative() {
            i64::MIN
          } else {
            i64::MAX
          }
        });
        let seconds = millis / 1_000;
        let milliseconds = millis % 1_000;
        let year = 1970 + i32::try_from(seconds / (365 * 24 * 3_600)).unwrap_or(0);
        let day_of_year = i32::try_from((seconds % (365 * 24 * 3_600)) / (24 * 3_600)).unwrap_or(0);
        let hour = i32::try_from((seconds % (24 * 3_600)) / 3_600).unwrap_or(0);
        let minute = i32::try_from((seconds % 3_600) / 60).unwrap_or(0);
        let second = i32::try_from(seconds % 60).unwrap_or(0);
        Ok(Value::String(
          format!(
            "{year:04}-01-{:02}T{hour:02}:{minute:02}:{second:02}.{milliseconds:03}Z",
            day_of_year.clamp(1, 31),
          )
          .into(),
        ))
      }),
      function_prototype,
    )),
  );
  DateIntrinsics {
    constructor,
    prototype: Value::Object(prototype),
  }
}
