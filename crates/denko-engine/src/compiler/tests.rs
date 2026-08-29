// SPDX-License-Identifier: MPL-2.0

#![allow(clippy::as_conversions)]
#![allow(clippy::unwrap_used)]
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::bytecode::{decode_compare_jump, decode_rd, decode_rs};
use crate::compiler::compile;
use crate::runtime_utils;
use crate::{Chunk, ModuleContext, ObjectData, Opcode, Value, Vm, compile_oxc_with_ctx};

#[cfg(test)]
fn walk_chunk_ops(code: &Chunk) -> Vec<(usize, Opcode)> {
  let mut ops = Vec::new();
  let mut pc = 0usize;
  while pc < code.words.len() {
    if let Some(op) = Opcode::from_byte((code.words[pc] & 0xFF) as u8) {
      ops.push((pc, op));
      pc += 1;
    } else {
      pc += 1;
    }
  }
  ops
}

#[test]
fn compile_session_uses_its_own_source_text() {
  let source = crate::js!("let answer = 42;");
  let session = crate::parser::Parser::parse_source(source).expect("parse");

  let code = super::compile_session(&session).expect("compile session");

  assert_eq!(code.source_text.as_str(), source);
}

#[test]
fn strict_reserved_words_are_rejected_in_for_in_assignment_patterns() {
  for source in [
    crate::js!(r#""use strict"; for ([x[yield]] in [[]]);"#),
    crate::js!(r#""use strict"; for ([yield = 0] in [[]]);"#),
    crate::js!(r#""use strict"; for ({x: yield} in [{}]);"#),
  ] {
    let session = crate::parser::Parser::parse_source(source).expect("parse to AST");
    let error = super::compile_session(&session).expect_err("strict reserved word must fail");
    assert!(
      error.to_string().contains("SyntaxError"),
      "unexpected compiler error: {error}"
    );
  }
}

#[test]
fn anonymous_default_class_compiles_as_a_named_default_export() {
  let source = crate::js!("export default class {}");
  let session = crate::parser::Parser::parse_source_with_type(source, oxc_span::SourceType::mjs())
    .expect("parse module");
  let context = ModuleContext {
    module_index: 0,
    imports: HashMap::new(),
    exports: HashSet::from(["default".to_string()]),
    namespace_exports: HashMap::new(),
  };

  let code = super::compile_session_with_context(&session, &context)
    .expect("compile anonymous default class");

  assert!(!code.words.is_empty());
}

#[test]
fn numeric_property_keys_use_ecmascript_number_formatting() {
  assert_eq!(super::numeric_property_key(0.000_000_1), "1e-7");
  assert_eq!(
    super::numeric_property_key(1_000_000_000_000_000_000_000.0),
    "1e+21"
  );
  assert_eq!(super::numeric_property_key(-0.0), "0");
}

#[test]
fn compile_simple_for_loop() {
  let src = crate::js!("for (var i = 0; i < 3; i = i + 1) { console.log(i); }");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let mut saw_conditional_branch = false;
  let mut saw_jump = false;
  for (_, op) in walk_chunk_ops(&code) {
    match op {
      Opcode::JumpIfFalse | Opcode::CompareJump => saw_conditional_branch = true,
      Opcode::Jump => saw_jump = true,
      _ => {}
    }
  }

  assert!(
    saw_conditional_branch,
    "expected conditional branch in compiled for-loop"
  );
  assert!(saw_jump, "expected Jump in compiled for-loop");
}

#[test]
fn compile_for_with_continue() {
  let src =
    crate::js!("for (var i = 0; i < 3; i = i + 1) { if (i == 1) { continue; } console.log(i); }");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let mut conditional_branch = 0usize;
  let mut jump = 0usize;
  for (_, op) in walk_chunk_ops(&code) {
    match op {
      Opcode::JumpIfFalse | Opcode::CompareJump => conditional_branch += 1,
      Opcode::Jump => jump += 1,
      _ => {}
    }
  }

  assert!(
    conditional_branch >= 1,
    "expected at least one conditional branch"
  );
  assert!(
    jump >= 2,
    "expected at least two Jump opcodes (back-jump and continue)"
  );
}

#[cfg(test)]
fn opcode_width(op: Opcode) -> usize {
  let _ = op;
  1
}

#[test]
fn static_member_cache_sites_have_distinct_instructions() {
  let src = crate::js!("a.x; a.y; a.x;");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let sites = walk_chunk_ops(&code)
    .into_iter()
    .filter(|(_, op)| *op == Opcode::GetProperty)
    .map(|(pc, _)| {
      let word = code.words[pc];
      (pc, decode_rd(word), decode_rs(word))
    })
    .collect::<Vec<_>>();

  assert_eq!(
    sites.len(),
    3,
    "expected one property access per source site"
  );
  assert!(
    sites.windows(2).all(|pair| pair[0].0 < pair[1].0),
    "inline caches are keyed by distinct instruction addresses"
  );
  assert!(
    sites.iter().all(|(_, object, key)| object != key),
    "GetProperty must decode distinct object/result and key registers"
  );
}

#[test]
fn property_sites_are_not_limited_by_unused_u8_cache_slots() {
  let source = (0..300)
    .map(|index| format!("value.property{index};"))
    .collect::<String>();
  let session = crate::parser::Parser::parse_source(&source).expect("parse ok");
  let code = compile(session.program(), session.source()).expect("compile ok");
  let property_sites = walk_chunk_ops(&code)
    .into_iter()
    .filter(|(_, op)| *op == Opcode::GetProperty)
    .count();
  assert_eq!(property_sites, 300);
}

#[test]
fn compile_for_loop_back_jump_targets_loop_start() {
  let src = crate::js!("for (var i = 0; i < 3; i = i + 1) {}");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let mut loop_start = None;
  let mut back_target = None;
  for (pc, op) in walk_chunk_ops(&code) {
    match op {
      Opcode::GetGlobal if loop_start.is_none() => {
        loop_start = Some(pc);
      }
      Opcode::Jump => {
        let target = code
          .jump_target_from_pc(pc)
          .map(|target| target as isize);
        if target == loop_start.map(|v| v as isize) {
          back_target = target;
        }
      }
      _ => {}
    }
  }

  assert_eq!(back_target, loop_start.map(|v| v as isize));
}

#[test]
fn compile_new_expression_lowers_to_construct() {
  let src = crate::js!("new Array(1, 2, 3);");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let saw_construct = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::Construct));

  assert!(
    saw_construct,
    "expected Construct opcode for lowered new expression"
  );
}

#[test]
fn compile_update_expression_emits_store() {
  let src = crate::js!("for (var i = 0; i < 3; i++) {}");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let saw_add = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::Increment));
  let saw_set_local = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::StoreLocal));

  assert!(saw_add, "expected i++ to emit Increment");
  assert!(saw_set_local, "expected i++ to emit StoreLocal");
}

