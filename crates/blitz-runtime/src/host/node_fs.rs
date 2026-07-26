// SPDX-License-Identifier: MPL-2.0
//! Direct host primitives for the initial Node filesystem compatibility slice.
//!
//! This module intentionally does not depend on the Effect filesystem service.
//! The public `node:fs` facades translate the small result records below into
//! Node-shaped exceptions, while this boundary owns the actual OS calls.

use std::cell::RefCell;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use blitz::Vm;
use blitz::errors::RuntimeError;
use blitz::gc;
use blitz::value::{ArrayData, ObjectData, PropertyKey, Value};
use url::Url;

use super::{object, set};

const OK: &str = "ok";
const VALUE: &str = "value";
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;

/// Install the private Node filesystem primitive object.
#[allow(clippy::too_many_lines)]
pub(super) fn install(
  vm: &Vm,
  allowed: bool,
) -> Value {
  let primitive = object(vm.realm().object_prototype());
  install_method(&primitive, "readFile", allowed, read_file);
  install_method(&primitive, "writeFile", allowed, write_file);
  install_method(&primitive, "stat", allowed, stat);
  install_method(&primitive, "mkdir", allowed, mkdir);
  install_method(&primitive, "readdir", allowed, readdir);
  install_method(&primitive, "rm", allowed, rm);
  install_method(&primitive, "unlink", allowed, unlink);
  install_method(&primitive, "rename", allowed, rename);
  install_method(&primitive, "exists", allowed, exists);
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn install_method(
  primitive: &Value,
  operation: &'static str,
  allowed: bool,
  method: fn(&mut Vm, &[Value]) -> Result<Value, RuntimeError>,
) {
  set(
    primitive,
    operation,
    Value::Builtin(Rc::new(move |vm, args| {
      if !allowed {
        return Ok(failure(
          vm,
          "ERR_ACCESS_DENIED",
          None,
          Some(operation),
          None,
          None,
          format!("Access to node:fs.{operation} is denied by the active compatibility profile"),
        ));
      }
      method(vm, args)
    })),
  );
}

fn read_file(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "readFile") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  let text = match read_encoding(vm, args.get(2)) {
    Ok(text) => text,
    Err(error) => return Ok(error),
  };

  match read_bounded(&path) {
    Ok(bytes) if text => Ok(success(
      vm,
      Value::String(
        String::from_utf8_lossy(&bytes)
          .into_owned()
          .into(),
      ),
    )),
    Ok(bytes) => {
      let value = make_uint8_array(vm, bytes)?;
      Ok(success(vm, value))
    }
    Err(error) if error.kind() == io::ErrorKind::FileTooLarge => Ok(failure(
      vm,
      "ERR_OUT_OF_RANGE",
      None,
      Some("read"),
      Some(&path),
      None,
      format!("File exceeds the Blitz node:fs limit of {MAX_FILE_BYTES} bytes"),
    )),
    Err(error) => Ok(io_failure(vm, "open", &path, None, &error)),
  }
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
  let file = File::open(path)?;
  let mut bytes = Vec::new();
  file
    .take(u64::try_from(MAX_FILE_BYTES).unwrap_or(u64::MAX) + 1)
    .read_to_end(&mut bytes)?;
  if bytes.len() > MAX_FILE_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::FileTooLarge,
      "file exceeds Blitz node:fs read limit",
    ));
  }
  Ok(bytes)
}

fn write_file(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "writeFile") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  let data = match args.get(2) {
    Some(value) => match bytes_value(vm, value) {
      Ok(data) => data,
      Err(error) => return Ok(error),
    },
    None => {
      return Ok(failure(
        vm,
        "ERR_INVALID_ARG_TYPE",
        None,
        Some("writeFile"),
        Some(&path),
        None,
        "The data argument must be a string or a Uint8Array".to_string(),
      ));
    }
  };
  let flag = match args.get(3) {
    None | Some(Value::Undefined) => "w",
    Some(Value::String(flag)) => flag.as_str_lossy(),
    Some(_) => {
      return Ok(failure(
        vm,
        "ERR_INVALID_ARG_TYPE",
        None,
        Some("writeFile"),
        Some(&path),
        None,
        "The flag option must be a string".to_string(),
      ));
    }
  };

  let file = match open_for_write(&path, flag) {
    Ok(file) => file,
    Err(error) => return Ok(io_failure(vm, "open", &path, None, &error)),
  };
  match write_all(file, &data) {
    Ok(()) => Ok(success(vm, Value::Undefined)),
    Err(error) => Ok(io_failure(vm, "write", &path, None, &error)),
  }
}

