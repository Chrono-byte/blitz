// SPDX-License-Identifier: MPL-2.0
//! Date constructor and prototype methods.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use num_traits::ToPrimitive;

use crate::RuntimeError;
use crate::ecmascript::semantics::{builtin_object, set_builtin_length, set_builtin_name};
use crate::runtime_utils::static_builtin_arg_offset;
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

const MS_PER_SECOND: i64 = 1_000;
const MS_PER_MINUTE: i64 = 60 * MS_PER_SECOND;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;
const MAX_TIME: f64 = 8_640_000_000_000_000.0;
const DATE_SLOT: &str = "[[PrimitiveValue]]";

/// The Date constructor and its prototype object.
pub(crate) struct DateIntrinsics {
  pub(crate) constructor: Value,
  pub(crate) prototype: Value,
}

#[derive(Clone, Copy)]
struct DateParts {
  year: i64,
  month: i64,
  day: i64,
  weekday: i64,
  hour: i64,
  minute: i64,
  second: i64,
  millisecond: i64,
}

fn time_clip(value: f64) -> f64 {
  if value.is_finite() && value.abs() <= MAX_TIME {
    value.trunc()
  } else {
    f64::NAN
  }
}

// Howard Hinnant's proleptic Gregorian calendar algorithms, expressed with
// Euclidean division so years before 1970 behave the same as later years.
fn days_from_civil(
  year: i64,
  month: i64,
  day: i64,
) -> i64 {
  let year = year - i64::from(month <= 2);
  let era = year.div_euclid(400);
  let year_of_era = year - era * 400;
  let adjusted_month = month + if month > 2 { -3 } else { 9 };
  let day_of_year = (153 * adjusted_month + 2).div_euclid(5) + day - 1;
  let day_of_era =
    year_of_era * 365 + year_of_era.div_euclid(4) - year_of_era.div_euclid(100) + day_of_year;
  era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
  let shifted = days + 719_468;
  let era = shifted.div_euclid(146_097);
  let day_of_era = shifted - era * 146_097;
  let year_of_era = (day_of_era - day_of_era.div_euclid(1_460) + day_of_era.div_euclid(36_524)
    - day_of_era.div_euclid(146_096))
  .div_euclid(365);
  let mut year = year_of_era + era * 400;
  let day_of_year =
    day_of_era - (365 * year_of_era + year_of_era.div_euclid(4) - year_of_era.div_euclid(100));
  let month_piece = (5 * day_of_year + 2).div_euclid(153);
  let day = day_of_year - (153 * month_piece + 2).div_euclid(5) + 1;
  let month = month_piece + if month_piece < 10 { 3 } else { -9 };
  year += i64::from(month <= 2);
  (year, month, day)
}

fn make_date(
  year: f64,
  month: f64,
  day: f64,
  hour: f64,
  minute: f64,
  second: f64,
  millisecond: f64,
) -> f64 {
  if ![year, month, day, hour, minute, second, millisecond]
    .iter()
    .all(|value| value.is_finite())
  {
    return f64::NAN;
  }
  let year = year.trunc().to_i64().unwrap_or(i64::MAX);
  let month = month.trunc().to_i64().unwrap_or(i64::MAX);
  let normalized_year = year.saturating_add(month.div_euclid(12));
  let normalized_month = month.rem_euclid(12) + 1;
  let days = days_from_civil(normalized_year, normalized_month, 1).saturating_add(
    day
      .trunc()
      .to_i64()
      .unwrap_or(i64::MAX)
      .saturating_sub(1),
  );
  let time = hour * MS_PER_HOUR as f64
    + minute * MS_PER_MINUTE as f64
    + second * MS_PER_SECOND as f64
    + millisecond;
  time_clip(days as f64 * MS_PER_DAY as f64 + time)
}

