// SPDX-License-Identifier: MPL-2.0

use crate::Vm;
use crate::compiler;
use crate::parser::Parser;
use crate::runtime_utils;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use std::cell::RefCell;
use std::rc::Rc;

fn run(source: &str) {
  let session = Parser::parse_source(source).expect("ECMAScript correctness fixture must parse");
  let chunk = compiler::compile_session(&session).expect("fixture must compile");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  runtime_utils::install_minimal_kernel_constructors(&globals);
  let mut vm = Vm::new(globals);
  vm.run(chunk)
    .expect("ECMAScript correctness fixture failed");
}

fn rejects(source: &str) {
  let session = Parser::parse_source(source).expect("early-error fixture must parse into an AST");
  compiler::compile_session(&session).expect_err("fixture must be rejected during compilation");
}

const ASSERT: &str = crate::js!(
  r#"
  function assert(condition, label) {
    if (!condition) throw new Error(label);
  }
"#
);

#[test]
fn declaration_instantiation_and_early_errors() {
  run(&format!(
    r#"
      {ASSERT}
      assert(hoisted() === 7, "function declaration is hoisted");
      function hoisted() {{ return 7; }}

      let classTdz = false;
      try {{ typeof Later; }} catch (_) {{ classTdz = true; }}
      class Later {{}}
      assert(classTdz, "class starts in the TDZ");
      assert(globalThis.Later === undefined, "class is not a global property");

      let outer = 1;
      let loopTdz = false;
      try {{ for (let outer = outer; false;) {{}} }} catch (_) {{ loopTdz = true; }}
      assert(loopTdz, "for lexical initializer observes its own TDZ");

      class ReservedProperty {{
        \u0069mplements() {{ return 20; }}
        *yield() {{ yield 22; }}
      }}
      const reserved = new ReservedProperty();
      assert(reserved.implements() === 20 && reserved.yield().next().value === 22,
        "reserved words remain valid literal property names in strict class bodies");

      var yield = 23;
      const nonStrictArrow = (value = yield) => value;
      assert(nonStrictArrow() === 23,
        "yield remains an identifier in non-strict arrow parameters outside generators");
    "#
  ));

  for source in [
    crate::js!(r#"const f = () => { let duplicate; let duplicate; };"#),
    crate::js!(r#"try {} catch (error) { const error = 1; }"#),
    crate::js!(r#"function value() {} let value;"#),
    crate::js!(r#""use strict"; let value; delete value;"#),
    crate::js!(r#"function *outer() { (value = yield) => value; }"#),
  ] {
    rejects(source);
  }
}

#[test]
fn optional_chains_short_circuit_whole_chains_and_preserve_receivers() {
  run(&format!(
    r#"
      {ASSERT}
      let effects = 0;
      const key = () => {{ effects += 1; return "value"; }};
      const args = () => {{ effects += 10; return [1, 2]; }};
      let missing = null;
      assert(missing?.value.deep === undefined, "whole static chain short-circuits");
      assert(missing?.[key()].deep === undefined, "whole computed chain short-circuits");
      assert(missing?.method(...args()) === undefined, "optional call short-circuits");
      assert(effects === 0, "short-circuited keys and arguments are not evaluated");

      const receiver = {{ value: 4, method(...values) {{ return this.value + values[0]; }} }};
      assert(receiver?.method(...[3]) === 7, "optional method call preserves this");
    "#
  ));
}

#[test]
fn private_names_are_lexical_branded_and_hidden() {
  run(&format!(
    r#"
      {ASSERT}
      class First {{
        #value = 1;
        read(other) {{ return other.#value; }}
        makeReader() {{
          return class Reader {{ read(other) {{ return other.#value; }} }};
        }}
      }}
      class Second {{ #value = 2; }}
      const first = new First();
      let wrongBrand = false;
      try {{ first.read(new Second()); }} catch (error) {{ wrongBrand = error instanceof TypeError; }}
      assert(wrongBrand, "same spelling in another class has a different private name");
      assert(new (first.makeReader())().read(first) === 1, "nested class captures outer private name");
      assert(first["\u001fprivate#value"] === undefined, "private name is not a magic string");
      assert(Reflect.ownKeys(first).length === 0, "private state is absent from own keys");
    "#
  ));
}

#[test]
fn sloppy_duplicate_parameters_use_the_last_binding_and_mapping() {
  run(&format!(
    r#"
      {ASSERT}
      function duplicate(value, value) {{
        assert(value === 2, "last argument initializes duplicate binding");
        value = 3;
        assert(arguments[0] === 1, "earlier duplicate argument is unmapped");
        assert(arguments[1] === 3, "last duplicate argument remains mapped");
        arguments[0] = 4;
        assert(value === 3, "earlier argument does not alias binding");
        arguments[1] = 5;
        return value;
      }}
      assert(duplicate(1, 2) === 5, "last mapped argument updates binding");
    "#
  ));
}

#[test]
fn per_iteration_lexical_cells_are_distinct() {
  run(&format!(
    r#"
      {ASSERT}
      const classic = [];
      for (let index = 0; index < 3; index++) classic.push(() => index);
      assert(classic[0]() === 0 && classic[1]() === 1 && classic[2]() === 2,
        "classic for closures capture distinct cells");

      const values = [];
      for (const value of [1, 2, 3]) values.push(() => value);
      assert(values[0]() === 1 && values[1]() === 2 && values[2]() === 3,
        "for-of closures capture distinct cells");
    "#
  ));
}

#[test]
fn assignment_references_and_logical_assignment_evaluate_once() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      let keyCalls = 0;
      const target = {{ get value() {{ order += "g"; return 2; }},
                        set value(next) {{ order += "s" + next; }} }};
      const object = () => {{ order += "o"; return target; }};
      const key = () => {{ order += "k"; keyCalls += 1; return "value"; }};
      const rhs = () => {{ order += "r"; return 3; }};
      object()[key()] += rhs();
      assert(order === "okgrs5", "compound assignment preserves reference evaluation order");
      assert(keyCalls === 1, "computed key is evaluated once");

      let effects = 0;
      let present = 1;
      let absent;
      present ??= (++effects);
      absent ??= (++effects);
      assert(present === 1 && absent === 1 && effects === 1, "nullish assignment is lazy");
    "#
  ));
}

#[test]
fn classes_evaluate_keys_fields_and_super_in_spec_order() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      const key = () => {{ order += "k"; return "field"; }};
      class Base {{ method() {{ return 2; }} }}
      class Derived extends Base {{
        [key()] = (order += "i", 3);
        method() {{ return super.method() + this.field; }}
      }}
      assert(order === "k", "computed field key is evaluated at class definition");
      const value = new Derived();
      assert(order === "ki", "field initializer runs during construction");
      assert(value.method() === 5, "super method uses home object and lexical this");

      const objectBase = {{ method() {{ return this.amount; }} }};
      const objectDerived = {{
        amount: 9,
        method() {{ return (() => super.method())(); }},
        *generator() {{ yield super.method(); }}
      }};
      Object.setPrototypeOf(objectDerived, objectBase);
      assert(objectDerived.method() === 9 && objectDerived.generator().next().value === 9,
        "object methods and nested arrows retain their home object for super properties");

      class SpreadBase {{ constructor(first, second) {{ this.total = first + second; }} }}
      class SpreadDerived extends SpreadBase {{
        constructor(values) {{ super(...values); }}
      }}
      assert(new SpreadDerived([4, 6]).total === 10,
        "spread super calls construct the base and initialize derived this");

      let immutable = false;
      try {{ Derived = 1; }} catch (_) {{ immutable = true; }}
      assert(immutable, "class declaration binding is immutable");
    "#
  ));
}

#[test]
fn abrupt_completion_runs_finally_and_closes_iterators() {
  run(&format!(
    r#"
      {ASSERT}
      let finalizers = "";
      function completion() {{
        try {{
          try {{ return 1; }} finally {{ finalizers += "a"; }}
        }} finally {{ finalizers += "b"; }}
      }}
      assert(completion() === 1 && finalizers === "ab", "nested finally runs before return");

      let closed = 0;
      const iterable = {{
        [Symbol.iterator]() {{
          return {{
            next() {{ return {{ value: 1, done: false }}; }},
            return() {{ closed += 1; return {{ done: true }}; }}
          }};
        }}
      }};
      for (const value of iterable) {{ assert(value === 1, "iterator value"); break; }}
      assert(closed === 1, "break performs IteratorClose");
    "#
  ));
}

#[test]
fn destructuring_defaults_rest_and_computed_keys_follow_order() {
  run(&format!(
    r#"
      {ASSERT}
      let effects = "";
      const key = () => {{ effects += "k"; return "selected"; }};
      const fallback = () => {{ effects += "d"; return 4; }};
      const source = {{ selected: undefined, retained: 3 }};
      const {{ [key()]: selected = fallback(), ...rest }} = source;
      assert(selected === 4, "default applies only to undefined");
      assert(rest.retained === 3 && !("selected" in rest), "rest excludes computed key");
      assert(effects === "kd", "computed key precedes default");

      let untouched = 1;
      ({{ missing: untouched = 2 }} = {{ missing: null }});
      assert(untouched === null, "destructuring default does not apply to null");
    "#
  ));
}

#[test]
fn parameter_environments_precede_function_body_instantiation() {
  run(&format!(
    r#"
      {ASSERT}
      let bodyName = 9;
      function defaults(value = bodyName) {{
        var bodyName = 1;
        return value;
      }}
      assert(defaults() === 9, "body var is not visible to parameter initializer");

      let laterTdz = false;
      try {{
        (function (first = second, second = 2) {{ return first; }})();
      }} catch (error) {{ laterTdz = error instanceof ReferenceError; }}
      assert(laterTdz, "later parameter starts in TDZ");

      let calls = 0;
      function once(value = (++calls)) {{ return value; }}
      assert(once(4) === 4 && calls === 0, "default is skipped for present argument");
      assert(once(undefined) === 1 && calls === 1, "default runs for undefined");

      function rest(first, ...remaining) {{ return [first, remaining.length, remaining[1]]; }}
      const result = rest(1, 2, 3);
      assert(result[0] === 1 && result[1] === 2 && result[2] === 3,
        "rest parameters collect remaining arguments");
      assert(rest.length === 1, "rest parameter is excluded from function length");
      assert((function (a, b = 1, c) {{}}).length === 1,
        "function length stops before first default parameter");
      assert(((a, b = 1, c) => 0).length === 1,
        "arrow length stops before first default parameter");
    "#
  ));
}

#[test]
fn calls_construction_and_arrows_preserve_this_and_new_target() {
  run(&format!(
    r#"
      {ASSERT}
      function strictThis() {{ "use strict"; return this; }}
      function sloppyThis() {{ return this; }}
      assert(strictThis() === undefined, "strict call preserves undefined this");
      assert(sloppyThis() === globalThis, "sloppy call substitutes globalThis");

      const holder = {{
        value: 6,
        method() {{
          const arrow = () => this.value;
          return arrow();
        }}
      }};
      assert(holder.method() === 6, "arrow captures lexical this");

      function Constructor(value) {{
        this.value = value;
        this.target = new.target;
      }}
      const instance = new Constructor(7);
      assert(instance.value === 7, "constructor receives arguments");
      assert(instance.target === Constructor, "new.target is the active constructor");
      assert(instance instanceof Constructor, "constructed object uses prototype");
    "#
  ));
}

#[test]
fn property_descriptors_accessors_and_key_order_follow_ecmascript() {
  run(&format!(
    r#"
      {ASSERT}
      const symbol = Symbol("key");
      const object = {{ b: 1, 2: 2, a: 3, 1: 4, [symbol]: 5 }};
      const keys = Reflect.ownKeys(object);
      assert(keys[0] === "1" && keys[1] === "2" && keys[2] === "b" && keys[3] === "a",
        "integer keys precede strings while strings retain insertion order");
      assert(keys[4] === symbol, "symbols follow string keys");

      const receiver = {{ stored: 1 }};
      const prototype = {{
        get value() {{ return this.stored; }},
        set value(next) {{ this.stored = next; }}
      }};
      Object.setPrototypeOf(receiver, prototype);
      assert(receiver.value === 1, "inherited getter receives original receiver");
      receiver.value = 8;
      assert(receiver.stored === 8, "inherited setter receives original receiver");

      Object.defineProperty(receiver, "fixed", {{ value: 1, configurable: false }});
      assert(delete receiver.fixed === false, "sloppy delete reports non-configurable property");
      let strictDelete = false;
      try {{ (function () {{ "use strict"; delete receiver.fixed; }})(); }}
      catch (error) {{ strictDelete = error instanceof TypeError; }}
      assert(strictDelete, "strict delete throws for non-configurable property");

      const protoTarget = {{ marker: 11 }};
      const protoReceiver = {{}};
      protoReceiver.__proto__ = protoTarget;
      assert(protoReceiver.marker === 11 && protoReceiver.__proto__ === protoTarget,
        "legacy __proto__ accessor delegates to prototype internal methods");
    "#
  ));
}

#[test]
fn sparse_arrays_preserve_holes_and_spread_materializes_undefined() {
  run(&format!(
    r#"
      {ASSERT}
      const sparse = [, 2, , 4];
      assert(sparse.length === 4, "elisions contribute to array length");
      assert(!(0 in sparse) && !(2 in sparse), "elisions create holes");

      let visits = "";
      const mapped = sparse.map((value, index) => {{ visits += index; return value * 2; }});
      assert(visits === "13", "map skips holes");
      assert(!(0 in mapped) && mapped[1] === 4 && mapped[3] === 8,
        "map preserves holes");

      const spread = [...sparse];
      assert(0 in spread && spread[0] === undefined, "spread materializes holes as undefined");
      assert(spread.length === 4, "spread preserves length through iteration");
    "#
  ));
}

#[test]
fn switch_labels_and_loop_control_preserve_evaluation_order() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      switch (2) {{
        case 1: order += "a"; break;
        default: order += "d";
        case 2: order += "b";
        case 3: order += "c"; break;
      }}
      assert(order === "bc", "matching case starts fallthrough without running default");

      let values = "";
      outer: for (let row = 0; row < 3; row++) {{
        for (let column = 0; column < 3; column++) {{
          if (column === 1) continue outer;
          values += row;
        }}
      }}
      assert(values === "012", "labeled continue targets outer loop update");

      let finalized = 0;
      labeled: while (true) {{
        try {{ break labeled; }} finally {{ finalized += 1; }}
      }}
      assert(finalized === 1, "labeled break crosses finally exactly once");
    "#
  ));
}

#[test]
fn named_function_expressions_keep_private_self_bindings() {
  run(&format!(
    r#"
      {ASSERT}
      const factorial = function recurse(value) {{
        if (value <= 1) return 1;
        return value * recurse(value - 1);
      }};
      assert(factorial(5) === 120, "named function expression can recurse");
      assert(typeof recurse === "undefined", "function expression name does not escape");

      const original = function self() {{
        self = 1;
        return self;
      }};
      assert(original() === original, "sloppy writes to function self binding are ignored");
    "#
  ));
}

#[test]
fn finally_completion_rules_preserve_or_override_abrupt_completions() {
  run(&format!(
    r#"
      {ASSERT}
      function preserve() {{ try {{ return 1; }} finally {{ 2; }} }}
      function override() {{ try {{ return 1; }} finally {{ return 2; }} }}
      function throwOverride() {{ try {{ throw 1; }} finally {{ throw 2; }} }}
      assert(preserve() === 1, "normal finally preserves pending return");
      assert(override() === 2, "return in finally overrides pending return");
      let thrown;
      try {{ throwOverride(); }} catch (error) {{ thrown = error; }}
      assert(thrown === 2, "throw in finally overrides pending throw");
    "#
  ));
}

#[test]
fn primitive_coercion_uses_to_primitive_once_with_the_correct_hint() {
  run(&format!(
    r#"
      {ASSERT}
      let hints = "";
      const value = {{
        [Symbol.toPrimitive](hint) {{ hints += hint[0]; return hint === "string" ? "key" : 4; }}
      }};
      assert(value + 1 === 5, "addition applies default-hint ToPrimitive");
      const object = {{ [value]: 8 }};
      assert(object.key === 8, "computed property applies string-hint ToPrimitive");
      assert(hints === "ds", "each coercion runs once with the expected hint");

      assert(Object.is(-0, -0) && !Object.is(-0, 0), "SameValue distinguishes signed zero");
      assert(Object.is(NaN, NaN), "SameValue considers NaN equal to itself");
      assert(1n + 2n === 3n && 2n ** 3n === 8n, "BigInt arithmetic remains in BigInt domain");
    "#
  ));
}

#[test]
fn generator_parameter_initialization_is_synchronous_and_propagates_abrupt_completion() {
  run(&format!(
    r#"
      {ASSERT}
      const sentinel = {{}};
      const iterable = {{
        [Symbol.iterator]() {{ throw sentinel; }}
      }};
      function* binding([value]) {{ yield value; }}

      let thrown;
      try {{ binding(iterable); }} catch (error) {{ thrown = error; }}
      assert(thrown === sentinel,
        "generator call propagates parameter iterator failure synchronously");

      let typeError = false;
      try {{ binding([null]); }} catch (error) {{ typeError = error instanceof TypeError; }}
      assert(!typeError, "flat binding accepts null as its element value");

      function* nested([[value]]) {{ yield value; }}
      try {{ nested([null]); }} catch (error) {{ typeError = error instanceof TypeError; }}
      assert(typeError, "nested parameter binding fails during generator call");
    "#
  ));
}

#[test]
fn generator_return_runs_finally_and_finishes_the_iterator() {
  run(&format!(
    r#"
      {ASSERT}
      let finalized = 0;
      function* values() {{
        try {{
          yield 1;
          yield 2;
        }} finally {{
          finalized += 1;
        }}
      }}
      const iterator = values();
      const first = iterator.next();
      assert(first.value === 1 && first.done === false, "first next yields value");
      const returned = iterator.return(9);
      assert(returned.value === 9 && returned.done === true, "return completes generator");
      assert(finalized === 1, "generator return runs finally");
      assert(iterator.next().done === true, "completed generator remains completed");
    "#
  ));
}

#[test]
fn completed_generator_throw_uses_a_javascript_throw_completion() {
  run(&format!(
    r#"
      {ASSERT}
      function* values() {{}}
      const iterator = values();
      iterator.next();

      const sentinel = 42;
      let caught;
      try {{ iterator.throw(sentinel); }} catch (error) {{ caught = error; }}
      assert(caught === sentinel, "completed generator throw is catchable");
    "#
  ));
}

#[test]
fn await_suspends_until_a_later_microtask() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      async function appendAfterAwait() {{
        order += "a";
        await 0;
        order += "b";
      }}

      const promise = appendAfterAwait();
      order += "c";
      assert(order === "ac", "await suspends the async function");
      promise.then(() => {{
        assert(order === "acb", "await resumes from a promise reaction");
      }});
    "#
  ));
}

#[test]
fn await_resumes_after_later_fulfillment_and_rejection() {
  run(&format!(
    r#"
      {ASSERT}
      let resolvePending;
      const pending = new Promise((resolve) => {{ resolvePending = resolve; }});

      async function fulfillLater() {{
        const first = await pending;
        const second = await Promise.resolve(2);
        return first + second;
      }}

      async function catchRejection() {{
        try {{
          await Promise.reject(9);
        }} catch (error) {{
          return error + 1;
        }}
      }}

      let fulfillment;
      let rejection;
      fulfillLater().then((value) => {{ fulfillment = value; }});
      catchRejection().then((value) => {{ rejection = value; }});
      Promise.resolve().then(() => {{ resolvePending(5); }});
      Promise.resolve().then(() => {{
        Promise.resolve().then(() => {{
          Promise.resolve().then(() => {{
            assert(fulfillment === 7, "later fulfillment resumes through both awaits");
            assert(rejection === 10, "await rejection resumes through the active catch");
          }});
        }});
      }});
    "#
  ));
}

#[test]
fn lexical_shadowing_and_closure_capture_resolve_the_correct_environment() {
  run(&format!(
    r#"
      {ASSERT}
      let value = "outer";
      function make() {{
        let value = "function";
        return {{
          functionScope: () => value,
          blockScope: (() => {{ let value = "block"; return () => value; }})()
        }};
      }}
      const closures = make();
      assert(closures.functionScope() === "function", "closure captures function lexical binding");
      assert(closures.blockScope() === "block", "closure captures block lexical binding");
      assert(value === "outer", "shadowing does not mutate outer binding");

      function varClosure() {{
        const functions = [];
        for (var index = 0; index < 3; index++) functions.push(() => index);
        return functions;
      }}
      const functions = varClosure();
      assert(functions[0]() === 3 && functions[2]() === 3,
        "var loop closures share one function-scoped cell");
    "#
  ));
}

#[test]
fn static_fields_and_blocks_run_once_in_source_order() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      class StaticOrder {{
        static first = (order += "a", 1);
        static {{ order += this.first === 1 ? "b" : "x"; this.fromBlock = 2; }}
        static second = (order += "c", this.fromBlock + 1);
      }}
      assert(order === "abc", "static elements execute in source order");
      assert(StaticOrder.first === 1, "first static field is installed");
      assert(StaticOrder.fromBlock === 2, "static block this is the constructor");
      assert(StaticOrder.second === 3, "later static field observes static block writes");
      new StaticOrder();
      new StaticOrder();
      assert(order === "abc", "construction does not rerun static elements");
    "#
  ));
}

