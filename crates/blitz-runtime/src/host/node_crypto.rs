// SPDX-License-Identifier: MPL-2.0
//! Secure random operations for the initial Node `node:crypto` slice.
//!
//! The private adapter is deliberately small: this module owns the OS entropy
//! boundary and never falls back to a pseudo-random generator. JavaScript
//! validation and Node-shaped exceptions live in the public facade; this
//! boundary repeats the important checks so the private primitive cannot be
//! used to bypass them.

use std::cell::RefCell;
use std::rc::Rc;

use blitz::Vm;
use blitz::errors::RuntimeError;
use blitz::gc;
use blitz::value::{ArrayData, ObjectData, Uint8ArrayByteView, Value};
use getrandom::getrandom;

use super::{object, set};

const OK: &str = "ok";
const VALUE: &str = "value";
const MAX_RANDOM_BYTES: usize = 16 * 1024 * 1024;
const MAX_RANDOM_INT_RANGE: u64 = 1 << 48;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Install the version-one private Node crypto random primitive.
///
/// `allowed` is the host-operation permission. The primitive remains present
/// when a generated graph requires it so graph/ABI validation stays atomic,
/// but denied calls fail before entropy is requested or VM state is changed.
pub(super) fn install(
  vm: &Vm,
  allowed: bool,
) -> Value {
  let primitive = object(vm.realm().object_prototype());

  set(
    &primitive,
    "randomUUID",
    Value::Builtin(Rc::new(move |vm, args| {
      if !allowed {
        return Ok(permission_failure(vm, "randomUUID"));
      }
      random_uuid(vm, args)
    })),
  );
  set(
    &primitive,
    "randomBytes",
    Value::Builtin(Rc::new(move |vm, args| {
      if !allowed {
        return Ok(permission_failure(vm, "randomBytes"));
      }
      random_bytes(vm, args)
    })),
  );
  set(
    &primitive,
    "randomFill",
    Value::Builtin(Rc::new(move |vm, args| {
      if !allowed {
        return Ok(permission_failure(vm, "randomFill"));
      }
      random_fill(vm, args)
    })),
  );
  set(
    &primitive,
    "randomInt",
    Value::Builtin(Rc::new(move |vm, args| {
      if !allowed {
        return Ok(permission_failure(vm, "randomInt"));
      }
      random_int(vm, args)
    })),
  );
  set(
    &primitive,
    "timingSafeEqual",
    Value::Builtin(Rc::new(move |vm, args| {
      if !allowed {
        return Ok(permission_failure(vm, "timingSafeEqual"));
      }
      timing_safe_equal(vm, args)
    })),
  );

  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn random_uuid(
  vm: &mut Vm,
  _args: &[Value],
) -> Result<Value, RuntimeError> {
  let mut bytes = [0_u8; 16];
  if let Err(error) = getrandom(&mut bytes) {
    return Ok(entropy_failure(vm, "randomUUID", error.to_string()));
  }

  // RFC 4122 section 4.4: version 4 and the RFC variant bits.
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  const HEX: &[u8; 16] = b"0123456789abcdef";
  let mut uuid = String::with_capacity(36);
  for (index, byte) in bytes.into_iter().enumerate() {
    if matches!(index, 4 | 6 | 8 | 10) {
      uuid.push('-');
    }
    uuid.push(char::from(HEX[usize::from(byte >> 4)]));
    uuid.push(char::from(HEX[usize::from(byte & 0x0f)]));
  }
  Ok(success(vm, Value::String(uuid)))
}

fn random_bytes(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let size = match random_size(vm, args.get(1), "size") {
    Ok(size) => size,
    Err(error) => return Ok(error),
  };
  let mut bytes = vec![0_u8; size];
  if let Err(error) = getrandom(&mut bytes) {
    return Ok(entropy_failure(vm, "randomBytes", error.to_string()));
  }
  let value = make_uint8_array(vm, bytes)?;
  Ok(success(vm, value))
}

fn random_fill(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let target = match args.get(1).cloned() {
    Some(target) => target,
    None => {
      return Ok(argument_type_failure(
        vm,
        "buffer",
        "randomFill requires a standalone Uint8Array",
      ));
    }
  };
  let view = match byte_view(vm, &target, "buffer") {
    Ok(view) => view,
    Err(error) => return Ok(error),
  };
  let offset = match index_argument(vm, args.get(2), "offset") {
    Ok(offset) => offset.unwrap_or(0),
    Err(error) => return Ok(error),
  };
  let view_length = view.len();
  if offset > view_length {
    return Ok(range_failure(
      vm,
      "offset",
      "offset is outside the Uint8Array",
    ));
  }
  let size = match index_argument(vm, args.get(3), "size") {
    Ok(size) => size.unwrap_or(view_length - offset),
    Err(error) => return Ok(error),
  };
  let Some(end) = offset.checked_add(size) else {
    return Ok(range_failure(vm, "size", "size is outside the Uint8Array"));
  };
  if end > view_length || size > MAX_RANDOM_BYTES {
    return Ok(range_failure(
      vm,
      "size",
      "size is outside the supported secure-random byte limit",
    ));
  }

  // Generate the complete result before touching the target. An entropy
  // failure therefore cannot leave a partially filled VM value behind.
  let mut bytes = vec![0_u8; size];
  if let Err(error) = getrandom(&mut bytes) {
    return Ok(entropy_failure(vm, "randomFill", error.to_string()));
  }
  if !view.write_from(offset, &bytes) {
    return Ok(argument_type_failure(
      vm,
      "buffer",
      "the Uint8Array became detached during randomFill",
    ));
  }
  Ok(success(vm, target))
}

fn random_int(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let first = args.get(1).cloned().unwrap_or(Value::Undefined);
  let second = args.get(2).cloned().unwrap_or(Value::Undefined);
  let (min_value, max_value) = if matches!(second, Value::Undefined) {
    (
      0_i64,
      match safe_integer(vm, &first, "max") {
        Ok(value) => value,
        Err(error) => return Ok(error),
      },
    )
  } else {
    let min = match safe_integer(vm, &first, "min") {
      Ok(value) => value,
      Err(error) => return Ok(error),
    };
    let max = match safe_integer(vm, &second, "max") {
      Ok(value) => value,
      Err(error) => return Ok(error),
    };
    (min, max)
  };

  if max_value <= min_value {
    return Ok(range_failure(vm, "max", "max must be greater than min"));
  }
  let range = match u64::try_from(i128::from(max_value) - i128::from(min_value)) {
    Ok(range) if range < MAX_RANDOM_INT_RANGE => range,
    _ => {
      return Ok(range_failure(vm, "max", "max - min must be less than 2^48"));
    }
  };
  let random = match secure_below(range) {
    Ok(random) => random,
    Err(error) => return Ok(entropy_failure(vm, "randomInt", error.to_string())),
  };
  Ok(success(
    vm,
    Value::Number((i128::from(min_value) + i128::from(random as i64)) as f64),
  ))
}

fn timing_safe_equal(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let Some(left) = args.get(1) else {
    return Ok(argument_type_failure(
      vm,
      "a",
      "timingSafeEqual requires two standalone Uint8Arrays",
    ));
  };
  let Some(right) = args.get(2) else {
    return Ok(argument_type_failure(
      vm,
      "b",
      "timingSafeEqual requires two standalone Uint8Arrays",
    ));
  };
  let left = match byte_view(vm, left, "a") {
    Ok(view) => view,
    Err(error) => return Ok(error),
  };
  let right = match byte_view(vm, right, "b") {
    Ok(view) => view,
    Err(error) => return Ok(error),
  };
  let length = left.len();
  if length != right.len() {
    return Ok(failure(
      vm,
      "ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH",
      "Input buffers must have the same byte length",
    ));
  }

  let mut left_bytes = vec![0; length];
  let mut right_bytes = vec![0; length];
  if !left.read_into(0, &mut left_bytes) || !right.read_into(0, &mut right_bytes) {
    return Ok(argument_type_failure(
      vm,
      "buffer",
      "the Uint8Array became detached during timingSafeEqual",
    ));
  }
  // Do not return on a byte mismatch. The length check above is intentionally
  // separate because Node rejects unequal lengths before comparing bytes.
  let mut difference = 0_u8;
  for (left_byte, right_byte) in left_bytes.into_iter().zip(right_bytes) {
    difference |= left_byte ^ right_byte;
  }
  Ok(success(vm, Value::Bool(difference == 0)))
}

fn secure_below(range: u64) -> Result<u64, getrandom::Error> {
  debug_assert!(range > 0 && range < MAX_RANDOM_INT_RANGE);
  // The rejected prefix makes every remainder equally likely. `wrapping_neg`
  // computes 2^64 - range without representing 2^64 in a u64.
  let threshold = range.wrapping_neg() % range;
  loop {
    let mut bytes = [0_u8; 8];
    getrandom(&mut bytes)?;
    let value = u64::from_le_bytes(bytes);
    if value >= threshold {
      return Ok(value % range);
    }
  }
}

fn byte_view(
  vm: &mut Vm,
  value: &Value,
  argument: &str,
) -> Result<Uint8ArrayByteView, Value> {
  let Some(view) = vm.uint8_array_byte_view(value) else {
    return Err(argument_type_failure(
      vm,
      argument,
      "the argument must be an attached Uint8Array or Buffer",
    ));
  };
  if view.len() > MAX_RANDOM_BYTES {
    return Err(range_failure(
      vm,
      argument,
      "the Uint8Array is outside the supported secure-random byte limit",
    ));
  }
  Ok(view)
}

fn random_size(
  vm: &mut Vm,
  value: Option<&Value>,
  argument: &str,
) -> Result<usize, Value> {
  let Some(Value::Number(value)) = value else {
    return Err(argument_type_failure(
      vm,
      argument,
      "size must be a non-negative integer",
    ));
  };
  if !value.is_finite() || value.fract() != 0.0 || *value < 0.0 {
    return Err(range_failure(
      vm,
      argument,
      "size must be a non-negative integer",
    ));
  }
  if *value > MAX_RANDOM_BYTES as f64 {
    return Err(range_failure(
      vm,
      argument,
      "size is outside the supported secure-random byte limit",
    ));
  }
  value
    .to_string()
    .parse::<usize>()
    .map_err(|_| range_failure(vm, argument, "size is too large"))
}

fn index_argument(
  vm: &mut Vm,
  value: Option<&Value>,
  argument: &str,
) -> Result<Option<usize>, Value> {
  let Some(value) = value else {
    return Ok(None);
  };
  if matches!(value, Value::Undefined) {
    return Ok(None);
  }
  let Value::Number(value) = value else {
    return Err(argument_type_failure(
      vm,
      argument,
      "offset and size must be non-negative integers",
    ));
  };
  if !value.is_finite() || value.fract() != 0.0 || *value < 0.0 {
    return Err(range_failure(
      vm,
      argument,
      "offset and size must be non-negative integers",
    ));
  }
  value
    .to_string()
    .parse::<usize>()
    .map(Some)
    .map_err(|_| range_failure(vm, argument, "offset or size is too large"))
}

fn safe_integer(
  vm: &mut Vm,
  value: &Value,
  argument: &str,
) -> Result<i64, Value> {
  let Value::Number(value) = value else {
    return Err(argument_type_failure(
      vm,
      argument,
      "min and max must be safe integers",
    ));
  };
  if !value.is_finite()
    || value.fract() != 0.0
    || *value < -MAX_SAFE_INTEGER
    || *value > MAX_SAFE_INTEGER
  {
    return Err(range_failure(
      vm,
      argument,
      "min and max must be safe integers",
    ));
  }
  #[allow(clippy::cast_possible_truncation)]
  Ok(*value as i64)
}

fn make_uint8_array(
  vm: &mut Vm,
  bytes: Vec<u8>,
) -> Result<Value, RuntimeError> {
  let elements = bytes
    .into_iter()
    .map(|byte| Some(Value::Number(f64::from(byte))))
    .collect();
  let array = Value::Array(
    gc::track_array(Rc::new(RefCell::new(ArrayData::new(elements)))),
    gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
      vm.realm().array_prototype(),
    )))),
  );
  let constructor = vm
    .get_global("Uint8Array")
    .ok_or_else(|| RuntimeError::from("Uint8Array is not installed"))?;
  vm.construct_value(&constructor, &[array])
}