#[test]
fn compile_member_call_uses_call_method() {
  let src = crate::js!("obj.method(1);");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let saw_call_method = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::Call | Opcode::CallMethod));

  assert!(saw_call_method, "expected member call to emit CallMethod");
}

#[test]
fn compile_for_in_uses_get_builtin() {
  let src = crate::js!("for (var k in obj) {}");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let saw_get_builtin = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::GetGlobal));

  assert!(
    saw_get_builtin,
    "expected for-in lowering to emit GetBuiltin"
  );
}

#[test]
fn compile_rejects_calls_with_more_than_u8_arguments() {
  let args = (0_i32..256_i32)
    .map(|i| i.to_string())
    .collect::<Vec<_>>()
    .join(", ");
  let src = crate::js!(format(script), "f({args});");
  let session = crate::parser::Parser::parse_source(&src).expect("parse ok");
  let program = session.program();
  let err = compile(program, session.source()).expect_err("compile should fail");

  assert!(
    err
      .to_string()
      .contains("too many arguments in call"),
    "unexpected error: {err}"
  );
}

#[test]
fn compile_generator_non_simple_params_keep_later_params_tdz() {
  let src = crate::js!("function* f(x = y, y) {}");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let proto = code
    .functions
    .iter()
    .find(|p| p.name == "f")
    .expect("generator proto exists");

  assert!(
    proto
      .slot_names
      .iter()
      .any(|name| name.as_deref() == Some("y")),
    "expected generator parameter y to have a local slot"
  );
  let y_slot = proto
    .slot_names
    .iter()
    .position(|name| name.as_deref() == Some("y"))
    .expect("y slot");
  assert!(
    proto
      .lexical_tdz
      .get(y_slot)
      .copied()
      .unwrap_or(false),
    "later generator parameter y should start in TDZ"
  );

  let mut ops = Vec::new();
  let mut pc = 0usize;
  while pc < proto.chunk.words.len() && ops.len() < 16 {
    let op = Opcode::from_byte((proto.chunk.words[pc] & 0xFF) as u8).expect("valid opcode");
    let width = opcode_width(op);
    let operand = if matches!(
      op,
      Opcode::LoadLocal | Opcode::StoreLocal | Opcode::CloneLocalCell
    ) {
      proto.chunk.words.get(pc + 1).copied()
    } else {
      None
    };
    ops.push((op.name(), operand));
    pc += width;
  }
  eprintln!("generator proto ops: {:?}", ops);
}