fn parts(value: f64) -> Option<DateParts> {
  if !value.is_finite() {
    return None;
  }
  let millis = value.trunc().to_i64()?;
  let days = millis.div_euclid(MS_PER_DAY);
  let within_day = millis.rem_euclid(MS_PER_DAY);
  let (year, month, day) = civil_from_days(days);
  Some(DateParts {
    year,
    month: month - 1,
    day,
    weekday: (days + 4).rem_euclid(7),
    hour: within_day.div_euclid(MS_PER_HOUR),
    minute: within_day
      .rem_euclid(MS_PER_HOUR)
      .div_euclid(MS_PER_MINUTE),
    second: within_day
      .rem_euclid(MS_PER_MINUTE)
      .div_euclid(MS_PER_SECOND),
    millisecond: within_day.rem_euclid(MS_PER_SECOND),
  })
}

fn now() -> f64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or_else(
      |error| -(error.duration().as_secs_f64() * 1_000.0),
      |duration| duration.as_secs_f64() * 1_000.0,
    )
    .trunc()
}

fn date_number(
  vm: &mut Vm,
  value: &Value,
) -> Result<f64, RuntimeError> {
  let Value::Object(object) = value else {
    return Err(vm.throw_type_error("Date method called on incompatible receiver"));
  };
  let result = object
    .borrow()
    .internal_slot(DATE_SLOT)
    .and_then(|value| match value {
      Value::Number(number) => Some(*number),
      _ => None,
    });
  result.ok_or_else(|| vm.throw_type_error("Date method called on incompatible receiver"))
}

fn set_date_number(
  vm: &mut Vm,
  value: &Value,
  number: f64,
) -> Result<Value, RuntimeError> {
  let Value::Object(object) = value else {
    return Err(vm.throw_type_error("Date method called on incompatible receiver"));
  };
  if object.borrow().internal_slot(DATE_SLOT).is_none() {
    return Err(vm.throw_type_error("Date method called on incompatible receiver"));
  }
  object
    .borrow_mut()
    .set_internal_slot(DATE_SLOT, Value::Number(number));
  Ok(Value::Number(number))
}

fn number_argument(
  vm: &mut Vm,
  args: &[Value],
  index: usize,
  default: f64,
) -> Result<f64, RuntimeError> {
  match args.get(index) {
    Some(value) => vm.to_number_value(value),
    None => Ok(default),
  }
}

fn parse_digits(input: &[u8]) -> Option<i64> {
  if input.is_empty() || !input.iter().all(u8::is_ascii_digit) {
    return None;
  }
  std::str::from_utf8(input).ok()?.parse().ok()
}

