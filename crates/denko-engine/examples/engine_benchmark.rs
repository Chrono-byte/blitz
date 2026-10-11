// SPDX-License-Identifier: MPL-2.0

use denko::{
  Vm, compiler,
  parser::Parser,
  value::{ObjectData, PropertyDescriptor, PropertyKey, Value},
};
use std::{cell::RefCell, rc::Rc, time::Instant};

#[derive(clap::Parser)]
struct Arguments {
  file: std::path::PathBuf,
  warmups: usize,
  samples: std::num::NonZeroUsize,
  #[arg(long)]
  disassemble: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let arguments = <Arguments as clap::Parser>::parse();
  let warmups = arguments.warmups;
  let samples = arguments.samples.get();
  let iterations = warmups
    .checked_add(samples)
    .ok_or("iteration count overflow")?;
  let source = std::fs::read_to_string(&arguments.file)?;
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  let session = Parser::parse_source(&source)?;
  vm.run(compiler::compile_session(&session)?)?;
  let benchmark = vm
    .get_global("benchmark")
    .ok_or("missing benchmark function")?;
  if arguments.disassemble
    && let Value::Closure { proto, .. } = &benchmark
  {
    eprintln!("slots: {:?}", proto.slot_names);
    for pc in 0..proto.chunk.words.len() {
      eprintln!("{pc:4}: {}", proto.chunk.disassemble_at(pc)?);
    }
  }
  let mut milliseconds = Vec::with_capacity(samples);
  let mut checksum = None;
  for index in 0..iterations {
    let start = Instant::now();
    let result = vm.call_value_with_this(&benchmark, &Value::Undefined, &[])?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let Value::Number(value) = result else {
      return Err("benchmark must return a number".into());
    };
    if !value.is_finite() || checksum.is_some_and(|previous| previous != value) {
      return Err("benchmark checksum changed".into());
    }
    checksum = Some(value);
    if index >= warmups {
      milliseconds.push(elapsed);
    }
  }
  println!(
    "{}",
    serde_json::json!({"milliseconds":milliseconds, "checksum":checksum, "native_execution":vm.native_execution_statistics()})
  );
  Ok(())
}