fn stat(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "stat") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  match fs::metadata(&path) {
    Ok(metadata) => Ok(success(vm, stats_value(vm, &metadata))),
    Err(error) => Ok(io_failure(vm, "stat", &path, None, &error)),
  }
}

fn mkdir(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "mkdir") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  let recursive = match boolean_arg(vm, args.get(2), "recursive", "mkdir") {
    Ok(value) => value,
    Err(error) => return Ok(error),
  };
  let result = if recursive {
    fs::create_dir_all(&path)
  } else {
    fs::create_dir(&path)
  };
  match result {
    Ok(()) => Ok(success(vm, Value::Undefined)),
    Err(error) => Ok(io_failure(vm, "mkdir", &path, None, &error)),
  }
}

fn readdir(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "readdir") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  let entries = match fs::read_dir(&path) {
    Ok(entries) => entries,
    Err(error) => return Ok(io_failure(vm, "scandir", &path, None, &error)),
  };
  let mut values = Vec::new();
  for entry in entries {
    if values.len() >= MAX_DIRECTORY_ENTRIES {
      return Ok(failure(
        vm,
        "ERR_OUT_OF_RANGE",
        None,
        Some("scandir"),
        Some(&path),
        None,
        format!("Directory exceeds the Blitz node:fs limit of {MAX_DIRECTORY_ENTRIES} entries"),
      ));
    }
    let entry = match entry {
      Ok(entry) => entry,
      Err(error) => return Ok(io_failure(vm, "scandir", &path, None, &error)),
    };
    let file_type = match entry.file_type() {
      Ok(file_type) => file_type,
      Err(error) => return Ok(io_failure(vm, "scandir", &path, None, &error)),
    };
    let item = object(vm.realm().object_prototype());
    set(
      &item,
      "name",
      Value::String(
        entry
          .file_name()
          .to_string_lossy()
          .into_owned()
          .into(),
      ),
    );
    set(&item, "isFile", Value::Bool(file_type.is_file()));
    set(&item, "isDirectory", Value::Bool(file_type.is_dir()));
    set(&item, "isSymbolicLink", Value::Bool(file_type.is_symlink()));
    values.push(Some(item));
  }
  Ok(success(vm, vm.new_array(values)))
}

fn rm(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "rm") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  let recursive = match boolean_arg(vm, args.get(2), "recursive", "rm") {
    Ok(value) => value,
    Err(error) => return Ok(error),
  };
  let force = match boolean_arg(vm, args.get(3), "force", "rm") {
    Ok(value) => value,
    Err(error) => return Ok(error),
  };
  if !recursive && path.is_dir() {
    return Ok(failure(
      vm,
      "ERR_FS_EISDIR",
      None,
      Some("rm"),
      Some(&path),
      None,
      "Path is a directory: rm returned EISDIR (is a directory)".to_string(),
    ));
  }
  let result = if recursive {
    fs::remove_dir_all(&path).or_else(|error| {
      if path.is_file() {
        fs::remove_file(&path)
      } else {
        Err(error)
      }
    })
  } else {
    fs::remove_file(&path)
  };
  match result {
    Ok(()) => Ok(success(vm, Value::Undefined)),
    Err(error) if force && error.kind() == io::ErrorKind::NotFound => {
      Ok(success(vm, Value::Undefined))
    }
    Err(error) => {
      let syscall = if recursive || path.is_dir() {
        "rmdir"
      } else {
        "unlink"
      };
      Ok(io_failure(vm, syscall, &path, None, &error))
    }
  }
}