fn success(
  vm: &Vm,
  value: Value,
) -> Value {
  let result = object(vm.realm().object_prototype());
  set(&result, OK, Value::Bool(true));
  set(&result, VALUE, value);
  result
}

fn failure(
  vm: &Vm,
  code: &str,
  message: &str,
) -> Value {
  let result = object(vm.realm().object_prototype());
  set(&result, OK, Value::Bool(false));
  set(&result, "code", Value::String(code.to_string()));
  set(&result, "message", Value::String(message.to_string()));
  result
}

fn argument_type_failure(
  vm: &Vm,
  argument: &str,
  message: impl Into<String>,
) -> Value {
  let message = format!("The {argument} argument is invalid: {}", message.into());
  failure(vm, "ERR_INVALID_ARG_TYPE", &message)
}

fn range_failure(
  vm: &Vm,
  argument: &str,
  message: &str,
) -> Value {
  failure(
    vm,
    "ERR_OUT_OF_RANGE",
    &format!("The value of {argument} is out of range: {message}"),
  )
}

fn permission_failure(
  vm: &Vm,
  operation: &str,
) -> Value {
  let result = failure(
    vm,
    "ERR_ACCESS_DENIED",
    "Permission denied: secure Node crypto random access is disabled",
  );
  set(&result, "operation", Value::String(operation.to_string()));
  result
}

