// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bytecode::{Constant, Opcode, decode_aux16, decode_rd, decode_rs};
use crate::value::ArrayData;
use crate::vm::jit::NumericBinaryOp;

pub(in crate::vm) struct CachedCallback {
  _chunk: Arc<crate::bytecode::Chunk>,
  plan: Option<CallbackPlan>,
}

enum CallbackPlan {
  Argument(usize),
  Upvalue(usize),
  Number(f64),
  Property {
    argument: usize,
    key: PropertyKey,
  },
  Numeric {
    left: usize,
    right: NumericInput,
    operation: NumericBinaryOp,
  },
}

#[derive(Clone, Copy)]
enum NumericInput {
  Argument(usize),
  Constant(f64),
}

fn opcode(
  word: u32,
  expected: Opcode,
) -> bool {
  word as u8 == expected as u8
}

impl CallbackPlan {
  fn eligible(proto: &Function) -> bool {
    !(proto.is_async
      || proto.is_generator
      || proto.is_class_constructor
      || proto.is_derived_constructor
      || proto.has_rest
      || !proto.has_simple_parameter_list)
  }

  fn decode(proto: &Function) -> Option<Self> {
    if !Self::eligible(proto) {
      return None;
    }
    let words = proto.chunk.words.as_slice();
    let argument = |word| {
      let slot = decode_aux16(word) as usize;
      (opcode(word, Opcode::LoadLocal) && slot < proto.arity).then_some(slot)
    };
    let number = |word| {
      if !opcode(word, Opcode::Const) {
        return None;
      }
      match proto
        .chunk
        .constants
        .get(decode_aux16(word) as usize)
      {
        Some(Constant::Number(value)) => Some(*value),
        _ => None,
      }
    };
    if let [load, ret] = words {
      if !opcode(*ret, Opcode::Return) || decode_rd(*ret) != decode_rd(*load) {
        return None;
      }
      return argument(*load)
        .map(Self::Argument)
        .or_else(|| number(*load).map(Self::Number))
        .or_else(|| {
          let slot = decode_aux16(*load) as usize;
          (opcode(*load, Opcode::GetUpvalue) && slot < proto.upvalues.len())
            .then_some(Self::Upvalue(slot))
        });
    }
    let [left, right, operation, ret] = words else {
      return None;
    };
    if !opcode(*ret, Opcode::Return)
      || decode_rd(*ret) != decode_rd(*operation)
      || decode_rd(*operation) != decode_rd(*left)
      || decode_rs(*operation) != decode_rd(*right)
      || decode_rd(*left) == decode_rd(*right)
    {
      return None;
    }
    let left = argument(*left)?;
    if opcode(*operation, Opcode::GetProperty) && opcode(*right, Opcode::Const) {
      let Constant::String(key) = proto
        .chunk
        .constants
        .get(decode_aux16(*right) as usize)?
      else {
        return None;
      };
      return Some(Self::Property {
        argument: left,
        key: PropertyKey::String(key.clone()),
      });
    }
    let operation = [
      (Opcode::Add, NumericBinaryOp::Add),
      (Opcode::Sub, NumericBinaryOp::Subtract),
      (Opcode::Mul, NumericBinaryOp::Multiply),
      (Opcode::Div, NumericBinaryOp::Divide),
    ]
    .into_iter()
    .find_map(|(expected, numeric)| opcode(*operation, expected).then_some(numeric))?;
    let right = argument(*right)
      .map(NumericInput::Argument)
      .or_else(|| number(*right).map(NumericInput::Constant))?;
    Some(Self::Numeric {
      left,
      right,
      operation,
    })
  }