fn unlink(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "unlink") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  match fs::remove_file(&path) {
    Ok(()) => Ok(success(vm, Value::Undefined)),
    Err(error) => Ok(io_failure(vm, "unlink", &path, None, &error)),
  }
}

fn rename(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let from = match path_value(vm, args.get(1), "rename") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  let to = match path_value(vm, args.get(2), "rename") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  match fs::rename(&from, &to) {
    Ok(()) => Ok(success(vm, Value::Undefined)),
    Err(error) => Ok(io_failure(vm, "rename", &from, Some(&to), &error)),
  }
}

fn exists(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let path = match path_value(vm, args.get(1), "existsSync") {
    Ok(path) => path,
    Err(error) => return Ok(error),
  };
  Ok(success(vm, Value::Bool(fs::metadata(path).is_ok())))
}

fn path_value(
  vm: &mut Vm,
  value: Option<&Value>,
  operation: &str,
) -> Result<PathBuf, Value> {
  let Some(value) = value else {
    return Err(failure(
      vm,
      "ERR_INVALID_ARG_TYPE",
      None,
      Some(operation),
      None,
      None,
      "The path argument must be a string or a file URL".to_string(),
    ));
  };
  let path = match value {
    Value::String(path) => PathBuf::from(path.as_str_lossy()),
    Value::Object(_) => {
      let href = match vm.get_property_host(value, &PropertyKey::String("href".to_string().into()))
      {
        Ok(Value::String(href)) => href,
        Ok(_) => {
          return Err(failure(
            vm,
            "ERR_INVALID_ARG_TYPE",
            None,
            Some(operation),
            None,
            None,
            "The URL path must have a string href".to_string(),
          ));
        }
        Err(error) => {
          return Err(failure(
            vm,
            "ERR_INVALID_ARG_TYPE",
            None,
            Some(operation),
            None,
            None,
            error.to_string(),
          ));
        }
      };
      let parsed = match Url::parse(href.as_str_lossy()) {
        Ok(parsed) => parsed,
        Err(error) => {
          return Err(failure(
            vm,
            "ERR_INVALID_URL",
            None,
            Some(operation),
            None,
            None,
            error.to_string(),
          ));
        }
      };
      match parsed.to_file_path() {
        Ok(path) => path,
        Err(()) => {
          return Err(failure(
            vm,
            "ERR_INVALID_FILE_URL_PATH",
            None,
            Some(operation),
            None,
            None,
            "The URL must use a local file path".to_string(),
          ));
        }
      }
    }
    _ => {
      return Err(failure(
        vm,
        "ERR_INVALID_ARG_TYPE",
        None,
        Some(operation),
        None,
        None,
        "The path argument must be a string or a file URL".to_string(),
      ));
    }
  };
  if path.as_os_str().is_empty() {
    return Err(failure(
      vm,
      "ERR_INVALID_ARG_VALUE",
      None,
      Some(operation),
      Some(&path),
      None,
      "The path argument must not be empty".to_string(),
    ));
  }
  Ok(path)
}

fn read_encoding(
  vm: &mut Vm,
  value: Option<&Value>,
) -> Result<bool, Value> {
  match value {
    None | Some(Value::Undefined) | Some(Value::Null) => Ok(false),
    Some(Value::String(encoding))
      if encoding
        .as_str_lossy()
        .eq_ignore_ascii_case("utf8")
        || encoding
          .as_str_lossy()
          .eq_ignore_ascii_case("utf-8") =>
    {
      Ok(true)
    }
    Some(Value::String(encoding)) => Err(failure(
      vm,
      "ERR_INVALID_ARG_VALUE",
      None,
      Some("readFile"),
      None,
      None,
      format!("The encoding '{encoding}' is not supported by Blitz"),
    )),
    Some(_) => Err(failure(
      vm,
      "ERR_INVALID_ARG_TYPE",
      None,
      Some("readFile"),
      None,
      None,
      "The encoding option must be a string".to_string(),
    )),
  }
}