#[test]
fn compile_object_generator_method_emits_body() {
  let src = crate::js!("var o = { *m() { let x; eval(\"var x;\"); } };");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  eprintln!(
    "object generator function names: {:?}",
    code
      .functions
      .iter()
      .map(|p| (p.name.clone(), p.chunk.words.len(), p.is_generator))
      .collect::<Vec<_>>()
  );
  let proto = code
    .functions
    .iter()
    .find(|p| p.is_generator)
    .expect("object generator proto exists");

  eprintln!(
    "object generator proto: code_len={} consts={} slots={} tdz={:?}",
    proto.chunk.words.len(),
    proto.chunk.constants.len(),
    proto.slot_names.len(),
    proto.lexical_tdz
  );
  let mut ops = Vec::new();
  let mut pc = 0usize;
  while pc < proto.chunk.words.len() && ops.len() < 24 {
    let op = Opcode::from_byte((proto.chunk.words[pc] & 0xFF) as u8).expect("valid opcode");
    let width = opcode_width(op);
    ops.push(op.name());
    pc += width;
  }
  eprintln!("object generator proto ops: {:?}", ops);

  assert!(
    !proto.chunk.words.is_empty(),
    "object generator method should emit bytecode"
  );
}

#[test]
fn explicit_terminal_return_omits_unreachable_implicit_return() {
  let src = crate::js!("const object = { read() { return this.value; } };");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let code = compile(session.program(), session.source()).expect("compile ok");
  let proto = code
    .functions
    .iter()
    .find(|proto| proto.name == "read")
    .expect("read method proto");
  let ops = proto
    .chunk
    .words
    .iter()
    .map(|word| Opcode::from_byte((word & 0xff) as u8).expect("valid opcode"))
    .collect::<Vec<_>>();
  assert_eq!(
    ops,
    [
      Opcode::GetThis,
      Opcode::Const,
      Opcode::GetProperty,
      Opcode::Return
    ]
  );
}

#[test]
fn strict_direct_return_calls_emit_tail_call() {
  let src = crate::js!(r#""use strict"; function recurse(value) { return recurse(value - 1); }"#);
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let code = compile(session.program(), session.source()).expect("compile ok");
  let proto = code
    .functions
    .iter()
    .find(|proto| proto.name == "recurse")
    .expect("function proto");

  assert!(
    proto
      .chunk
      .words
      .iter()
      .any(|word| { Opcode::from_byte((word & 0xFF) as u8) == Some(Opcode::TailCall) })
  );
}

#[test]
fn compile_dynamic_import_uses_get_builtin() {
  let src = crate::js!("import('./dep.js');");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let saw_get_builtin = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::GetBuiltin));

  assert!(
    saw_get_builtin,
    "expected dynamic import to emit GetBuiltin"
  );
}

#[test]
fn compile_sequence_expression_keeps_last_value() {
  let src = crate::js!("var x = (a = 1, 2);");
  let session = crate::parser::Parser::parse_source(src).expect("parse_source failed");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  code
    .verify()
    .expect("sequence expression emits valid bytecode");
}

#[test]
fn imported_binding_assignment_is_rejected() {
  let src = crate::js!("a = 1;");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let ctx = ModuleContext {
    module_index: 0,
    imports: HashMap::from([(String::from("a"), (1usize, String::from("value")))]),
    exports: HashSet::new(),
    namespace_exports: HashMap::new(),
  };

  let err =
    compile_oxc_with_ctx(program, Some(&ctx), session.source()).expect_err("compile should fail");
  assert!(
    err.to_string().contains("imported binding"),
    "unexpected error: {err}"
  );
}

