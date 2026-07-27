// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::compiler;
use crate::parser::Parser;
use crate::runtime_utils;
use crate::runtime_utils::strict_equal;
use crate::value::ArrayData;
use crate::vm::runtime::property::{get_property, same_value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

fn test_globals() -> Rc<RefCell<ObjectData>> {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Clone::clone(&globals))),
  );
  runtime_utils::install_minimal_kernel_constructors(&globals);
  globals
}

fn run_script(src: &str) -> (Vm, Rc<RefCell<ObjectData>>) {
  let session = Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  let chunk = compiler::compile_oxc(program, session.source()).expect("compile ok");

  let globals = test_globals();
  let mut vm = Vm::new(Clone::clone(&globals));
  vm.run(chunk).expect("run ok");
  (vm, globals)
}

mod test {
  #![allow(clippy::panic)]
  use super::*;

  #[test]
  fn byte_typed_arrays_share_array_buffer_storage() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const buffer = new ArrayBuffer(8);
        const bytes = new Uint8Array(buffer);
        const signed = new Int8Array(buffer, 2, 3);
        const clamped = new Uint8ClampedArray(buffer, 4, 2);
        bytes[0] = 255;
        signed[0] = -2;
        clamped[0] = 300;
        bytes[5] = 127;
        const sub = bytes.subarray(1, 4);
        sub[0] = 42;
        const copy = bytes.slice(0, 3);
        bytes[1] = 99;
        bytes.set(bytes.subarray(0, 3), 1);
        globalThis.result = [
          bytes[0], bytes[1], bytes[2], bytes[3], signed[0], clamped[0], bytes[5],
          bytes.buffer === buffer, signed.byteOffset, signed.byteLength, signed.length,
          sub.buffer === buffer, copy[1]
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "255,255,99,254,99,255,127,true,2,3,3,true,42"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn byte_view_branding_cannot_be_spoofed_by_properties() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const fake = {
          "[[TypedArrayKind]]": "Uint8Array",
          "[[TypedArrayData]]": [99],
          "[[TypedArrayLength]]": 1,
          "[[TypedArrayByteOffset]]": 0
        };
        let methodRejected = false;
        try {
          Uint8Array.prototype.subarray.call(fake);
        } catch (error) {
          methodRejected = true;
        }
        fake[0] = 7;
        const fromFake = new Uint8Array(fake);
        globalThis.result = [
          methodRejected, fake[0], fromFake.length, fake["[[TypedArrayKind]]"]
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("true,7,0,Uint8Array".to_string().into()))
    );
  }

  #[test]
  fn uint8_array_host_view_uses_hidden_brand_offset_and_backing() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const backing = new Uint8Array([9, 1, 2, 9]);
        globalThis.buffer = backing.buffer;
        globalThis.view = new Uint8Array(backing.buffer, 1, 2);
        view["[[TypedArrayKind]]"] = "Int8Array";
        view["[[TypedArrayByteOffset]]"] = 0;
      "#
    ));
    let view = vm.get_global("view").expect("view global");
    let host_view = vm
      .uint8_array_byte_view(&view)
      .expect("genuine attached Uint8Array view");
    let mut bytes = [0; 2];
    assert!(host_view.read_into(0, &mut bytes));
    assert_eq!(bytes, [1, 2]);
    assert!(host_view.write_from(0, &[7, 8]));
    assert_eq!(host_view.read(0), Some(7));
    assert_eq!(host_view.read(1), Some(8));

    let Value::Object(buffer) = vm.get_global("buffer").expect("buffer global") else {
      panic!("ArrayBuffer should be an object");
    };
    buffer.borrow_mut().set_internal_slot(
      crate::ecmascript::semantics::byte_views::ARRAY_BUFFER_DATA_SLOT,
      Value::Null,
    );
    assert!(host_view.is_empty());
    assert_eq!(host_view.read(0), None);
    assert!(!host_view.write(0, 1));

    let fake = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    assert!(vm.uint8_array_byte_view(&fake).is_none());
  }

  #[test]
  fn byte_typed_array_numeric_conversions_are_view_specific() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const u = new Uint8Array([257, -1, NaN]);
        const i = new Int8Array([255, 128, 127]);
        const c = new Uint8ClampedArray([1.5, 2.5, -1, 300]);
        globalThis.result = [u[0], u[1], u[2], i[0], i[1], i[2], c[0], c[1], c[2], c[3]].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "1,255,0,-1,-128,127,2,2,0,255".to_string().into()
      ))
    );
  }

  #[test]
  fn risc_registers_keep_new_closure_environments_alive_during_gc() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const closures = [];
        for (let index = 0; index < 500; index += 1) {
          const captured = index;
          closures.push(() => captured);
        }
        let total = 0;
        for (let index = 0; index < closures.length; index += 1) {
          total += closures[index]();
        }
        globalThis.closure_total = total;
      "#
    ));

    assert_eq!(
      vm.get_global("closure_total"),
      Some(Value::Number(124_750.0))
    );
  }

  #[test]
  fn strict_tail_recursion_reuses_the_active_vm_frame() {
    let (vm, _) = run_script(crate::js!(
      r#"
        "use strict";
        function countDown(remaining, total) {
          if (remaining === 0) return total;
          return countDown(remaining - 1, total + 1);
        }
        globalThis.tail_count = countDown(50_000, 0);
      "#
    ));

    assert_eq!(vm.get_global("tail_count"), Some(Value::Number(50_000.0)));
  }

  #[test]
  fn strict_tail_call_to_native_resumes_the_caller() {
    let (vm, _) = run_script(crate::js!(
      r#"
        "use strict";
        function parse(value) {
          return parseInt(value);
        }
        globalThis.parsed_tail_value = parse("3");
        globalThis.after_native_tail_call = 4;
      "#
    ));

    assert_eq!(vm.get_global("parsed_tail_value"), Some(Value::Number(3.0)));
    assert_eq!(
      vm.get_global("after_native_tail_call"),
      Some(Value::Number(4.0))
    );
  }

  #[test]
  fn ordinary_recursion_uses_vm_managed_register_frames() {
    let (vm, _) = run_script(crate::js!(
      r#"
        function depth(remaining) {
          if (remaining === 0) return 0;
          return 1 + depth(remaining - 1);
        }
        globalThis.recursion_depth = depth(10_000);
      "#
    ));

    assert_eq!(
      vm.get_global("recursion_depth"),
      Some(Value::Number(10_000.0))
    );
  }

  #[test]
  fn recursive_method_calls_use_vm_managed_register_frames() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const counter = {
          depth(remaining) {
            if (remaining === 0) return 0;
            return 1 + this.depth(remaining - 1);
          }
        };
        globalThis.method_recursion_depth = counter.depth(10_000);
      "#
    ));

    assert_eq!(
      vm.get_global("method_recursion_depth"),
      Some(Value::Number(10_000.0))
    );
  }

  #[test]
  fn recursive_exception_unwinding_restores_the_handler_registers() {
    let (vm, _) = run_script(crate::js!(
      r#"
        function descend(remaining) {
          if (remaining === 0) throw 42;
          return 1 + descend(remaining - 1);
        }
        try {
          descend(2_000);
        } catch (error) {
          globalThis.recursive_error = error;
        }
      "#
    ));

    assert_eq!(vm.get_global("recursive_error"), Some(Value::Number(42.0)));
  }

  #[test]
  fn return_cleanup_prevents_unsafe_tail_frame_replacement() {
    let (vm, _) = run_script(crate::js!(
      r#"
        "use strict";
        let finalized = false;
        function value() { return 42; }
        function wrapped() {
          try {
            return value();
          } finally {
            finalized = true;
          }
        }
        globalThis.cleaned_tail_result = wrapped();
        globalThis.cleaned_tail_finalized = finalized;
      "#
    ));

    assert_eq!(
      vm.get_global("cleaned_tail_result"),
      Some(Value::Number(42.0))
    );
    assert_eq!(
      vm.get_global("cleaned_tail_finalized"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn optional_member_access_short_circuits_to_undefined() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let missing;
        let computed = false;
        globalThis.static_result = missing?.value;
        globalThis.computed_result = missing?.[computed = true];
        globalThis.present_result = ({ value: 42 })?.value;
        globalThis.computed_ran = computed;
      "#
    ));

    assert_eq!(vm.get_global("static_result"), Some(Value::Undefined));
    assert_eq!(vm.get_global("computed_result"), Some(Value::Undefined));
    assert_eq!(vm.get_global("present_result"), Some(Value::Number(42.0)));
    assert_eq!(vm.get_global("computed_ran"), Some(Value::Bool(false)));
  }

  #[test]
  fn arrays_retain_all_pushed_objects_across_function_calls() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const clients = [];
        function addClient(name) {
          clients.push({ name });
        }
        function listClients() {
          let names = "";
          for (let index = 0; index < clients.length; index += 1) {
            names += clients[index].name + ",";
          }
          return names;
        }
        addClient("Alice");
        addClient("Bob");
        globalThis.client_count = clients.length;
        globalThis.second_client = clients[1].name;
        globalThis.client_names = listClients();
      "#
    ));

    assert_eq!(vm.get_global("client_count"), Some(Value::Number(2.0)));
    assert_eq!(
      vm.get_global("second_client"),
      Some(Value::String("Bob".to_string().into()))
    );
    assert_eq!(
      vm.get_global("client_names"),
      Some(Value::String("Alice,Bob,".to_string().into()))
    );
  }

  #[test]
  fn loop_body_lexical_bindings_are_recreated_each_iteration() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const clients = [{ name: "Alice" }, { name: "Bob" }];
        const readers = [];
        let names = "";
        for (let index = 0; index < clients.length; index += 1) {
          const client = clients[index];
          names += client.name + ",";
          readers.push(() => client.name);
        }
        globalThis.loop_body_names = names;
        globalThis.loop_body_captures = readers[0]() + "," + readers[1]();
      "#
    ));

    assert_eq!(
      vm.get_global("loop_body_names"),
      Some(Value::String("Alice,Bob,".to_string().into()))
    );
    assert_eq!(
      vm.get_global("loop_body_captures"),
      Some(Value::String("Alice,Bob".to_string().into()))
    );
  }

  #[test]
  fn host_can_invoke_a_long_lived_closure_with_captured_bindings() {
    let (mut vm, _) = run_script(crate::js!(
      r#"
        const prefix = "message:";
        const suffix = "!";
        globalThis.inputHandler = (line) => prefix + line + suffix;
      "#
    ));
    let handler = vm
      .get_global("inputHandler")
      .expect("input handler");

    let result = vm
      .call_value_with_this(
        &handler,
        &Value::Undefined,
        &[Value::String("hello".to_string().into())],
      )
      .expect("invoke retained closure");

    assert_eq!(result, Value::String("message:hello!".to_string().into()));
  }

  #[test]
  fn nested_event_handlers_created_by_host_calls_keep_their_environments() {
    let (mut vm, _) = run_script(crate::js!(
      r#"
        const clients = [];
        function clientNames() {
          let names = "";
          for (let index = 0; index < clients.length; index += 1) {
            names += clients[index].nickname + ",";
          }
          return names;
        }
        function handler(socket) {
          let nickname = null;
          socket.onmessage = (message) => {
            if (nickname === null) {
              nickname = message;
              clients.push({ socket, nickname });
              return clientNames();
            }
            return nickname + ":" + message;
          };
        }
        globalThis.handler = handler;
      "#
    ));
    let handler = vm.get_global("handler").expect("host handler");
    let first_socket = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    let second_socket = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    for socket in [&first_socket, &second_socket] {
      vm.call_value_with_this(&handler, &Value::Undefined, std::slice::from_ref(socket))
        .expect("create event handler");
    }
    let onmessage_key = PropertyKey::String("onmessage".to_string().into());
    let first_callback = vm
      .get_property_host(&first_socket, &onmessage_key)
      .expect("first event handler");
    let second_callback = vm
      .get_property_host(&second_socket, &onmessage_key)
      .expect("second event handler");

    let first_result = vm
      .call_value_with_this(
        &first_callback,
        &first_socket,
        &[Value::String("Alice".to_string().into())],
      )
      .expect("invoke first event handler");
    let second_result = vm
      .call_value_with_this(
        &second_callback,
        &second_socket,
        &[Value::String("Bob".to_string().into())],
      )
      .expect("invoke second event handler");

    assert_eq!(first_result, Value::String("Alice,".to_string().into()));
    assert_eq!(
      second_result,
      Value::String("Alice,Bob,".to_string().into())
    );
    let repeat_result = vm
      .call_value_with_this(
        &first_callback,
        &first_socket,
        &[Value::String("hello".to_string().into())],
      )
      .expect("invoke first event handler again");
    assert_eq!(
      repeat_result,
      Value::String("Alice:hello".to_string().into())
    );
  }

  #[test]
  fn string_trim_removes_ecmascript_whitespace() {
    let (vm, _) = run_script(crate::js!(
      r#"
        globalThis.trimmed = "\uFEFF \t\nhello\r\n \u00A0".trim();
        globalThis.trimmed_start = "  hello  ".trimStart();
        globalThis.trimmed_end = "  hello  ".trimEnd();
      "#
    ));

    assert_eq!(
      vm.get_global("trimmed"),
      Some(Value::String("hello".to_string().into()))
    );
    assert_eq!(
      vm.get_global("trimmed_start"),
      Some(Value::String("hello  ".to_string().into()))
    );
    assert_eq!(
      vm.get_global("trimmed_end"),
      Some(Value::String("  hello".to_string().into()))
    );
  }

  #[test]
  fn strings_preserve_exact_utf16_code_units() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const high = String.fromCharCode(0xD800);
        const highFromCodePoint = String.fromCodePoint(0xD800);
        const pair = String.fromCharCode(0xD83D, 0xDE00);
        const sliced = high.slice(0, 1);
        const keyed = {};
        keyed[high] = 7;
        const split = pair.split("");
        const expression = new RegExp(high);
        const matched = expression.exec(high);
        let enumeratedCodeUnit = 0;
        for (const key in keyed) enumeratedCodeUnit = key.charCodeAt(0);
        let uriRejected = false;
        try {
          encodeURIComponent(high);
        } catch (error) {
          uriRejected = true;
        }

        globalThis.result = [
          high.length,
          high.charCodeAt(0),
          highFromCodePoint === high,
          high[0] === high,
          sliced === high,
          sliced.length,
          sliced.charCodeAt(0),
          high.toUpperCase() === high,
          (high + "x").charCodeAt(0),
          ("x" + high).charCodeAt(1),
          pair.length,
          split.length,
          split[0].charCodeAt(0),
          split[1].charCodeAt(0),
          Object.keys(keyed)[0].charCodeAt(0),
          enumeratedCodeUnit,
          keyed[high],
          matched[0].charCodeAt(0),
          uriRejected,
          encodeURIComponent(";") === "%3B",
          decodeURIComponent(high) === high,
          decodeURIComponent("%F0%9F%98%80").length,
          JSON.stringify(high)
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "1,55296,true,true,true,1,55296,true,55296,55296,2,2,55357,56832,55296,55296,7,55296,true,true,true,2,\"\\ud800\""
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn regexp_uses_utf16_indices_captures_and_last_index() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const captured = /(a)(b+)/.exec("😀abbb");
        const unicode = /./gu;
        const first = unicode.exec("😀x");
        const firstLastIndex = unicode.lastIndex;
        const second = unicode.exec("😀x");
        const secondLastIndex = unicode.lastIndex;
        const legacy = /./g;
        legacy.exec("😀");

        globalThis.result = [
          captured.index, captured[0], captured[1], captured[2],
          first[0], firstLastIndex, second[0], secondLastIndex,
          legacy.lastIndex
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("2,abbb,a,bbb,😀,2,x,3,1".to_string().into()))
    );
  }

  #[test]
  fn regexp_supports_ecmascript_backreferences_case_folding_and_replacement_offsets() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const backreference = /(a|b)\1/.test("zaa");
        const folded = /[a-z]/iu.test("\u212A");
        const sticky = /a/y;
        sticky.lastIndex = 2;
        const stickyMatch = sticky.exec("😀a");
        const replaced = "😀a".replace(/a/, (_, offset) => offset);
        const emptyGlobal = "ab".replace(/(?:)/g, "-");
        const namedReplacement =
          "ab".replace(/(?<first>a)(?<missing>z)?/, "$<missing>-$<first>-$<unknown>");
        const namedCallback =
          "ab".replace(/(?<letter>a)/, (...args) => args[args.length - 1].letter);
        const literalNamedReplacement = "a".replace(/a/, "$<missing>");
        const overridden = /a/g;
        let overriddenCalls = 0;
        overridden.exec = function() {
          overriddenCalls++;
          return overriddenCalls === 1
            ? { 0: "b", index: 1, length: 1, groups: undefined }
            : null;
        };
        const overriddenReplacement = "aaa".replace(overridden, "x");

        globalThis.result = [
          backreference, folded, stickyMatch.index, sticky.lastIndex,
          replaced, emptyGlobal, namedReplacement, namedCallback,
          literalNamedReplacement, overriddenReplacement, overriddenCalls
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "true,true,2,3,😀2,-a-b-,-a-b,ab,$<missing>,axa,2"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn regexp_match_search_and_constructor_dispatch_follow_es6_protocols() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const globalMatches = "aba".match(/a/g);
        const captured = "😀ab".match(/(a)/);
        const searchExpression = /a/g;
        searchExpression.lastIndex = 2;
        const searchIndex = "ba".search(searchExpression);
        const original = /x/gi;
        const copied = new RegExp(original);
        const same = RegExp(original) === original;
        const custom = {
          [Symbol.match](value) { return "matched:" + value; },
          [Symbol.search](value) { return value.length; }
        };
        let startsWithRejected = false;
        try { "x".startsWith(original); } catch (error) {
          startsWithRejected = error instanceof TypeError;
        }
        original[Symbol.match] = false;

        globalThis.result = [
          globalMatches.join(":"),
          captured.index,
          captured[1],
          searchIndex,
          searchExpression.lastIndex,
          same,
          copied !== original,
          copied.source,
          copied.flags,
          "abc".match(custom),
          "abc".search(custom),
          startsWithRejected,
          "x".startsWith(original) === false
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "a:a,2,a,1,2,true,true,x,gi,matched:abc,3,true,true"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn regexp_constructor_rejects_invalid_ecmascript_patterns() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let rejected = false;
        try {
          new RegExp("[", "u");
        } catch (error) {
          rejected = true;
        }
        globalThis.result = rejected;
      "#
    ));

    assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
  }

  #[test]
  fn regexp_exposes_named_groups_indices_and_canonical_flags() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const expression = new RegExp("(?<letter>a)(b)?", "ymigd");
        const sourceDescriptor =
          Object.getOwnPropertyDescriptor(RegExp.prototype, "source");
        expression.lastIndex = 2;
        const match = expression.exec("😀a");
        globalThis.result = [
          expression.flags,
          expression.global,
          expression.ignoreCase,
          expression.multiline,
          expression.sticky,
          expression.unicode,
          expression.hasIndices,
          Object.hasOwn(expression, "source"),
          typeof sourceDescriptor.get === "function" &&
            sourceDescriptor.enumerable === false &&
            sourceDescriptor.configurable === true,
          match.groups.letter,
          match[2] === undefined,
          match.indices[0][0], match.indices[0][1],
          match.indices.groups.letter[0], match.indices.groups.letter[1]
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "dgimy,true,true,true,true,false,true,false,true,a,true,2,3,2,3"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn string_methods_support_chat_server_operations() {
    let (vm, _) = run_script(crate::js!(
      r#"
        globalThis.lowercase = "WebSocket İ".toLowerCase();
        globalThis.uppercase = "straße".toUpperCase();
        globalThis.generic = String.prototype.toLowerCase.call(123);
        globalThis.starts_with = "JOIN Alice".startsWith("JOIN ");
        globalThis.replaced = "a\nb\tc".replace(/[\u0000-\u001f]/g, "");
        globalThis.replacement_patterns = "abc".replace("b", "[$&][$`][$']");
        const customSplitter = {
          [Symbol.split](value, limit) { return [value, limit]; }
        };
        const customSplit = "abc".split(customSplitter, -1);
        const regexSplit = "😀a,b".split(/(,)/);
        const unicodeEmptySplit = "😀x".split(/(?:)/u);
        const undefinedLimitSplit = "a,b".split(",", undefined);
        const negativeLimitSplit = "a,b".split(",", -1);
        globalThis.string_boundaries = [
          "abc".slice(-Infinity) === "abc",
          "abc".slice(Infinity) === "",
          Number.isNaN("abc".charCodeAt(-1)),
          Number.isNaN("abc".charCodeAt(Infinity)),
          "abc".startsWith("a", -Infinity),
          undefinedLimitSplit.length === 2,
          negativeLimitSplit.length === 2,
          customSplit[0] === "abc",
          customSplit[1] === -1,
          regexSplit.length === 3 && regexSplit[0] === "😀a" &&
            regexSplit[1] === "," && regexSplit[2] === "b",
          unicodeEmptySplit.length === 2 &&
            unicodeEmptySplit[0] === "😀" && unicodeEmptySplit[1] === "x"
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("lowercase"),
      Some(Value::String("websocket i\u{307}".to_string().into()))
    );
    assert_eq!(
      vm.get_global("uppercase"),
      Some(Value::String("STRASSE".to_string().into()))
    );
    assert_eq!(
      vm.get_global("generic"),
      Some(Value::String("123".to_string().into()))
    );
    assert_eq!(vm.get_global("starts_with"), Some(Value::Bool(true)));
    assert_eq!(
      vm.get_global("replaced"),
      Some(Value::String("abc".to_string().into()))
    );
    assert_eq!(
      vm.get_global("replacement_patterns"),
      Some(Value::String("a[b][a][c]c".to_string().into()))
    );
    assert_eq!(
      vm.get_global("string_boundaries"),
      Some(Value::String(
        "true,true,true,true,true,true,true,true,true,true,true"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn const_bindings_initialize_once_and_reject_reassignment() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const root = 1;
        let root_threw = false;
        try { root = 2; } catch (error) { root_threw = error instanceof TypeError; }

        function localConst() {
          const local = 3;
          try { local = 4; } catch (error) { return error instanceof TypeError; }
          return false;
        }

        const { value: destructured } = { value: 5 };
        let destructured_threw = false;
        try { destructured = 6; } catch (error) {
          destructured_threw = error instanceof TypeError;
        }

        globalThis.const_result =
          root === 1 && root_threw && localConst() &&
          destructured === 5 && destructured_threw;
      "#
    ));

    assert!(matches!(
      vm.get_global("const_result"),
      Some(Value::Bool(true))
    ));
  }

  #[test]
  fn for_const_uses_a_fresh_immutable_cell_per_iteration() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let closures = [];
        for (const value of [1, 2, 3]) closures.push(() => value);
        let reassignment_threw = false;
        try {
          for (const value of [1]) value = 2;
        } catch (error) {
          reassignment_threw = error instanceof TypeError;
        }
        globalThis.for_const_result =
          closures[0]() === 1 && closures[1]() === 2 &&
          closures[2]() === 3 && reassignment_threw;
      "#
    ));

    assert!(matches!(
      vm.get_global("for_const_result"),
      Some(Value::Bool(true))
    ));
  }

  #[test]
  fn for_of_closes_iterators_for_abrupt_completions() {
    let (vm, _) = run_script(crate::js!(
      r#"
        function iterableWithReturn(returnValue) {
          var iterable = {};
          iterable[Symbol.iterator] = function() {
            return {
              next: function() { return { value: 1, done: false }; },
              return: returnValue
            };
          };
          return iterable;
        }

        var nullReturnCompleted = false;
        for (var value of iterableWithReturn(null)) {
          nullReturnCompleted = true;
          break;
        }

        var originalError = {};
        var observedError;
        var throwingReturn = {};
        throwingReturn[Symbol.iterator] = function() {
          var iterator = {
            next: function() { return { value: 1, done: false }; }
          };
          Object.defineProperty(iterator, "return", {
            get: function() { throw {}; }
          });
          return iterator;
        };
        try {
          for (var value of throwingReturn) throw originalError;
        } catch (error) {
          observedError = error;
        }

        var returnClosed = false;
        var finallyRanBeforeClose = false;
        function returnFromLoop() {
          var iterable = iterableWithReturn(function() {
            finallyRanBeforeClose = finallyRanBeforeClose === "finalized";
            returnClosed = true;
            return {};
          });
          for (var value of iterable) {
            try {
              return 42;
            } finally {
              finallyRanBeforeClose = "finalized";
            }
          }
        }

        var continueClosed = false;
        var continueIterable = iterableWithReturn(function() {
          continueClosed = true;
          return {};
        });
        outer: do {
          for (var value of continueIterable) continue outer;
        } while (false);

        globalThis.for_of_close_ok =
          nullReturnCompleted
          && observedError === originalError
          && returnFromLoop() === 42
          && returnClosed
          && finallyRanBeforeClose === true
          && continueClosed;
      "#
    ));

    assert_eq!(vm.get_global("for_of_close_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn labeled_loop_breaks_complete_through_risc_cleanup() {
    for source in [
      crate::js!(
        r#"
        var labeled_loop_result = 0;
        outer: while (true) {
          try {} finally { break outer; }
        }
        labeled_loop_result = 1;
      "#
      ),
      crate::js!(
        r#"
        var labeled_loop_result = 0;
        outer: do {
          try {} finally { break outer; }
        } while (true);
        labeled_loop_result = 1;
      "#
      ),
    ] {
      let (vm, _) = run_script(source);
      assert_eq!(
        vm.get_global("labeled_loop_result"),
        Some(Value::Number(1.0))
      );
    }
  }

  #[test]
  fn array_destructuring_in_for_of_preserves_the_bound_value() {
    for source in [
      crate::js!(
        r#"
        var x;
        for ([x] of [[1]]) { break; }
        var array_destructuring_result = x;
      "#
      ),
      crate::js!(
        r#"
        for (var [x] of [[1]]) { break; }
        var array_destructuring_result = x;
      "#
      ),
    ] {
      let (vm, _) = run_script(source);
      assert_eq!(
        vm.get_global("array_destructuring_result"),
        Some(Value::Number(1.0))
      );
    }
  }

  #[test]
  fn risc_core_execution_handles_control_flow() {
    let source = crate::js!(
      r#"
      var x = 1;
      while (x < 5) x = x + 1;
      var result = x * 2;
    "#
    );
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");

    let risc_globals = test_globals();
    let mut risc_vm = Vm::new(Rc::clone(&risc_globals));
    risc_vm.run(chunk).expect("risc run");

    assert!(matches!(
      risc_vm.get_global("result"),
      Some(Value::Number(10.0_f64))
    ));
  }

  #[test]
  fn exponentiation_and_unary_operators_match_ecmascript() {
    let (vm, _) = run_script(crate::js!(
      r#"
        globalThis.exp_positive_infinity = 1 ** Infinity;
        globalThis.exp_negative_infinity = (-1) ** -Infinity;
        globalThis.exp_negative = 2 ** -2;
        globalThis.bitwise_not = ~(3 ** 2);
        globalThis.unary_plus = +("2");
      "#
    ));

    assert!(matches!(
      vm.get_global("exp_positive_infinity"),
      Some(Value::Number(value)) if value.is_nan()
    ));
    assert!(matches!(
      vm.get_global("exp_negative_infinity"),
      Some(Value::Number(value)) if value.is_nan()
    ));
    assert_eq!(vm.get_global("exp_negative"), Some(Value::Number(0.25_f64)));
    assert_eq!(vm.get_global("bitwise_not"), Some(Value::Number(-10.0_f64)));
    assert_eq!(vm.get_global("unary_plus"), Some(Value::Number(2.0_f64)));
  }

  #[test]
  fn risc_functions_literals_and_overflow_arguments_are_stable() {
    let source = crate::js!(
      r#"
      function sum(a, b, c, d, e, f, g, h) {
        return a + b + c + d + e + f + g + h;
      }
      var array = [1, 2, 3];
      var object = { value: 4, other: 5 };
      var result = sum(array[0], array[1], array[2], object.value, object.other, 6, 7, 8);
    "#
    );
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");

    let risc_globals = test_globals();
    let mut risc_vm = Vm::new(Rc::clone(&risc_globals));
    risc_vm.run(chunk).expect("risc run");

    assert!(matches!(
      risc_vm.get_global("result"),
      Some(Value::Number(36.0_f64))
    ));
  }

  #[test]
  fn returning_to_strict_root_preserves_strict_mode() {
    let (vm, _) = run_script(crate::js!(
      r#"
        "use strict";
        function helper() {}
        helper();
        let threw = false;
        try { leaked_after_call = 1; } catch (error) { threw = error instanceof ReferenceError; }
        globalThis.strict_after_call = threw;
      "#
    ));

    assert_eq!(vm.get_global("strict_after_call"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("leaked_after_call"), None);
  }

  #[test]
  fn ordinary_nested_call_does_not_inherit_new_target() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let nested_target;
        function helper() { nested_target = new.target; }
        function Constructor() { helper(); }
        new Constructor();
        globalThis.nested_new_target_is_undefined = nested_target === undefined;
      "#
    ));

    assert_eq!(
      vm.get_global("nested_new_target_is_undefined"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn throwing_an_object_does_not_add_own_properties() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let thrown = {};
        try { throw thrown; } catch (error) {}
        globalThis.throw_preserved_object =
          Object.getOwnPropertyDescriptor(thrown, "constructor") === undefined;
      "#
    ));

    assert_eq!(
      vm.get_global("throw_preserved_object"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn relational_comparison_applies_to_primitive() {
    let source = crate::js!(
      r#"
      var left = { valueOf: function () { return 1; } };
      var relational_result = left < 2;
    "#
    );
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run");
    assert_eq!(vm.get_global("relational_result"), Some(Value::Bool(true)));
  }

  #[test]
  fn risc_ordinary_builtin_call_does_not_prepend_receiver() {
    let source = crate::js!("var builtin_argument = capture(\"expected\");");
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    let globals = test_globals();
    globals.borrow_mut().properties.insert(
      PropertyKey::String("capture".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(|_vm, args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
      }))),
    );
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run");

    assert_eq!(
      vm.get_global("builtin_argument"),
      Some(Value::String("expected".to_string().into()))
    );
  }

  #[test]
  fn property_get_ic_invalidates_after_tracked_write() {
    let (vm, _) = run_script(crate::js!(
      r#"
        var cached_object = { value: 1 };
        var before_write = cached_object.value;
        cached_object.value = 2;
        globalThis.after_write = cached_object.value;
      "#
    ));

    assert_eq!(vm.get_global("before_write"), Some(Value::Number(1.0)));
    assert_eq!(vm.get_global("after_write"), Some(Value::Number(2.0)));
    assert!(!vm.caches.property_get.is_empty());
  }

  #[test]
  fn host_roots_reuse_slots_without_reviving_stale_handles() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let first = vm.create_host_root(Value::Number(1.0));

    assert_eq!(vm.host_root(first), Some(Value::Number(1.0)));
    vm.release_host_root(first)
      .expect("release first root");
    assert_eq!(vm.host_root(first), None);

    let second = vm.create_host_root(Value::Number(2.0));
    assert_eq!(first.slot, second.slot);
    assert_ne!(first.generation, second.generation);
    assert!(
      vm.set_host_root(first, Value::Number(3.0))
        .is_err()
    );
    assert!(vm.release_host_root(first).is_err());
    assert_eq!(vm.host_root(second), Some(Value::Number(2.0)));
  }

  #[test]
  fn host_roots_are_shared_by_forks_and_rejected_by_other_vms() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let parent_root = vm.create_host_root(Value::Number(1.0));
    let mut child = vm.fork_execution();

    child
      .set_host_root(parent_root, Value::Number(2.0))
      .expect("fork shares root store");
    assert_eq!(vm.host_root(parent_root), Some(Value::Number(2.0)));

    let child_root = child.create_host_root(Value::Number(3.0));
    assert_eq!(vm.host_root(child_root), Some(Value::Number(3.0)));
    child
      .release_host_root(child_root)
      .expect("release child root");
    assert_eq!(vm.host_root(child_root), None);

    let mut unrelated = Vm::new(test_globals());
    assert!(
      unrelated
        .set_host_root(parent_root, Value::Undefined)
        .is_err()
    );
  }

  #[test]
  fn prototype_gc_defers_while_multiple_vms_are_alive() {
    let vm = Vm::new(test_globals());
    let mut sibling = Vm::new(test_globals());
    let protected = crate::gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    protected.borrow_mut().properties.insert(
      PropertyKey::String("answer".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(42.0)),
    );
    sibling.create_host_root(Value::Object(Rc::clone(&protected)));

    let stats = vm.collect_garbage_prototype();
    assert_eq!(stats.swept_objects, 0);
    assert!(
      protected
        .borrow()
        .properties
        .contains_key(&PropertyKey::String("answer".to_string().into()))
    );
  }

  #[test]
  fn prototype_gc_preserves_suspended_generator_vm_roots() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let first = crate::js!(
      r#"
        function* values() {
          const held = { answer: 42 };
          yield 0;
          return held.answer;
        }
        globalThis.iterator = values();
        iterator.next();
      "#
    );
    let session = Parser::parse_source(first).expect("parse generator setup");
    let chunk = compiler::compile_oxc(session.program(), session.source()).expect("compile setup");
    vm.run(chunk).expect("start generator");

    assert_eq!(vm.collect_garbage_prototype().swept_objects, 0);

    let second = crate::js!("globalThis.generatorAnswer = iterator.next().value;");
    let session = Parser::parse_source(second).expect("parse generator resume");
    let chunk = compiler::compile_oxc(session.program(), session.source()).expect("compile resume");
    vm.run(chunk).expect("resume generator");
    assert_eq!(vm.get_global("generatorAnswer"), Some(Value::Number(42.0)));
  }

  #[test]
  fn nested_run_carries_thrown_value_and_restores_outer_exception() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.exceptions.last_uncaught = Some(Value::String("outer".to_string().into()));

    let source = crate::js!("throw 42;");
    let session = Parser::parse_source(source).expect("parse nested throw");
    let chunk =
      compiler::compile_oxc_repl(session.program(), session.source()).expect("compile nested");
    let failure = vm
      .run_nested(chunk, true)
      .expect_err("nested run throws");

    assert_eq!(failure.thrown, Some(Value::Number(42.0)));
    assert_eq!(
      vm.exceptions.last_uncaught,
      Some(Value::String("outer".to_string().into()))
    );
  }

  #[test]
  fn direct_eval_throw_is_caught_with_original_identity() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const token = {};
        let same = false;
        try {
          eval("throw token");
        } catch (error) {
          same = error === token;
        }
        globalThis.directEvalThrowSame = same;
      "#
    ));

    assert_eq!(
      vm.get_global("directEvalThrowSame"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn root_run_and_reset_clear_active_transient_state() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let poison = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    let poison_state = |vm: &mut Vm| {
      vm.construction.new_target = Some(poison.clone());
      vm.construction.recent_return_value = Some(poison.clone());
      vm.construction
        .derived_this
        .push(Some(poison.clone()));
      vm.exceptions.last_uncaught = Some(poison.clone());
      vm.execution.active.arguments.object = Some(poison.clone());
      vm.execution.active.arguments.mapped_count = 1;
    };
    let assert_cleared = |vm: &Vm| {
      assert!(vm.construction.new_target.is_none());
      assert!(vm.construction.recent_return_value.is_none());
      assert!(vm.construction.derived_this.is_empty());
      assert!(vm.exceptions.last_uncaught.is_none());
      assert!(vm.execution.active.arguments.object.is_none());
      assert_eq!(vm.execution.active.arguments.mapped_count, 0);
    };

    poison_state(&mut vm);
    let source = crate::js!("undefined;");
    let session = Parser::parse_source(source).expect("parse root");
    let chunk = compiler::compile_oxc(session.program(), session.source()).expect("compile root");
    vm.run(chunk).expect("run root");
    assert_cleared(&vm);

    poison_state(&mut vm);
    vm.reset(&HashSet::new());
    assert_cleared(&vm);
  }

  #[test]
  fn object_get_own_property_descriptor_reports_data_flags() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var obj = {};
        Object.defineProperty(obj, "prop", {
          value: "prop",
          enumerable: false,
          writable: false,
          configurable: false,
        });
        var desc = Object.getOwnPropertyDescriptor(obj, "prop");
        globalThis.desc_value = desc.value;
        globalThis.desc_enumerable = desc.enumerable;
        globalThis.desc_writable = desc.writable;
        globalThis.desc_configurable = desc.configurable;
        "#
    ));

    assert_eq!(
      vm.get_global("desc_value"),
      Some(Value::String("prop".to_string().into()))
    );
    assert_eq!(vm.get_global("desc_enumerable"), Some(Value::Bool(false)));
    assert_eq!(vm.get_global("desc_writable"), Some(Value::Bool(false)));
    assert_eq!(vm.get_global("desc_configurable"), Some(Value::Bool(false)));
  }

  #[test]
  fn standard_builtin_methods_use_non_enumerable_data_descriptors() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const arrayMethod = Object.getOwnPropertyDescriptor(Array.prototype, "forEach");
        const promiseMethod = Object.getOwnPropertyDescriptor(Promise.prototype, "then");
        const ordinary = Object.getOwnPropertyDescriptor({ value: 1 }, "value");
        const arrayKeys = Object.keys(Array.prototype);
        const promiseKeys = Object.keys(Promise.prototype);
        const intrinsicTablesAreHidden = [
          Array.prototype,
          Promise.prototype,
          Object.prototype,
          String.prototype,
          Map.prototype,
          Set.prototype,
          Reflect,
          Math,
          JSON
        ].every(object => Object.keys(object).length === 0);

        Array.prototype.forEach = 42;
        const reassigned = Array.prototype.forEach === 42;
        const deleted = delete Array.prototype.forEach;

        globalThis.result = [
          arrayMethod.writable,
          arrayMethod.enumerable,
          arrayMethod.configurable,
          promiseMethod.writable,
          promiseMethod.enumerable,
          promiseMethod.configurable,
          ordinary.writable,
          ordinary.enumerable,
          ordinary.configurable,
          arrayKeys.includes("forEach"),
          promiseKeys.includes("then"),
          intrinsicTablesAreHidden,
          reassigned,
          deleted
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "true,false,true,true,false,true,true,true,true,false,false,true,true,true"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn standard_builtin_metadata_and_constants_use_spec_attributes() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const prototype = Object.getOwnPropertyDescriptor(Array, "prototype");
        const name = Object.getOwnPropertyDescriptor(Array, "name");
        const length = Object.getOwnPropertyDescriptor(Array, "length");
        const iterator = Object.getOwnPropertyDescriptor(Symbol, "iterator");
        const nan = Object.getOwnPropertyDescriptor(globalThis, "NaN");

        globalThis.result = [
          prototype.writable, prototype.enumerable, prototype.configurable,
          name.writable, name.enumerable, name.configurable,
          length.writable, length.enumerable, length.configurable,
          iterator.writable, iterator.enumerable, iterator.configurable,
          nan.writable, nan.enumerable, nan.configurable
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "false,false,false,false,false,true,false,false,true,false,false,false,false,false,false"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn object_get_own_property_descriptor_works_when_extracted() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var obj = { a: 1, b: true };
        var getOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
        var desc = getOwnPropertyDescriptor(obj, "a");
        globalThis.desc_value = desc && desc.value;
        globalThis.desc_enumerable = desc && desc.enumerable;
        "#
    ));

    assert_eq!(vm.get_global("desc_value"), Some(Value::Number(1.0)));
    assert_eq!(vm.get_global("desc_enumerable"), Some(Value::Bool(true)));
  }

  #[test]
  fn array_copy_within_handles_negative_target() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var arr = [0, 1, 2, 3];
        arr.copyWithin(-1, 0);
        globalThis.result = arr;
        "#
    ));

    match vm.get_global("result") {
      Some(Value::Array(arr, _)) => {
        let arr = arr.borrow();
        assert_eq!(arr.len(), 4);
        assert!(matches!(arr[0], Some(Value::Number(n)) if (n - 0.0).abs() < f64::EPSILON));
        assert!(matches!(arr[1], Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON));
        assert!(matches!(arr[2], Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON));
        assert!(matches!(arr[3], Some(Value::Number(n)) if (n - 0.0).abs() < f64::EPSILON));
      }
      other => panic!("unexpected result: {:?}", other),
    }
  }

  #[test]
  fn constructor_species_property_round_trips() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var array = [];
        array.constructor = function() {};
        var Ctor = function() {};
        array.constructor[Symbol.species] = Ctor;
        globalThis.species_value = array.constructor[Symbol.species];
        "#
    ));

    assert!(matches!(
      vm.get_global("species_value"),
      Some(Value::Closure { .. })
    ));
  }

  #[test]
  fn proxy_wrapping_array_is_array() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var proxy = new Proxy([], {});
        globalThis.is_array = Array.isArray(proxy);
        "#
    ));

    assert_eq!(vm.get_global("is_array"), Some(Value::Bool(true)));
  }

  #[test]
  fn reflect_own_keys_matches_get_own_property_descriptor() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var obj = { a: 1, b: true };
        var keys = Reflect.ownKeys(obj);
        globalThis.k0 = keys[0];
        globalThis.k1 = keys[1];
        globalThis.d0 = Object.getOwnPropertyDescriptor(obj, keys[0]) !== undefined;
        globalThis.d1 = Object.getOwnPropertyDescriptor(obj, keys[1]) !== undefined;
        "#
    ));

    assert_eq!(
      vm.get_global("k0"),
      Some(Value::String("a".to_string().into()))
    );
    assert_eq!(
      vm.get_global("k1"),
      Some(Value::String("b".to_string().into()))
    );
    assert_eq!(vm.get_global("d0"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("d1"), Some(Value::Bool(true)));
  }

  #[test]
  fn reflect_own_keys_uses_ecmascript_key_order() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var first = Symbol("first");
        var second = Symbol("second");
        var obj = {};
        obj.b = 1;
        obj[2] = 2;
        obj["4294967295"] = 3;
        obj.a = 4;
        obj[1] = 5;
        obj[first] = 6;
        obj.c = 7;
        obj[second] = 8;
        var keys = Reflect.ownKeys(obj);
        globalThis.key_order = keys.slice(0, 6).join(",");
        globalThis.symbol_order = keys[6] === first && keys[7] === second;
      "#
    ));

    assert_eq!(
      vm.get_global("key_order"),
      Some(Value::String("1,2,b,4294967295,a,c".to_string().into()))
    );
    assert_eq!(vm.get_global("symbol_order"), Some(Value::Bool(true)));
  }

  #[test]
  fn reflect_own_keys_excludes_array_holes() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var array = [];
        array.length = 4;
        array[2] = "present";
        array.extra = true;
        globalThis.keys = Reflect.ownKeys(array).join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("keys"),
      Some(Value::String("2,length,extra".to_string().into()))
    );
  }

  #[test]
  fn proxy_own_keys_trap_controls_order_and_preserves_symbols() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var symbol = Symbol("key");
        var target = { a: 1, b: 2 };
        target[symbol] = 3;
        var proxy = new Proxy(target, {
          ownKeys: function() { return [symbol, "b", "a", "extra"]; }
        });
        var keys = Reflect.ownKeys(proxy);
        globalThis.order_ok = keys[0] === symbol
          && keys[1] === "b"
          && keys[2] === "a"
          && keys[3] === "extra";
      "#
    ));

    assert_eq!(vm.get_global("order_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn proxy_own_keys_rejects_invalid_trap_results() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var duplicate_rejected = false;
        try {
          Reflect.ownKeys(new Proxy({}, {
            ownKeys: function() { return ["a", "a"]; }
          }));
        } catch (error) {
          duplicate_rejected = error instanceof TypeError;
        }

        var fixed = {};
        Object.defineProperty(fixed, "fixed", { configurable: false });
        var fixed_rejected = false;
        try {
          Reflect.ownKeys(new Proxy(fixed, {
            ownKeys: function() { return []; }
          }));
        } catch (error) {
          fixed_rejected = error instanceof TypeError;
        }

        var sealed = { present: true };
        Object.preventExtensions(sealed);
        var sealed_rejected = false;
        try {
          Reflect.ownKeys(new Proxy(sealed, {
            ownKeys: function() { return ["present", "extra"]; }
          }));
        } catch (error) {
          sealed_rejected = error instanceof TypeError;
        }

        var valid = {};
        Object.defineProperty(valid, "fixed", { configurable: false });
        Object.preventExtensions(valid);
        var valid_keys = Reflect.ownKeys(new Proxy(valid, {
          ownKeys: function() { return ["fixed"]; }
        }));
        var valid_result_accepted = valid_keys.length === 1 && valid_keys[0] === "fixed";

        globalThis.invalid_results_rejected = duplicate_rejected
          && fixed_rejected
          && sealed_rejected
          && valid_result_accepted;
      "#
    ));

    assert_eq!(
      vm.get_global("invalid_results_rejected"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_get_own_property_descriptor_drives_enumeration() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var target = {};
        var proxy = new Proxy(target, {
          ownKeys: function() { return ["virtual"]; },
          getOwnPropertyDescriptor: function(target, key) {
            return {
              value: 42,
              writable: true,
              enumerable: true,
              configurable: true
            };
          }
        });
        var desc = Object.getOwnPropertyDescriptor(proxy, "virtual");
        globalThis.proxy_descriptor_ok = desc.value === 42
          && desc.enumerable === true
          && Object.keys(proxy).join(",") === "virtual";
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_descriptor_ok"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_get_own_property_descriptor_enforces_invariants() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var fixed = {};
        Object.defineProperty(fixed, "value", { configurable: false });
        var hidden_fixed_rejected = false;
        try {
          Object.getOwnPropertyDescriptor(new Proxy(fixed, {
            getOwnPropertyDescriptor: function() { return undefined; }
          }), "value");
        } catch (error) {
          hidden_fixed_rejected = error instanceof TypeError;
        }

        var sealed = { value: 1 };
        Object.preventExtensions(sealed);
        var hidden_sealed_rejected = false;
        try {
          Object.getOwnPropertyDescriptor(new Proxy(sealed, {
            getOwnPropertyDescriptor: function() { return undefined; }
          }), "value");
        } catch (error) {
          hidden_sealed_rejected = error instanceof TypeError;
        }

        var new_fixed_rejected = false;
        try {
          Object.getOwnPropertyDescriptor(new Proxy({}, {
            getOwnPropertyDescriptor: function() {
              return { configurable: false };
            }
          }), "value");
        } catch (error) {
          new_fixed_rejected = error instanceof TypeError;
        }

        globalThis.proxy_descriptor_invariants = hidden_fixed_rejected
          && hidden_sealed_rejected
          && new_fixed_rejected;
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_descriptor_invariants"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_get_and_set_traps_receive_the_original_receiver() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var get_receiver;
        var set_receiver;
        var set_value;
        var target = {};
        var proxy = new Proxy(target, {
          get: function(actual_target, key, receiver) {
            get_receiver = receiver;
            return key === "value" && actual_target === target ? 42 : undefined;
          },
          set: function(actual_target, key, value, receiver) {
            set_receiver = receiver;
            set_value = value;
            return actual_target === target && key === "value";
          }
        });

        var read = proxy.value;
        proxy.value = 7;
        var custom_receiver = {};
        var reflected = Reflect.set(proxy, "value", 9, custom_receiver);
        globalThis.proxy_get_set_receiver_ok = read === 42
          && get_receiver === proxy
          && set_receiver === custom_receiver
          && set_value === 9
          && reflected
          && target.value === undefined;
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_get_set_receiver_ok"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_get_and_set_traps_enforce_target_invariants() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var fixed = {};
        Object.defineProperty(fixed, "value", {
          value: 1,
          writable: false,
          configurable: false
        });

        var bad_get_rejected = false;
        var bad_set_rejected = false;
        try {
          new Proxy(fixed, { get: function() { return 2; } }).value;
        } catch (error) {
          bad_get_rejected = error instanceof TypeError;
        }
        try {
          Reflect.set(new Proxy(fixed, { set: function() { return true; } }), "value", 2);
        } catch (error) {
          bad_set_rejected = error instanceof TypeError;
        }

        var accessor = {};
        Object.defineProperty(accessor, "value", {
          get: undefined,
          set: undefined,
          configurable: false
        });
        var missing_getter_rejected = false;
        var missing_setter_rejected = false;
        try {
          new Proxy(accessor, { get: function() { return 1; } }).value;
        } catch (error) {
          missing_getter_rejected = error instanceof TypeError;
        }
        try {
          Reflect.set(new Proxy(accessor, { set: function() { return true; } }), "value", 1);
        } catch (error) {
          missing_setter_rejected = error instanceof TypeError;
        }

        var allowed_get = new Proxy(fixed, { get: function() { return 1; } }).value === 1;
        var allowed_set = Reflect.set(
          new Proxy(fixed, { set: function() { return true; } }),
          "value",
          1
        );
        globalThis.proxy_get_set_invariants = bad_get_rejected
          && bad_set_rejected
          && missing_getter_rejected
          && missing_setter_rejected
          && allowed_get
          && allowed_set;
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_get_set_invariants"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_set_false_distinguishes_reflect_and_strict_assignment() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var proxy = new Proxy({}, { set: function() { return false; } });
        var reflected = Reflect.set(proxy, "value", 1);
        var strict_rejected = false;
        try {
          (function() { "use strict"; proxy.value = 1; })();
        } catch (error) {
          strict_rejected = error instanceof TypeError;
        }
        globalThis.proxy_set_false_ok = reflected === false && strict_rejected;
      "#
    ));

    assert_eq!(vm.get_global("proxy_set_false_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn proxy_get_and_set_fallbacks_preserve_receiver_semantics() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var accessor_receiver;
        var target = {};
        Object.defineProperty(target, "accessor", {
          get: function() { return this; },
          set: function() { accessor_receiver = this; },
          configurable: true
        });
        var proxy = new Proxy(target, {});

        proxy.value = 3;
        var custom_receiver = {};
        var reflected = Reflect.set(proxy, "other", 4, custom_receiver);
        var getter_receiver_ok = proxy.accessor === proxy;
        proxy.accessor = 5;

        var empty_accessor = {};
        Object.defineProperty(empty_accessor, "value", {
          get: undefined,
          set: undefined,
          configurable: false
        });
        var empty_proxy = new Proxy(empty_accessor, {});
        var empty_accessor_ok = empty_proxy.value === undefined
          && Reflect.set(empty_proxy, "value", 1) === false;

        globalThis.proxy_get_set_fallback_ok = target.value === 3
          && target.other === undefined
          && custom_receiver.other === 4
          && reflected
          && getter_receiver_ok
          && accessor_receiver === proxy
          && empty_accessor_ok;
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_get_set_fallback_ok"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_apply_trap_and_fallback_preserve_call_semantics() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        function target(a, b) { return this.base + a + b; }
        var receiver = { base: 10 };
        var observed = false;
        var proxy = new Proxy(target, {
          apply: function(actual_target, this_arg, arguments_list) {
            observed = actual_target === target
              && this_arg === receiver
              && Array.isArray(arguments_list)
              && arguments_list.length === 2;
            return this_arg.base + arguments_list[0] * arguments_list[1];
          }
        });
        var trapped = proxy.call(receiver, 2, 3);

        var fallback = new Proxy(target, {});
        var forwarded = fallback.call(receiver, 4, 5);

        var non_callable_trap_rejected = false;
        try {
          new Proxy(target, { apply: 1 })();
        } catch (error) {
          non_callable_trap_rejected = error instanceof TypeError;
        }

        var non_callable_target_rejected = false;
        try {
          new Proxy({}, { apply: function() { return 1; } })();
        } catch (error) {
          non_callable_target_rejected = error instanceof TypeError;
        }

        var revocable = Proxy.revocable(target, {});
        revocable.revoke();
        var revoked_rejected = false;
        try {
          revocable.proxy();
        } catch (error) {
          revoked_rejected = error instanceof TypeError;
        }

        globalThis.proxy_apply_ok = trapped === 16
          && forwarded === 19
          && observed
          && non_callable_trap_rejected
          && non_callable_target_rejected
          && revoked_rejected;
      "#
    ));

    assert_eq!(vm.get_global("proxy_apply_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn proxy_construct_trap_and_fallback_preserve_new_target() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        function Target(value) { this.value = value; }
        function Alternate() {}
        var direct_new_target_ok = false;
        var reflected_new_target_ok = false;
        var proxy;
        proxy = new Proxy(Target, {
          construct: function(actual_target, arguments_list, new_target) {
            direct_new_target_ok = direct_new_target_ok || new_target === proxy;
            reflected_new_target_ok = reflected_new_target_ok || new_target === Alternate;
            return {
              target_ok: actual_target === Target,
              value: arguments_list[0]
            };
          }
        });

        var direct = new proxy(7);
        var reflected = Reflect.construct(proxy, [8], Alternate);

        var fallback = new Proxy(Target, {});
        var forwarded = Reflect.construct(fallback, [9], Alternate);

        var primitive_rejected = false;
        try {
          new new Proxy(Target, {
            construct: function() { return 1; }
          })();
        } catch (error) {
          primitive_rejected = error instanceof TypeError;
        }

        var non_callable_trap_rejected = false;
        try {
          new new Proxy(Target, { construct: 1 })();
        } catch (error) {
          non_callable_trap_rejected = error instanceof TypeError;
        }

        var non_constructor_target_rejected = false;
        try {
          new new Proxy({}, {
            construct: function() { return {}; }
          })();
        } catch (error) {
          non_constructor_target_rejected = error instanceof TypeError;
        }

        var revocable = Proxy.revocable(Target, {});
        revocable.revoke();
        var revoked_rejected = false;
        try {
          new revocable.proxy();
        } catch (error) {
          revoked_rejected = error instanceof TypeError;
        }

        globalThis.proxy_construct_ok = direct.target_ok
          && direct.value === 7
          && reflected.target_ok
          && reflected.value === 8
          && direct_new_target_ok
          && reflected_new_target_ok
          && forwarded.value === 9
          && Object.getPrototypeOf(forwarded) === Alternate.prototype
          && primitive_rejected
          && non_callable_trap_rejected
          && non_constructor_target_rejected
          && revoked_rejected;
      "#
    ));

    assert_eq!(vm.get_global("proxy_construct_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn proxy_define_property_trap_and_invariants() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var observed = false;
        var target = {};
        var proxy = new Proxy(target, {
          defineProperty: function(target, key, desc) {
            observed = key === "virtual" && desc.value === 7;
            return true;
          }
        });
        var accepted = Reflect.defineProperty(proxy, "virtual", {
          value: 7,
          configurable: true
        });

        var sealed = {};
        Object.preventExtensions(sealed);
        var sealed_rejected = false;
        try {
          Reflect.defineProperty(new Proxy(sealed, {
            defineProperty: function() { return true; }
          }), "new", { value: 1 });
        } catch (error) {
          sealed_rejected = error instanceof TypeError;
        }

        var fixed = {};
        Object.defineProperty(fixed, "value", {
          value: 1,
          writable: false,
          configurable: false
        });
        var fixed_rejected = false;
        try {
          Reflect.defineProperty(new Proxy(fixed, {
            defineProperty: function() { return true; }
          }), "value", { value: 2 });
        } catch (error) {
          fixed_rejected = error instanceof TypeError;
        }

        globalThis.proxy_define_ok = accepted
          && observed
          && target.virtual === undefined
          && sealed_rejected
          && fixed_rejected;
      "#
    ));

    assert_eq!(vm.get_global("proxy_define_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn proxy_has_and_delete_traps_enforce_invariants() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var target = { value: 1 };
        var proxy = new Proxy(target, {
          has: function() { return false; },
          deleteProperty: function() { return true; }
        });
        var virtual_has = "value" in proxy;
        var virtual_delete = Reflect.deleteProperty(proxy, "value");

        var fixed = {};
        Object.defineProperty(fixed, "value", { configurable: false });
        var fixed_has_rejected = false;
        var fixed_delete_rejected = false;
        try {
          "value" in new Proxy(fixed, { has: function() { return false; } });
        } catch (error) {
          fixed_has_rejected = error instanceof TypeError;
        }
        try {
          Reflect.deleteProperty(new Proxy(fixed, {
            deleteProperty: function() { return true; }
          }), "value");
        } catch (error) {
          fixed_delete_rejected = error instanceof TypeError;
        }

        var sealed = { value: 1 };
        Object.preventExtensions(sealed);
        var sealed_has_rejected = false;
        var sealed_delete_rejected = false;
        try {
          "value" in new Proxy(sealed, { has: function() { return false; } });
        } catch (error) {
          sealed_has_rejected = error instanceof TypeError;
        }
        try {
          Reflect.deleteProperty(new Proxy(sealed, {
            deleteProperty: function() { return true; }
          }), "value");
        } catch (error) {
          sealed_delete_rejected = error instanceof TypeError;
        }

        globalThis.proxy_has_delete_ok = virtual_has === false
          && virtual_delete === true
          && target.value === 1
          && fixed_has_rejected
          && fixed_delete_rejected
          && sealed_has_rejected
          && sealed_delete_rejected;
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_has_delete_ok"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_extensibility_traps_enforce_target_state() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var target = {};
        var proxy = new Proxy(target, {
          isExtensible: function(target) {
            return Reflect.isExtensible(target);
          },
          preventExtensions: function(target) {
            return Reflect.preventExtensions(target);
          }
        });
        var initially_extensible = Reflect.isExtensible(proxy);
        var prevented = Reflect.preventExtensions(proxy);
        var finally_extensible = Object.isExtensible(proxy);

        var bad_is_extensible = false;
        try {
          Reflect.isExtensible(new Proxy({}, {
            isExtensible: function() { return false; }
          }));
        } catch (error) {
          bad_is_extensible = error instanceof TypeError;
        }

        var bad_prevent_extensions = false;
        try {
          Reflect.preventExtensions(new Proxy({}, {
            preventExtensions: function() { return true; }
          }));
        } catch (error) {
          bad_prevent_extensions = error instanceof TypeError;
        }

        globalThis.proxy_extensibility_ok = initially_extensible
          && prevented
          && !finally_extensible
          && bad_is_extensible
          && bad_prevent_extensions;
      "#
    ));

    assert_eq!(
      vm.get_global("proxy_extensibility_ok"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn proxy_prototype_traps_and_reflect_methods_enforce_invariants() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var target = {};
        var reported = {};
        var proxy = new Proxy(target, {
          getPrototypeOf: function() { return reported; },
          setPrototypeOf: function() { return true; }
        });
        var extensible_get = Reflect.getPrototypeOf(proxy) === reported;
        var extensible_set = Reflect.setPrototypeOf(proxy, reported);
        var target_unchanged = Reflect.getPrototypeOf(target) !== reported;

        Object.preventExtensions(target);
        var invalid_get = false;
        var invalid_set = false;
        try {
          Reflect.getPrototypeOf(proxy);
        } catch (error) {
          invalid_get = error instanceof TypeError;
        }
        try {
          Reflect.setPrototypeOf(proxy, reported);
        } catch (error) {
          invalid_set = error instanceof TypeError;
        }

        var actual = Reflect.getPrototypeOf(target);
        var valid = new Proxy(target, {
          getPrototypeOf: function() { return actual; },
          setPrototypeOf: function() { return true; }
        });
        var matching_get = Object.getPrototypeOf(valid) === actual;
        var matching_set = Reflect.setPrototypeOf(valid, actual);

        globalThis.proxy_prototype_ok = extensible_get
          && extensible_set
          && target_unchanged
          && invalid_get
          && invalid_set
          && matching_get
          && matching_set;
      "#
    ));

    assert_eq!(vm.get_global("proxy_prototype_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn object_integrity_operations_use_proxy_internal_methods() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var target = { value: 1 };
        var prevented = false;
        var described = false;
        var defined = false;
        var proxy = new Proxy(target, {
          preventExtensions: function(target) {
            prevented = true;
            return Reflect.preventExtensions(target);
          },
          ownKeys: function(target) {
            return Reflect.ownKeys(target);
          },
          getOwnPropertyDescriptor: function(target, key) {
            described = true;
            return Reflect.getOwnPropertyDescriptor(target, key);
          },
          defineProperty: function(target, key, desc) {
            defined = true;
            return Reflect.defineProperty(target, key, desc);
          },
          isExtensible: function(target) {
            return Reflect.isExtensible(target);
          }
        });
        var frozen_result = Object.freeze(proxy) === proxy;
        var descriptor = Object.getOwnPropertyDescriptor(target, "value");
        var frozen = Object.isFrozen(proxy);

        var primitive_behavior = Object.freeze(1) === 1
          && Object.seal("x") === "x"
          && Object.isFrozen(undefined)
          && Object.isSealed(null)
          && !Object.isExtensible(1);

        globalThis.proxy_integrity_ok = frozen_result
          && prevented
          && described
          && defined
          && frozen
          && descriptor.writable === false
          && descriptor.configurable === false
          && primitive_behavior;
      "#
    ));

    assert_eq!(vm.get_global("proxy_integrity_ok"), Some(Value::Bool(true)));
  }

  #[test]
  fn builtin_object_property_lookup_uses_props_and_prototype() {
    let proto = Rc::new(RefCell::new(ObjectData::new()));
    proto.borrow_mut().properties.insert(
      PropertyKey::String("inherited".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(7.0)),
    );

    let props = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
      proto,
    ))));
    props.borrow_mut().properties.insert(
      PropertyKey::String("own".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(3.0)),
    );

    let builtin = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), props);

    assert!(matches!(
      get_property(&builtin, &PropertyKey::String("own".to_string().into()), 0)
        .expect("test should not fail"),
      Value::Number(3.0_f64)
    ));
    assert!(matches!(
      get_property(
        &builtin,
        &PropertyKey::String("inherited".to_string().into()),
        0
      )
      .expect("test should not fail"),
      Value::Number(7.0_f64)
    ));
  }

  #[test]
  fn set_property_tracked_mutates_builtin_object_props() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let props = Rc::new(RefCell::new(ObjectData::new()));
    let builtin = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), Clone::clone(&props));

    vm.set_property_tracked(
      &builtin,
      &PropertyKey::String("prototype".to_string().into()),
      Value::Number(11.0),
    )
    .unwrap_or_default();

    let stored = props
      .borrow()
      .properties
      .get(&PropertyKey::String("prototype".to_string().into()))
      .cloned();
    assert!(matches!(
      stored,
      Some(PropertyDescriptor::Data {
        value: Value::Number(_),
        ..
      })
    ));
  }

  #[test]
  fn same_value_treats_nan_as_equal() {
    assert!(same_value(
      &Value::Number(f64::NAN),
      &Value::Number(f64::NAN)
    ));
  }

  #[test]
  fn same_value_distinguishes_signed_zero() {
    assert!(!same_value(&Value::Number(0.0), &Value::Number(-0.0)));
    assert!(same_value(&Value::Number(-0.0), &Value::Number(-0.0)));
  }

  #[test]
  fn object_assign_copies_properties_and_returns_target() {
    let (mut vm, _) = run_script(crate::js!(
      r#"
            var target = { a: 1 };
            var result = Object.assign(target, { b: 2 }, null, { c: 3 });
        "#
    ));

    let target = vm.get_global("target").expect("target");
    let result = vm.get_global("result").expect("result");
    assert!(strict_equal(&target, &result));
    let a = vm
      .get_property_host(&target, &PropertyKey::String("a".to_string().into()))
      .expect("test should not fail");
    let b = vm
      .get_property_host(&target, &PropertyKey::String("b".to_string().into()))
      .expect("test should not fail");
    let c = vm
      .get_property_host(&target, &PropertyKey::String("c".to_string().into()))
      .expect("test should not fail");
    assert!(matches!(a, Value::Number(n) if (n - 1.0).abs() < f64::EPSILON));
    assert!(matches!(b, Value::Number(n) if (n - 2.0).abs() < f64::EPSILON));
    assert!(matches!(c, Value::Number(n) if (n - 3.0).abs() < f64::EPSILON));
  }

  #[test]
  fn object_from_entries_set_prototype_of_and_to_locale_string_work() {
    let (mut vm, _) = run_script(crate::js!(
      r#"
            var entries = [['a', 1], ['b', 2]];
            var created = Object.fromEntries(entries);
            var proto = { marker: true };
            var target = {};
            var proto_result = Object.setPrototypeOf(target, proto);
            var locale = ({}).toLocaleString();
        "#
    ));

    match vm.get_global("created") {
      Some(Value::Object(obj)) => {
        assert!(matches!(
            vm.get_property_host(&Value::Object(Clone::clone(&obj)), &PropertyKey::String("a".to_string().into())).ok(),
            Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON
        ));
        assert!(matches!(
            vm.get_property_host(&Value::Object(Clone::clone(&obj)), &PropertyKey::String("b".to_string().into())).ok(),
            Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON
        ));
      }
      other => panic!("expected object, got {:?}", other),
    }

    let target = vm.get_global("target").expect("target");
    let proto = vm.get_global("proto").expect("proto");
    let target_proto = vm.get_prototype(&target);
    assert!(strict_equal(&target_proto, &proto));
    assert!(strict_equal(
      &vm
        .get_global("proto_result")
        .expect("proto_result"),
      &target
    ));
    assert_eq!(
      vm.get_global("locale"),
      Some(Value::String("[object Object]".to_string().into()))
    );
  }

  #[test]
  fn object_freeze_and_seal_report_state() {
    let (vm, _) = run_script(crate::js!(
      r#"
            var frozen_obj = { a: 1 };
            var sealed_obj = { b: 2 };
            Object.freeze(frozen_obj);
            Object.seal(sealed_obj);
            var frozen = Object.isFrozen(frozen_obj);
            var sealed = Object.isSealed(sealed_obj);
            var not_frozen = Object.isFrozen(sealed_obj);
        "#
    ));

    assert!(matches!(vm.get_global("frozen"), Some(Value::Bool(true))));
    assert!(matches!(vm.get_global("sealed"), Some(Value::Bool(true))));
    assert!(matches!(
      vm.get_global("not_frozen"),
      Some(Value::Bool(false))
    ));
  }

  #[test]
  fn array_includes_uses_same_value_zero_and_index_of_uses_strict_equality() {
    let (vm, _) = run_script(crate::js!(
      r#"
            var arr = [NaN, 1];
            var includes_nan = arr.includes(NaN);
            var index_of_nan = arr.indexOf(NaN);
            globalThis.arr_len = arr.length;
            globalThis.arr_0 = arr[0];
            globalThis.arr_1 = arr[1];
        "#
    ));

    assert!(matches!(
      vm.get_global("includes_nan"),
      Some(Value::Bool(true))
    ));
    assert!(matches!(
        vm.get_global("index_of_nan"),
        Some(Value::Number(n)) if (n + 1.0).abs() < f64::EPSILON
    ));
  }

  #[test]
  fn dense_array_property_lookup_sees_index_and_length() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![
      Some(Value::Number(f64::NAN)),
      Some(Value::Number(1.0)),
    ])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(2.0)),
    );
    let arr = Value::Array(arr_data, props);

    assert!(
      vm.has_property_host(&arr, &PropertyKey::String("length".to_string().into()))
        .expect("test should not fail")
    );
    assert!(
      vm.has_property_host(&arr, &PropertyKey::String("0".to_string().into()))
        .expect("test should not fail")
    );
    assert!(matches!(
        vm.get_property_host(&arr, &PropertyKey::String("0".to_string().into()))
            .expect("test should not fail"),
        Value::Number(n) if n.is_nan()
    ));
  }

  #[test]
  fn array_of_builds_array_from_arguments() {
    let (vm, _) = run_script(crate::js!(
      r#"
            var created = Array.of(4, 5, 6);
        "#
    ));

    match vm.get_global("created") {
      Some(Value::Array(arr, props)) => {
        assert_eq!(arr.borrow().len(), 3);
        assert!(
          matches!(arr.borrow().first().cloned().flatten(), Some(Value::Number(n)) if (n - 4.0).abs() < f64::EPSILON)
        );
        assert!(
          matches!(arr.borrow().get(1).cloned().flatten(), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
        );
        assert!(
          matches!(arr.borrow().get(2).cloned().flatten(), Some(Value::Number(n)) if (n - 6.0).abs() < f64::EPSILON)
        );
        assert!(matches!(
            props.borrow().properties.get(&PropertyKey::String("length".to_string().into())),
            Some(PropertyDescriptor::Data {
                value: Value::Number(n),
                ..
            }) if (*n - 3.0).abs() < f64::EPSILON
        ));
      }
      other => panic!("expected array, got {:?}", other),
    }
  }

  #[test]
  fn array_from_follows_iterator_mapping_and_constructor_semantics() {
    let (vm, _) = run_script(crate::js!(
      r#"
        var array = [1, 2];
        array[Symbol.iterator] = function() {
          return { next: function() {
            if (this.i === undefined) this.i = 0;
            return this.i++ === 0
              ? { value: 9, done: false }
              : { value: undefined, done: true };
          }};
        };
        var receiver = { base: 10 };
        var mapped = Array.from(array, function(value, index) {
          return this.base + value + index;
        }, receiver);

        var calls = [];
        function Constructor(length) {
          calls.push(arguments.length + ":" + (length === undefined ? "u" : length));
        }
        var iterable_result = Array.from.call(Constructor, [3, 4]);
        var array_like_result = Array.from.call(Constructor, { 0: 5, length: 1 });

        globalThis.array_from_focused =
          mapped.length === 1 && mapped[0] === 19 &&
          calls[0] === "0:u" && calls[1] === "1:1" &&
          iterable_result[0] === 3 && iterable_result[1] === 4 &&
          array_like_result[0] === 5 && array_like_result.length === 1 &&
          Array.from.name === "from" && Array.from.length === 1;
      "#
    ));

    assert!(matches!(
      vm.get_global("array_from_focused"),
      Some(Value::Bool(true))
    ));
  }

  #[test]
  fn array_from_closes_iterators_and_validates_results() {
    let (vm, _) = run_script(crate::js!(
      r#"
        var closed = 0;
        var source = {};
        source[Symbol.iterator] = function() {
          return {
            next: function() { return { value: 1, done: false }; },
            return: function() { closed++; return {}; }
          };
        };
        try { Array.from(source, function() { throw "stop"; }); } catch (error) {}

        var invalid = {};
        invalid[Symbol.iterator] = function() {
          return { next: function() { return 1; } };
        };
        var invalid_threw = false;
        try { Array.from(invalid); } catch (error) { invalid_threw = true; }

        globalThis.array_from_iterator_checks = closed === 1 && invalid_threw;
      "#
    ));

    assert!(matches!(
      vm.get_global("array_from_iterator_checks"),
      Some(Value::Bool(true))
    ));
  }

  #[test]
  fn same_value_uses_reference_identity_for_objects() {
    let obj = Rc::new(RefCell::new(ObjectData::new()));
    let a = Value::Object(Clone::clone(&obj));
    let b = Value::Object(obj);
    let c = Value::Object(Rc::new(RefCell::new(ObjectData::new())));

    assert!(same_value(&a, &b));
    assert!(!same_value(&a, &c));
  }

  #[test]
  fn to_string_handles_promise_values() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let promise = vm.promise_value(Rc::new(RefCell::new(PromiseState::Pending {
      on_fulfilled: Vec::new(),
      on_rejected: Vec::new(),
    })));

    assert_eq!(
      vm.to_string(&promise)
        .expect("test should not fail"),
      "[object Promise]"
    );
  }

  #[test]
  fn to_string_formats_integral_numbers_without_trailing_decimal() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    assert_eq!(
      vm.to_string(&Value::Number(1.0))
        .expect("test should not fail"),
      "1"
    );
    assert_eq!(
      vm.to_string(&Value::Number(2.0))
        .expect("test should not fail"),
      "2"
    );
    assert_eq!(
      vm.to_string(&Value::Number(-0.0))
        .expect("test should not fail"),
      "0"
    );
  }

  #[test]
  fn to_number_parses_radix_prefixed_strings() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    let n = vm
      .to_number_value(&Value::String("0x10".to_string().into()))
      .expect("test should not fail");
    assert!((n - 16.0).abs() < f64::EPSILON);
    let n = vm
      .to_number_value(&Value::String("0b101".to_string().into()))
      .expect("test should not fail");
    assert!((n - 5.0).abs() < f64::EPSILON);
    let n = vm
      .to_number_value(&Value::String("0o10".to_string().into()))
      .expect("test should not fail");
    assert!((n - 8.0).abs() < f64::EPSILON);
  }

  #[test]
  fn to_number_rejects_signed_radix_prefixed_strings() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    assert!(
      vm.to_number_value(&Value::String("-0x10".to_string().into()))
        .expect("test should not fail")
        .is_nan()
    );
    assert!(
      vm.to_number_value(&Value::String("+0b101".to_string().into()))
        .expect("test should not fail")
        .is_nan()
    );
  }

  #[test]
  fn dense_array_index_beyond_max_doesnt_dense_resize() {
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![Some(Value::Number(
      0.0,
    ))])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(1.0)),
    );
    let arr = Value::Array(Clone::clone(&arr_data), props);
    let key = PropertyKey::String("4294967295".to_string().into());

    set_property(&arr, &key, Value::String("x".to_string().into())).expect("test should not fail");

    assert_eq!(arr_data.borrow().len(), 1);
    assert!(matches!(
        get_property(&arr, &key, 0).expect("test should not fail"),
        Value::String(ref s) if s == "x"
    ));
    assert!(matches!(
        get_property(&arr, &PropertyKey::String("length".to_string().into()), 0).expect("test should not fail"),
        Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
    ));
  }

  #[test]
  fn sparse_array_index_updates_length_without_dense_resize() {
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![Some(Value::Number(
      0.0,
    ))])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(1.0)),
    );
    let arr = Value::Array(Clone::clone(&arr_data), props);
    let key = PropertyKey::String("4294967294".to_string().into());

    set_property(&arr, &key, Value::Number(1.0)).expect("test should not fail");

    assert_eq!(arr_data.borrow().len(), 1);
    assert!(matches!(
        get_property(&arr, &key, 0).expect("test should not fail"),
        Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
    ));
    assert!(matches!(
        get_property(&arr, &PropertyKey::String("length".to_string().into()), 0).expect("test should not fail"),
        Value::Number(n) if (n - 4294967295.0).abs() < f64::EPSILON
    ));
  }

  #[test]
  fn dense_array_element_redefinition_preserves_configurability() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![
      Some(Value::Number(1.0)),
      Some(Value::Number(2.0)),
    ])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(2.0)),
    );
    let arr = Value::Array(arr_data, Clone::clone(&props));
    let getter = Value::Builtin(Rc::new(|_, _| Ok(Value::Undefined)));
    let mut desc: PartialDescriptor = PartialDescriptor {
      get: Some(getter.clone()),
      has_get: true,
      ..Default::default()
    };

    desc.get = Some(getter);
    desc.has_get = true;

    assert!(
      vm.define_own_property(&arr, PropertyKey::String("0".to_string().into()), desc)
        .expect("test should not fail")
    );
    assert!(matches!(
      props
        .borrow()
        .properties
        .get(&PropertyKey::String("0".to_string().into())),
      Some(PropertyDescriptor::Accessor {
        configurable: true,
        ..
      })
    ));
    assert!(
      vm.delete_property_host(&arr, &PropertyKey::String("0".to_string().into()))
        .expect("test should not fail")
    );
    assert!(
      !vm
        .has_property_host(&arr, &PropertyKey::String("0".to_string().into()))
        .expect("test should not fail")
    );
  }

  #[test]
  fn map_and_set_iterator_and_brand_conformance() {
    let (vm, _) = run_script(crate::js!(
      r#"
        var map = new Map([["a", 1], ["b", 2]]);
        var fake = Object.create(Map.prototype);
        var brand_threw = false;
        try { Map.prototype.get.call(fake, "a"); } catch (error) { brand_threw = true; }

        var iterator = map.entries();
        var first = iterator.next();
        first.value[1] = 99;
        var source_unchanged = map.get("a") === 1;
        map.delete("b");
        map.set("c", 3);
        var second = iterator.next();
        var exhausted = iterator.next().done;
        map.set("d", 4);
        var permanently_done = iterator.next().done;

        var set = new Set([1, 2]);
        var set_iterator = set.values();
        var set_entry = set.entries().next().value;
        set_entry[0] = 9;
        var set_pair_fresh = set.has(1) && set_entry[1] === 1;
        var hidden_slots = Object.getOwnPropertySymbols(map).length === 0 &&
          Object.getOwnPropertySymbols(set).length === 0 &&
          Object.getOwnPropertySymbols(iterator).length === 0;
        var negative_zero_map = new Map([[-0, 1]]);
        var negative_zero_set = new Set([-0]);
        var zeros_canonical = 1 / negative_zero_map.keys().next().value === Infinity &&
          1 / negative_zero_set.values().next().value === Infinity;
        var iterator_proto = Object.getPrototypeOf(Object.getPrototypeOf(iterator));
        var iterator_shapes = iterator_proto[Symbol.iterator].call(iterator) === iterator &&
          Object.getPrototypeOf(first) === Object.prototype &&
          Object.getPrototypeOf(first.value) === Array.prototype;
        var invalid_next = false;
        try { set_iterator.next.call({}); } catch (error) { invalid_next = true; }

        globalThis.collection_focused =
          brand_threw && source_unchanged && second.value[0] === "c" &&
          exhausted && permanently_done &&
          Set.prototype.keys === Set.prototype.values &&
          Set.prototype.values === Set.prototype[Symbol.iterator] &&
          set_pair_fresh && hidden_slots && zeros_canonical && iterator_shapes && invalid_next &&
          Object.prototype.toString.call(map) === "[object Map]" &&
          Object.prototype.toString.call(set) === "[object Set]" &&
          Map.prototype.get.name === "get" && Map.prototype.get.length === 1 &&
          Set.prototype.add.name === "add" && Set.prototype.add.length === 1 &&
          Object.getOwnPropertyDescriptor(Map.prototype, "get").enumerable === false &&
          Object.getOwnPropertyDescriptor(Map.prototype, "size").get.length === 0 &&
          iterator.next.name === "next" && iterator.next.length === 0 &&
          Object.getOwnPropertyDescriptor(iterator.next, "length").enumerable === false &&
          iterator.next.hasOwnProperty("length");
      "#
    ));

    assert_eq!(vm.get_global("collection_focused"), Some(Value::Bool(true)));
  }

  #[test]
  fn map_and_set_clear_and_for_each_follow_live_iteration() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const map = new Map([["a", 1], ["b", 2]]);
        const mapSeen = [];
        map.forEach((value, key, receiver) => {
          mapSeen.push([key, value, receiver === map]);
          if (key === "a") {
            map.delete("b");
            map.set("c", 3);
          }
        });
        const mapReturn = map.clear();

        const set = new Set([1, 2]);
        const setSeen = [];
        set.forEach((value, key, receiver) => {
          setSeen.push([value, key, receiver === set]);
          if (value === 1) {
            set.delete(2);
            set.add(3);
          }
        });
        const setReturn = set.clear();

        globalThis.collection_for_each_result =
          mapSeen.length === 2 && mapSeen[0][0] === "a" &&
          mapSeen[1][0] === "c" && mapSeen[1][1] === 3 && mapSeen[1][2] &&
          mapReturn === undefined && map.size === 0 &&
          setSeen.length === 2 && setSeen[0][0] === 1 && setSeen[0][1] === 1 &&
          setSeen[1][0] === 3 && setSeen[1][1] === 3 && setSeen[1][2] &&
          setReturn === undefined && set.size === 0;
      "#
    ));

    assert_eq!(
      vm.get_global("collection_for_each_result"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn map_values_is_a_live_self_iterating_iterator() {
    let (vm, _) = run_script(crate::js!(
      r#"
        var map = new Map([["first", 1], ["second", 2]]);
        var iterator = map.values();
        var first = iterator.next();
        var self = iterator[Symbol.iterator]() === iterator;
        map.delete("second");
        map.set("third", 3);
        var second = iterator.next();
        var third = iterator.next();
        var done = iterator.next();
        var iteratorMap = new Map([["a", 1], ["b", 2]]);
        var reversed = Array.from(iteratorMap.values()).reverse();
        var keys = Array.from(iteratorMap.keys());
        var entries = Array.from(iteratorMap.entries());
        var entriesAlias = iteratorMap.entries === iteratorMap[Symbol.iterator];
        var chainedMap = new Map([["chained", 4]]);
        var chainedHolder = { state: { finalizers: chainedMap } };
        var chained = Array.from(chainedHolder.state.finalizers.values());
        globalThis.map_values_result =
          self && first.value === 1 && !first.done &&
          second.value === 3 && third.done && done.done &&
          reversed[0] === 2 && reversed[1] === 1 &&
          keys[0] === "a" && keys[1] === "b" &&
          entries[0][0] === "a" && entries[0][1] === 1 && entriesAlias &&
          chained[0] === 4;
      "#
    ));

    assert_eq!(vm.get_global("map_values_result"), Some(Value::Bool(true)));
  }

  #[test]
  fn map_and_set_constructors_consume_iterables_with_spec_validation() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const source = new Map([['first', 1], ['second', 2]]);
        const copied = new Map(source);
        class CountingMap extends Map {
          set(key, value) {
            this.calls = (this.calls || 0) + 1;
            return super.set(key, value);
          }
        }
        const customAdderMap = new CountingMap([['x', 1], ['y', 2]]);
        const custom = {
          [Symbol.iterator]() {
            let index = 0;
            return {
              next() {
                return index < 2
                  ? { value: { 0: 'custom-' + index, 1: index++ }, done: false }
                  : { done: true };
              }
            };
          }
        };
        const customMap = new Map(custom);
        let nextGetterCalls = 0;
        const cachedNextMap = new Map({
          [Symbol.iterator]() {
            let done = false;
            return {
              get next() {
                nextGetterCalls += 1;
                return () => done
                  ? { done: true }
                  : (done = true, { value: ["cached", 1], done: false });
              }
            };
          }
        });
        const copiedSet = new Set(new Set([1, 2]));
        const customSet = new Set({
          [Symbol.iterator]() {
            let index = 0;
            return { next() {
              return index < 2 ? { value: index++, done: false } : { done: true };
            }};
          }
        });
        const nanMap = new Map([[NaN, 1]]);
        nanMap.set(NaN, 2);
        const nanSet = new Set([NaN, NaN]);
        const liveSet = new Set([1, 2, 3]);
        const liveSetIterator = liveSet[Symbol.iterator]();
        const liveSetFirst = liveSetIterator.next();
        liveSet.delete(1);
        const liveSetSecond = liveSetIterator.next();

        let malformed = false;
        try { new Map([1]); } catch (error) { malformed = error instanceof TypeError; }
        let accessorThrew = false;
        let closed = 0;
        const throwing = {
          [Symbol.iterator]() {
            return {
              next() {
                return { value: { get 0() { throw new Error('entry'); }, 1: 1 }, done: false };
              },
              return() { closed += 1; return {}; }
            };
          }
        };
        try { new Map(throwing); } catch (error) { accessorThrew = error instanceof Error; }
        globalThis.collection_constructor_result =
          copied.size === 2 && copied.get('second') === 2 &&
          customAdderMap.calls === 2 && customAdderMap.get('y') === 2 &&
          customMap.get('custom-1') === 1 && cachedNextMap.get('cached') === 1 &&
          nextGetterCalls === 1 && copiedSet.size === 2 &&
          customSet.size === 2 && nanMap.size === 1 && nanMap.get(NaN) === 2 &&
          nanSet.size === 1 && liveSetFirst.value === 1 && liveSetSecond.value === 2 &&
          malformed && accessorThrew && closed === 1;
      "#
    ));

    assert_eq!(
      vm.get_global("collection_constructor_result"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn weak_map_supports_object_identity_and_standard_methods() {
    let (vm, _) = run_script(crate::js!(
      r#"
        globalThis.first = {};
        globalThis.second = {};
        const callable = function() {};
        var wm = new WeakMap([[first, 1], [callable, 2]]);
        const chained = wm.set(second, 3);
        wm.set(first, 4);
        const hasSecond = wm.has(second);
        const gotFirst = wm.get(first);
        const gotCallable = wm.get(callable);
        const hasFreshObject = wm.has({});
        const hasPrimitive = wm.has(1);
        const gotPrimitive = wm.get(1);
        const deletedPrimitive = wm.delete(1);
        const deletedSecond = wm.delete(second);
        const hasDeletedSecond = wm.has(second);

        let invalidKeyRejected = false;
        let callRejected = false;
        let brandRejected = false;
        try { wm.set(1, "invalid"); } catch (error) {
          invalidKeyRejected = error instanceof TypeError;
        }
        try { WeakMap(); } catch (error) {
          callRejected = error instanceof TypeError;
        }
        try { WeakMap.prototype.get.call(new Map(), first); } catch (error) {
          brandRejected = error instanceof TypeError;
        }

        globalThis.weak_map_result = [
          hasSecond,
          gotFirst,
          gotCallable,
          chained === wm,
          hasFreshObject,
          hasPrimitive,
          gotPrimitive === undefined,
          deletedPrimitive,
          deletedSecond,
          !hasDeletedSecond,
          invalidKeyRejected,
          callRejected,
          brandRejected,
          new WeakMap(null) instanceof WeakMap,
          Object.prototype.toString.call(wm)
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("weak_map_result"),
      Some(Value::String(
        "true,4,2,true,false,false,true,false,true,true,true,true,true,true,[object WeakMap]"
          .to_string()
          .into()
      ))
    );
  }

  #[test]
  fn weak_map_entries_follow_ephemeron_reachability() {
    let (mut vm, _) = run_script(crate::js!(
      r#"
        globalThis.weak_map_for_gc = new WeakMap();
        globalThis.weak_map_key_for_gc = {};
        weak_map_for_gc.set(weak_map_key_for_gc, {
          key: weak_map_key_for_gc
        });
      "#
    ));
    let weak_map = vm
      .get_global("weak_map_for_gc")
      .expect("WeakMap global");
    let Value::Object(data) = weak_map else {
      panic!("WeakMap must be an ordinary object");
    };
    assert_eq!(
      data
        .borrow()
        .weak_map_entries
        .as_ref()
        .expect("WeakMap data")
        .len(),
      1
    );

    assert!(
      vm.get_global("weak_map_key_for_gc").is_some(),
      "key is rooted before collection"
    );

    vm.collect_garbage_prototype();
    assert_eq!(
      data
        .borrow()
        .weak_map_entries
        .as_ref()
        .expect("reachable WeakMap data")
        .len(),
      1
    );

    let source = crate::js!("globalThis.weak_map_key_for_gc = null;");
    let session = Parser::parse_source(source).expect("parse WeakMap key release");
    let chunk =
      compiler::compile_oxc(session.program(), session.source()).expect("compile WeakMap release");
    vm.run(chunk).expect("release WeakMap key");
    vm.collect_garbage_prototype();

    assert!(
      data
        .borrow()
        .weak_map_entries
        .as_ref()
        .expect("reachable WeakMap data")
        .is_empty()
    );
  }

  #[test]
  fn weak_map_supports_symbol_keys_and_upsert_methods() {
    let (vm, _) = run_script(crate::js!(
      r#"
        const symbol = Symbol("key");
        const key = {};
        const map = new WeakMap();
        map.set(symbol, 1);
        map.set(Symbol.hasInstance, 2);
        const inserted = map.getOrInsert(key, 3);
        const retained = map.getOrInsert(key, 4);
        const computedKey = {};
        const computed = map.getOrInsertComputed(computedKey, current => {
          map.set(current, 5);
          return 6;
        });
        let registeredRejected = false;
        try {
          map.set(Symbol.for("registered"), 7);
        } catch (error) {
          registeredRejected = error instanceof TypeError;
        }
        globalThis.weak_map_upsert_result = [
          map.get(symbol),
          map.get(Symbol.hasInstance),
          inserted,
          retained,
          computed,
          map.get(computedKey),
          registeredRejected,
          WeakMap.prototype.getOrInsert.length,
          WeakMap.prototype.getOrInsertComputed.length
        ].join(",");
      "#
    ));

    assert_eq!(
      vm.get_global("weak_map_upsert_result"),
      Some(Value::String("1,2,3,3,6,6,true,2,2".to_string().into()))
    );
  }

  #[test]
  fn reverse_is_generic_and_preserves_sparse_properties() {
    let (vm, _) = run_script(crate::js!(
      r#"
        var array = [];
        array[1] = "one";
        array[3] = "three";
        array.length = 5;
        array.reverse();
        var object = { length: 4 };
        object[0] = "zero";
        object[2] = "two";
        Array.prototype.reverse.call(object);
        globalThis.reverse_sparse_result =
          array.length === 5 && array[1] === "three" && array[3] === "one" &&
          !array.hasOwnProperty("0") && !array.hasOwnProperty("2") &&
          object[1] === "two" && object[3] === "zero" &&
          !object.hasOwnProperty("0") && !object.hasOwnProperty("2");
      "#
    ));

    assert_eq!(
      vm.get_global("reverse_sparse_result"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn reverse_with_length_shrink_clears_lower_index() {
    let (mut vm, _) = run_script(crate::js!(
      r#"
            var array = ["first", "second"];
            Object.defineProperty(array, 0, {
                get: function() {
                    array.length = 0;
                    return "first";
                }
            });
            array.reverse();
        "#
    ));

    match vm.get_global("array") {
      Some(Value::Array(arr, props)) => {
        let array_val = Value::Array(arr, props);

        assert!(
          !vm
            .has_property_host(&array_val, &PropertyKey::String("0".to_string().into()))
            .expect("expected false, got true")
        );
        assert!(
          vm.has_property_host(&array_val, &PropertyKey::String("1".to_string().into()))
            .expect("expected true, got false")
        );
        assert!(matches!(
            vm.get_property_host(
                &array_val,
                &PropertyKey::String("1".to_string().into())
            )
            .expect("expected 'first', got something else"),
            Value::String(ref s) if s == "first"
        ));
      }
      other => panic!("expected array, got {:?}", other),
    }
  }

  #[test]
  fn top_level_assignment_updates_global_binding() {
    let session = Parser::parse_source(crate::js!("var x; x = 5;")).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON));
  }

  #[test]
  fn eval_for_loop_completion_is_undefined() {
    let src = crate::js!(
      r#"
            var supreme, count;
            supreme = 5;
            var evaluated = eval("for(count=0;;) {if (count===supreme)break;else count++; }");
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    assert!(
      matches!(vm.get_global("supreme"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
    );
    assert!(
      matches!(vm.get_global("count"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
    );
    assert!(matches!(vm.get_global("evaluated"), Some(Value::Undefined)));
  }

  #[test]
  fn eval_array_spread_literal_expands_values() {
    let src = crate::js!(r#"var evaluated = eval('[...[1, 2, 3], 4, 5]');"#);
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    match vm.get_global("evaluated") {
      Some(Value::Array(arr, _)) => {
        let values: Vec<Value> = arr
          .borrow()
          .iter()
          .filter_map(Clone::clone)
          .collect();
        assert_eq!(values.len(), 5);
        assert!(matches!(values[0], Value::Number(n) if (n - 1.0).abs() < f64::EPSILON));
        assert!(matches!(values[1], Value::Number(n) if (n - 2.0).abs() < f64::EPSILON));
        assert!(matches!(values[2], Value::Number(n) if (n - 3.0).abs() < f64::EPSILON));
        assert!(matches!(values[3], Value::Number(n) if (n - 4.0).abs() < f64::EPSILON));
        assert!(matches!(values[4], Value::Number(n) if (n - 5.0).abs() < f64::EPSILON));
      }
      other => panic!("expected evaluated array, got {:?}", other),
    }
  }

  #[test]
  fn with_statement_coerces_operand_to_object() {
    let session =
      Parser::parse_source(crate::js!("with (undefined) { x = 1; }")).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
      .run(chunk)
      .expect_err("with(undefined) should throw");

    assert!(
      err
        .to_string()
        .contains("Cannot convert undefined or null to object")
    );
    assert!(vm.get_global("x").is_none());
  }

  #[test]
  fn with_statement_reads_bindings_from_object_environment() {
    let session = Parser::parse_source(crate::js!(
      r#"
            var result;
            var obj = { value: "from-with", NaN: "shadowed" };
            with (obj) {
                result = value;
                result = result + ":" + NaN;
            }
        "#
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("from-with:shadowed".into()))
    );
  }

  #[test]
  fn with_statement_inside_constructor_reads_and_writes_object_bindings() {
    let session = Parser::parse_source(crate::js!(
      r#"
            var result = "result";
            var myObj = { value: "myObj_value", NaN: "obj_NaN" };
            var st_NaN = "NaN";
            var f = function() {
              with (myObj) {
                st_NaN = NaN;
                var value = "value";
                return value;
              }
            };
            var obj = new f();
        "#
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
      vm.get_global("st_NaN"),
      Some(Value::String("obj_NaN".into()))
    );
  }

  #[test]
  fn with_statement_handles_common_identifier_shadowing_cases() {
    let session = Parser::parse_source(crate::js!(
      r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var st_NaN = "NaN";
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseInt = "parseInt";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";
            var myObj = {
              p1: "a",
              p2: "b",
              p3: "c",
              value: "myObj_value",
              valueOf: function() { return "obj_valueOf"; },
              parseInt: function() { return "obj_parseInt"; },
              NaN: "obj_NaN",
              Infinity: "obj_Infinity",
              eval: function() { return "obj_eval"; },
              parseFloat: function() { return "obj_parseFloat"; },
              isNaN: function() { return "obj_isNaN"; },
              isFinite: function() { return "obj_isFinite"; }
            };
            var f = function() {
              with (myObj) {
                st_p1 = p1;
                st_p2 = p2;
                st_p3 = p3;
                st_parseInt = parseInt;
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
              }
            };
            var obj = new f();
        "#
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
      vm.get_global("st_NaN"),
      Some(Value::String("obj_NaN".into()))
    );
    assert_eq!(
      vm.get_global("st_Infinity"),
      Some(Value::String("obj_Infinity".into()))
    );
    match vm.get_global("st_parseInt") {
      Some(Value::Closure { .. }) => {}
      other => panic!("expected function object for parseInt, got {:?}", other),
    }
    match vm.get_global("st_parseFloat") {
      Some(Value::Closure { .. }) => {}
      other => panic!("expected function object for parseFloat, got {:?}", other),
    }
  }

  #[test]
  fn with_statement_prefers_object_bindings_over_global_nans() {
    let session = Parser::parse_source(crate::js!(
      r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var myObj = {
              p1: "a",
              p2: "b",
              p3: "c",
              value: "myObj_value",
              valueOf: function() { return "obj_valueOf"; },
              parseInt: function() { return "obj_parseInt"; },
              NaN: "obj_NaN",
              Infinity: "obj_Infinity",
              eval: function() { return "obj_eval"; },
              parseFloat: function() { return "obj_parseFloat"; },
              isNaN: function() { return "obj_isNaN"; },
              isFinite: function() { return "obj_isFinite"; }
            };
            var st_NaN = "NaN";
            var f = function() {
              with (myObj) {
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
                p1 = "x1";
                this.p2 = "x2";
                delete p3;
                var value = "value";
              }
            };
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";
            f();
        "#
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
      vm.get_global("st_NaN"),
      Some(Value::String("obj_NaN".into()))
    );
    assert_eq!(
      vm.get_global("st_Infinity"),
      Some(Value::String("obj_Infinity".into()))
    );
    match vm.get_global("st_eval") {
      Some(Value::Closure { .. }) => {}
      other => panic!("expected function object for eval, got {:?}", other),
    }
    match vm.get_global("st_parseFloat") {
      Some(Value::Closure { .. }) => {}
      other => panic!("expected function object for parseFloat, got {:?}", other),
    }
    match vm.get_global("st_isNaN") {
      Some(Value::Closure { .. }) => {}
      other => panic!("expected function object for isNaN, got {:?}", other),
    }
    match vm.get_global("st_isFinite") {
      Some(Value::Closure { .. }) => {}
      other => panic!("expected function object for isFinite, got {:?}", other),
    }
  }

  #[test]
  fn with_statement_prefers_object_bindings_over_global_nans_in_plain_call() {
    let session = Parser::parse_source(crate::js!(
      r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var result = "result";
            var myObj = {p1: "a",
                         p2: "b",
                         p3: "c",
                         value: "myObj_value",
                         valueOf: function(){return "obj_valueOf";},
                         parseInt: function(){return "obj_parseInt";},
                         NaN: "obj_NaN",
                         Infinity: "obj_Infinity",
                         eval: function(){return "obj_eval";},
                         parseFloat: function(){return "obj_parseFloat";},
                         isNaN: function(){return "obj_isNaN";},
                         isFinite: function(){return "obj_isFinite";}
            };
            var del;
            var st_p1 = "p1";
            var st_p2 = "p2";
            var st_p3 = "p3";
            var st_parseInt = "parseInt";
            var st_NaN = "NaN";
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";

            var f = function(){
              with(myObj){
                st_p1 = p1;
                st_p2 = p2;
                st_p3 = p3;
                st_parseInt = parseInt;
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
                p1 = "x1";
                this.p2 = "x2";
                del = delete p3;
                var p4 = "x4";
                p5 = "x5";
                var value = "value";
              }
            };
            f();
        "#
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
      vm.get_global("st_NaN"),
      Some(Value::String("obj_NaN".into()))
    );
  }

  #[test]
  fn with_statement_var_initializer_writes_through_object_binding() {
    let session = Parser::parse_source(crate::js!(
      r#"
            var result;
            var myObj = { value: "myObj_value" };
            with (myObj) {
              var value = "value";
            }
            result = value;
        "#
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(vm.get_global("result"), Some(Value::Undefined));
    match vm.get_global("myObj") {
      Some(Value::Object(obj)) => {
        let value_key = PropertyKey::String("value".to_string().into());
        let obj_borrow = obj.borrow();
        match obj_borrow.properties.get(&value_key) {
          Some(PropertyDescriptor::Data { value, .. }) => {
            assert_eq!(value, &Value::String("value".into()))
          }
          other => panic!("expected data property for myObj.value, got {:?}", other),
        }
      }
      other => panic!("expected object for myObj, got {:?}", other),
    }
  }

  #[test]
  fn strict_assignment_to_non_extensible_object_throws() {
    let src = crate::js!(
      r#"
            function f() {
                'use strict';
                var _8_7_2_5 = {};
                Object.preventExtensions(_8_7_2_5);
                _8_7_2_5.b = 11;
            }
            f();
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
      .run(chunk)
      .expect_err("strict assignment should throw");

    assert!(
      err
        .to_string()
        .contains("Cannot add property to non-extensible object")
    );
    assert!(vm.get_global("b").is_none());
  }

  #[test]
  fn mapped_arguments_object_exposes_callee() {
    let session = Parser::parse_source(crate::js!(
      r#"
            function testcase() {
                var desc = Object.getOwnPropertyDescriptor(arguments, "callee");
                return typeof arguments.callee === "function"
                    && desc.configurable === true
                    && desc.enumerable === false
                    && desc.writable === true;
            }
            var result = testcase();
        "#
    ))
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("result"), Some(Value::Bool(true))));
  }

  #[test]
  fn function_apply_uses_create_list_from_array_like() {
    let (vm, _) = run_script(crate::js!(
      r#"
        function capture() {
          return {
            receiver: this,
            count: arguments.length,
            first: arguments[0],
            hole: arguments[1] === undefined,
            third: arguments[2]
          };
        }
        function invokeWithArguments() {
          return capture.apply(this, arguments);
        }
        const receiver = { tag: "receiver" };
        const fromArguments = invokeWithArguments.call(receiver, "a", "b");
        const fromArrayLike = capture.apply(receiver, { 0: "a", 2: "c", length: 3 });
        let proxyGets = 0;
        const proxiedArrayLike = new Proxy({ 0: "proxy", length: 1 }, {
          get(target, key, receiver) {
            proxyGets += 1;
            return Reflect.get(target, key, receiver);
          }
        });
        const fromProxy = capture.apply(receiver, proxiedArrayLike);

        let lengthReads = 0;
        let indexReads = 0;
        let abrupt;
        const accessorArgs = {
          get length() { lengthReads += 1; return 2.9; },
          get 0() { indexReads += 1; return "accessor"; },
          get 1() { indexReads += 1; throw "boom"; }
        };
        try {
          capture.apply(receiver, accessorArgs);
        } catch (error) {
          abrupt = error;
        }

        let nonObjectError;
        try {
          capture.apply(receiver, 1);
        } catch (error) {
          nonObjectError = error;
        }

        let lengthTouched = false;
        let nonCallableError;
        try {
          Function.prototype.apply.call({}, receiver, {
            get length() { lengthTouched = true; throw "length should not be read"; }
          });
        } catch (error) {
          nonCallableError = error;
        }

        globalThis.apply_result =
          fromArguments.receiver === receiver
          && fromArguments.count === 2
          && fromArguments.first === "a"
          && fromArrayLike.receiver === receiver
          && fromArrayLike.count === 3
          && fromArrayLike.first === "a"
          && fromArrayLike.hole
          && fromArrayLike.third === "c"
          && fromProxy.first === "proxy"
          && proxyGets === 2
          && lengthReads === 1
          && indexReads === 2
          && abrupt === "boom"
          && typeof nonObjectError === "object"
          && typeof nonCallableError === "object"
          && !lengthTouched
          && capture.apply(receiver, null).count === 0
          && capture.apply(receiver, undefined).count === 0;
      "#
    ));

    assert_eq!(vm.get_global("apply_result"), Some(Value::Bool(true)));
  }

  #[test]
  fn strict_arguments_object_poison_pill_throws_on_callee() {
    let session = Parser::parse_source(crate::js!(
      r#"
            function testcase() {
                'use strict';
                return arguments.callee;
            }
            testcase();
        "#
    ))
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
      .run(chunk)
      .expect_err("strict callee access should throw");
    assert!(
      err
        .to_string()
        .contains("strict arguments callee/caller is forbidden")
    );
  }

  #[test]
  fn mapped_arguments_object_prototype_and_aliasing() {
    let session = Parser::parse_source(crate::js!(
      r#"
            function testcase(a) {
                arguments[0] = 2;
                a = 3;
                return Object.getPrototypeOf(arguments) === Object.prototype
                    && arguments[0] === 3
                    && a === 3
                    && Object.getOwnPropertyDescriptor(arguments, "length").enumerable === false;
            }
            var result = testcase(1);
        "#
    ))
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("result"), Some(Value::Bool(true))));
  }

  #[test]
  fn arguments_object_is_iterable_with_array_iterator() {
    let session = Parser::parse_source(crate::js!(
      r#"
            function mapped(a, b, c) {
                return arguments[Symbol.iterator].name
                    + ":" + [...arguments].join(",");
            }
            function unmapped([a], b, c) {
                return arguments[Symbol.iterator].name
                    + ":" + [...arguments].join(",");
            }
            var mapped_result = mapped(1, 2, 3);
            var unmapped_result = unmapped([1], 2, 3);
        "#
    ))
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert!(matches!(
        vm.get_global("mapped_result"),
        Some(Value::String(s)) if s == "values:1,2,3"
    ));
    assert!(matches!(
        vm.get_global("unmapped_result"),
        Some(Value::String(s)) if s == "values:1,2,3"
    ));
  }

  #[test]
  fn void_operator_evaluates_to_undefined() {
    let session = Parser::parse_source(crate::js!("var x = void 0;")).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Undefined)));
  }

  #[test]
  fn logical_and_short_circuits_and_preserves_left_operand() {
    let session = Parser::parse_source(crate::js!("var x = 0 && bump();")).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let calls = Rc::new(RefCell::new(0usize));
    let globals = test_globals();
    globals.borrow_mut().properties.insert(
      PropertyKey::String("bump".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new({
        let calls = Clone::clone(&calls);
        move |_vm, _args| {
          *calls.borrow_mut() += 1;
          Ok(Value::Number(99.0))
        }
      }))),
    );

    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert_eq!(*calls.borrow(), 0);
    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if n == 0.0_f64));
  }

  #[test]
  fn logical_or_short_circuits_and_preserves_left_operand() {
    let session = Parser::parse_source(crate::js!("var x = 1 || bump();")).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let calls = Rc::new(RefCell::new(0usize));
    let globals = test_globals();
    globals.borrow_mut().properties.insert(
      PropertyKey::String("bump".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new({
        let calls = Clone::clone(&calls);
        move |_vm, _args| {
          *calls.borrow_mut() += 1;
          Ok(Value::Number(99.0))
        }
      }))),
    );

    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert_eq!(*calls.borrow(), 0);
    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if n == 1.0_f64));
  }

  #[test]
  fn conditional_expression_merges_both_arms_and_nested_results() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var truthy = true ? 11 : 12;
        var falsy = false ? 21 : 22;
        var nested_left = true ? (false ? 31 : 32) : 33;
        var nested_right = false ? 41 : (true ? 42 : 43);
      "#
    ));

    assert_eq!(vm.get_global("truthy"), Some(Value::Number(11.0)));
    assert_eq!(vm.get_global("falsy"), Some(Value::Number(22.0)));
    assert_eq!(vm.get_global("nested_left"), Some(Value::Number(32.0)));
    assert_eq!(vm.get_global("nested_right"), Some(Value::Number(42.0)));
  }

  #[test]
  fn catch_handler_receives_thrown_value_at_cfg_entry() {
    let (vm, _globals) = run_script(crate::js!(
      "var caught; try { throw 42; } catch (error) { caught = error; }"
    ));

    assert_eq!(vm.get_global("caught"), Some(Value::Number(42.0)));
  }

  #[test]
  fn lexical_binding_reads_before_init_throw_reference_error() {
    let session =
      Parser::parse_source(crate::js!("function f(){ x; let x = 1; } f();")).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
      .run(chunk)
      .expect_err("run should fail for TDZ read");
    let msg = err.to_string();
    assert!(
      msg.contains("Cannot access lexical binding before initialization"),
      "unexpected error: {msg}"
    );
  }

  #[test]
  fn captured_lexical_binding_reads_before_init_throw_reference_error() {
    let session = Parser::parse_source(crate::js!(
      "function g(){ function f(){ return x + 1; } return f(); let x = 1; } g();"
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
      .run(chunk)
      .expect_err("run should fail for captured TDZ read");
    let msg = err.to_string();
    assert!(
      msg.contains("Cannot access lexical binding before initialization"),
      "unexpected error: {msg}"
    );
  }

  #[test]
  fn try_block_lexical_shadow_does_not_escape_to_catch() {
    let session = Parser::parse_source(
      crate::js!("var out = (function(x){ try { let x = 'inner'; throw 0; } catch(e) { return x; } })('outer');"),
        )
        .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("out"), Some(Value::String(s)) if s == "outer"));
  }

  #[test]
  fn for_in_lexical_declaration_with_initializer_is_syntax_error() {
    let session = Parser::parse_source(crate::js!("for (let x = 3 in {}) {}"))
      .expect("parser currently accepts syntax");
    let program = session.program();
    let err = compiler::compile(program, session.source())
      .expect_err("compile should reject lexical initializer in for-in");
    assert!(err.to_string().contains("SyntaxError"));
  }

  #[test]
  fn var_binding_reads_before_init_return_undefined() {
    let session = Parser::parse_source(crate::js!(
      "function f(){ return x; var x = 1; } var out = f();"
    ))
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("out"), Some(Value::Undefined)));
  }

  #[test]
  fn generator_next_yields_values_and_done() {
    let src = crate::js!(
      r#"
            function* g() { yield 1; yield 2; return 3; }
            var it = g();
            var a = it.next();
            var b = it.next();
            var c = it.next();
            var d = it.next();
            function* h() { yield yield 4; }
            var nested = h();
            var e = nested.next();
            var f = nested.next();
            var g_result = nested.next(9);
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    let a = vm.get_global("a").expect("a exists");
    let b = vm.get_global("b").expect("b exists");
    let c = vm.get_global("c").expect("c exists");
    let d = vm.get_global("d").expect("d exists");
    let e = vm.get_global("e").expect("e exists");
    let f = vm.get_global("f").expect("f exists");
    let g_result = vm
      .get_global("g_result")
      .expect("g_result exists");

    assert!(matches!(
        get_property(&a, &PropertyKey::String("value".to_string().into()), 0).expect("test should not fail"),
        Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
    ));
    assert!(matches!(
      get_property(&a, &PropertyKey::String("done".to_string().into()), 0)
        .expect("test should not fail"),
      Value::Bool(false)
    ));
    assert!(matches!(
        get_property(&b, &PropertyKey::String("value".to_string().into()), 0).expect("test should not fail"),
        Value::Number(n) if (n - 2.0).abs() < f64::EPSILON
    ));
    assert!(matches!(
        get_property(&c, &PropertyKey::String("value".to_string().into()), 0).expect("test should not fail"),
        Value::Number(n) if (n - 3.0).abs() < f64::EPSILON
    ));
    assert!(matches!(
      get_property(&c, &PropertyKey::String("done".to_string().into()), 0)
        .expect("test should not fail"),
      Value::Bool(true)
    ));
    assert!(matches!(
      get_property(&d, &PropertyKey::String("done".to_string().into()), 0)
        .expect("test should not fail"),
      Value::Bool(true)
    ));
    assert!(matches!(
      get_property(&e, &PropertyKey::String("value".to_string().into()), 0).expect("test should not fail"),
      Value::Number(n) if (n - 4.0).abs() < f64::EPSILON
    ));
    assert!(matches!(
      get_property(&f, &PropertyKey::String("value".to_string().into()), 0)
        .expect("test should not fail"),
      Value::Undefined
    ));
    assert!(matches!(
      get_property(
        &g_result,
        &PropertyKey::String("done".to_string().into()),
        0
      )
      .expect("test should not fail"),
      Value::Bool(true)
    ));
  }

  #[test]
  fn yield_star_delegates_through_the_iterator_protocol() {
    let src = crate::js!(
      r#"
            var nextCount = 0;
            var initialArgumentCount = -1;
            var initialArgument = 1;
            var resumedArgument = 0;
            var delegatedResult = { value: 4 };
            var iterable = {};
            iterable[Symbol.iterator] = function() {
              return {
                next: function(value) {
                  nextCount += 1;
                  if (nextCount === 1) {
                    initialArgumentCount = arguments.length;
                    initialArgument = value;
                    return delegatedResult;
                  }
                  resumedArgument = value;
                  return { done: true, value: 9 };
                }
              };
            };
            function* delegate() { return yield* iterable; }
            var iterator = delegate();
            var first = iterator.next(123);
            var second = iterator.next(42);
            var preservedResult = first === delegatedResult;
            var completionValue = second.value;
            var observedInitialArgumentCount = initialArgumentCount;
            var observedInitialArgument = initialArgument;
            var observedResumedArgument = resumedArgument;

            function* stringDelegate() { yield* 'ab'; }
            var stringIterator = stringDelegate();
            var firstCharacter = stringIterator.next().value;
            var secondCharacter = stringIterator.next().value;

            var sparse = [, 2];
            function* arrayDelegate() { yield* sparse; }
            var arrayIterator = arrayDelegate();
            var holeResult = arrayIterator.next();
            sparse[1] = 3;
            var liveArrayValue = arrayIterator.next().value;
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    let initial_argument_count = vm.get_global("observedInitialArgumentCount");
    assert!(
      matches!(initial_argument_count, Some(Value::Number(1.0))),
      "unexpected initial argument count: {initial_argument_count:?}"
    );
    assert!(matches!(
      vm.get_global("observedInitialArgument"),
      Some(Value::Undefined)
    ));
    assert!(matches!(
      vm.get_global("observedResumedArgument"),
      Some(Value::Number(42.0))
    ));
    assert!(matches!(
      vm.get_global("preservedResult"),
      Some(Value::Bool(true))
    ));
    assert!(matches!(
      vm.get_global("completionValue"),
      Some(Value::Number(9.0))
    ));
    assert!(matches!(vm.get_global("firstCharacter"), Some(Value::String(value)) if value == "a"));
    assert!(matches!(vm.get_global("secondCharacter"), Some(Value::String(value)) if value == "b"));
    assert!(matches!(
      vm.get_global("holeResult"),
      Some(result)
        if matches!(
          get_property(&result, &PropertyKey::String("value".to_string().into()), 0),
          Ok(Value::Undefined)
        )
    ));
    assert!(matches!(
      vm.get_global("liveArrayValue"),
      Some(Value::Number(3.0))
    ));
  }

  #[test]
  fn generator_abrupt_completions_run_catch_and_finally() {
    let (vm, _globals) = run_script(crate::js!(
      r#"
        var thrown = {};
        var caught;
        var badIterable = {};
        badIterable[Symbol.iterator] = function() {
          return { next: function() { throw thrown; } };
        };
        function* catchesDelegatedError() {
          try { yield* badIterable; } catch (error) { caught = error; }
        }
        var caughtResult = catchesDelegatedError().next();

        var delegatedFinalized = false;
        var returnReceived;
        var returnIterable = {};
        returnIterable[Symbol.iterator] = function() {
          return {
            next: function() { return { done: false }; },
            return: function(value) {
              returnReceived = value;
              return { done: true, value: 333 };
            }
          };
        };
        function* delegatesReturn() {
          try { yield* returnIterable; } finally { delegatedFinalized = true; }
        }
        var delegatedIterator = delegatesReturn();
        delegatedIterator.next();
        var delegatedReturnResult = delegatedIterator.return(222);

        var directFinalized = false;
        function* directReturn() {
          try { yield 1; } finally { directFinalized = true; }
        }
        var directIterator = directReturn();
        directIterator.next();
        var directReturnResult = directIterator.return(7);

        var innerFinalized = false;
        var outerFinalized = false;
        function* nestedReturn() {
          try {
            try { yield 1; } finally { innerFinalized = true; }
          } finally {
            outerFinalized = true;
          }
        }
        var nestedIterator = nestedReturn();
        nestedIterator.next();
        var nestedReturnResult = nestedIterator.return(8);

        var overrideOuterFinalized = false;
        function* overrideReturn() {
          try {
            try { yield 1; } finally { return 9; }
          } finally {
            overrideOuterFinalized = true;
          }
        }
        var overrideIterator = overrideReturn();
        overrideIterator.next();
        var overrideReturnResult = overrideIterator.return(8);

        globalThis.generator_abrupt_completion_ok =
          caught === thrown
          && caughtResult.done === true
          && caughtResult.value === undefined
          && delegatedFinalized
          && returnReceived === 222
          && delegatedReturnResult.done === true
          && delegatedReturnResult.value === 333
          && directFinalized
          && directReturnResult.done === true
          && directReturnResult.value === 7
          && innerFinalized
          && outerFinalized
          && nestedReturnResult.done === true
          && nestedReturnResult.value === 8
          && overrideOuterFinalized
          && overrideReturnResult.done === true
          && overrideReturnResult.value === 9;
      "#
    ));

    assert_eq!(vm.get_global("innerFinalized"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("outerFinalized"), Some(Value::Bool(true)));
    assert_eq!(
      vm.get_global("overrideOuterFinalized"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("generator_abrupt_completion_ok"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn class_declaration_binds_constructor_function() {
    let src = crate::js!(
      r#"
            class A {}
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("A"), Some(Value::Closure { .. })));
  }

  #[test]
  fn class_fields_initialize_instances_and_static_properties_in_source_order() {
    let src = crate::js!(
      r#"
      let keyCalls = 0;
      const key = () => { keyCalls += 1; return "computed"; };
      let setterCalls = 0;
      class Base {
        inherited = 1;
        set shadowed(value) { setterCalls += value; }
      }
      class Fields extends Base {
        first = 2;
        shadowed = 9;
        #secret = 5;
        [key()] = this.first + 1;
        missing;
        static answer = 42;
        readSecret() { return this.#secret; }
        constructor(useFirstBranch) {
          if (useFirstBranch) {
            super();
          } else {
            super();
          }
          this.after = this.computed + 1;
        }
      }
      const one = new Fields(true);
      const two = new Fields(false);
      globalThis.fieldsOk = one.inherited === 1
        && one.first === 2
        && one.shadowed === 9
        && setterCalls === 0
        && one.readSecret() === 5
        && one.computed === 3
        && one.after === 4
        && one.missing === undefined
        && two.computed === 3
        && Fields.answer === 42
        && keyCalls === 1;
    "#
    );
    let session = Parser::parse_source(src).expect("parse class fields");
    let chunk =
      compiler::compile(session.program(), session.source()).expect("compile class fields");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run class fields");
    assert_eq!(vm.get_global("fieldsOk"), Some(Value::Bool(true)));
  }

  #[test]
  fn object_spread_and_set_delete_support_effect_runtime_patterns() {
    let src = crate::js!(
      r#"
      const marker = Symbol.for("spread-marker");
      const methods = {
        get: () => 1,
        update: () => 2,
        [marker]: 3
      };
      const hook = {
        pipe() { return 4; },
        ...methods
      };
      const roots = new Set([hook]);
      const deleted = roots.delete(hook);
      globalThis.runtimePatternsOk = hook.get() === 1
        && hook.update() === 2
        && hook[marker] === 3
        && hook.pipe() === 4
        && deleted
        && roots.size === 0
        && !roots.has(hook);
    "#
    );
    let session = Parser::parse_source(src).expect("parse runtime patterns");
    let chunk =
      compiler::compile(session.program(), session.source()).expect("compile runtime patterns");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run runtime patterns");
    assert_eq!(vm.get_global("runtimePatternsOk"), Some(Value::Bool(true)));
  }

  #[test]
  fn derived_constructor_this_survives_nested_calls_after_super() {
    let src = crate::js!(
      r#"
      const helper = () => 1;
      class Base {}
      class Middle extends Base {}
      class Derived extends Middle {
        field = helper();
        constructor() {
          super();
          helper();
        }
      }
      globalThis.derivedThisOk = new Derived().field === 1;
    "#
    );
    let session = Parser::parse_source(src).expect("parse derived constructor");
    let chunk =
      compiler::compile(session.program(), session.source()).expect("compile derived constructor");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run derived constructor");
    assert_eq!(vm.get_global("derivedThisOk"), Some(Value::Bool(true)));
  }

  #[test]
  fn super_property_access_uses_home_object_and_lexical_this() {
    let src = crate::js!(
      r#"
            class C {
                methodThis() { return this; }
                method() { return super.toString(); }
                arrow() { return (() => super.toString())(); }
                arrowThis() { return (() => this)() === this; }
                assign(v) { super.value = v; return this.value; }
            }
            var c = new C();
            var method_this = c.methodThis();
            var method_result = c.method();
            var arrow_result = c.arrow();
            var arrow_this_matches = c.arrowThis();
            var assign_result = c.assign(9);
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    let method_result = vm.get_global("method_result");
    let method_this = vm.get_global("method_this");
    let arrow_result = vm.get_global("arrow_result");
    let arrow_this_matches = vm.get_global("arrow_this_matches");
    let assign_result = vm.get_global("assign_result");
    assert!(
      matches!(method_this, Some(Value::Object(_))),
      "unexpected method_this: {:?}",
      method_this
    );
    assert!(
      matches!(method_result, Some(Value::String(ref s)) if s == "[object Object]"),
      "unexpected method_result: {:?}",
      method_result
    );
    assert!(
      matches!(arrow_result, Some(Value::String(ref s)) if s == "[object Object]"),
      "unexpected arrow_result: {:?}",
      arrow_result
    );
    assert!(
      matches!(arrow_this_matches, Some(Value::Bool(true))),
      "unexpected arrow_this_matches: {:?}",
      arrow_this_matches
    );
    assert!(
      matches!(assign_result, Some(Value::Number(n)) if (n - 9.0).abs() < f64::EPSILON),
      "unexpected assign_result: {:?}",
      assign_result
    );
  }

  #[test]
  fn risc_super_construct_initializes_derived_this() {
    let source = crate::js!(
      r#"
      class Base { constructor(value) { this.value = value; } }
      class Derived extends Base { constructor(value) { super(value + 1); } }
      var derived_value = new Derived(4).value;
    "#
    );
    let session = Parser::parse_source(source).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(
      vm.get_global("derived_value"),
      Some(Value::Number(value)) if (value - 5.0).abs() < f64::EPSILON
    ));
  }

  #[test]
  fn microtasks_run_before_dynamic_import_jobs() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let order = Rc::new(RefCell::new(Vec::<&'static str>::new()));

    vm.queue_microtask({
      let order = Clone::clone(&order);
      move |_vm| {
        order.borrow_mut().push("microtask");
        Ok(())
      }
    });
    vm.queue_dynamic_import_job({
      let order = Clone::clone(&order);
      move |_vm| {
        order.borrow_mut().push("dynamic-import");
        Ok(())
      }
    });

    vm.flush_microtasks().expect("flush ok");
    assert_eq!(&*order.borrow(), &["microtask", "dynamic-import"]);
  }

  #[test]
  fn array_reduce_without_initial_value_throws_on_empty_array() {
    let src = crate::js!(r#"[].reduce(function(acc, value) { return acc + value; });"#);
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm.run(chunk).expect_err("reduce should throw");
    assert!(
      err
        .to_string()
        .contains("Reduce of empty array with no initial value")
    );
  }

  #[test]
  fn object_keys_orders_indices_before_strings() {
    let src = crate::js!(
      r#"
            var o = {};
            o.b = 1;
            o[2] = 2;
            o.a = 3;
            o[1] = 4;
            var keys = Object.keys(o);
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    match vm.get_global("keys") {
      Some(Value::Array(arr, _)) => {
        let values: Vec<String> = arr
          .borrow()
          .iter()
          .filter_map(|v| match v {
            Some(Value::String(s)) => Some(s.to_string_lossy()),
            _ => None,
          })
          .collect();
        assert_eq!(values, vec!["1", "2", "b", "a"]);
      }
      other => panic!("expected keys array, got {:?}", other),
    }
  }

  #[test]
  fn for_in_keys_walks_prototype_chain_without_duplicates() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    let mut proto = ObjectData::new();
    proto.properties.insert(
      PropertyKey::String("b".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(2.0)),
    );
    proto.properties.insert(
      PropertyKey::String("1".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(1.0)),
    );
    let proto = Rc::new(RefCell::new(proto));

    let mut obj = ObjectData::with_prototype(Value::Object(proto));
    obj.properties.insert(
      PropertyKey::String("a".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(3.0)),
    );
    obj.properties.insert(
      PropertyKey::String("1".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(4.0)),
    );
    let obj = Value::Object(Rc::new(RefCell::new(obj)));

    let keys = vm.for_in_keys_of_value(&obj).expect("keys ok");
    assert_eq!(keys, vec!["1", "a", "b"]);
  }

  #[test]
  fn global_this_matches_ecmascript_public_surface() {
    let globals = test_globals();
    let vm = Vm::new(Clone::clone(&globals));

    let mut actual = vm
      .list_own_keys_of_value(&Value::Object(globals))
      .expect("globalThis keys");
    actual.sort();

    let mut expected = vec![
      "Infinity",
      "NaN",
      "undefined",
      "eval",
      "isFinite",
      "isNaN",
      "parseFloat",
      "parseInt",
      "encodeURI",
      "encodeURIComponent",
      "decodeURI",
      "decodeURIComponent",
      "Object",
      "Function",
      "Boolean",
      "Symbol",
      "Error",
      "AggregateError",
      "EvalError",
      "RangeError",
      "ReferenceError",
      "SyntaxError",
      "TypeError",
      "URIError",
      "Number",
      "BigInt",
      "Math",
      "Date",
      "String",
      "RegExp",
      "Array",
      "Int8Array",
      "Uint8Array",
      "Uint8ClampedArray",
      "Int16Array",
      "Uint16Array",
      "Int32Array",
      "Uint32Array",
      "Float32Array",
      "Float64Array",
      "BigInt64Array",
      "BigUint64Array",
      "Map",
      "Set",
      "WeakMap",
      "WeakSet",
      "WeakRef",
      "FinalizationRegistry",
      "ShadowRealm",
      "Temporal",
      "ArrayBuffer",
      "SharedArrayBuffer",
      "DataView",
      "Atomics",
      "Promise",
      "Reflect",
      "Proxy",
      "JSON",
      "globalThis",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(actual, expected);
  }

  #[test]
  fn json_stringify_preserves_descriptor_fields() {
    let src = crate::js!(
      r#"
            var desc = Object.getOwnPropertyDescriptor(globalThis, "globalThis");
            Object.defineProperty(globalThis, "keys", {
                value: Object.keys(desc).join(","),
                writable: true,
                enumerable: false,
                configurable: true,
            });
            Object.defineProperty(globalThis, "enumerableValue", {
                value: String(desc.enumerable),
                writable: true,
                enumerable: false,
                configurable: true,
            });
            Object.defineProperty(globalThis, "text", {
                value: JSON.stringify(Object.defineProperties({}, {
                    configurable: { value: true, writable: true, enumerable: true },
                    enumerable: { value: false, writable: true, enumerable: true },
                    writable: { value: true, writable: true, enumerable: true },
                    value: { value: {}, writable: true, enumerable: true },
                })),
                writable: true,
                enumerable: false,
                configurable: true,
            });
        "#
    );
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    match vm.get_global("keys") {
      Some(Value::String(s)) => {
        assert_eq!(s, "value,writable,enumerable,configurable");
      }
      other => panic!("expected descriptor keys string, got {:?}", other),
    }
    match vm.get_global("enumerableValue") {
      Some(Value::String(s)) => {
        assert_eq!(s, "false");
      }
      other => panic!("expected descriptor enumerable value, got {:?}", other),
    }
    match vm.get_global("text") {
      Some(Value::String(s)) => {
        assert_eq!(
          s,
          r#"{"configurable":true,"enumerable":false,"writable":true,"value":{}}"#
        );
      }
      other => panic!("expected serialized descriptor string, got {:?}", other),
    }
  }

  #[test]
  fn vm_new_populates_intrinsics_table() {
    let globals = test_globals();
    let vm = Vm::new(globals);
    let intrinsics = &vm.realm().intrinsics;
    assert!(intrinsics.object_constructor.is_some());
    assert!(intrinsics.throw_type_error.is_some());
    assert!(intrinsics.array_iterator_prototype.is_some());
    assert!(!intrinsics.typed_array_constructors.is_empty());
  }

  #[test]
  fn promise_all_streams_values_and_closes_after_resolve_throws() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let returnCount = 0;
        const iterable = {
          [Symbol.iterator]() {
            return {
              next() { return { value: null, done: false }; },
              return() { returnCount += 1; }
            };
          }
        };
        Promise.resolve = function() { throw "resolve failed"; };
        Promise.all(iterable);
        globalThis.result = returnCount;
      "#
    ));

    assert_eq!(vm.get_global("result"), Some(Value::Number(1.0)));
  }

  #[test]
  fn promise_all_preserves_original_throw_when_iterator_return_is_not_callable() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let rejected;
        function BadPromise(executor) {
          executor(function() {}, function(reason) { rejected = reason; });
        }
        BadPromise.resolve = function() { throw "resolve failed"; };
        const iterable = {
          [Symbol.iterator]() {
            return {
              next() { return { done: false }; },
              return: 0
            };
          }
        };
        Promise.all.call(BadPromise, iterable);
        globalThis.result = rejected;
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("resolve failed".to_string().into()))
    );
  }

  #[test]
  fn for_await_closes_sync_iterator_when_yielded_promise_rejects() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let returnCount = 0;
        let caught = false;
        const syncIterable = {
          [Symbol.iterator]() {
            return {
              next() {
                return { value: Promise.reject("rejected"), done: false };
              },
              return() {
                returnCount += 1;
              }
            };
          }
        };
        async function run() {
          try {
            for await (const value of syncIterable) {}
          } catch (error) {
            caught = error === "rejected";
          }
        }
        run();
        globalThis.result = returnCount + "," + caught;
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("1,true".to_string().into()))
    );
  }

  #[test]
  fn for_await_uses_async_iterator_and_awaits_close_on_break() {
    let (vm, _) = run_script(crate::js!(
      r#"
        let nextCount = 0;
        let returnCount = 0;
        let seen;
        const asyncIterable = {
          [Symbol.asyncIterator]() {
            return {
              next() {
                nextCount += 1;
                return Promise.resolve({ value: 42, done: false });
              },
              return() {
                returnCount += 1;
                return Promise.resolve({ value: undefined, done: true });
              }
            };
          }
        };
        async function run() {
          for await (const value of asyncIterable) {
            seen = value;
            break;
          }
        }
        run();
        globalThis.result = nextCount + "," + returnCount + "," + seen;
      "#
    ));

    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("1,1,42".to_string().into()))
    );
  }
}