fn parse_iso_date(input: &str) -> f64 {
  let bytes = input.as_bytes();
  let (year, mut cursor) = if bytes.first() == Some(&b'+') || bytes.first() == Some(&b'-') {
    if bytes.len() < 7 {
      return f64::NAN;
    }
    let magnitude = parse_digits(&bytes[1..7]);
    let Some(magnitude) = magnitude else {
      return f64::NAN;
    };
    (
      if bytes[0] == b'-' {
        -magnitude
      } else {
        magnitude
      },
      7,
    )
  } else {
    if bytes.len() < 4 {
      return f64::NAN;
    }
    let Some(year) = parse_digits(&bytes[..4]) else {
      return f64::NAN;
    };
    (year, 4)
  };
  let mut month = 1;
  let mut day = 1;
  if bytes.get(cursor) == Some(&b'-') {
    if bytes.len() < cursor + 3 {
      return f64::NAN;
    }
    let Some(parsed) = parse_digits(&bytes[cursor + 1..cursor + 3]) else {
      return f64::NAN;
    };
    month = parsed;
    cursor += 3;
    if bytes.get(cursor) == Some(&b'-') {
      if bytes.len() < cursor + 3 {
        return f64::NAN;
      }
      let Some(parsed) = parse_digits(&bytes[cursor + 1..cursor + 3]) else {
        return f64::NAN;
      };
      day = parsed;
      cursor += 3;
    }
  }
  let leap =
    |year: i64| year.rem_euclid(4) == 0 && year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0;
  let month_days = match month {
    2 if leap(year) => 29,
    2 => 28,
    4 | 6 | 9 | 11 => 30,
    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
    _ => 0,
  };
  if day < 1 || day > month_days {
    return f64::NAN;
  }
  let mut hour = 0;
  let mut minute = 0;
  let mut second = 0;
  let mut millisecond = 0;
  let mut offset_minutes = 0;
  if cursor < bytes.len() {
    if !matches!(bytes[cursor], b'T' | b't' | b' ') || bytes.len() < cursor + 6 {
      return f64::NAN;
    }
    let Some(parsed_hour) = parse_digits(&bytes[cursor + 1..cursor + 3]) else {
      return f64::NAN;
    };
    if bytes[cursor + 3] != b':' {
      return f64::NAN;
    }
    let Some(parsed_minute) = parse_digits(&bytes[cursor + 4..cursor + 6]) else {
      return f64::NAN;
    };
    hour = parsed_hour;
    minute = parsed_minute;
    cursor += 6;
    if bytes.get(cursor) == Some(&b':') {
      if bytes.len() < cursor + 3 {
        return f64::NAN;
      }
      let Some(parsed_second) = parse_digits(&bytes[cursor + 1..cursor + 3]) else {
        return f64::NAN;
      };
      second = parsed_second;
      cursor += 3;
      if bytes.get(cursor) == Some(&b'.') {
        let start = cursor + 1;
        cursor = start;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
          cursor += 1;
        }
        if cursor == start {
          return f64::NAN;
        }
        let digits = &bytes[start..cursor];
        let take = digits.len().min(3);
        let Some(mut fraction) = parse_digits(&digits[..take]) else {
          return f64::NAN;
        };
        for _ in take..3 {
          fraction *= 10;
        }
        millisecond = fraction;
      }
    }
    if hour > 24 || minute > 59 || second > 59 || (hour == 24 && (minute != 0 || second != 0)) {
      return f64::NAN;
    }
    match bytes.get(cursor) {
      None => {}
      Some(b'Z' | b'z') if cursor + 1 == bytes.len() => {}
      Some(sign @ (b'+' | b'-')) if bytes.len() == cursor + 6 && bytes[cursor + 3] == b':' => {
        let Some(offset_hour) = parse_digits(&bytes[cursor + 1..cursor + 3]) else {
          return f64::NAN;
        };
        let Some(offset_minute) = parse_digits(&bytes[cursor + 4..cursor + 6]) else {
          return f64::NAN;
        };
        if offset_hour > 23 || offset_minute > 59 {
          return f64::NAN;
        }
        let offset = offset_hour * 60 + offset_minute;
        offset_minutes = if *sign == b'+' { offset } else { -offset };
      }
      _ => return f64::NAN,
    }
  }
  let value = make_date(
    year as f64,
    (month - 1) as f64,
    day as f64,
    hour as f64,
    minute as f64,
    second as f64,
    millisecond as f64,
  );
  time_clip(value - offset_minutes as f64 * MS_PER_MINUTE as f64)
}

fn iso_string(value: f64) -> Option<String> {
  let value_parts = parts(value)?;
  let year = match value_parts.year {
    0..=9999 => format!("{:04}", value_parts.year),
    year if year < 0 => format!("-{:06}", year.unsigned_abs()),
    year => format!("+{year:06}"),
  };
  Some(format!(
    "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
    value_parts.month + 1,
    value_parts.day,
    value_parts.hour,
    value_parts.minute,
    value_parts.second,
    value_parts.millisecond,
  ))
}

fn readable_string(value: f64) -> String {
  const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  let Some(value_parts) = parts(value) else {
    return "Invalid Date".to_string();
  };
  format!(
    "{} {} {:02} {:04} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
    WEEKDAYS[value_parts.weekday as usize],
    MONTHS[value_parts.month as usize],
    value_parts.day,
    value_parts.year,
    value_parts.hour,
    value_parts.minute,
    value_parts.second,
  )
}

fn utc_string(value: f64) -> String {
  const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  let Some(value_parts) = parts(value) else {
    return "Invalid Date".to_string();
  };
  format!(
    "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
    WEEKDAYS[value_parts.weekday as usize],
    value_parts.day,
    MONTHS[value_parts.month as usize],
    value_parts.year,
    value_parts.hour,
    value_parts.minute,
    value_parts.second,
  )
}

fn install(
  target: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  key: PropertyKey,
  name: &str,
  length: usize,
  function: BuiltinFn,
) {
  let value = builtin_object(function, function_prototype);
  set_builtin_name(&value, name);
  set_builtin_length(&value, length as f64);
  target
    .borrow_mut()
    .properties
    .insert(key, PropertyDescriptor::builtin_data(value));
}

