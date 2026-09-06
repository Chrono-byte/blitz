// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::vm::region::NumericOperation as N;

#[derive(Clone)]
enum Symbol {
  Uninitialized,
  Numeric,
  Constant(Value),
}
impl Symbol {
  fn numeric(&self) -> bool {
    matches!(self, Self::Numeric | Self::Constant(Value::Number(_)))
  }
  fn value(&self) -> Option<&Value> {
    if let Self::Constant(value) = self {
      Some(value)
    } else {
      None
    }
  }
}

impl Vm {
  /// Resolve a namespace/property without invoking accessors or proxy traps.
  fn pure_region_object_property(
    value: &Value,
    key: &js_string::JsString,
  ) -> Option<Value> {
    let mut current = value.clone();
    for _ in 0..MAX_PROTOTYPE_DEPTH {
      let Value::Object(object) = current else {
        return None;
      };
      let object = object.borrow();
      if !object.internal_slots.is_empty() || object.module_namespace_exports.is_some() {
        return None;
      }
      if let Some(descriptor) = object
        .properties
        .get(&PropertyKey::String(key.clone()))
      {
        return match descriptor {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        };
      }
      current = object.get_prototype();
    }
    None
  }

  fn pure_region_global(
    &mut self,
    name: &str,
    slots: &[usize],
    writes: u16,
  ) -> Option<Value> {
    let mut environment = Some(Rc::clone(&self.execution.active.lexical_env));
    while let Some(current) = environment {
      let current = current.borrow();
      if current.binding_object().is_some() {
        return None;
      }
      if let Some(cell) = current.own_binding_cell(name) {
        if slots.iter().enumerate().any(|(index, slot)| {
          writes & (1 << index) != 0 && self.execution.active.locals[*slot].same_storage(&cell)
        }) {
          return None;
        }
        break;
      }
      environment = current.outer();
    }
    // The object-environment guard above makes this resolution non-observable.
    // TDZ/missing bindings decline so the interpreter handles their completion.
    self.resolve_identifier_value(name).ok().flatten()
  }

  // Resolve the intrinsic primitive-string prototype without any path capable
  // of calling user code.
  fn string_region_property(
    &self,
    text: &js_string::JsString,
    key: &js_string::JsString,
  ) -> Option<Value> {
    if key == "length" {
      return Some(Value::Number(usize_to_f64(text.len())));
    }
    // Indexed own properties precede prototype methods on primitive strings.
    if key.as_str_lossy().parse::<usize>().is_ok() {
      return None;
    }
    let mut prototype = self
      .realm_state
      .realm
      .intrinsics
      .string_prototype
      .clone()?;
    for _ in 0..MAX_PROTOTYPE_DEPTH {
      let Value::Object(object) = prototype else {
        return None;
      };
      let object = object.borrow();
      if !object.internal_slots.is_empty() {
        return None;
      }
      if let Some(descriptor) = object
        .properties
        .get(&PropertyKey::String(key.clone()))
      {
        return match descriptor {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        };
      }
      prototype = object.get_prototype();
    }
    None
  }