#[test]
fn template_literals_preserve_evaluation_order_and_string_coercion() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      const interpolation = (label, value) => {{ order += label; return value; }};
      const result = `a${{interpolation("x", 1)}}b${{interpolation("y", true)}}c${{interpolation("z", null)}}`;
      assert(result === "a1btruecnull", "template substitutions use string coercion");
      assert(order === "xyz", "template substitutions evaluate left to right");

      function tag(strings, value) {{
        return [strings[0], strings.raw[0], value, strings[1]];
      }}
      const tagged = tag`line\\n${{2}}end`;
      assert(tagged[0] === "line\\n" && tagged[1] === "line\\\\n",
        "tag receives cooked and raw template strings");
      assert(tagged[2] === 2 && tagged[3] === "end", "tag receives substitutions and tail");
    "#
  ));
}

#[test]
fn relational_and_equality_coercions_observe_spec_order() {
  run(&format!(
    r#"
      {ASSERT}
      let order = "";
      const left = {{ valueOf() {{ order += "l"; return 1; }} }};
      const right = {{ valueOf() {{ order += "r"; return 2; }} }};
      assert(left < right, "relational comparison uses primitive numeric values");
      assert(order === "lr", "relational operands coerce left to right");

      order = "";
      const numericString = {{ valueOf() {{ order += "v"; return "2"; }} }};
      assert(numericString == 2, "abstract equality coerces object primitive");
      assert(order === "v", "abstract equality invokes primitive conversion once");
      assert(!(numericString === 2), "strict equality does not coerce");
      assert(order === "v", "strict equality performs no additional coercion");
    "#
  ));
}