fn replace_parts(
  vm: &mut Vm,
  args: &[Value],
  minimum: usize,
  update: impl FnOnce(&mut DateParts, &mut Vm, &[Value]) -> Result<(), RuntimeError>,
) -> Result<Value, RuntimeError> {
  let receiver = args.first().cloned().unwrap_or(Value::Undefined);
  let current = date_number(vm, &receiver)?;
  let base = if current.is_finite() {
    current
  } else if minimum == 6 {
    0.0
  } else {
    return set_date_number(vm, &receiver, f64::NAN);
  };
  let mut value_parts = parts(base).expect("finite clipped Date has calendar parts");
  update(&mut value_parts, vm, args)?;
  let value = make_date(
    value_parts.year as f64,
    value_parts.month as f64,
    value_parts.day as f64,
    value_parts.hour as f64,
    value_parts.minute as f64,
    value_parts.second as f64,
    value_parts.millisecond as f64,
  );
  set_date_number(vm, &receiver, value)
}

/// Creates the Date constructor, static methods, and prototype methods.
pub(crate) fn create_date_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> DateIntrinsics {
  let prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  prototype
    .borrow_mut()
    .set_internal_slot(DATE_SLOT, Value::Number(f64::NAN));
  let constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_prototype),
  ))));
  let prototype_for_constructor = Rc::clone(&prototype);
  let constructor_function: BuiltinFn = Rc::new(move |vm, args| {
    let constructing = vm.pending_constructor_new_target().is_some();
    if !constructing {
      return Ok(Value::String(readable_string(now()).into()));
    }
    let offset = static_builtin_arg_offset(vm, args, "Date");
    let supplied = args.len().saturating_sub(offset);
    let timestamp = match supplied {
      0 => now(),
      1 => {
        let primitive = vm.to_primitive(&args[offset], None)?;
        match primitive {
          Value::String(string) => parse_iso_date(string.as_str_lossy()),
          other => time_clip(vm.to_number_value(&other)?),
        }
      }
      _ => {
        let mut year = number_argument(vm, args, offset, f64::NAN)?;
        if year.is_finite() && (0.0..=99.0).contains(&year.trunc()) {
          year += 1_900.0;
        }
        make_date(
          year,
          number_argument(vm, args, offset + 1, f64::NAN)?,
          number_argument(vm, args, offset + 2, 1.0)?,
          number_argument(vm, args, offset + 3, 0.0)?,
          number_argument(vm, args, offset + 4, 0.0)?,
          number_argument(vm, args, offset + 5, 0.0)?,
          number_argument(vm, args, offset + 6, 0.0)?,
        )
      }
    };
    let instance_prototype = vm
      .pending_constructor_new_target()
      .and_then(|new_target| {
        vm.get_property_host(&new_target, &PropertyKey::prototype())
          .ok()
      })
      .filter(Value::is_object)
      .unwrap_or_else(|| Value::Object(Rc::clone(&prototype_for_constructor)));
    let mut object = ObjectData::with_prototype(instance_prototype);
    object.set_internal_slot(DATE_SLOT, Value::Number(timestamp));
    Ok(Value::Object(Rc::new(RefCell::new(object))))
  });
  let constructor = Value::BuiltinObject(constructor_function, Rc::clone(&constructor_properties));
  set_builtin_name(&constructor, "Date");
  set_builtin_length(&constructor, 7.0);
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::prototype(),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&prototype))),
    );

  install(
    &constructor_properties,
    function_prototype,
    PropertyKey::String("now".into()),
    "now",
    0,
    Rc::new(|_vm, _args| Ok(Value::Number(now()))),
  );
  install(
    &constructor_properties,
    function_prototype,
    PropertyKey::String("parse".into()),
    "parse",
    1,
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Date");
      let input = vm.to_string(args.get(offset).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(parse_iso_date(input.as_str_lossy())))
    }),
  );
  install(
    &constructor_properties,
    function_prototype,
    PropertyKey::String("UTC".into()),
    "UTC",
    7,
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Date");
      let mut year = number_argument(vm, args, offset, f64::NAN)?;
      if year.is_finite() && (0.0..=99.0).contains(&year.trunc()) {
        year += 1_900.0;
      }
      Ok(Value::Number(make_date(
        year,
        number_argument(vm, args, offset + 1, 0.0)?,
        number_argument(vm, args, offset + 2, 1.0)?,
        number_argument(vm, args, offset + 3, 0.0)?,
        number_argument(vm, args, offset + 4, 0.0)?,
        number_argument(vm, args, offset + 5, 0.0)?,
        number_argument(vm, args, offset + 6, 0.0)?,
      )))
    }),
  );

  prototype.borrow_mut().properties.insert(
    PropertyKey::String("constructor".into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  for name in ["valueOf", "getTime"] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      0,
      Rc::new(|vm, args| {
        Ok(Value::Number(date_number(
          vm,
          args.first().unwrap_or(&Value::Undefined),
        )?))
      }),
    );
  }

  type PartReader = fn(DateParts) -> i64;
  for (name, reader) in [
    ("getFullYear", (|parts: DateParts| parts.year) as PartReader),
    ("getUTCFullYear", |parts: DateParts| parts.year),
    ("getMonth", |parts: DateParts| parts.month),
    ("getUTCMonth", |parts: DateParts| parts.month),
    ("getDate", |parts: DateParts| parts.day),
    ("getUTCDate", |parts: DateParts| parts.day),
    ("getDay", |parts: DateParts| parts.weekday),
    ("getUTCDay", |parts: DateParts| parts.weekday),
    ("getHours", |parts: DateParts| parts.hour),
    ("getUTCHours", |parts: DateParts| parts.hour),
    ("getMinutes", |parts: DateParts| parts.minute),
    ("getUTCMinutes", |parts: DateParts| parts.minute),
    ("getSeconds", |parts: DateParts| parts.second),
    ("getUTCSeconds", |parts: DateParts| parts.second),
    ("getMilliseconds", |parts: DateParts| parts.millisecond),
    ("getUTCMilliseconds", |parts: DateParts| parts.millisecond),
    ("getYear", |parts: DateParts| parts.year - 1_900),
  ] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      0,
      Rc::new(move |vm, args| {
        let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
        Ok(parts(value).map_or(Value::Number(f64::NAN), |parts| {
          Value::Number(reader(parts) as f64)
        }))
      }),
    );
  }
  install(
    &prototype,
    function_prototype,
    PropertyKey::String("getTimezoneOffset".into()),
    "getTimezoneOffset",
    0,
    Rc::new(|vm, args| {
      let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(if value.is_finite() {
        0.0
      } else {
        f64::NAN
      }))
    }),
  );

  for name in ["toString", "toLocaleString"] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      0,
      Rc::new(|vm, args| {
        let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
        Ok(Value::String(readable_string(value).into()))
      }),
    );
  }
  for name in ["toUTCString", "toGMTString"] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      0,
      Rc::new(|vm, args| {
        let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
        Ok(Value::String(utc_string(value).into()))
      }),
    );
  }
  for name in ["toDateString", "toLocaleDateString"] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      0,
      Rc::new(|vm, args| {
        let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
        let text = readable_string(value);
        Ok(Value::String(
          text
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ")
            .into(),
        ))
      }),
    );
  }
  for name in ["toTimeString", "toLocaleTimeString"] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      0,
      Rc::new(|vm, args| {
        let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
        let text = readable_string(value);
        Ok(Value::String(
          text
            .split_whitespace()
            .skip(4)
            .collect::<Vec<_>>()
            .join(" ")
            .into(),
        ))
      }),
    );
  }
  install(
    &prototype,
    function_prototype,
    PropertyKey::String("toISOString".into()),
    "toISOString",
    0,
    Rc::new(|vm, args| {
      let value = date_number(vm, args.first().unwrap_or(&Value::Undefined))?;
      let Some(result) = iso_string(value) else {
        return Err(vm.throw_range_error("Invalid time value"));
      };
      Ok(Value::String(result.into()))
    }),
  );
  install(
    &prototype,
    function_prototype,
    PropertyKey::String("toJSON".into()),
    "toJSON",
    1,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let primitive = vm.to_primitive(&receiver, Some("number"))?;
      if matches!(primitive, Value::Number(number) if !number.is_finite()) {
        return Ok(Value::Null);
      }
      let method = vm.get_property_host(&receiver, &PropertyKey::String("toISOString".into()))?;
      if !method.is_callable() {
        return Err(vm.throw_type_error("Date toISOString property is not callable"));
      }
      vm.call_value_with_this(&method, &receiver, &[])
    }),
  );

  install(
    &prototype,
    function_prototype,
    PropertyKey::String("setTime".into()),
    "setTime",
    1,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let value = time_clip(number_argument(vm, args, 1, f64::NAN)?);
      set_date_number(vm, &receiver, value)
    }),
  );

  macro_rules! setters {
    ($names:expr, $length:expr, $body:expr) => {
      for name in $names {
        install(
          &prototype,
          function_prototype,
          PropertyKey::String(name.into()),
          name,
          $length,
          Rc::new(|vm, args| replace_parts(vm, args, 0, $body)),
        );
      }
    };
  }
  setters!(
    ["setMilliseconds", "setUTCMilliseconds"],
    1,
    |parts: &mut DateParts, vm: &mut Vm, args: &[Value]| {
      parts.millisecond = number_argument(vm, args, 1, f64::NAN)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      Ok(())
    }
  );
  setters!(
    ["setSeconds", "setUTCSeconds"],
    2,
    |parts: &mut DateParts, vm: &mut Vm, args: &[Value]| {
      parts.second = number_argument(vm, args, 1, f64::NAN)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.millisecond = number_argument(vm, args, 2, parts.millisecond as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      Ok(())
    }
  );
  setters!(
    ["setMinutes", "setUTCMinutes"],
    3,
    |parts: &mut DateParts, vm: &mut Vm, args: &[Value]| {
      parts.minute = number_argument(vm, args, 1, f64::NAN)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.second = number_argument(vm, args, 2, parts.second as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.millisecond = number_argument(vm, args, 3, parts.millisecond as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      Ok(())
    }
  );
  setters!(
    ["setHours", "setUTCHours"],
    4,
    |parts: &mut DateParts, vm: &mut Vm, args: &[Value]| {
      parts.hour = number_argument(vm, args, 1, f64::NAN)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.minute = number_argument(vm, args, 2, parts.minute as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.second = number_argument(vm, args, 3, parts.second as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.millisecond = number_argument(vm, args, 4, parts.millisecond as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      Ok(())
    }
  );
  setters!(
    ["setDate", "setUTCDate"],
    1,
    |parts: &mut DateParts, vm: &mut Vm, args: &[Value]| {
      parts.day = number_argument(vm, args, 1, f64::NAN)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      Ok(())
    }
  );
  setters!(
    ["setMonth", "setUTCMonth"],
    2,
    |parts: &mut DateParts, vm: &mut Vm, args: &[Value]| {
      parts.month = number_argument(vm, args, 1, f64::NAN)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      parts.day = number_argument(vm, args, 2, parts.day as f64)?
        .trunc()
        .to_i64()
        .unwrap_or(i64::MAX);
      Ok(())
    }
  );
  for name in ["setFullYear", "setUTCFullYear"] {
    install(
      &prototype,
      function_prototype,
      PropertyKey::String(name.into()),
      name,
      3,
      Rc::new(|vm, args| {
        replace_parts(vm, args, 6, |parts, vm, args| {
          parts.year = number_argument(vm, args, 1, f64::NAN)?
            .trunc()
            .to_i64()
            .unwrap_or(i64::MAX);
          parts.month = number_argument(vm, args, 2, parts.month as f64)?
            .trunc()
            .to_i64()
            .unwrap_or(i64::MAX);
          parts.day = number_argument(vm, args, 3, parts.day as f64)?
            .trunc()
            .to_i64()
            .unwrap_or(i64::MAX);
          Ok(())
        })
      }),
    );
  }
  install(
    &prototype,
    function_prototype,
    PropertyKey::String("setYear".into()),
    "setYear",
    1,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let mut year = number_argument(vm, args, 1, f64::NAN)?;
      if !year.is_finite() {
        return set_date_number(vm, &receiver, f64::NAN);
      }
      year = year.trunc();
      if (0.0..=99.0).contains(&year) {
        year += 1_900.0;
      }
      let base = date_number(vm, &receiver)?;
      let mut value_parts =
        parts(if base.is_finite() { base } else { 0.0 }).expect("epoch has valid calendar parts");
      value_parts.year = year.to_i64().unwrap_or(i64::MAX);
      let value = make_date(
        value_parts.year as f64,
        value_parts.month as f64,
        value_parts.day as f64,
        value_parts.hour as f64,
        value_parts.minute as f64,
        value_parts.second as f64,
        value_parts.millisecond as f64,
      );
      set_date_number(vm, &receiver, value)
    }),
  );

  install(
    &prototype,
    function_prototype,
    PropertyKey::Symbol(SymbolId::well_known_to_primitive()),
    "[Symbol.toPrimitive]",
    1,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      if !receiver.is_object() {
        return Err(vm.throw_type_error("Date @@toPrimitive receiver must be an object"));
      }
      let Some(Value::String(hint)) = args.get(1) else {
        return Err(vm.throw_type_error("Invalid hint for Date @@toPrimitive"));
      };
      let order = match hint.as_str_lossy() {
        "number" => ["valueOf", "toString"],
        "string" | "default" => ["toString", "valueOf"],
        _ => return Err(vm.throw_type_error("Invalid hint for Date @@toPrimitive")),
      };
      for name in order {
        let method =
          vm.get_property_host(&receiver, &PropertyKey::String(name.to_string().into()))?;
        if Vm::is_callable_value(&method) {
          let result = vm.call_value_with_this(&method, &receiver, &[])?;
          if !result.is_object() {
            return Ok(result);
          }
        }
      }
      Err(vm.throw_type_error("Date @@toPrimitive could not produce a primitive"))
    }),
  );
  if let Some(PropertyDescriptor::Data { writable, .. }) = prototype
    .borrow_mut()
    .properties
    .get_mut(&PropertyKey::Symbol(SymbolId::well_known_to_primitive()))
  {
    *writable = false;
  }

  DateIntrinsics {
    constructor,
    prototype: Value::Object(prototype),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compiler;
  use crate::parser::Parser;
  use crate::runtime_utils;

  fn run(source: &str) -> Vm {
    let session = Parser::parse_source(source).expect("parse Date test");
    let chunk = compiler::compile_session(&session).expect("compile Date test");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run Date test");
    vm
  }

  #[test]
  fn date_uses_gregorian_calendar_and_iso_parsing() {
    let vm = run(crate::js!(
      r#"
        const leap = new Date("2024-02-29T23:05:06.007Z");
        const beforeEpoch = new Date("1969-12-31T23:59:59.999Z");
        globalThis.result = [
          leap.toISOString(),
          leap.getUTCFullYear(),
          leap.getUTCMonth(),
          leap.getUTCDate(),
          leap.getUTCDay(),
          beforeEpoch.getTime(),
          beforeEpoch.toISOString(),
          Date.UTC(99, 0, 1),
        ].join("|");
      "#
    ));
    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "2024-02-29T23:05:06.007Z|2024|1|29|4|-1|1969-12-31T23:59:59.999Z|915148800000".into()
      ))
    );
  }

  #[test]
  fn date_setters_normalize_and_invalid_dates_follow_the_contract() {
    let vm = run(crate::js!(
      r#"
        const value = new Date("2020-01-31T00:00:00.000Z");
        value.setUTCMonth(1);
        const invalid = new Date(NaN);
        let isoThrows = false;
        try { invalid.toISOString(); } catch (error) { isoThrows = error instanceof RangeError; }
        invalid.setUTCFullYear(2000, 1, 29);
        globalThis.result = [
          value.toISOString(),
          invalid.toISOString(),
          new Date(0).toJSON(),
          new Date(NaN).toJSON(),
          isoThrows,
        ].join("|");
      "#
    ));
    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "2020-03-02T00:00:00.000Z|2000-02-29T00:00:00.000Z|1970-01-01T00:00:00.000Z||true".into()
      ))
    );
  }
}
