// SPDX-License-Identifier: MPL-2.0
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::js_number_to_string;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::runtime::property::{object_identity, values_equal};
use num_bigint::BigInt as BigIntValue;
use num_traits::FromPrimitive;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

fn compare_bigint_number(
  integer: &BigIntValue,
  number: f64,
) -> Option<Ordering> {
  if number.is_nan() {
    return None;
  }
  if number == f64::INFINITY {
    return Some(Ordering::Less);
  }
  if number == f64::NEG_INFINITY {
    return Some(Ordering::Greater);
  }

  let truncated = BigIntValue::from_f64(number.trunc())?;
  match integer.cmp(&truncated) {
    Ordering::Equal if number.fract() > 0.0 => Some(Ordering::Less),
    Ordering::Equal if number.fract() < 0.0 => Some(Ordering::Greater),
    ordering => Some(ordering),
  }
}

/// Convert a runtime value with ECMAScript `ToInt32` semantics.
///
/// # Errors
///
/// Returns a `RuntimeError` if the value cannot be converted to an `i32`.
pub(super) fn to_int32_with_vm(
  vm: &mut Vm,
  v: &Value,
) -> Result<i32, RuntimeError> {
  let u = to_uint32_with_vm(vm, v)?;
  if u & 0x8000_0000 != 0 {
    Ok(u as i32)
  } else {
    Ok((u as i32) & 0x7FFF_FFFF)
  }
}

/// Convert a runtime value with ECMAScript `ToUint32` semantics.
///
/// # Errors
///
/// Returns a `RuntimeError` if the value cannot be converted to a `u32`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(super) fn to_uint32_with_vm(
  vm: &mut Vm,
  v: &Value,
) -> Result<u32, RuntimeError> {
  let n = vm.to_number_value(v)?;

  if n.is_nan() || n == 0.0_f64 || n.is_infinite() {
    return Ok(0);
  }

  const TWO32: f64 = 4294967296.0;
  let int_part = n.trunc();
  let vmod = int_part.rem_euclid(TWO32);
  // vmod is in the range [0, 2^32), so this cast is safe
  let result = vmod as u32;
  Ok(result)
}

/// Parse a JavaScript number string into a `f64`.
pub(super) fn parse_js_number_string(s: &js_string::JsString) -> f64 {
  let s = s.as_str_lossy().trim();
  if s.is_empty() {
    return 0.0;
  }
  if matches!(s, "Infinity" | "+Infinity") {
    return f64::INFINITY;
  }
  if s == "-Infinity" {
    return f64::NEG_INFINITY;
  }

  if let Some(hex) = s
    .strip_prefix("0x")
    .or_else(|| s.strip_prefix("0X"))
  {
    return parse_radix_number_string(hex, 16).unwrap_or(f64::NAN);
  }
  if let Some(bin) = s
    .strip_prefix("0b")
    .or_else(|| s.strip_prefix("0B"))
  {
    return parse_radix_number_string(bin, 2).unwrap_or(f64::NAN);
  }
  if let Some(oct) = s
    .strip_prefix("0o")
    .or_else(|| s.strip_prefix("0O"))
  {
    return parse_radix_number_string(oct, 8).unwrap_or(f64::NAN);
  }

  let (sign, body) = match s.as_bytes().first() {
    Some(b'+') => (1.0_f64, &s[1..]),
    Some(b'-') => (-1.0_f64, &s[1..]),
    _ => (1.0_f64, s),
  };

  sign * body.parse::<f64>().unwrap_or(f64::NAN)
}

/// Parse a radix number string into a `f64`.
fn parse_radix_number_string(
  body: &str,
  radix: u32,
) -> Option<f64> {
  if body.is_empty() {
    return None;
  }

  let mut value = 0.0_f64;
  let radix_f = radix as f64;

  for byte in body.bytes() {
    let digit = match byte {
      b'0'..=b'9' => u32::from(byte - b'0'),
      b'a'..=b'z' => u32::from(byte - b'a' + 10),
      b'A'..=b'Z' => u32::from(byte - b'A' + 10),
      _ => return None,
    };
    if digit >= radix {
      return None;
    }
    value = value * radix_f + digit as f64;
  }

  Some(value)
}