fn boolean_arg(
  vm: &mut Vm,
  value: Option<&Value>,
  name: &str,
  operation: &str,
) -> Result<bool, Value> {
  match value {
    None | Some(Value::Undefined) => Ok(false),
    Some(Value::Bool(value)) => Ok(*value),
    Some(_) => Err(failure(
      vm,
      "ERR_INVALID_ARG_TYPE",
      None,
      Some(operation),
      None,
      None,
      format!("The {name} option must be a boolean"),
    )),
  }
}

fn bytes_value(
  vm: &mut Vm,
  value: &Value,
) -> Result<Vec<u8>, Value> {
  if let Value::String(value) = value {
    if value.len() > MAX_FILE_BYTES {
      return Err(failure(
        vm,
        "ERR_OUT_OF_RANGE",
        None,
        Some("writeFile"),
        None,
        None,
        format!("Data exceeds the Blitz node:fs limit of {MAX_FILE_BYTES} bytes"),
      ));
    }
    return Ok(value.as_str_lossy().as_bytes().to_vec());
  }
  let length = match vm.get_property_host(value, &PropertyKey::String("length".to_string().into()))
  {
    Ok(Value::Number(length)) if length.is_finite() && length >= 0.0 && length.fract() == 0.0 => {
      match length.to_string().parse::<usize>() {
        Ok(length) => length,
        Err(_) => {
          return Err(failure(
            vm,
            "ERR_INVALID_ARG_VALUE",
            None,
            Some("writeFile"),
            None,
            None,
            "The byte array is too large".to_string(),
          ));
        }
      }
    }
    Ok(_) => {
      return Err(failure(
        vm,
        "ERR_INVALID_ARG_TYPE",
        None,
        Some("writeFile"),
        None,
        None,
        "The data argument must be a string or a Uint8Array".to_string(),
      ));
    }
    Err(error) => {
      return Err(failure(
        vm,
        "ERR_INVALID_ARG_TYPE",
        None,
        Some("writeFile"),
        None,
        None,
        error.to_string(),
      ));
    }
  };
  if length > MAX_FILE_BYTES {
    return Err(failure(
      vm,
      "ERR_OUT_OF_RANGE",
      None,
      Some("writeFile"),
      None,
      None,
      format!("Data exceeds the Blitz node:fs limit of {MAX_FILE_BYTES} bytes"),
    ));
  }
  let mut bytes = Vec::with_capacity(length);
  for index in 0..length {
    let item = match vm.get_property_host(value, &PropertyKey::String(index.to_string().into())) {
      Ok(Value::Number(item))
        if item.is_finite() && item.fract() == 0.0 && (0.0..=255.0).contains(&item) =>
      {
        item
      }
      Ok(_) => {
        return Err(failure(
          vm,
          "ERR_INVALID_ARG_TYPE",
          None,
          Some("writeFile"),
          None,
          None,
          "The data argument must contain byte values".to_string(),
        ));
      }
      Err(error) => {
        return Err(failure(
          vm,
          "ERR_INVALID_ARG_TYPE",
          None,
          Some("writeFile"),
          None,
          None,
          error.to_string(),
        ));
      }
    };
    let byte = match item.to_string().parse::<u8>() {
      Ok(byte) => byte,
      Err(_) => {
        return Err(failure(
          vm,
          "ERR_INVALID_ARG_VALUE",
          None,
          Some("writeFile"),
          None,
          None,
          "The data argument contains an invalid byte".to_string(),
        ));
      }
    };
    bytes.push(byte);
  }
  Ok(bytes)
}

fn open_for_write(
  path: &Path,
  flag: &str,
) -> io::Result<File> {
  let mut options = OpenOptions::new();
  match flag {
    "w" => {
      options.write(true).create(true).truncate(true);
    }
    "wx" | "xw" => {
      options.write(true).create_new(true);
    }
    "a" => {
      options.append(true).create(true);
    }
    "ax" | "xa" => {
      options.append(true).create_new(true);
    }
    "w+" => {
      options
        .read(true)
        .write(true)
        .create(true)
        .truncate(true);
    }
    "wx+" | "xw+" => {
      options.read(true).write(true).create_new(true);
    }
    "a+" => {
      options.read(true).append(true).create(true);
    }
    "ax+" | "xa+" => {
      options.read(true).append(true).create_new(true);
    }
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "unsupported write flag",
      ));
    }
  }
  options.open(path)
}