  fn evaluate(
    &self,
    arguments: &[Value],
    environment: &crate::closure_environment::ClosureEnvironment,
  ) -> Option<Value> {
    let argument = |slot| {
      arguments
        .get(slot)
        .cloned()
        .unwrap_or(Value::Undefined)
    };
    match self {
      Self::Argument(slot) => {
        let value = argument(*slot);
        (!matches!(value, Value::Uninitialized)).then_some(value)
      }
      Self::Upvalue(slot) => {
        let value = environment.borrow().get(*slot)?.value();
        (!matches!(value, Value::Uninitialized)).then_some(value)
      }
      Self::Number(value) => Some(Value::Number(*value)),
      Self::Property {
        argument: slot,
        key,
      } => {
        let Value::Object(object) = argument(*slot) else {
          return None;
        };
        let object = object.borrow();
        if !object.internal_slots.is_empty() {
          return None;
        }
        match object.properties.get(key) {
          Some(PropertyDescriptor::Data { value, .. })
            if !matches!(value, Value::Uninitialized) =>
          {
            Some(value.clone())
          }
          _ => None,
        }
      }
      Self::Numeric {
        left,
        right,
        operation,
      } => {
        let Value::Number(left) = argument(*left) else {
          return None;
        };
        let right = match right {
          NumericInput::Argument(slot) => match argument(*slot) {
            Value::Number(value) => value,
            _ => return None,
          },
          NumericInput::Constant(value) => *value,
        };
        Some(Value::Number(match operation {
          NumericBinaryOp::Add => left + right,
          NumericBinaryOp::Subtract => left - right,
          NumericBinaryOp::Multiply => left * right,
          NumericBinaryOp::Divide => left / right,
        }))
      }
    }
  }
}

impl Vm {
  pub(crate) fn try_map_numeric_callback(
    &mut self,
    receiver: &Value,
    target: &Value,
    callback: &Value,
    length: usize,
  ) -> Result<bool, RuntimeError> {
    if !self.caches.optimized_execution_enabled {
      return Ok(false);
    }
    let Value::Closure { proto, .. } = callback else {
      return Ok(false);
    };
    if !CallbackPlan::eligible(proto) {
      return Ok(false);
    }
    let cached = self
      .caches
      .callbacks
      .entry((Arc::as_ptr(&proto.chunk) as usize, proto.arity))
      .or_insert_with(|| CachedCallback {
        _chunk: Arc::clone(&proto.chunk),
        plan: CallbackPlan::decode(proto),
      });
    let Some(CallbackPlan::Numeric {
      left,
      right,
      operation,
    }) = &cached.plan
    else {
      return Ok(false);
    };
    let (left, right, operation) = (*left, *right, *operation);
    if left > 1 || matches!(right, NumericInput::Argument(slot) if slot > 1) {
      return Ok(false);
    }
    let (Value::Array(source, source_props), Value::Array(destination, destination_props)) =
      (receiver, target)
    else {
      return Ok(false);
    };
    if Rc::ptr_eq(source, destination) {
      return Ok(false);
    }
    let plain_length = |properties: &Rc<RefCell<ObjectData>>| {
      let properties = properties.borrow();
      properties.internal_slots.is_empty()
        && properties.properties.len() == 1
        && matches!(properties.properties.get(&PropertyKey::String("length".into())),
          Some(PropertyDescriptor::Data { value: Value::Number(value), .. }) if *value == usize_to_f64(length))
    };
    if !plain_length(source_props)
      || !plain_length(destination_props)
      || !destination_props.borrow().extensible
    {
      return Ok(false);
    }
    let source = source.borrow();
    if source.len() != length || destination.borrow().len() > length || !source.is_dense_numeric() {
      return Ok(false);
    }
    self.bump_object_generation(target);
    let mut destination = destination.borrow_mut();
    let remaining = length - destination.len();
    destination.reserve_numeric_result(remaining);
    match operation {
      NumericBinaryOp::Add => {
        self.map_numeric_arguments(&source, &mut destination, length, left, right, |a, b| a + b)
      }
      NumericBinaryOp::Subtract => {
        self.map_numeric_arguments(&source, &mut destination, length, left, right, |a, b| a - b)
      }
      NumericBinaryOp::Multiply => {
        self.map_numeric_arguments(&source, &mut destination, length, left, right, |a, b| a * b)
      }
      NumericBinaryOp::Divide => {
        self.map_numeric_arguments(&source, &mut destination, length, left, right, |a, b| a / b)
      }
    }?;
    Ok(true)
  }