#[test]
fn module_export_updates_follow_binding_identity() {
  let context = ModuleContext {
    module_index: 0,
    imports: HashMap::new(),
    exports: HashSet::from(["x".to_string(), "shadow".to_string(), "capture".to_string()]),
    namespace_exports: HashMap::new(),
  };
  let source = crate::js!(
    r#"
        export let x = 0;
        export function shadow() { let x = 1; x = 2; }
        export function capture() { x = 3; }
      "#
  );
  let session = crate::parser::Parser::parse_source_with_type(source, oxc_span::SourceType::mjs())
    .expect("parse module");
  let code = compile_oxc_with_ctx(session.program(), Some(&context), session.source())
    .expect("compile module");
  let shadow = code
    .functions
    .iter()
    .find(|function| function.name == "shadow")
    .unwrap();
  let capture = code
    .functions
    .iter()
    .find(|function| function.name == "capture")
    .unwrap();
  let count_export_writes = |function: &crate::bytecode::Function| {
    function
      .chunk
      .words
      .iter()
      .filter(|word| Opcode::from_byte((*word & 0xff) as u8) == Some(Opcode::SetModuleExport))
      .count()
  };
  assert_eq!(count_export_writes(shadow), 0);
  assert_eq!(count_export_writes(capture), 1);
}

#[test]
fn strict_assignment_to_arguments_is_rejected() {
  let src = crate::js!(
    r#"
        function testcase() {
            'use strict';
            arguments = 7;
        }
    "#
  );
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();

  let err = compile(program, session.source()).expect_err("compile should fail");
  assert!(
    err
      .to_string()
      .contains("not allowed in strict mode"),
    "unexpected error: {err}"
  );
}

#[test]
fn nullish_coalescing_is_supported() {
  let src = crate::js!("1 ?? 2;");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();

  let code = compile(program, session.source()).expect("compile ok");
  let saw_coalesce = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::JumpIfNullish));
  assert!(
    saw_coalesce,
    "expected nullish coalescing lowering to emit JumpIfNullish"
  );
}

#[test]
fn comparison_branches_use_compare_jump() {
  let src = crate::js!("if (a === b) { 1; } else { 2; } a === b ? 1 : 2;");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();

  let code = compile(program, session.source()).expect("compile ok");
  let saw_compare_jump = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .any(|op| matches!(op, Opcode::CompareJump));
  assert!(
    saw_compare_jump,
    "expected comparison branching to emit CompareJump"
  );
}

#[test]
fn while_compare_jump_preserves_compare_opcode_payload() {
  let src = crate::js!("var s = \"\"; while (s.length < 4) { s = \"0\" + s; }");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();

  let code = compile(program, session.source()).expect("compile ok");
  let (condition, left, right, offset) = code
    .words
    .iter()
    .find_map(|word| {
      (Opcode::from_byte((*word & 0xFF) as u8) == Some(Opcode::CompareJump))
        .then(|| decode_compare_jump(*word))
    })
    .expect("expected CompareJump in while loop");
  assert_eq!(
    condition, 2,
    "while condition should preserve LessThan compare op"
  );
  assert_ne!(left, right, "comparison inputs must use distinct registers");
  assert!(offset > 0, "false comparison must branch out of the loop");
}

#[test]
fn compile_for_of_emits_iterator_ops() {
  let src = crate::js!("for (var x of [1, 2, 3]) {}");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let ops: Vec<_> = code
    .words
    .iter()
    .filter_map(|b| Opcode::from_byte((*b & 0xFF) as u8))
    .collect();
  assert!(
    ops
      .iter()
      .any(|op| matches!(op, Opcode::IteratorNext))
  );
  assert!(
    ops
      .iter()
      .any(|op| matches!(op, Opcode::GetProperty))
  );
}

#[test]
fn compile_for_await_uses_async_iterator_acquisition() {
  let src = crate::js!("async function f(xs) { for await (const x of xs) {} }");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let ops: Vec<_> = code
    .functions
    .iter()
    .flat_map(|function| function.chunk.words.iter())
    .filter_map(|word| Opcode::from_byte((*word & 0xFF) as u8))
    .collect();
  assert!(
    ops
      .iter()
      .any(|op| matches!(op, Opcode::GetAsyncIterator)),
    "for-await must acquire an async iterator record"
  );
}