pub(super) fn parse_js_bigint_string(s: &js_string::JsString) -> Option<BigIntValue> {
  let s = s.as_str_lossy().trim();
  if s.is_empty() {
    return None;
  }
  let (neg, body) = match s.as_bytes().first() {
    Some(b'+') => (false, &s[1..]),
    Some(b'-') => (true, &s[1..]),
    _ => (false, s),
  };
  let mut parsed = if let Some(hex) = body
    .strip_prefix("0x")
    .or_else(|| body.strip_prefix("0X"))
  {
    BigIntValue::parse_bytes(hex.as_bytes(), 16)?
  } else if let Some(bin) = body
    .strip_prefix("0b")
    .or_else(|| body.strip_prefix("0B"))
  {
    BigIntValue::parse_bytes(bin.as_bytes(), 2)?
  } else if let Some(oct) = body
    .strip_prefix("0o")
    .or_else(|| body.strip_prefix("0O"))
  {
    BigIntValue::parse_bytes(oct.as_bytes(), 8)?
  } else {
    BigIntValue::parse_bytes(body.as_bytes(), 10)?
  };
  if neg {
    parsed = -parsed;
  }
  Some(parsed)
}

impl Vm {
  /// Convert a runtime value with ECMAScript `ToInt32` semantics.
  ///
  /// # Errors
  ///
  /// Returns a `RuntimeError` if the value cannot be converted to an `i32`.
  pub fn to_int32_value(
    &mut self,
    v: &Value,
  ) -> Result<i32, RuntimeError> {
    to_int32_with_vm(self, v)
  }

  /// Convert a runtime value with ECMAScript `ToUint32` semantics.
  ///
  /// # Errors
  ///
  /// Returns a `RuntimeError` if the value cannot be converted to a `u32`.
  pub fn to_uint32_value(
    &mut self,
    v: &Value,
  ) -> Result<u32, RuntimeError> {
    to_uint32_with_vm(self, v)
  }
}