  fn map_numeric_arguments(
    &mut self,
    source: &ArrayData,
    destination: &mut ArrayData,
    length: usize,
    left: usize,
    right: NumericInput,
    operation: impl Fn(f64, f64) -> f64,
  ) -> Result<(), RuntimeError> {
    match (left == 0, right) {
      (true, NumericInput::Argument(0)) => {
        self.map_numeric_batches(source, destination, length, |_, value| {
          operation(value, value)
        })
      }
      (true, NumericInput::Argument(_)) => {
        self.map_numeric_batches(source, destination, length, |index, value| {
          operation(value, usize_to_f64(index))
        })
      }
      (false, NumericInput::Argument(0)) => {
        self.map_numeric_batches(source, destination, length, |index, value| {
          operation(usize_to_f64(index), value)
        })
      }
      (false, NumericInput::Argument(_)) => {
        self.map_numeric_batches(source, destination, length, |index, _| {
          operation(usize_to_f64(index), usize_to_f64(index))
        })
      }
      (true, NumericInput::Constant(constant)) => {
        self.map_numeric_batches(source, destination, length, |_, value| {
          operation(value, constant)
        })
      }
      (false, NumericInput::Constant(constant)) => {
        self.map_numeric_batches(source, destination, length, |index, _| {
          operation(usize_to_f64(index), constant)
        })
      }
    }
  }

  fn map_numeric_batches(
    &mut self,
    source: &ArrayData,
    destination: &mut ArrayData,
    length: usize,
    mapped: impl Fn(usize, f64) -> f64,
  ) -> Result<(), RuntimeError> {
    for start in (0..length).step_by(64) {
      self.poll_execution_interrupt()?;
      let count = (length - start).min(64);
      let mut batch = [0.0; 64];
      for (offset, output) in batch[..count].iter_mut().enumerate() {
        let index = start + offset;
        *output = mapped(
          index,
          source
            .numeric_element(index)
            .expect("validated dense numbers"),
        );
      }
      destination.write_numeric_batch(start, &batch[..count]);
      let completed = count as u64;
      self.profiling.jit_stats.leaf_callback_calls = self
        .profiling
        .jit_stats
        .leaf_callback_calls
        .saturating_add(completed);
      self.profiling.jit_stats.batched_callback_calls = self
        .profiling
        .jit_stats
        .batched_callback_calls
        .saturating_add(completed);
    }
    Ok(())
  }

  pub(in crate::vm) fn try_leaf_callback(
    &mut self,
    proto: &Rc<Function>,
    environment: &crate::closure_environment::ClosureEnvironment,
    arguments: &[Value],
    direct: bool,
  ) -> Result<Option<Value>, RuntimeError> {
    if !self.caches.optimized_execution_enabled
      || self.execution.interpreter_depth == 0
      || !CallbackPlan::eligible(proto)
    {
      return Ok(None);
    }
    let cached = self
      .caches
      .callbacks
      .entry((Arc::as_ptr(&proto.chunk) as usize, proto.arity))
      .or_insert_with(|| CachedCallback {
        _chunk: Arc::clone(&proto.chunk),
        plan: CallbackPlan::decode(proto),
      });
    let Some(plan) = &cached.plan else {
      return Ok(None);
    };
    if direct
      && !matches!(
        plan,
        CallbackPlan::Upvalue(_) | CallbackPlan::Property { .. }
      )
    {
      return Ok(None);
    }
    let result = plan.evaluate(arguments, environment);
    if result.is_some() {
      self.profiling.jit_stats.leaf_callback_calls = self
        .profiling
        .jit_stats
        .leaf_callback_calls
        .saturating_add(1);
      self.poll_execution_interrupt()?;
    } else {
      self.profiling.jit_stats.leaf_callback_fallbacks = self
        .profiling
        .jit_stats
        .leaf_callback_fallbacks
        .saturating_add(1);
    }
    Ok(result)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn specialized_numeric_map_matches_generic_argument_and_operation_combinations() {
    let source = crate::js!(
      r#"
      var input = [];
      for (var index = 0; index < 130; index = index + 1) input.push(index - 65);
      var callbacks = [
        function(v, i) { return v + v; }, function(v, i) { return v + i; },
        function(v, i) { return i + v; }, function(v, i) { return i + i; },
        function(v, i) { return v + 2; }, function(v, i) { return i + 2; },
        function(v, i) { return v - v; }, function(v, i) { return v - i; },
        function(v, i) { return i - v; }, function(v, i) { return i - i; },
        function(v, i) { return v - 2; }, function(v, i) { return i - 2; },
        function(v, i) { return v * v; }, function(v, i) { return v * i; },
        function(v, i) { return i * v; }, function(v, i) { return i * i; },
        function(v, i) { return v * 2; }, function(v, i) { return i * 2; },
        function(v, i) { return v / v; }, function(v, i) { return v / i; },
        function(v, i) { return i / v; }, function(v, i) { return i / i; },
        function(v, i) { return v / 2; }, function(v, i) { return i / 2; }
      ];
      var outputs = [];
      for (var c = 0; c < callbacks.length; c = c + 1) {
        outputs.push(input.map(callbacks[c]).join(','));
      }
      globalThis.result = outputs.join(';');
    "#
    );
    let run = |enabled| {
      let globals = Rc::new(RefCell::new(ObjectData::new()));
      globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".into()),
        PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
      );
      crate::runtime_utils::install_minimal_kernel_constructors(&globals);
      let mut vm = Vm::new(globals);
      vm.caches.optimized_execution_enabled = enabled;
      let session = crate::parser::Parser::parse_source(source).expect("parse");
      vm.run(crate::compiler::compile_session(&session).expect("compile"))
        .expect("map");
      (
        vm.get_global("result"),
        vm.profiling.jit_stats.batched_callback_calls,
      )
    };
    let (optimized, batches) = run(true);
    let (generic, _) = run(false);
    assert_eq!(optimized, generic);
    assert_eq!(batches, 24 * 130);
  }