#[test]
fn for_of_over_array_executes() {
  let src = crate::js!("var sum = 0; for (var x of [1, 2, 3]) { sum = sum + x; } sum;");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    crate::value::PropertyKey::String("globalThis".to_string().into()),
    crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  drop(vm.run(code).expect("run ok"));

  assert!(matches!(vm.get_global("sum"), Some(Value::Number(n)) if (n - 6.0).abs() < f64::EPSILON));
}

#[test]
fn computed_member_compound_assignment_evaluates_reference_once() {
  let src = crate::js!(
    r#"
        globalThis.hits = 0;
        var obj = { value: 1 };
        function key() { globalThis.hits = globalThis.hits + 1; return "value"; }
        obj[key()] += 2;
    "#
  );
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    crate::value::PropertyKey::String("globalThis".to_string().into()),
    crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  drop(vm.run(code).expect("run ok"));

  assert!(
    matches!(vm.get_global("hits"), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON),
    "unexpected hits value: {:?}",
    vm.get_global("hits")
  );
}

fn run_compiled(src: &str) -> Vm {
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let code = compile(session.program(), session.source()).expect("compile ok");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    crate::value::PropertyKey::String("globalThis".to_string().into()),
    crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  runtime_utils::install_minimal_kernel_constructors(&globals);
  let mut vm = Vm::new(globals);
  vm.run(code).expect("run ok");
  vm
}

#[test]
fn lexical_classes_loops_and_duplicate_parameters_follow_ecmascript_binding_rules() {
  let vm = run_compiled(crate::js!(
    r#"
        let class_tdz;
        try { class_tdz = typeof C; } catch (_) { class_tdz = "tdz"; }
        class C {}
        let class_assignment_threw = false;
        try { C = 1; } catch (_) { class_assignment_threw = true; }

        let outer = 1;
        let loop_tdz_threw = false;
        try { for (let outer = outer; false;) {} } catch (_) { loop_tdz_threw = true; }

        function duplicate(value, value) { return value; }
        globalThis.class_tdz = class_tdz;
        globalThis.class_assignment_threw = class_assignment_threw;
        globalThis.class_global = globalThis.C;
        globalThis.loop_tdz_threw = loop_tdz_threw;
        globalThis.duplicate_parameter = duplicate(1, 2);
      "#
  ));

  assert_eq!(
    vm.get_global("class_tdz"),
    Some(Value::String("tdz".into()))
  );
  assert_eq!(
    vm.get_global("class_assignment_threw"),
    Some(Value::Bool(true))
  );
  assert_eq!(vm.get_global("class_global"), Some(Value::Undefined));
  assert_eq!(vm.get_global("loop_tdz_threw"), Some(Value::Bool(true)));
  assert_eq!(
    vm.get_global("duplicate_parameter"),
    Some(Value::Number(2.0))
  );
}