  pub(super) fn try_string_body_region(
    &mut self,
    region: crate::vm::region::StringBodyRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
    {
      return Ok(false);
    }
    let chunk = self.execution.active.chunk.clone();
    let words = &chunk.words[region.entry_pc..region.completed_pc - 1];
    let opcode = |word: u32| Opcode::from_byte((word & 0xff) as u8);
    let mut slots = Vec::new();
    let mut writes = 0u16;
    let mut reads_before_write = 0u16;
    for &word in words {
      if matches!(opcode(word), Some(Opcode::LoadLocal | Opcode::StoreLocal)) {
        let slot = decode_aux16(word) as usize;
        let index = match slots.iter().position(|entry| *entry == slot) {
          Some(index) => index,
          None if slots.len() < 16 => {
            slots.push(slot);
            slots.len() - 1
          }
          _ => return Ok(false),
        };
        if opcode(word) == Some(Opcode::StoreLocal) {
          writes |= 1 << index;
        } else if writes & (1 << index) == 0 {
          reads_before_write |= 1 << index;
        }
      }
    }
    let mut locals = [0.0; 16];
    let mut symbols: [Symbol; 16] = std::array::from_fn(|_| Symbol::Uninitialized);
    for (index, slot) in slots.iter().copied().enumerate() {
      let Some(cell) = self.execution.active.locals.get(slot) else {
        return Ok(false);
      };
      if slots[..index]
        .iter()
        .any(|previous| self.execution.active.locals[*previous].same_storage(cell))
      {
        return Ok(false);
      }
      if writes & (1 << index) != 0
        && (slot < self.execution.active.arguments.mapped_count
          || self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
          || matches!(cell.value(), Value::Uninitialized))
      {
        return Ok(false);
      }
      match cell.value() {
        Value::Number(number) => {
          locals[index] = number;
          symbols[index] = Symbol::Numeric;
        }
        value @ (Value::String(_) | Value::Object(_)) if writes & (1 << index) == 0 => {
          symbols[index] = Symbol::Constant(value)
        }
        _ if reads_before_write & (1 << index) == 0 => {}
        _ => return Ok(false),
      }
    }
    let mut states: [Symbol; 16] = std::array::from_fn(|_| Symbol::Uninitialized);
    let mut operations = Vec::new();
    let mut changed = 0u16;
    let mut calls = 0;
    for (offset, &word) in words.iter().enumerate() {
      if offset == 2 {
        continue;
      }
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      match opcode(word) {
        Some(Opcode::GetGlobal) => {
          let Some(crate::bytecode::Constant::String(name)) =
            chunk.constants.get(decode_aux16(word) as usize)
          else {
            return Ok(false);
          };
          let Some(value) = self.pure_region_global(name.as_str_lossy(), &slots, writes) else {
            return Ok(false);
          };
          if let Value::Number(number) = value {
            operations.push(N::Constant(rd, number));
          }
          states[rd] = Symbol::Constant(value);
        }
        Some(Opcode::LoadLocal | Opcode::StoreLocal) => {
          let slot = slots
            .iter()
            .position(|slot| *slot == decode_aux16(word) as usize)
            .expect("collected local");
          if opcode(word) == Some(Opcode::LoadLocal) {
            states[rd] = symbols[slot].clone();
            match &states[rd] {
              Symbol::Numeric => operations.push(N::Load(rd, slot)),
              Symbol::Constant(Value::Number(n)) => operations.push(N::Constant(rd, *n)),
              Symbol::Constant(_) => {}
              Symbol::Uninitialized => return Ok(false),
            }
          } else {
            if matches!(states[rd], Symbol::Uninitialized) {
              return Ok(false);
            }
            symbols[slot] = if states[rd].numeric() {
              operations.push(N::Store(slot, rd));
              Symbol::Numeric
            } else {
              states[rd].clone()
            };
          }
        }
        Some(Opcode::Const) => {
          states[rd] = match chunk.constants.get(decode_aux16(word) as usize) {
            Some(crate::bytecode::Constant::Number(n)) => {
              operations.push(N::Constant(rd, *n));
              Symbol::Constant(Value::Number(*n))
            }
            Some(crate::bytecode::Constant::String(s)) => {
              Symbol::Constant(Value::String(s.clone()))
            }
            _ => return Ok(false),
          };
        }
        Some(Opcode::Move) => {
          if matches!(states[rs], Symbol::Uninitialized) {
            return Ok(false);
          }
          states[rd] = states[rs].clone();
          if states[rd].numeric() {
            operations.push(N::Copy(rd, rs));
          }
        }
        Some(Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod) => {
          if !states[rd].numeric() || !states[rs].numeric() {
            return Ok(false);
          }
          operations.push(N::Binary(opcode(word).expect("opcode"), rd, rs));
          states[rd] = Symbol::Numeric;
        }
        Some(Opcode::Neg) => {
          if !states[rs].numeric() {
            return Ok(false);
          }
          operations.push(N::Negate(rd, rs));
          states[rd] = Symbol::Numeric;
        }
        Some(Opcode::GetProperty) => {
          let (Some(receiver), Some(Value::String(key))) = (states[rd].value(), states[rs].value())
          else {
            return Ok(false);
          };
          let resolved = match receiver {
            Value::String(text) => self.string_region_property(text, key),
            Value::Object(_) => Self::pure_region_object_property(receiver, key),
            _ => None,
          };
          let Some(value) = resolved else {
            return Ok(false);
          };
          if let Value::Number(n) = value {
            operations.push(N::Constant(rd, n));
          }
          states[rd] = Symbol::Constant(value);
        }
        Some(Opcode::CallMethod) => {
          let Some(Value::BuiltinObject(function, props)) = states[0].value() else {
            return Ok(false);
          };
          let count = decode_aux16(word) as usize;
          if matches!(
            props
              .borrow()
              .internal_slot("[[DenkoPlainJSONSerializer]]"),
            Some(Value::Bool(true))
          ) {
            if count != 1 {
              return Ok(false);
            }
            let Some(input) = states[2].value() else {
              return Ok(false);
            };
            let Some(output) = crate::ecmascript::builtins::json::stringify_plain_record(input)
            else {
              return Ok(false);
            };
            states[0] = Symbol::Constant(Value::String(output.into()));
            calls += 1;
          } else {
            if !matches!(
              props
                .borrow()
                .internal_slot("[[DenkoPureStringOperation]]"),
              Some(Value::Bool(true))
            ) {
              return Ok(false);
            }
            let mut arguments = Vec::with_capacity(count + 1);
            for state in &states[1..count + 2] {
              let Some(value @ (Value::String(_) | Value::Number(_))) = state.value() else {
                return Ok(false);
              };
              arguments.push(value.clone());
            }
            if !matches!(arguments[0], Value::String(_)) {
              return Ok(false);
            }
            // Tagged functions with these primitive arguments cannot call JS or throw.
            let result = function(self, &arguments)?;
            if let Value::Number(n) = result {
              operations.push(N::Constant(0, n));
            }
            states[0] = Symbol::Constant(result);
            calls += 1;
          }
        }
        _ => return Ok(false),
      }
      if opcode(word) != Some(Opcode::StoreLocal) {
        changed |= 1 << rd;
      }
      if offset == 1
        && (!states[decode_rd(words[0]) as usize].numeric()
          || !states[decode_rd(words[1]) as usize].numeric())
      {
        return Ok(false);
      }
    }
    for (index, symbol) in symbols.iter().enumerate().take(slots.len()) {
      if reads_before_write & writes & (1 << index) != 0 && !symbol.numeric() {
        return Ok(false);
      }
    }
    if calls == 0 || operations.len() < 2 {
      return Ok(false);
    }
    let comparison = (decode_rd(words[0]) as usize, decode_rd(words[1]) as usize);
    let Some(compiled) =
      self
        .profiling
        .jit
        .prepared_numeric_body(&chunk, region.entry_pc, &operations, comparison)
    else {
      return Ok(false);
    };
    let mut frame = crate::vm::jit::numeric_body::NumericBodyFrame {
      registers: [0.0; 16],
      locals,
      iterations: 0,
      completed: 0,
    };
    let mut iterations = 0u64;
    self.poll_execution_interrupt()?;
    loop {
      compiled.execute(&mut frame);
      iterations += u64::from(frame.iterations);
      self.profiling.jit_stats.native_loop_iterations = self
        .profiling
        .jit_stats
        .native_loop_iterations
        .saturating_add(u64::from(frame.iterations));
      if frame.iterations > 0 {
        self.write_string_region_state(
          registers,
          &slots,
          writes,
          &symbols,
          &frame.locals,
          changed,
          &states,
          &frame.registers,
        );
      }
      if frame.completed != 0 {
        // The final header supersedes the last body's register values.
        registers[comparison.0] = Value::Number(frame.registers[comparison.0]);
        registers[comparison.1] = Value::Number(frame.registers[comparison.1]);
        self.execution.active.pc = region.completed_pc;
        break;
      }
      self.execution.active.pc = region.entry_pc;
      self.poll_execution_interrupt()?;
      if iterations >= 4096 {
        break;
      }
    }
    self.profiling.jit_stats.native_loop_entries = self
      .profiling
      .jit_stats
      .native_loop_entries
      .saturating_add(1);
    Ok(true)
  }