impl PartialOrd for Value {
  fn partial_cmp(
    &self,
    other: &Value,
  ) -> Option<std::cmp::Ordering> {
    match (self, other) {
      // Both numbers: numeric comparison
      (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
      // Both strings: string comparison
      (Value::String(a), Value::String(b)) => a.partial_cmp(b),
      (Value::BigInt(a), Value::BigInt(b)) => a.partial_cmp(b),
      // Number and String: convert string to number
      (Value::Number(a), Value::String(b)) => {
        let b_num = parse_js_number_string(b);
        if b_num.is_nan() {
          None
        } else {
          a.partial_cmp(&b_num)
        }
      }
      // String and Number: convert string to number
      (Value::String(a), Value::Number(b)) => {
        let a_num = parse_js_number_string(a);
        if a_num.is_nan() {
          None
        } else {
          a_num.partial_cmp(b)
        }
      }
      (Value::BigInt(a), Value::Number(b)) => compare_bigint_number(a, *b),
      (Value::Number(a), Value::BigInt(b)) => compare_bigint_number(b, *a).map(Ordering::reverse),
      (Value::BigInt(a), Value::String(b)) => {
        parse_js_bigint_string(b).and_then(|bb| a.partial_cmp(&bb))
      }
      (Value::String(a), Value::BigInt(b)) => {
        parse_js_bigint_string(a).and_then(|aa| aa.partial_cmp(b))
      }
      // Boolean: convert to number (1.0 or 0.0)
      (Value::Bool(a), other) => {
        let a_num = if *a { 1.0 } else { 0.0 };
        Value::Number(a_num).partial_cmp(other)
      }
      (other, Value::Bool(b)) => {
        let b_num = if *b { 1.0 } else { 0.0 };
        other.partial_cmp(&Value::Number(b_num))
      }
      _ => None,
    }
  }
}

impl Vm {
  pub fn to_object_value(
    &mut self,
    v: Value,
  ) -> Result<Value, RuntimeError> {
    match v {
      Value::Object(_)
      | Value::Array(..)
      | Value::Proxy(_)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _)
      | Value::Promise(..) => Ok(v),
      Value::Undefined | Value::Null => {
        Err(self.throw_type_error("Cannot convert undefined or null to object"))
      }
      Value::Bool(b) => {
        let ctor = self
          .get_global("Boolean")
          .unwrap_or(Value::Undefined);
        self.construct_value_with_new_target(&ctor, &[Value::Bool(b)], ctor.clone())
      }
      Value::Number(n) => {
        let ctor = self
          .get_global("Number")
          .unwrap_or(Value::Undefined);
        self.construct_value_with_new_target(&ctor, &[Value::Number(n)], ctor.clone())
      }
      Value::String(s) => {
        let ctor = self
          .get_global("String")
          .unwrap_or(Value::Undefined);
        self.construct_value_with_new_target(&ctor, &[Value::String(s)], ctor.clone())
      }
      Value::BigInt(n) => {
        let ctor = self
          .get_global("BigInt")
          .unwrap_or(Value::Undefined);
        self.construct_value(&ctor, &[Value::BigInt(n)])
      }
      Value::Symbol(id, desc) => {
        // Wrap Symbol in an object.
        let mut data = ObjectData::new();
        data.set_internal_slot("[[SymbolData]]", Value::Symbol(id, desc));
        // Try to find Symbol.prototype
        if let Some(proto) = self
          .realm_state
          .realm
          .intrinsics
          .symbol_prototype
          .clone()
        {
          data.set_prototype_value(proto);
        }
        Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(data)))))
      }
      Value::Uninitialized => Err(self.throw_type_error("Uninitialized value")),
      Value::Builtin(_) => {
        // Builtins are callable values and are returned unchanged here.
        Ok(v)
      }
    }
  }

  /// Parse and evaluate a single expression source string in a forked VM
  /// execution context.
  ///
  /// The expression result is returned directly.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the evaluation fails.
  pub fn to_primitive(
    &mut self,
    v: &Value,
    hint: Option<&str>,
  ) -> Result<Value, RuntimeError> {
    match v {
      Value::Object(_)
      | Value::Array(..)
      | Value::Proxy(_)
      | Value::Closure { .. }
      | Value::Builtin(_)
      | Value::BuiltinObject(_, _)
      | Value::Promise(..) => {
        let temp_root_base = self.roots.temporary.len();
        self.roots.temporary.push(v.clone());
        if crate::trace::enabled() {
          eprintln!(
            "[trace] to_primitive called on {} with hint={:?}",
            v.type_name(),
            hint
          );
          // Print the full value being coerced to help debug which
          // object instance is visited during ToPrimitive calls.
          crate::trace::value_string("[trace] to_primitive value", v);
        }
        // GetMethod(value, @@toPrimitive) must perform an observable property
        // access, including accessors and Proxy traps.
        let to_prim_key = PropertyKey::Symbol(SymbolId::well_known_to_primitive());
        let method = self.get_property_with_receiver(v, &to_prim_key, v)?;
        if !matches!(method, Value::Undefined | Value::Null) {
          if !Self::is_callable_value(&method) {
            self.roots.temporary.truncate(temp_root_base);
            return Err(self.throw_type_error("@@toPrimitive is not callable"));
          }
          if crate::trace::enabled() {
            crate::trace::value_string("[trace] @@toPrimitive candidate method", &method);
            crate::trace::value_string("[trace] @@toPrimitive receiver", v);
            if let Some(id) = object_identity(v) {
              eprintln!("[trace] @@toPrimitive receiver identity={id}");
            }
          }
          let rv = self.call_with_this(
            &method,
            v,
            &[Value::String(hint.unwrap_or("default").to_string().into())],
          )?;
          if crate::trace::enabled() {
            crate::trace::value_string("[trace] @@toPrimitive returned", &rv);
            eprintln!("[trace] @@toPrimitive return type = {}", rv.type_name());
          }
          if !matches!(
            rv,
            Value::Object(_)
              | Value::Array(..)
              | Value::Proxy(_)
              | Value::Closure { .. }
              | Value::Builtin(_)
              | Value::BuiltinObject(_, _)
              | Value::Promise(..)
          ) {
            if crate::trace::enabled() {
              crate::trace::value_string("[trace] @@toPrimitive accepted primitive", &rv);
            }
            self.roots.temporary.truncate(temp_root_base);
            return Ok(rv);
          }
          if crate::trace::enabled() {
            crate::trace::value_string("[trace] @@toPrimitive produced non-primitive", &rv);
          }
          self.roots.temporary.truncate(temp_root_base);
          return Err(self.throw_type_error("@@toPrimitive must return a primitive value"));
        }

        let order = match hint {
          Some("string") => ["toString", "valueOf"],
          Some("number") => ["valueOf", "toString"],
          _ => ["valueOf", "toString"],
        };
        for name in order {
          let key = PropertyKey::String(name.to_string().into());
          let method = self.get_property_with_receiver(v, &key, v)?;
          if Self::is_callable_value(&method) {
            if crate::trace::enabled() {
              crate::trace::value_string(&format!("[trace] candidate method '{}':", name), &method);
              crate::trace::value_string("[trace] candidate receiver", v);
              if let Some(id) = object_identity(v) {
                eprintln!("[trace] candidate receiver identity={id}");
              }
            }
            let rv = self.call_with_this(&method, v, &[])?;
            if crate::trace::enabled() {
              crate::trace::value_string(&format!("[trace] {} returned", name), &rv);
              eprintln!("[trace] {} return type = {}", name, rv.type_name());
            }
            if !matches!(
              rv,
              Value::Object(_)
                | Value::Array(..)
                | Value::Proxy(_)
                | Value::Closure { .. }
                | Value::Builtin(_)
                | Value::BuiltinObject(_, _)
                | Value::Promise(..)
            ) {
              self.roots.temporary.truncate(temp_root_base);
              return Ok(rv);
            }
          }
        }
        self.roots.temporary.truncate(temp_root_base);
        Err(self.throw_type_error("Cannot convert object to primitive value"))
      }
      _ => Ok(v.clone()),
    }
  }

  /// Apply ECMAScript `ToString` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_string(
    &mut self,
    v: &Value,
  ) -> Result<js_string::JsString, RuntimeError> {
    match v {
      Value::String(s) => Ok(s.clone()),
      Value::Number(n) => Ok(js_number_to_string(*n).into()),
      Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
      Value::Undefined => Ok("undefined".into()),
      Value::Null => Ok("null".into()),
      Value::BigInt(n) => Ok(n.to_string().into()),
      Value::Symbol(_, _) => {
        Err(self.throw_type_error("Cannot convert a Symbol value to a string"))
      }
      Value::Promise(..) => Ok("[object Promise]".into()),
      _ => {
        let prim = self.to_primitive(v, Some("string"))?;
        self.to_string(&prim)
      }
    }
  }

  /// Return a string reference for a string value, or throw a type error if the value is not a string.
  pub fn to_string_ref<'a>(
    &mut self,
    v: &'a Value,
  ) -> Result<&'a js_string::JsString, RuntimeError> {
    match v {
      Value::String(s) => Ok(s),
      _ => Err(self.throw_type_error("Cannot convert to string reference")),
    }
  }
  /// Return the stored source or native template for a callable's `toString`.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the value is not a callable.
  pub fn function_to_string(
    &mut self,
    value: &Value,
  ) -> Result<String, RuntimeError> {
    if !Self::is_callable_value(value) {
      return Err(self.throw_type_error("Function.prototype.toString called on non-callable"));
    }
    if let Value::Closure { proto, .. } = value
      && let Some(src) = proto.source_text()
    {
      return Ok(src.to_string());
    }
    Ok(self.format_native_function_source(value))
  }

  fn native_function_name(
    &mut self,
    value: &Value,
  ) -> Option<String> {
    match self.get_property_host(value, &PropertyKey::String("name".to_string().into())) {
      Ok(Value::String(name)) => Some(name.to_string_lossy()),
      Ok(_) => None,
      Err(_) => None,
    }
  }

  fn format_native_function_source(
    &mut self,
    value: &Value,
  ) -> String {
    let name = self
      .native_function_name(value)
      .unwrap_or_default();
    if name.is_empty() {
      "function () { [native code] }".to_string()
    } else {
      format!("function {}() {{ [native code] }}", name)
    }
  }

  /// Apply ECMAScript `ToNumber` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_number_value(
    &mut self,
    v: &Value,
  ) -> Result<f64, RuntimeError> {
    match v {
      Value::Number(n) => Ok(*n),
      Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
      Value::String(s) => Ok(parse_js_number_string(s)),
      Value::Undefined => Ok(f64::NAN),
      Value::Null => Ok(0.0),
      Value::Uninitialized => {
        Err(self.throw_reference_error("Cannot access lexical binding before initialization"))
      }
      Value::BigInt(_) => Err(self.throw_type_error("Cannot convert BigInt to number")),
      Value::Symbol(_, _) => Err(self.throw_type_error("Cannot convert Symbol to number")),
      _ => {
        let prim = self.to_primitive(v, Some("number"))?;
        self.to_number_value(&prim)
      }
    }
  }

  /// Apply ECMAScript `ToPropertyKey` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_property_key(
    &mut self,
    v: &Value,
  ) -> Result<PropertyKey, RuntimeError> {
    // Fast path: String values are already property keys, skip ToPrimitive entirely
    if let Value::String(s) = v {
      return Ok(PropertyKey::String(s.clone()));
    }

    match v {
      Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym.clone())),
      Value::Object(data) => {
        if let Some(symbol_key) = {
          let d = data.borrow();
          if let Some(PropertyDescriptor::Data {
            value: Value::Symbol(sym, _),
            ..
          }) = d.properties.get(&PropertyKey::String(
            "[[PrimitiveValue]]".to_string().into(),
          )) {
            Some(PropertyKey::Symbol(sym.clone()))
          } else {
            None
          }
        } {
          return Ok(symbol_key);
        }
        let prim = self.to_primitive(v, Some("string"))?;
        match prim {
          Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym)),
          _ => Ok(PropertyKey::String(self.to_string(&prim)?)),
        }
      }
      _ => {
        let prim = self.to_primitive(v, Some("string"))?;
        match prim {
          Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym)),
          _ => Ok(PropertyKey::String(self.to_string(&prim)?)),
        }
      }
    }
  }

  /// Apply ECMAScript `ToBoolean` coercion.
  pub fn to_boolean(
    &self,
    v: &Value,
  ) -> bool {
    v.is_truthy()
  }

  /// ECMAScript abstract equality comparison (`==`).
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn abstract_equality(
    &mut self,
    x: &Value,
    y: &Value,
  ) -> Result<bool, RuntimeError> {
    // Same-value for identical discriminants
    if std::mem::discriminant(x) == std::mem::discriminant(y) {
      return Ok(values_equal(x, y));
    }

    // null == undefined
    if matches!(x, Value::Null) && matches!(y, Value::Undefined) {
      return Ok(true);
    }
    if matches!(x, Value::Undefined) && matches!(y, Value::Null) {
      return Ok(true);
    }

    // Number == String
    if matches!(x, Value::Number(_)) && matches!(y, Value::String(_)) {
      let yn = self.to_number_value(y)?;
      if let Value::Number(xn) = x {
        return Ok(*xn == yn);
      }
    }
    if matches!(x, Value::String(_)) && matches!(y, Value::Number(_)) {
      let xn = self.to_number_value(x)?;
      if let Value::Number(yn) = y {
        return Ok(xn == *yn);
      }
    }

    // BigInt and String
    if matches!(x, Value::BigInt(_)) && matches!(y, Value::String(_)) {
      if let (Value::BigInt(xn), Value::String(ys)) = (x, y)
        && let Some(yn) = parse_js_bigint_string(ys)
      {
        return Ok(xn == &yn);
      }
      return Ok(false);
    }
    if matches!(x, Value::String(_)) && matches!(y, Value::BigInt(_)) {
      return self.abstract_equality(y, x);
    }

    // BigInt and Number
    if let (Value::BigInt(xn), Value::Number(yn)) = (x, y) {
      if !yn.is_finite() {
        return Ok(false);
      }
      if let Some(yn_big) = BigIntValue::from_f64(*yn) {
        return Ok(xn == &yn_big);
      }
      return Ok(false);
    }
    if let (Value::Number(_), Value::BigInt(_)) = (x, y) {
      return self.abstract_equality(y, x);
    }

    // Boolean -> Number coercion
    if matches!(x, Value::Bool(_)) {
      let xn = if let Value::Bool(b) = x {
        if *b { 1.0 } else { 0.0 }
      } else {
        0.0
      };
      return self.abstract_equality(&Value::Number(xn), y);
    }
    if matches!(y, Value::Bool(_)) {
      let yn = if let Value::Bool(b) = y {
        if *b { 1.0 } else { 0.0 }
      } else {
        0.0
      };
      return self.abstract_equality(x, &Value::Number(yn));
    }

    // Object compared with primitive: ToPrimitive on object
    if matches!(
      x,
      Value::Object(_)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::Builtin(_)
        | Value::BuiltinObject(_, _)
        | Value::Proxy(_)
        | Value::Promise(..)
    ) && matches!(
      y,
      Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_, _) | Value::Bool(_)
    ) {
      let xprim = self.to_primitive(x, None)?;
      return self.abstract_equality(&xprim, y);
    }
    if matches!(
      y,
      Value::Object(_)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::Builtin(_)
        | Value::BuiltinObject(_, _)
        | Value::Proxy(_)
        | Value::Promise(..)
    ) && matches!(
      x,
      Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_, _) | Value::Bool(_)
    ) {
      let yprim = self.to_primitive(y, None)?;
      return self.abstract_equality(x, &yprim);
    }

    Ok(false)
  }

  /// ECMAScript Abstract Relational Comparison. `left_first` controls the
  /// observable `ToPrimitive` order without changing which operands are
  /// compared as `left < right`.
  pub fn abstract_relational_comparison(
    &mut self,
    left: &Value,
    right: &Value,
    left_first: bool,
  ) -> Result<Option<bool>, RuntimeError> {
    let (left, right) = if left_first {
      (
        self.to_primitive(left, Some("number"))?,
        self.to_primitive(right, Some("number"))?,
      )
    } else {
      let right = self.to_primitive(right, Some("number"))?;
      let left = self.to_primitive(left, Some("number"))?;
      (left, right)
    };

    if let (Value::String(left), Value::String(right)) = (&left, &right) {
      return Ok(Some(left.cmp(right).is_lt()));
    }

    Ok(
      left
        .partial_cmp(&right)
        .map(std::cmp::Ordering::is_lt),
    )
  }
}