fn write_all(
  mut file: File,
  data: &[u8],
) -> io::Result<()> {
  file.write_all(data)
}

fn stats_value(
  vm: &Vm,
  metadata: &Metadata,
) -> Value {
  let value = object(vm.realm().object_prototype());
  for (name, number) in [
    ("dev", metadata_dev(metadata)),
    ("ino", metadata_ino(metadata)),
    ("mode", metadata_mode(metadata)),
    ("nlink", metadata_nlink(metadata)),
    ("uid", metadata_uid(metadata)),
    ("gid", metadata_gid(metadata)),
    ("rdev", metadata_rdev(metadata)),
    ("size", metadata.len()),
    ("blksize", metadata_blksize(metadata)),
    ("blocks", metadata_blocks(metadata)),
  ] {
    set(&value, name, number_value(number));
  }
  let accessed = system_time_millis(metadata.accessed());
  let modified = system_time_millis(metadata.modified());
  let created = system_time_millis(metadata.created());
  for (name, number) in [
    ("atimeMs", accessed),
    ("mtimeMs", modified),
    ("ctimeMs", modified),
    ("birthtimeMs", created),
  ] {
    set(&value, name, Value::Number(number));
  }
  set(&value, "isFile", Value::Bool(metadata.is_file()));
  set(&value, "isDirectory", Value::Bool(metadata.is_dir()));
  set(&value, "isSymbolicLink", Value::Bool(false));
  set(&value, "isBlockDevice", Value::Bool(false));
  set(&value, "isCharacterDevice", Value::Bool(false));
  set(&value, "isFIFO", Value::Bool(false));
  set(&value, "isSocket", Value::Bool(false));
  value
}

fn metadata_dev(metadata: &Metadata) -> u64 {
  platform_metadata_dev(metadata)
}

fn metadata_ino(metadata: &Metadata) -> u64 {
  platform_metadata_ino(metadata)
}

fn metadata_mode(metadata: &Metadata) -> u64 {
  platform_metadata_mode(metadata)
}

fn metadata_nlink(metadata: &Metadata) -> u64 {
  platform_metadata_nlink(metadata)
}

fn metadata_uid(metadata: &Metadata) -> u64 {
  platform_metadata_uid(metadata)
}

fn metadata_gid(metadata: &Metadata) -> u64 {
  platform_metadata_gid(metadata)
}

fn metadata_rdev(metadata: &Metadata) -> u64 {
  platform_metadata_rdev(metadata)
}

fn metadata_blksize(metadata: &Metadata) -> u64 {
  platform_metadata_blksize(metadata)
}

fn metadata_blocks(metadata: &Metadata) -> u64 {
  platform_metadata_blocks(metadata)
}

#[cfg(unix)]
fn platform_metadata_dev(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  metadata.dev()
}