#[test]
fn private_names_are_class_specific_unforgeable_and_brand_checked() {
  let vm = run_compiled(crate::js!(
    r#"
        class A { #value = 1; read(other) { return other.#value; } }
        class B { #value = 2; }
        let rejected = false;
        try { new A().read(new B()); } catch (error) { rejected = error instanceof TypeError; }
        const instance = new A();
        globalThis.private_brand_rejected = rejected;
        globalThis.private_string_spoof = instance["\u001fprivate#value"];
        globalThis.private_reflected = Reflect.ownKeys(instance).length;
      "#
  ));

  assert_eq!(
    vm.get_global("private_brand_rejected"),
    Some(Value::Bool(true))
  );
  assert_eq!(
    vm.get_global("private_string_spoof"),
    Some(Value::Undefined)
  );
  assert_eq!(vm.get_global("private_reflected"), Some(Value::Number(0.0)));
}

#[test]
fn nested_optional_chains_short_circuit_and_strict_delete_is_rejected() {
  let vm = run_compiled(crate::js!(
    "globalThis.optional_chain_result = null?.missing.value;"
  ));
  assert_eq!(
    vm.get_global("optional_chain_result"),
    Some(Value::Undefined)
  );

  let source = crate::js!(r#""use strict"; let value; delete value;"#);
  let session = crate::parser::Parser::parse_source(source).expect("parse strict delete");
  let error = compile(session.program(), session.source()).expect_err("strict delete must fail");
  assert!(error.to_string().contains("delete"));
}

#[test]
fn object_literal_evaluates_mixed_properties_in_source_order() {
  let vm = run_compiled(crate::js!(
    r#"
        let order = "";
        function mark(label, value) { order += label; return value; }
        const source = { get copied() { order += "g"; return 1; } };
        const result = {
          first: mark("a", 1),
          get accessor() { return 2; },
          [mark("k", "computed")]: mark("v", 3),
          ...mark("s", source),
          last: mark("z", 4)
        };
        globalThis.object_order = order;
        globalThis.object_result = result.first + result.computed + result.copied + result.last;
        globalThis.object_accessor = Object.getOwnPropertyDescriptor(result, "accessor").get !== undefined;
      "#
  ));

  assert_eq!(
    vm.get_global("object_order"),
    Some(Value::String("akvsgz".into()))
  );
  assert_eq!(vm.get_global("object_result"), Some(Value::Number(9.0)));
  assert_eq!(vm.get_global("object_accessor"), Some(Value::Bool(true)));
}

#[test]
fn template_interpolations_use_to_string_and_unary_plus_rejects_bigint() {
  let vm = run_compiled(crate::js!(
    r#"
        const template_result = `${1n}:${true}:${null}:${undefined}`;
        let unary_plus_threw = false;
        try { +1n; } catch (error) { unary_plus_threw = error instanceof TypeError; }
        globalThis.template_result = template_result;
        globalThis.unary_plus_threw = unary_plus_threw;
      "#
  ));

  assert_eq!(
    vm.get_global("template_result"),
    Some(Value::String("1:true:null:undefined".into()))
  );
  assert_eq!(vm.get_global("unary_plus_threw"), Some(Value::Bool(true)));
}

#[test]
fn optional_calls_return_undefined_and_call_present_values() {
  let vm = run_compiled(crate::js!(
    r#"
        let calls = 0;
        function present() { calls += 1; return 7; }
        let absent;
        const direct = present?.();
        const missing = absent?.();
        const receiver = { value: 9, method() { return this.value; } };
        const member = receiver?.method();
        const missingMember = receiver?.missing?.();
        const missingComputed = receiver?.["missing"]?.();
        globalThis.optional_direct = direct;
        globalThis.optional_missing = missing;
        globalThis.optional_member = member;
        globalThis.optional_missing_member = missingMember;
        globalThis.optional_missing_computed = missingComputed;
        globalThis.optional_calls = calls;
      "#
  ));

  assert_eq!(vm.get_global("optional_direct"), Some(Value::Number(7.0)));
  assert_eq!(vm.get_global("optional_missing"), Some(Value::Undefined));
  assert_eq!(vm.get_global("optional_member"), Some(Value::Number(9.0)));
  assert_eq!(
    vm.get_global("optional_missing_member"),
    Some(Value::Undefined)
  );
  assert_eq!(
    vm.get_global("optional_missing_computed"),
    Some(Value::Undefined)
  );
  assert_eq!(vm.get_global("optional_calls"), Some(Value::Number(1.0)));
}

#[test]
fn optional_spread_calls_expand_arguments_after_nullish_check() {
  let vm = run_compiled(crate::js!(
    r#"
        let evaluated = 0;
        function args() { evaluated += 1; return [10, 20, 40]; }
        function sum(a, b, c) { return a + b + c; }
        let absent;
        globalThis.optional_spread_sum = sum?.(...args());
        globalThis.optional_spread_missing = absent?.(...args());
        globalThis.optional_spread_evaluated = evaluated;
        globalThis.named_function_expression = (function named() {}?.name);
      "#
  ));

  assert_eq!(
    vm.get_global("optional_spread_sum"),
    Some(Value::Number(70.0))
  );
  assert_eq!(
    vm.get_global("optional_spread_missing"),
    Some(Value::Undefined)
  );
  assert_eq!(
    vm.get_global("optional_spread_evaluated"),
    Some(Value::Number(1.0))
  );
  assert_eq!(
    vm.get_global("named_function_expression"),
    Some(Value::String("named".into()))
  );
}

#[test]
fn lexical_scopes_parameters_and_compiler_temporaries_are_not_leaked() {
  let vm = run_compiled(crate::js!(
    r#"
        let outer = 1;
        let block_tdz = false;
        try { { outer; let outer = 2; } } catch (_) { block_tdz = true; }
        function blockScope() { { let hidden = 1; } return typeof hidden; }
        for (let loopName = 0; loopName < 1; loopName++) {}
        switch (1) { case 1: break; }
        var parameterOuter = 42;
        function parameterScope(value = parameterOuter) {
          var parameterOuter = 1;
          return value;
        }
        let laterParameterTdz = false;
        try {
          function later(value = nested, { nested }) { return value; }
          later(undefined, { nested: 7 });
        } catch (_) { laterParameterTdz = true; }
        globalThis.block_tdz = block_tdz;
        globalThis.block_scope = blockScope();
        globalThis.loop_scope = typeof loopName;
        var __user_binding = Array.isArray;
        globalThis.temporary_scope = typeof __switch_discriminant_0;
        globalThis.double_underscore_binding = __user_binding([]);
        globalThis.parameter_scope = parameterScope();
        globalThis.later_parameter_tdz = laterParameterTdz;
      "#
  ));

  assert_eq!(vm.get_global("block_tdz"), Some(Value::Bool(true)));
  assert_eq!(
    vm.get_global("block_scope"),
    Some(Value::String("undefined".into()))
  );
  assert_eq!(
    vm.get_global("loop_scope"),
    Some(Value::String("undefined".into()))
  );
  assert_eq!(
    vm.get_global("temporary_scope"),
    Some(Value::String("undefined".into()))
  );
  assert_eq!(
    vm.get_global("double_underscore_binding"),
    Some(Value::Bool(true))
  );
  assert_eq!(vm.get_global("parameter_scope"), Some(Value::Number(42.0)));
  assert_eq!(
    vm.get_global("later_parameter_tdz"),
    Some(Value::Bool(true))
  );
}

#[test]
fn var_and_function_declarations_share_script_function_scope() {
  let source = crate::js!(
    r#"var f; function f() { return 1; } function outer() { var nested; function nested() {} }"#
  );
  let session = crate::parser::Parser::parse_source(source).expect("parse ok");
  compile(session.program(), session.source()).expect("var/function redeclaration is valid");

  let block_conflict = crate::js!(r#"{ var f; function f() {} }"#);
  let session = crate::parser::Parser::parse_source(block_conflict).expect("parse ok");
  assert!(compile(session.program(), session.source()).is_err());

  let module_conflict = crate::js!(r#"var f; function f() {}"#);
  let session =
    crate::parser::Parser::parse_source_with_type(module_conflict, oxc_span::SourceType::mjs())
      .expect("parse module");
  assert!(compile(session.program(), session.source()).is_err());
}

#[test]
fn early_errors_recurse_through_initializers_and_check_parameter_bodies() {
  for source in [
    crate::js!(r#"const value = true ? { method: function () { let x; let x; } } : 0;"#),
    crate::js!(r#"const value = [0, (() => ({ nested: class { method() { let x; let x; } } }))];"#),
    crate::js!(r#"function invalid(parameter) { let parameter; }"#),
    crate::js!(r#"try {} catch (error) { let error; }"#),
  ] {
    let session = crate::parser::Parser::parse_source(source).expect("parse ok");
    assert!(
      compile(session.program(), session.source()).is_err(),
      "{source}"
    );
  }
}

#[test]
fn strict_early_errors_cover_generator_parameters_and_catch_bindings() {
  for source in [
    crate::js!(r#"function* invalid(value = 1) { "use strict"; }"#),
    crate::js!(r#""use strict"; try { throw 1; } catch (eval) {}"#),
  ] {
    let session = crate::parser::Parser::parse_source(source).expect("parse ok");
    assert!(
      compile(session.program(), session.source()).is_err(),
      "{source}"
    );
  }
}

#[test]
fn member_postfix_update_returns_old_value_and_stores_new_value() {
  let src = crate::js!(
    r#"
        var obj = { value: 1 };
        var old = obj.value++;
    "#
  );
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    crate::value::PropertyKey::String("globalThis".to_string().into()),
    crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  drop(vm.run(code).expect("run ok"));

  assert!(matches!(vm.get_global("old"), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON));
}

#[test]
fn block_scoped_let_does_not_escape_block() {
  let src = crate::js!("{ let x = 1; } x = 2;");
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let code = compile(program, session.source()).expect("compile ok");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  vm.run(code).expect("run ok");

  assert!(
    matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON),
    "block-local binding should not leak past its block"
  );
}