  #[allow(clippy::too_many_arguments)]
  fn write_string_region_state(
    &mut self,
    registers: &mut [Value; 16],
    slots: &[usize],
    writes: u16,
    symbols: &[Symbol; 16],
    locals: &[f64; 16],
    changed: u16,
    states: &[Symbol; 16],
    numeric: &[f64; 16],
  ) {
    let value = |state: &Symbol, number| match state {
      Symbol::Constant(value) if !matches!(value, Value::Number(_)) => value.clone(),
      _ => Value::Number(number),
    };
    for (index, slot) in slots.iter().copied().enumerate() {
      if writes & (1 << index) != 0 {
        self.execution.active.locals[slot].set(value(&symbols[index], locals[index]));
      }
    }
    for index in 0..16 {
      if changed & (1 << index) != 0 {
        registers[index] = value(&states[index], numeric[index]);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn invariant_json_regions_revalidate_inputs_and_preserve_results() {
    let source = crate::js!(
      r#"
      function stringifyMany(record, count) {
        var last = 'untouched';
        for (var i = 0; i < count; i = i + 1) last = JSON.stringify(record);
        return last;
      }
      var record = { a: 1, '10': 'x', '2': false, omitted: undefined, sign: -0 };
      var first = stringifyMany(record, 5001);
      record.a = 2;
      var second = stringifyMany(record, 131);
      var empty = stringifyMany(record, 0);
      var result = first === '{"2":false,"10":"x","a":1,"sign":0}'
        && second === '{"2":false,"10":"x","a":2,"sign":0}' && empty === 'untouched';
    "#
    );
    for optimized in [false, true] {
      let vm = evaluate(source, optimized);
      assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
      if optimized {
        assert!(
          vm.native_execution_statistics()
            .native_loop_iterations
            >= 5132
        );
      }
    }
  }

  #[test]
  fn invariant_json_regions_decline_observable_getters_methods_and_mutations() {
    let source = crate::js!(
      r#"
      function stringifyMany(record, count) {
        var last = 'untouched';
        for (var i = 0; i < count; i = i + 1) last = JSON.stringify(record);
        return last;
      }
      var originalJSON = JSON;
      var reads = 0;
      JSON = { get stringify() { reads++; return originalJSON.stringify; } };
      var empty = stringifyMany({a: 1}, 0);
      var first = stringifyMany({a: 1}, 3);
      JSON = { stringify() { return 'replacement'; } };
      var replaced = stringifyMany({a: 1}, 2);
      JSON = originalJSON;
      var propertyReads = 0;
      var getter = stringifyMany({ get a() { propertyReads++; return propertyReads; } }, 3);
      var conversions = 0;
      var custom = stringifyMany({ toJSON() { conversions++; return conversions; } }, 3);
      var mutable = { a: 0 };
      var changed = '';
      for (var i = 0; i < 3; i = i + 1) {
        mutable.a = i;
        changed = JSON.stringify(mutable);
      }
      var result = empty === 'untouched' && reads === 3 && first === '{"a":1}'
        && replaced === 'replacement' && getter === '{"a":3}' && propertyReads === 3
        && custom === '3' && conversions === 3 && changed === '{"a":2}';
    "#
    );
    for optimized in [false, true] {
      let vm = evaluate(source, optimized);
      assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
    }
  }

  fn evaluate(
    source: &str,
    optimized: bool,
  ) -> Vm {
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    let code = crate::compiler::compile_repl_session(&session).expect("compile");
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    vm.caches.optimized_execution_enabled = optimized;
    vm.run(code).expect("evaluate");
    vm
  }

  #[test]
  fn string_regions_preserve_multiple_calls_unicode_and_rounding_order() {
    let source = r#"
      function run(text) {
        var total=10000000000000000;
        for(var i=0;i<131;i=i+1) {
          total=total+text.indexOf("x");
          total=total+text.slice(1,4).length;
          total=total+text.charCodeAt(2);
        }
        return total;
      }
      var result=run("é😀xyz");
    "#;
    let baseline = evaluate(source, false).get_global("result");
    let optimized = evaluate(source, true);
    assert_eq!(optimized.get_global("result"), baseline);
    assert!(
      optimized
        .native_execution_statistics()
        .native_loop_iterations
        >= 131
    );
  }

  #[test]
  fn string_regions_fall_back_for_getters_coercion_and_changing_receivers() {
    let source = r#"
      var original=String.prototype.indexOf;
      var gets=0;
      Object.defineProperty(String.prototype,"indexOf",{get:function(){"use strict";if(this!=="ax")throw "receiver";gets=gets+1;return original;},configurable:true});
      function run(text) {
        var total=0;
        for(var i=0;i<17;i=i+1) { total=total+text.indexOf("x"); }
        return total;
      }
      var a=run("ax");
      Object.defineProperty(String.prototype,"indexOf",{value:original,writable:true,configurable:true});
      var coerces=0;
      var b=run({toString:function(){coerces=coerces+1;return "xx";},indexOf:original});
      function changing(){var s="ax";var sum=0;for(var i=0;i<19;i=i+1){sum=sum+s.indexOf("x");s="x";}return sum;}
      var result=a===17&&gets===17&&b===0&&coerces===17&&changing()===1;
    "#;
    for optimized in [false, true] {
      assert_eq!(
        evaluate(source, optimized).get_global("result"),
        Some(Value::Bool(true))
      );
    }
  }

  #[test]
  fn primitive_string_methods_use_intrinsics_after_global_replacement() {
    let source = r#"
      "use strict";
      var original=String;
      String={prototype:{indexOf:function(){return 99;}}};
      function run(text){var sum=0;for(var i=0;i<129;i=i+1){sum=sum+text.indexOf("x");}return sum;}
      var a=run("ax");
      String=null;
      var b="abc".slice(1,3);
      var result=a===129&&b==="bc";
      String=original;
    "#;
    for optimized in [false, true] {
      assert_eq!(
        evaluate(source, optimized).get_global("result"),
        Some(Value::Bool(true))
      );
    }
  }

  #[test]
  fn primitive_string_accessor_errors_propagate() {
    let source = r#"
      var seen=false;
      Object.defineProperty(String.prototype,"custom",{get:function(){"use strict";seen=this==="abc";throw 42;},configurable:true});
      var caught=false;
      try { "abc".custom; } catch(error) { caught=error===42; }
      var result=seen&&caught;
    "#;
    for optimized in [false, true] {
      assert_eq!(
        evaluate(source, optimized).get_global("result"),
        Some(Value::Bool(true))
      );
    }
  }

  #[test]
  fn string_regions_preserve_zero_iteration_and_replaced_methods() {
    let source = r#"
      function run(limit){var text="abc";var sum=0;for(var i=0;i<limit;i=i+1){sum=sum+text.charCodeAt(1);}return sum;}
      var a=run(0);
      String.prototype.charCodeAt=function(){return 7;};
      var result=a===0&&run(129)===903;
    "#;
    for optimized in [false, true] {
      assert_eq!(
        evaluate(source, optimized).get_global("result"),
        Some(Value::Bool(true))
      );
    }
  }
}