#[cfg(not(unix))]
fn platform_metadata_dev(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_ino(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  metadata.ino()
}

#[cfg(not(unix))]
fn platform_metadata_ino(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_mode(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  u64::from(metadata.mode())
}

#[cfg(not(unix))]
fn platform_metadata_mode(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_nlink(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  metadata.nlink()
}

#[cfg(not(unix))]
fn platform_metadata_nlink(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_uid(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  u64::from(metadata.uid())
}

#[cfg(not(unix))]
fn platform_metadata_uid(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_gid(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  u64::from(metadata.gid())
}

#[cfg(not(unix))]
fn platform_metadata_gid(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_rdev(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  metadata.rdev()
}

#[cfg(not(unix))]
fn platform_metadata_rdev(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_blksize(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  metadata.blksize()
}

#[cfg(not(unix))]
fn platform_metadata_blksize(_metadata: &Metadata) -> u64 {
  0
}

#[cfg(unix)]
fn platform_metadata_blocks(metadata: &Metadata) -> u64 {
  use std::os::unix::fs::MetadataExt;
  metadata.blocks()
}

#[cfg(not(unix))]
fn platform_metadata_blocks(_metadata: &Metadata) -> u64 {
  0
}

fn system_time_millis(time: io::Result<SystemTime>) -> f64 {
  time
    .ok()
    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
    .map_or(0.0, |duration| duration.as_secs_f64() * 1_000.0)
}

fn number_value(value: u64) -> Value {
  Value::Number(value as f64)
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

#[allow(clippy::too_many_arguments)]
fn failure(
  vm: &Vm,
  code: &str,
  errno: Option<i32>,
  syscall: Option<&str>,
  path: Option<&Path>,
  dest: Option<&Path>,
  message: String,
) -> Value {
  let result = object(vm.realm().object_prototype());
  set(&result, OK, Value::Bool(false));
  set(&result, "code", Value::String(code.to_string().into()));
  if let Some(errno) = errno {
    set(&result, "errno", Value::Number(f64::from(errno)));
  }
  if let Some(syscall) = syscall {
    set(
      &result,
      "syscall",
      Value::String(syscall.to_string().into()),
    );
  }
  if let Some(path) = path {
    set(
      &result,
      "path",
      Value::String(path.to_string_lossy().into_owned().into()),
    );
  }
  if let Some(dest) = dest {
    set(
      &result,
      "dest",
      Value::String(dest.to_string_lossy().into_owned().into()),
    );
  }
  set(&result, "message", Value::String(message.into()));
  result
}

fn io_failure(
  vm: &Vm,
  syscall: &str,
  path: &Path,
  dest: Option<&Path>,
  error: &io::Error,
) -> Value {
  let code = match error.kind() {
    io::ErrorKind::NotFound => "ENOENT",
    io::ErrorKind::PermissionDenied => "EACCES",
    io::ErrorKind::AlreadyExists => "EEXIST",
    io::ErrorKind::NotADirectory => "ENOTDIR",
    io::ErrorKind::IsADirectory => "EISDIR",
    io::ErrorKind::DirectoryNotEmpty => "ENOTEMPTY",
    io::ErrorKind::InvalidInput => "EINVAL",
    io::ErrorKind::Unsupported => "ENOTSUP",
    _ => "EIO",
  };
  let errno = error
    .raw_os_error()
    .map(|errno| -errno.checked_abs().unwrap_or(i32::MAX))
    .or(match code {
      "ENOENT" => Some(-2),
      "EACCES" => Some(-13),
      "EEXIST" => Some(-17),
      "ENOTDIR" => Some(-20),
      "EISDIR" => Some(-21),
      "EINVAL" => Some(-22),
      "ENOTEMPTY" => Some(-39),
      _ => None,
    });
  failure(
    vm,
    code,
    errno,
    Some(syscall),
    Some(path),
    dest,
    error.to_string(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
  use std::cell::RefCell;
  use std::fs;
  use std::rc::Rc;

  fn test_vm() -> Vm {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    Vm::new(globals)
  }

  #[test]
  fn direct_operations_cover_files_and_node_error_fields() {
    let directory = std::env::temp_dir().join(format!(
      "blitz-node-fs-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos()
    ));
    fs::create_dir_all(&directory).expect("create test directory");
    let path = directory.join("value.txt");
    let mut vm = test_vm();
    let path_value = Value::String(path.to_string_lossy().into_owned().into());
    let data = Value::String("hello".to_string().into());
    let write = write_file(
      &mut vm,
      &[
        Value::Undefined,
        path_value.clone(),
        data,
        Value::String("w".to_string().into()),
      ],
    )
    .expect("write operation");
    assert!(matches!(property(&write, OK), Value::Bool(true)));

    let read = read_file(&mut vm, &[Value::Undefined, path_value.clone()]).expect("read operation");
    assert!(matches!(property(&read, OK), Value::Bool(true)));
    let stats = stat(&mut vm, &[Value::Undefined, path_value.clone()]).expect("stat operation");
    assert!(matches!(property(&stats, OK), Value::Bool(true)));

    let nested = directory.join("nested");
    let nested_value = Value::String(nested.to_string_lossy().into_owned().into());
    let made = mkdir(
      &mut vm,
      &[Value::Undefined, nested_value.clone(), Value::Bool(true)],
    )
    .expect("mkdir operation");
    assert!(matches!(property(&made, OK), Value::Bool(true)));
    let listed = readdir(
      &mut vm,
      &[
        Value::Undefined,
        Value::String(directory.to_string_lossy().into_owned().into()),
      ],
    )
    .expect("readdir operation");
    assert!(matches!(property(&listed, OK), Value::Bool(true)));
    let moved = directory.join("moved.txt");
    let renamed = rename(
      &mut vm,
      &[
        Value::Undefined,
        path_value,
        Value::String(moved.to_string_lossy().into_owned().into()),
      ],
    )
    .expect("rename operation");
    assert!(matches!(property(&renamed, OK), Value::Bool(true)));
    let removed_file = unlink(
      &mut vm,
      &[
        Value::Undefined,
        Value::String(moved.to_string_lossy().into_owned().into()),
      ],
    )
    .expect("unlink operation");
    assert!(matches!(property(&removed_file, OK), Value::Bool(true)));
    let removed_directory = rm(
      &mut vm,
      &[
        Value::Undefined,
        nested_value,
        Value::Bool(true),
        Value::Bool(false),
      ],
    )
    .expect("rm operation");
    assert!(matches!(
      property(&removed_directory, OK),
      Value::Bool(true)
    ));

    let missing = read_file(
      &mut vm,
      &[
        Value::Undefined,
        Value::String(
          directory
            .join("missing")
            .to_string_lossy()
            .into_owned()
            .into(),
        ),
      ],
    )
    .expect("missing read operation");
    assert_eq!(
      property(&missing, "code"),
      Value::String("ENOENT".to_string().into())
    );
    assert_eq!(
      property(&missing, "syscall"),
      Value::String("open".to_string().into())
    );
    assert!(matches!(property(&missing, "errno"), Value::Number(value) if value < 0.0));

    fs::remove_dir_all(directory).expect("remove test directory");
  }

  #[test]
  fn denied_primitive_fails_before_touching_the_filesystem() {
    let mut vm = test_vm();
    let primitive = install(&vm, false);
    let read = property(&primitive, "readFile");
    let result = vm
      .call_value_with_this(
        &read,
        &Value::Undefined,
        &[
          Value::Undefined,
          Value::String("/definitely/not/read".to_string().into()),
        ],
      )
      .expect("permission denial is a result record");
    assert_eq!(
      property(&result, "code"),
      Value::String("ERR_ACCESS_DENIED".to_string().into())
    );
    assert_eq!(
      property(&result, "syscall"),
      Value::String("readFile".to_string().into())
    );
  }

  #[test]
  fn source_facade_covers_sync_callbacks_promises_urls_and_directories() {
    use blitz::module::{VirtualModule, evaluate_entry_shared};
    use oxc_span::SourceType;
    use std::path::Path;

    let directory = std::env::temp_dir().join(format!(
      "blitz-node-fs-facade-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos()
    ));
    let mut vm = test_vm();
    super::super::url::install(&mut vm);
    let bridge = object(vm.realm().object_prototype());
    set(&bridge, "abiVersion", Value::Number(1.0));
    set(&bridge, "nodeFileSystem", install(&vm, true));
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
          "blitz-internal:node-file-system",
          include_str!(
            "../../../../tools/effect-assets/src/facades/blitz-internal-node-file-system.js"
          ),
          SourceType::mjs(),
          None,
        ))
        .expect("register Node filesystem private adapter");
      loader
        .register_virtual_module(VirtualModule::javascript(
          "node:fs/promises",
          include_str!("../../../../tools/effect-assets/src/facades/node-fs-promises.js"),
          SourceType::mjs(),
          None,
        ))
        .expect("register Node filesystem promises facade");
      loader
        .register_virtual_module(VirtualModule::javascript(
          "node:fs",
          include_str!("../../../../tools/effect-assets/src/facades/node-fs.js"),
          SourceType::mjs(),
          None,
        ))
        .expect("register Node filesystem facade");
      loader
        .register_virtual_module(VirtualModule::javascript(
          "node-fs-facade-entry",
          [
            "import fs, { readFile as readFileCallback, readFileSync, writeFileSync, statSync, mkdirSync, readdirSync, renameSync, existsSync } from 'node:fs';\n",
            "import { readFile as readFilePromise, writeFile as writeFilePromise, stat, rm } from 'node:fs/promises';\n",
            &format!(
              "const root = {};\n",
              serde_json::to_string(&directory.to_string_lossy().into_owned())
                .expect("serialize test path")
            ),
            "const file = root + '/value.txt';\n",
            "const moved = root + '/moved.txt';\n",
            "mkdirSync(root);\n",
            "writeFileSync(file, 'hello');\n",
            "if (readFileSync(file, 'utf8') !== 'hello') throw new Error('sync text mismatch');\n",
            "if (!(readFileSync(file) instanceof Uint8Array)) throw new Error('sync bytes mismatch');\n",
            "if (!existsSync(file) || !statSync(file).isFile()) throw new Error('sync stat mismatch');\n",
            "try { readFileSync(root + '/missing'); throw new Error('missing read did not throw'); } catch (error) { if (error.code !== 'ENOENT' || error.syscall !== 'open' || error.path !== root + '/missing' || error.errno >= 0) throw error; globalThis.nodeFsErrorPassed = true; }\n",
            "if (readdirSync(root).indexOf('value.txt') < 0) throw new Error('readdir mismatch');\n",
            "const urlPath = { href: 'file://' + file };\n",
            "if (readFileSync(urlPath, 'utf8') !== 'hello') throw new Error('URL path mismatch');\n",
            "renameSync(file, moved);\n",
            "const promiseFile = root + '/promise.txt';\n",
            "const removable = root + '/removable';\n",
            "mkdirSync(removable);\n",
            "writeFilePromise(promiseFile, 'promise').then(() => readFilePromise(promiseFile, 'utf8')).then(value => { if (value !== 'promise') throw new Error('promise read/write mismatch'); globalThis.nodeFsPromiseReadWritePassed = true; });\n",
            "rm(removable, { recursive: true }).then(() => { globalThis.nodeFsPromiseRmPassed = true; });\n",
            "if (fs.constants.F_OK !== 0 || typeof fs.promises.readFile !== 'function') throw new Error('constants/promises mismatch');\n",
            "readFileCallback(moved, 'utf8', (error, value) => { if (error || value !== 'hello') throw error || new Error('callback mismatch'); globalThis.nodeFsCallbackPassed = true; });\n",
            "stat(moved).then(value => { if (!value.isFile()) throw new Error('promise stat mismatch'); globalThis.nodeFsPromisePassed = true; });\n",
          ]
          .concat(),
          SourceType::mjs(),
          None,
        ))
        .expect("register Node filesystem facade entry");
      let entry = loader
        .construct(Path::new("."), "node-fs-facade-entry")
        .expect("construct Node filesystem facade entry");
      loader
        .instantiate_all()
        .expect("instantiate Node filesystem facade entry");
      entry_path = entry.borrow().path.clone();
    }
    evaluate_entry_shared(&loader, &entry_path, &mut vm, false)
      .expect("evaluate Node filesystem facade entry");
    vm.flush_microtasks()
      .expect("flush Node filesystem facade jobs");

    assert_eq!(vm.get_global("nodeFsErrorPassed"), Some(Value::Bool(true)));
    assert_eq!(
      vm.get_global("nodeFsCallbackPassed"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("nodeFsPromisePassed"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("nodeFsPromiseReadWritePassed"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("nodeFsPromiseRmPassed"),
      Some(Value::Bool(true))
    );
    fs::remove_dir_all(directory).expect("remove facade test directory");
  }

  fn property(
    value: &Value,
    name: &str,
  ) -> Value {
    let Value::Object(object) = value else {
      panic!("result is an object");
    };
    match object
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
    {
      Some(blitz::value::PropertyDescriptor::Data { value, .. }) => value.clone(),
      _ => Value::Undefined,
    }
  }
}
