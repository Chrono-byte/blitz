// SPDX-License-Identifier: MPL-2.0
use super::*;

fn evaluate(
  source: &str,
  expected: &str,
) {
  let session = crate::parser::Parser::parse_source(source).expect("parse");
  let code = crate::compiler::compile_repl_session(&session).expect("compile");
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  vm.run(code).expect("run");
  vm.flush_microtasks().expect("flush");
  let result = vm.get_global("result").expect("result");
  assert!(
    matches!(&result, Value::String(s) if s.as_str_lossy() == expected),
    "{result:?}"
  );
}

#[test]
fn async_delegation_awaits_native_results_but_preserves_yielded_promise_identity() {
  evaluate(
    crate::js!(
      r#"
    var result = '';
    var log = [];
    var count = 0;
    var yielded = Promise.resolve(4);
    var source = {
      [Symbol.asyncIterator]() { return this; },
      next(value) {
        log.push('next:' + value + ':' + arguments.length);
        count++;
        return Promise.resolve({ value: count === 1 ? yielded : Promise.resolve(9), done: count > 1 });
      }
    };
    async function* g() { return yield* source; }
    var iterator = g();
    iterator.next(123).then(a => {
      log.push((a.value === yielded) + ':' + a.done);
      return iterator.next(7);
    }).then(b => { log.push(b.value + ':' + b.done); result = log.join(','); });
  "#
    ),
    "next:undefined:1,true:false,next:7:1,9:true",
  );
}

#[test]
fn async_delegation_unwraps_sync_values_and_closes_on_rejection() {
  evaluate(
    crate::js!(
      r#"
    var result = '';
    var log = [];
    var source = {
      [Symbol.iterator]() { return this; },
      next() { return {value: Promise.reject('bad'), done: false}; },
      return() { log.push('closed'); return {}; }
    };
    async function* g() { try { yield* source; } catch (e) { yield e; } }
    g().next().then(a => { log.push(a.value + ':' + a.done); result = log.join(','); });
  "#
    ),
    "closed,bad:false",
  );
}

#[test]
fn async_delegation_forwards_throw_and_return_through_finally() {
  evaluate(
    crate::js!(
      r#"
    var result = '';
    var log = [];
    var source = {
      [Symbol.asyncIterator]() { return this; },
      next() { return Promise.resolve({ value: 1, done: false }); },
      throw(e) { log.push('throw:' + e); return Promise.resolve({ value: 2, done: false }); },
      return(v) { log.push('return:' + v); return Promise.resolve({ value: 8, done: true }); }
    };
    async function* g() { try { yield* source; } finally { yield 3; } }
    var i = g();
    i.next().then(a => { log.push(a.value); return i.throw('x'); })
      .then(a => { log.push(a.value); return i.return(Promise.resolve(7)); })
      .then(a => { log.push(a.value + ':' + a.done); return i.next(); })
      .then(a => { log.push(a.value + ':' + a.done); result = log.join(','); });
  "#
    ),
    "1,throw:x,2,return:7,3:false,8:true",
  );
}

#[test]
fn async_delegation_awaits_missing_throw_cleanup_before_rejecting() {
  evaluate(
    crate::js!(
      r#"
    var result = '';
    var log = [];
    var source = {
      [Symbol.asyncIterator]() { return this; },
      next() { return Promise.resolve({ value: 1, done: false }); },
      return() { log.push('close:' + arguments.length); return Promise.resolve().then(() => { log.push('closed'); return {}; }); }
    };
    async function* g() { yield* source; }
    var i = g();
    i.next().then(() => i.throw('x')).then(() => { result = 'wrong'; }, e => {
      log.push(e instanceof TypeError); result = log.join(',');
    });
  "#
    ),
    "close:0,closed,true",
  );
}

#[test]
fn async_delegation_rejects_nonobject_results_and_keeps_original_rejection() {
  for (body, expected) in [
    ("return Promise.resolve(1);", "true"),
    ("return Promise.reject('original');", "original"),
  ] {
    evaluate(
      &crate::js!(
        format(script),
        r#"
      var result = '';
      var source = {{ [Symbol.asyncIterator]() {{ return this; }}, next() {{ {body} }} }};
      async function* g() {{ yield* source; }}
      g().next().then(() => {{ result = 'wrong'; }}, e => {{ result = typeof e === 'string' ? e : String(e instanceof TypeError); }});
    "#
      ),
      expected,
    );
  }
}

#[test]
fn async_delegation_without_return_preserves_return_completion_through_finally() {
  for symbol in ["asyncIterator", "iterator"] {
    evaluate(
      &crate::js!(
        format(script),
        r#"
      var result = '';
      var source = {{ [Symbol.{symbol}]() {{ return this; }}, next() {{ return {{value: 1, done: false}}; }} }};
      async function* g() {{ try {{ yield* source; }} finally {{ yield 2; }} }}
      var i = g();
      i.next().then(() => i.return(Promise.resolve(9)))
        .then(a => {{ result = a.value + ':' + a.done; return i.next(); }})
        .then(a => {{ result += ',' + a.value + ':' + a.done; }});
    "#
      ),
      "2:false,9:true",
    );
  }
}

#[test]
fn async_delegation_sync_method_errors_reach_catch_after_a_job_turn() {
  evaluate(
    crate::js!(
      r#"
    var result = '';
    var log = [];
    var source = { [Symbol.iterator]() { return this; }, next() { throw 'bad'; } };
    async function* g() { try { yield* source; } catch (e) { log.push(e); } }
    g().next().then(() => { result = log.join(','); });
    log.push('caller');
  "#
    ),
    "caller,bad",
  );
}

#[test]
fn async_from_sync_awaits_adapter_and_loop_promises_in_separate_jobs() {
  evaluate(
    crate::js!(
      r#"
      var result = '';
      var events = [];
      Promise.resolve().then(() => events.push('t1'))
        .then(() => events.push('t2')).then(() => events.push('t3'))
        .then(() => events.push('t4')).then(() => { result = events.join(','); });
      Object.defineProperty(Promise.prototype, 'constructor', {
        get() { events.push('ctor'); return Promise; }, configurable: true
      });
      async function consume() {
        var value = Promise.resolve(1);
        events.push('start');
        for await (var item of [value]) { events.push('body'); }
        events.push('end');
      }
      consume();
    "#
    ),
    "start,ctor,ctor,t1,t2,body,ctor,t3,t4,end",
  );
}

#[test]
fn async_from_sync_constructor_errors_reject_before_loop_resumption() {
  evaluate(
    crate::js!(
      r#"
      var result = '';
      var events = [];
      var marker = {};
      Promise.resolve().then(() => events.push('t1'))
        .then(() => events.push('t2')).then(() => { result = events.join(','); });
      async function consume() {
        var value = Promise.resolve(1);
        Object.defineProperty(value, 'constructor', { get() { throw marker; } });
        events.push('start');
        for await (var item of [value]) { events.push('unreachable'); }
      }
      consume().catch(error => events.push(error === marker ? 'caught' : 'wrong'));
    "#
    ),
    "start,t1,t2,caught",
  );
}