  #[test]
  fn direct_leaf_calls_read_live_captures_and_preserve_fallback_effects() {
    let source = crate::js!(
      r#"
      function capture(value) {
        return [function() { return value; }, function(next) { value = next; }];
      }
      var a = capture(3), b = capture(9);
      var result = a[0]() + ':' + b[0]();
      a[1](12);
      result += ':' + a[0]() + ':' + b[0]();
      var getterCalls = 0;
      function property(value) { return value.x; }
      var object = { get x() { getterCalls++; return 17; } };
      result += ':' + property(object) + ':' + getterCalls;
      function optional(value = (++getterCalls)) { return value; }
      result += ':' + optional() + ':' + getterCalls;
      function tdz() {
        var read = function() { return later; };
        try { read(); } catch (error) { return error instanceof ReferenceError; }
        let later = 1;
      }
      result += ':' + tdz();
      var c = capture(-0);
      result += ':' + (1 / c[0]() === -Infinity);
    "#
    );
    for enabled in [false, true] {
      let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
      vm.caches.optimized_execution_enabled = enabled;
      let session = crate::parser::Parser::parse_source(source).expect("parse");
      vm.run(crate::compiler::compile_repl_session(&session).expect("compile"))
        .expect("calls");
      assert_eq!(
        vm.get_global("result"),
        Some(Value::String("3:9:12:9:17:1:2:2:true:true".into()))
      );
      if enabled {
        assert!(vm.profiling.jit_stats.leaf_callback_calls >= 5);
      }
    }
  }

  #[test]
  fn callback_decoder_rejects_changed_register_flow_and_parameter_setup() {
    let session = crate::parser::Parser::parse_source(crate::js!(
      "function square(value) { return value * value; }"
    ))
    .expect("parse");
    let root = crate::compiler::compile_session(&session).expect("compile");
    let proto = &root.functions[0];
    assert!(CallbackPlan::decode(proto).is_some());
    for offset in 0..4 {
      let mut changed = (*proto).clone();
      std::sync::Arc::make_mut(&mut changed.chunk).words[offset] ^= 1 << 8;
      assert!(
        CallbackPlan::decode(&changed).is_none(),
        "changed register at {offset}"
      );
    }
    let mut changed = (*proto).clone();
    changed.has_simple_parameter_list = false;
    assert!(CallbackPlan::decode(&changed).is_none());
    changed.has_simple_parameter_list = true;
    changed.is_async = true;
    assert!(CallbackPlan::decode(&changed).is_none());
  }
}