#[test]
fn bigint_and_comparison_opcodes_preserve_ecmascript_semantics() {
  run(&format!(
    r#"
      {ASSERT}
      assert((5n & 3n) === 1n, "BigInt bitwise and");
      assert((5n | 2n) === 7n, "BigInt bitwise or");
      assert((5n ^ 1n) === 4n, "BigInt bitwise xor");
      assert((3n << 2n) === 12n, "BigInt left shift");
      assert((12n >> 2n) === 3n, "BigInt right shift");

      let updated = 1n;
      const previous = updated++;
      assert(previous === 1n && updated === 2n, "BigInt postfix update");

      let order = "";
      const left = {{ valueOf() {{ order += "l"; return 1; }} }};
      const right = {{ valueOf() {{ order += "r"; return 2; }} }};
      assert(left <= right && order === "lr", "less-than-or-equal coerces left first");
      order = "";
      assert(!(left > right) && order === "lr", "greater-than coerces left first");
      assert(9007199254740993n > 9007199254740992,
        "mixed BigInt and Number comparison remains exact");

      const proxy = new Proxy({{ valueOf() {{ return 1; }} }}, {{}});
      assert(proxy == 1, "proxy participates in abstract equality coercion");
      assert(Promise.resolve() == "[object Promise]",
        "promise participates in abstract equality coercion");
    "#
  ));
}

