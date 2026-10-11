// SPDX-License-Identifier: MPL-2.0

use denko::{
  Vm, compiler,
  parser::Parser,
  value::{ObjectData, Value},
};
use std::{cell::RefCell, rc::Rc, time::Instant};

#[test]
#[ignore = "manual scaling benchmark; run in release mode with --ignored --nocapture"]
fn typed_view_scaling() {
  for length in [4096usize, 16384, 65536, 262144] {
    let source = format!(
      "var values = new Float64Array({length}); function fill(values) {{ for (var i = 0; i < values.length; i = i + 1) values[i] = i; return i; }} function sum(values) {{ var total = 0; for (var i = 0; i < values.length; i = i + 1) total = total + values[i]; return total; }}"
    );
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    let session = Parser::parse_source(&source).unwrap();
    vm.run(compiler::compile_repl_session(&session).unwrap())
      .unwrap();
    let values = vm.get_global("values").unwrap();
    for (name, expected) in [
      ("fill", length as f64),
      ("sum", (length * (length - 1) / 2) as f64),
    ] {
      let function = vm.get_global(name).unwrap();
      let mut samples = Vec::new();
      for iteration in 0..14 {
        let start = Instant::now();
        let result = vm
          .call_value_with_this(&function, &Value::Undefined, std::slice::from_ref(&values))
          .unwrap();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(result, Value::Number(expected));
        if iteration >= 3 {
          samples.push(elapsed);
        }
      }
      samples.sort_by(f64::total_cmp);
      eprintln!("{name} {length}: {:.3} ms", samples[samples.len() / 2]);
    }
    assert!(
      vm.native_execution_statistics()
        .native_loop_iterations
        > 0
    );
  }
}