fn entropy_failure(
  vm: &Vm,
  operation: &str,
  message: String,
) -> Value {
  let result = failure(
    vm,
    "ERR_SYSTEM_ERROR",
    &format!("{operation} could not obtain secure operating-system entropy: {message}"),
  );
  set(&result, "operation", Value::String(operation.to_string()));
  set(&result, "syscall", Value::String("getrandom".to_string()));
  result
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::module::{VirtualModule, evaluate_entry_shared};
  use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey};
  use oxc_span::SourceType;
  use std::path::Path;

  fn test_vm() -> Vm {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    Vm::new(globals)
  }

  fn property(
    value: &Value,
    name: &str,
  ) -> Value {
    let Value::Object(value) = value else {
      panic!("expected result object");
    };
    match value
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string()))
    {
      Some(PropertyDescriptor::Data { value, .. }) => value.clone(),
      _ => Value::Undefined,
    }
  }

  #[test]
  fn secure_random_operations_have_expected_shapes_and_errors() {
    let mut vm = test_vm();
    let uuid = random_uuid(&mut vm, &[]).expect("randomUUID operation");
    let Value::String(uuid) = property(&uuid, VALUE) else {
      panic!("randomUUID should return a string");
    };
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "4");
    assert!(matches!(uuid.as_bytes()[19], b'8'..=b'b'));

    let bytes = random_bytes(&mut vm, &[Value::Undefined, Value::Number(32.0)])
      .expect("randomBytes operation");
    let bytes = property(&bytes, VALUE);
    let Value::Object(bytes) = &bytes else {
      panic!("randomBytes should return Uint8Array");
    };
    assert_eq!(
      property(&Value::Object(Rc::clone(bytes)), "length",),
      Value::Number(32.0)
    );

    let constructor = vm
      .get_global("Uint8Array")
      .expect("Uint8Array global");
    let target = vm
      .construct_value(&constructor, &[Value::Number(8.0)])
      .expect("construct target");
    let filled = random_fill(
      &mut vm,
      &[
        Value::Undefined,
        target.clone(),
        Value::Number(2.0),
        Value::Number(3.0),
      ],
    )
    .expect("randomFill operation");
    assert!(matches!(property(&filled, OK), Value::Bool(true)));
    let filled_value = property(&filled, VALUE);
    assert!(matches!(
      (&filled_value, &target),
      (Value::Object(left), Value::Object(right)) if Rc::ptr_eq(left, right)
    ));

    let equal = timing_safe_equal(&mut vm, &[Value::Undefined, target.clone(), target.clone()])
      .expect("timingSafeEqual operation");
    assert_eq!(property(&equal, VALUE), Value::Bool(true));
    vm.set_property_host(&target, &Value::String("0".to_string()), Value::Number(1.0))
      .expect("set deterministic comparison byte");
    let other = vm
      .construct_value(&constructor, &[Value::Number(8.0)])
      .expect("construct other target");
    let unequal = timing_safe_equal(&mut vm, &[Value::Undefined, target, other])
      .expect("timingSafeEqual unequal operation");
    assert_eq!(property(&unequal, VALUE), Value::Bool(false));

    let short = vm
      .construct_value(&constructor, &[Value::Number(1.0)])
      .expect("construct short target");
    let long = vm
      .construct_value(&constructor, &[Value::Number(2.0)])
      .expect("construct long target");
    let bad_length =
      timing_safe_equal(&mut vm, &[Value::Undefined, short, long]).expect("length error operation");
    assert_eq!(
      property(&bad_length, "code"),
      Value::String("ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH".to_string())
    );
  }

  #[test]
  fn random_int_uses_valid_bounds_and_permission_is_checked_at_the_host_boundary() {
    let mut vm = test_vm();
    for _ in 0..32 {
      let result = random_int(
        &mut vm,
        &[Value::Undefined, Value::Number(-10.0), Value::Number(10.0)],
      )
      .expect("randomInt operation");
      assert!(
        matches!(property(&result, VALUE), Value::Number(value) if (-10.0..10.0).contains(&value))
      );
    }
    let invalid = random_int(
      &mut vm,
      &[Value::Undefined, Value::Number(3.0), Value::Number(3.0)],
    )
    .expect("invalid randomInt operation");
    assert_eq!(
      property(&invalid, "code"),
      Value::String("ERR_OUT_OF_RANGE".to_string())
    );

    let denied = install(&vm, false);
    let method = vm
      .get_property_host(&denied, &PropertyKey::String("randomBytes".to_string()))
      .expect("randomBytes method");
    let denied = vm
      .call_value_with_this(&method, &denied, &[Value::Number(8.0)])
      .expect("permission result");
    assert_eq!(
      property(&denied, "code"),
      Value::String("ERR_ACCESS_DENIED".to_string())
    );
  }

  #[test]
  fn source_facade_covers_uuid_bytes_fill_int_and_timing_operations() {
    let mut vm = test_vm();
    let bridge = object(vm.realm().object_prototype());
    set(&bridge, "abiVersion", Value::Number(1.0));
    set(&bridge, "nodeCryptoRandom", install(&vm, true));
    vm.realm().set_global("__blitz_host_v1", bridge);

    let loader = vm.module_loader_handle();
    let entry_path;
    {
      let mut loader = loader.borrow_mut();
      loader
        .register_virtual_module(VirtualModule::javascript(
          "blitz-internal:host",
          "export const host = globalThis.__blitz_host_v1;",
          SourceType::mjs(),
          None,
        ))
        .expect("register test host bridge module");
      loader
        .register_virtual_module(VirtualModule::javascript(
          "blitz-internal:node-crypto-random",
          include_str!(
            "../../../../tools/effect-assets/src/facades/blitz-internal-node-crypto-random.js"
          ),
          SourceType::mjs(),
          None,
        ))
        .expect("register Node crypto private adapter");
      loader
        .register_virtual_module(VirtualModule::javascript(
          "node:crypto",
          include_str!("../../../../tools/effect-assets/src/facades/node-crypto.js"),
          SourceType::mjs(),
          None,
        ))
        .expect("register Node crypto facade");
      loader
        .register_virtual_module(VirtualModule::javascript(
          "node-crypto-facade-entry",
          r#"
            import crypto, { randomBytes, randomFill, randomFillSync, randomInt, randomUUID, timingSafeEqual } from "node:crypto";
            const id = randomUUID();
            if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)) throw new Error("invalid RFC 4122 v4 UUID");
            const bytes = randomBytes(16);
            if (!(bytes instanceof Uint8Array) || bytes.length !== 16) throw new Error("randomBytes slice mismatch");
            const target = new Uint8Array(8);
            if (randomFillSync(target, 2, 3) !== target) throw new Error("randomFillSync return mismatch");
            const backing = new Uint8Array([17, 17, 17, 17, 17, 17]);
            const offsetView = new Uint8Array(backing.buffer, 2, 2);
            if (randomFillSync(offsetView) !== offsetView || backing[0] !== 17 || backing[1] !== 17 || backing[4] !== 17 || backing[5] !== 17) throw new Error("randomFillSync offset view mismatch");
            const leftBacking = new Uint8Array([9, 1, 2, 9]);
            const leftView = new Uint8Array(leftBacking.buffer, 1, 2);
            if (!timingSafeEqual(leftView, new Uint8Array([1, 2]))) throw new Error("timingSafeEqual offset view mismatch");
            if (!timingSafeEqual(target, target)) throw new Error("timingSafeEqual mismatch");
            try { timingSafeEqual(new Uint8Array(1), new Uint8Array(2)); throw new Error("length mismatch did not throw"); } catch (error) { if (error.code !== "ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH") throw error; }
            if (crypto.randomUUID !== randomUUID || crypto.randomBytes !== randomBytes) throw new Error("default export mismatch");
            if ("webcrypto" in crypto || "subtle" in crypto || "getRandomValues" in crypto) throw new Error("unsupported Web Crypto export was exposed");
            try { randomBytes(-1); throw new Error("negative randomBytes size did not throw"); } catch (error) { if (error.code !== "ERR_OUT_OF_RANGE") throw error; }
            if (randomInt(-4, 4) < -4 || randomInt(-4, 4) >= 4) throw new Error("randomInt bounds mismatch");
            randomBytes(4, (error, value) => { if (error || !(value instanceof Uint8Array) || value.length !== 4) throw error || new Error("randomBytes callback mismatch"); globalThis.nodeCryptoBytesCallback = true; });
            randomFill(target, 1, 2, (error, value) => { if (error || value !== target) throw error || new Error("randomFill callback mismatch"); globalThis.nodeCryptoFillCallback = true; });
            randomInt(10, (error, value) => { if (error || value < 0 || value >= 10) throw error || new Error("randomInt callback mismatch"); globalThis.nodeCryptoIntCallback = true; });
          "#,
          SourceType::mjs(),
          None,
        ))
        .expect("register Node crypto facade entry");
      let entry = loader
        .construct(Path::new("."), "node-crypto-facade-entry")
        .expect("construct Node crypto facade entry");
      loader
        .instantiate_all()
        .expect("instantiate Node crypto facade entry");
      entry_path = entry.borrow().path.clone();
    }
    evaluate_entry_shared(&loader, &entry_path, &mut vm, false)
      .expect("evaluate Node crypto facade entry");
    vm.flush_microtasks()
      .expect("flush Node crypto callback jobs");
    assert_eq!(
      vm.get_global("nodeCryptoBytesCallback"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("nodeCryptoFillCallback"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("nodeCryptoIntCallback"),
      Some(Value::Bool(true))
    );
  }
}
