// SPDX-License-Identifier: MPL-2.0
//! Lazy Effect-native filesystem host primitives.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use denko::Vm;
use denko::errors::RuntimeError;
use denko::gc;
use denko::value::{ArrayData, BuiltinFn, ObjectData, PropertyKey, Value};
use url::Url;

use super::{effect_fail, effect_succeed, effect_suspend, object, set};

pub(super) fn install(vm: &Vm) -> Value {
  let primitive = object(vm.realm().object_prototype());
  set(&primitive, "readFile", Value::Builtin(read(false)));
  set(&primitive, "readTextFile", Value::Builtin(read(true)));
  set(&primitive, "writeFile", Value::Builtin(write(false)));
  set(&primitive, "writeTextFile", Value::Builtin(write(true)));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn read(text: bool) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let path = match path_value(vm, args.get(1), "read") {
      Ok(path) => path,
      Err(error) => return effect_fail(vm, error),
    };
    let thunk: BuiltinFn = Rc::new(move |vm, _args| match std::fs::read(&path) {
      Ok(bytes) if text => match String::from_utf8(bytes) {
        Ok(value) => effect_succeed(vm, Value::String(value.into())),
        Err(error) => {
          let failure = fs_error(
            vm,
            "InvalidData",
            "read",
            Some(&path),
            format!("file is not valid UTF-8: {error}"),
          );
          effect_fail(vm, failure)
        }
      },
      Ok(bytes) => {
        let value = make_uint8_array(vm, bytes)?;
        effect_succeed(vm, value)
      }
      Err(error) => {
        let failure = io_error(vm, "read", &path, &error);
        effect_fail(vm, failure)
      }
    });
    effect_suspend(vm, thunk)
  })
}

fn write(text: bool) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let path = match path_value(vm, args.get(1), "write") {
      Ok(path) => path,
      Err(error) => return effect_fail(vm, error),
    };
    let data = if text {
      match args.get(2) {
        Some(Value::String(value)) => value.as_str_lossy().as_bytes().to_vec(),
        _ => {
          let failure = fs_error(
            vm,
            "InvalidData",
            "write",
            Some(&path),
            "writeTextFile requires a string".to_string(),
          );
          return effect_fail(vm, failure);
        }
      }
    } else {
      match args.get(2).map(|value| bytes_value(vm, value)) {
        Some(Ok(bytes)) => bytes,
        Some(Err(message)) => {
          let failure = fs_error(vm, "InvalidData", "write", Some(&path), message);
          return effect_fail(vm, failure);
        }
        None => {
          let failure = fs_error(
            vm,
            "InvalidData",
            "write",
            Some(&path),
            "writeFile requires a Uint8Array".to_string(),
          );
          return effect_fail(vm, failure);
        }
      }
    };
    let thunk: BuiltinFn = Rc::new(move |vm, _args| match std::fs::write(&path, &data) {
      Ok(()) => effect_succeed(vm, Value::Undefined),
      Err(error) => {
        let failure = io_error(vm, "write", &path, &error);
        effect_fail(vm, failure)
      }
    });
    effect_suspend(vm, thunk)
  })
}

fn path_value(
  vm: &mut Vm,
  value: Option<&Value>,
  operation: &str,
) -> Result<PathBuf, Value> {
  let path = match value {
    Some(Value::String(path)) => PathBuf::from(path.as_str_lossy()),
    Some(value @ Value::Object(_)) => {
      let href = vm
        .get_property_host(value, &PropertyKey::String("href".to_string().into()))
        .map_err(|error| fs_error(vm, "InvalidPath", operation, None, error.to_string()))?;
      let Value::String(href) = href else {
        return Err(fs_error(
          vm,
          "InvalidPath",
          operation,
          None,
          "path object must be a file URL".to_string(),
        ));
      };
      Url::parse(href.as_str_lossy())
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .ok_or_else(|| {
          fs_error(
            vm,
            "InvalidPath",
            operation,
            None,
            "path URL must use the file: scheme".to_string(),
          )
        })?
    }
    _ => {
      return Err(fs_error(
        vm,
        "InvalidPath",
        operation,
        None,
        "path must be a string or file URL".to_string(),
      ));
    }
  };
  if path.as_os_str().is_empty() {
    return Err(fs_error(
      vm,
      "InvalidPath",
      operation,
      Some(&path),
      "path must not be empty".to_string(),
    ));
  }
  Ok(path)
}

fn bytes_value(
  vm: &mut Vm,
  value: &Value,
) -> Result<Vec<u8>, String> {
  let length = vm
    .get_property_host(value, &PropertyKey::String("length".to_string().into()))
    .map_err(|error| error.to_string())?;
  let Value::Number(length) = length else {
    return Err("writeFile requires a Uint8Array".to_string());
  };
  if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
    return Err("writeFile received an invalid byte length".to_string());
  }
  let length = length
    .to_string()
    .parse::<usize>()
    .map_err(|_| "writeFile byte length is too large".to_string())?;
  let mut bytes = Vec::with_capacity(length);
  for index in 0..length {
    let item = vm
      .get_property_host(value, &PropertyKey::String(index.to_string().into()))
      .map_err(|error| error.to_string())?;
    let Value::Number(item) = item else {
      return Err("writeFile requires numeric bytes".to_string());
    };
    if !item.is_finite() || item.fract() != 0.0 || !(0.0..=255.0).contains(&item) {
      return Err("writeFile bytes must be integers between 0 and 255".to_string());
    }
    bytes.push(
      item
        .to_string()
        .parse::<u8>()
        .map_err(|_| "writeFile byte is outside the supported range".to_string())?,
    );
  }
  Ok(bytes)
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

fn io_error(
  vm: &Vm,
  operation: &str,
  path: &Path,
  error: &io::Error,
) -> Value {
  let reason = match error.kind() {
    io::ErrorKind::NotFound => "NotFound",
    io::ErrorKind::PermissionDenied => "PermissionDenied",
    io::ErrorKind::AlreadyExists => "AlreadyExists",
    io::ErrorKind::InvalidInput => "InvalidPath",
    io::ErrorKind::NotADirectory => "NotDirectory",
    io::ErrorKind::IsADirectory => "IsDirectory",
    _ => "Io",
  };
  fs_error(vm, reason, operation, Some(path), error.to_string())
}

fn fs_error(
  vm: &Vm,
  reason: &str,
  operation: &str,
  path: Option<&Path>,
  message: String,
) -> Value {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("FileSystemError".to_string().into()),
  );
  set(&error, "reason", Value::String(reason.to_string().into()));
  set(
    &error,
    "operation",
    Value::String(operation.to_string().into()),
  );
  if let Some(path) = path {
    set(
      &error,
      "path",
      Value::String(path.to_string_lossy().into_owned().into()),
    );
  }
  set(&error, "message", Value::String(message.into()));
  error
}