#[test]
fn private_fields_require_receiver_owned_brands() {
  run(&format!(
    r#"
      {ASSERT}
      class Counter {{
        #value = 4;
        read(receiver) {{ return receiver.#value; }}
        write(receiver, value) {{ receiver.#value = value; }}
      }}
      const counter = new Counter();
      const inherited = Object.create(counter);
      let inheritedReadThrew = false;
      let inheritedWriteThrew = false;
      let unrelatedWriteThrew = false;
      try {{ counter.read(inherited); }} catch (error) {{ inheritedReadThrew = error instanceof TypeError; }}
      try {{ counter.write(inherited, 5); }} catch (error) {{ inheritedWriteThrew = error instanceof TypeError; }}
      try {{ counter.write({{}}, 5); }} catch (error) {{ unrelatedWriteThrew = error instanceof TypeError; }}
      assert(inheritedReadThrew && inheritedWriteThrew && unrelatedWriteThrew,
        "private access rejects missing own brands");
      counter.write(counter, 8);
      assert(counter.read(counter) === 8, "private assignment retains valid branded access");
    "#
  ));
}

#[test]
fn spread_array_literals_preserve_holes_and_array_prototype() {
  run(&format!(
    r#"
      {ASSERT}
      const leadingHole = [,...[]];
      const middleHole = [1,,...[],2];
      const spread = [...[]];
      assert(leadingHole.length === 1 && !(0 in leadingHole),
        "leading elision survives spread flattening");
      assert(middleHole.length === 3 && !(1 in middleHole),
        "middle elision survives spread flattening");
      assert(spread instanceof Array && Object.getPrototypeOf(spread) === Array.prototype,
        "spread literal uses the realm Array prototype");
    "#
  ));
}

#[test]
fn instanceof_honors_prototypes_and_symbol_has_instance() {
  run(&format!(
    r#"
      {ASSERT}
      function Constructor() {{}}
      const instance = new Constructor();
      assert(instance instanceof Constructor, "ordinary instanceof follows constructor prototype");
      Constructor.prototype = {{}};
      assert(!(instance instanceof Constructor), "prototype replacement affects instanceof");

      const matcher = {{ [Symbol.hasInstance](value) {{ return value && value.accepted === true; }} }};
      assert(({{ accepted: true }}) instanceof matcher, "custom @@hasInstance is called");
      assert(!(({{ accepted: false }}) instanceof matcher), "custom @@hasInstance result is booleanized");
    "#
  ));
}

#[test]
fn typed_array_construction_prefers_iterators_over_array_like_length() {
  run(&format!(
    r#"
      {ASSERT}
      let reads = 0;
      const source = {{
        length: 99,
        *[Symbol.iterator]() {{ reads += 1; yield 3; yield 5; }}
      }};
      const values = new Float64Array(source);
      assert(reads === 1, "typed array constructor consumes iterable once");
      assert(values.length === 2 && values[0] === 3 && values[1] === 5,
        "iterable takes precedence over array-like length");

      const sentinel = {{}};
      const abrupt = {{ [Symbol.iterator]() {{ throw sentinel; }} }};
      let thrown;
      try {{ new Float64Array(abrupt); }} catch (error) {{ thrown = error; }}
      assert(thrown === sentinel, "iterator acquisition failure propagates");

      const target = new Float64Array([1, 2]);
      assert(Reflect.defineProperty(target, "0", {{ value: 7 }}),
        "integer-indexed define succeeds for valid index");
      assert(target[0] === 7, "integer-indexed define writes backing storage");
      assert(!Reflect.defineProperty(target, "-0", {{ value: 9 }}),
        "negative zero is an invalid integer index");
      assert(target["-0"] === undefined && target["0.1"] === undefined,
        "invalid canonical numeric indices do not fall through to ordinary properties");
      const ownKeys = Reflect.ownKeys(target);
      assert(ownKeys.length === 2 && ownKeys[0] === "0" && ownKeys[1] === "1",
        "typed-array own keys expose indices without internal metadata");
      assert(Object.prototype.toString.call(target) === "[object Float64Array]",
        "typed-array toStringTag reports the element kind");

      const subarray = new Float64Array([10, 20, 30]).subarray(-0.6, undefined);
      assert(subarray.length === 3 && subarray[2] === 30,
        "subarray applies ToIntegerOrInfinity and treats undefined end as length");

      const writes = [];
      const setTarget = new Float64Array(3);
      setTarget.set({{
        length: 3,
        get 0() {{ writes.push(0); return 4; }},
        get 1() {{ writes.push(1); return 5; }},
        get 2() {{ writes.push(2); return 6; }}
      }});
      assert(writes.join("") === "012" && setTarget[1] === 5,
        "TypedArray set reads and writes array-like values in order");

      let conversions = 0;
      const fillValue = {{ valueOf() {{ conversions += 1; return 2; }} }};
      new Uint8ClampedArray(3).fill(fillValue);
      assert(conversions === 1, "TypedArray fill converts its value once");

      let invalidIteratorReceiver = false;
      try {{ Float64Array.prototype.values.call({{}}); }}
      catch (error) {{ invalidIteratorReceiver = error instanceof TypeError; }}
      assert(invalidIteratorReceiver, "TypedArray iterators validate their receiver");

      function Species(buffer, offset, length) {{
        return new Float64Array(buffer, offset, length);
      }}
      const backing = new ArrayBuffer(24);
      const nestedConstruct = new Species(backing, 8, 2);
      assert(nestedConstruct.length === 2 && Object.getPrototypeOf(nestedConstruct) === Float64Array.prototype,
        "nested construction uses the inner constructor as new.target");
    "#
  ));
}

#[test]
fn strings_use_utf16_indices_and_code_point_iteration() {
  run(&format!(
    r#"
      {ASSERT}
      const text = "A😀B";
      assert(text.length === 4, "string length counts UTF-16 code units");
      assert(text.charCodeAt(1) === 0xD83D && text.charCodeAt(2) === 0xDE00,
        "charCodeAt exposes surrogate code units");
      assert(text.codePointAt(1) === 0x1F600, "codePointAt combines surrogate pair");
      assert(String.fromCodePoint(0x1F600) === "😀", "fromCodePoint creates surrogate pair");

      const iterated = [...text];
      assert(iterated.length === 3 && iterated[1] === "😀",
        "string iterator advances by Unicode code point");
      assert("😀".slice(0, 1).length === 1, "slice indexes UTF-16 code units");
    "#
  ));
}
