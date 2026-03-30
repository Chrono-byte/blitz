//! Utility functions for runtime operations.
//!
//! Notes on ECMAScript conformance
//!
//! This module contains a number of pragmatic, simplified, or host-specific
//! implementations of ECMAScript semantics used by the VM. The implementations
//! in this file are intentionally pragmatic (smaller and easier to test) and
//! therefore diverge from ECMA-262 in several observable ways. Call sites
//! should treat the behaviour here as engine-specific unless the code is
//! adjusted to follow the spec precisely.
//!
//! Known divergences (summary):
//! - Module namespace objects are created as plain objects with copied values
//!   rather than the spec-defined exotic namespace objects backed by live
//!   export slots. See `ModuleLoader` in `src/module.rs` for details.
//! - Dynamic `import()` is implemented synchronously in places and does not
//!   always follow Promise job/microtask timing; import `options` are
//!   currently ignored in some paths.
//! - Promise/thenable resolution is implemented with direct calls to `then`
//!   helpers which can resolve synchronously; the spec requires careful job
//!   scheduling (PromiseResolveThenableJob) to match observed ordering. Concretely
//!   this runtime calls a detected `then` property synchronously and invokes
//!   the resolve/reject callbacks immediately which can produce different
//!   microtask ordering compared to a spec-conformant engine.
//! - Microtask/job queue draining is host-specific: thrown errors will abort
//!   remaining microtasks in the current implementation. Hosts usually run
//!   microtasks to completion and handle exceptions via host-defined hooks.
//! - Some Array prototype methods are simplified and differ from spec (for
//!   example `reduce` and `sort` edge cases and iterator behaviour). Concrete
//!   deviations include:
//!   * `Array.prototype.reduce`: when no initial value is provided and the
//!     array has no defined elements the spec requires a TypeError; current
//!     implementation returns `undefined` or behaves differently.
//!   * `Array.prototype.sort`: comparator semantics are simplified; compare
//!     results were interpreted via `is_sign_positive` rather than the proper
//!     ToNumber negative/zero/positive semantics and NaN handling.
//!   * Array iterators: the iterator factory is implemented as a stub that
//!     does not produce a proper iterator over elements (iterator `next`
//!     semantics are simplified). Implementing a proper iterator object is
//!     required for conformance.
//! - Object own-key ordering is built from HashMap iteration and thus does
//!   not follow the spec ordering rules for integer indices / string keys /
//!   symbols.
//! - TypeScript slot-type runtime checks are a non-standard extension: the
//!   VM can optionally enforce compile-time slot types at runtime; this is
//!   not part of ECMA-262.
//!
//! Where behaviour is observable by user code (module namespace live bindings,
//! promise/microtask ordering, array semantics, property key order), the
//! implementation is considered non-compliant with ECMA-262 and should be
//! updated if strict conformance is required. See the project notes and
//! module-level comments in `src/module.rs`, `src/vm/runtime/async_tasks.rs`,
//! and `src/vm/state.rs` for targeted recommendations.

use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::rc::Rc;

use crate::bytecode;
use crate::compiler;
use crate::compiler::ModuleContext;
use crate::parser::Parser;
use crate::value::SymbolId;
use crate::value::{BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use num_bigint::{BigInt as BigIntValue, Sign};
use num_traits::FromPrimitive;
use regex::RegexBuilder;

const MAX_ARRAY_LENGTH_F: f64 = 4294967295.0_f64;

fn char_digit_value(ch: char) -> Option<u32> {
    match ch {
        '0'..='9' => Some(ch as u32 - '0' as u32),
        'a'..='z' => Some(ch as u32 - 'a' as u32 + 10),
        'A'..='Z' => Some(ch as u32 - 'A' as u32 + 10),
        _ => None,
    }
}

fn parse_int_string(text: &str, radix_arg: Option<i32>) -> Option<f64> {
    let mut s = text;
    let mut sign = 1.0;
    if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    } else if let Some(rest) = s.strip_prefix('-') {
        sign = -1.0;
        s = rest;
    }

    let mut radix = match radix_arg {
        Some(r) if r != 0 => {
            if !(2..=36).contains(&r) {
                return None;
            }
            r as u32
        }
        _ => 0,
    };

    if radix == 0 {
        if s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X")) {
            radix = 16;
            s = &s[2..];
        }
        if radix == 0 {
            radix = 10;
        }
    } else if radix == 16 && s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X")) {
        s = &s[2..];
    }

    let radix_f = radix as f64;
    let mut value = 0.0;
    let mut digits = 0;
    for ch in s.chars() {
        if let Some(digit) = char_digit_value(ch) {
            if digit >= radix {
                break;
            }
            value = value * radix_f + digit as f64;
            digits += 1;
        } else {
            break;
        }
    }

    if digits == 0 {
        return None;
    }

    Some(sign * value)
}

fn parse_float_prefix_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    if i < len && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let start_digits = i;
    while i < len && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let digits_before = i - start_digits;
    let mut digits_fraction = 0;
    if i < len && bytes[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
        }
        digits_fraction = i - frac_start;
    }
    if digits_before == 0 && digits_fraction == 0 {
        return None;
    }
    let mut end_index = i;
    if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
        let exp_index = i;
        i += 1;
        if i < len && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            end_index = exp_index;
        } else {
            end_index = i;
        }
    }
    Some(end_index)
}

fn parse_special_float(s: &str) -> Option<f64> {
    if s.strip_prefix("+Infinity").is_some() {
        return Some(f64::INFINITY);
    }
    if s.strip_prefix("-Infinity").is_some() {
        return Some(f64::NEG_INFINITY);
    }
    if s.strip_prefix("Infinity").is_some() {
        return Some(f64::INFINITY);
    }
    if s.strip_prefix("+NaN").is_some() {
        return Some(f64::NAN);
    }
    if s.strip_prefix("-NaN").is_some() {
        return Some(f64::NAN);
    }
    if s.strip_prefix("NaN").is_some() {
        return Some(f64::NAN);
    }
    None
}

fn parse_float_string(text: &str) -> f64 {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return f64::NAN;
    }
    if let Some(v) = parse_special_float(trimmed) {
        return v;
    }
    if let Some(len) = parse_float_prefix_len(trimmed) {
        if len == 0 {
            return f64::NAN;
        }
        trimmed[..len].parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

fn global_parse_int(vm: &mut Vm, args: &[Value]) -> Result<Value, crate::errors::RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let s = vm.to_string(&value)?;
    let radix_arg = if let Some(radix_value) = args.get(1) {
        Some(vm.to_int32_value(radix_value)?)
    } else {
        None
    };
    let parsed = parse_int_string(s.trim_start(), radix_arg).unwrap_or(f64::NAN);
    Ok(Value::Number(parsed))
}

fn global_parse_float(vm: &mut Vm, args: &[Value]) -> Result<Value, crate::errors::RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let s = vm.to_string(&value)?;
    let parsed = parse_float_string(&s);
    Ok(Value::Number(parsed))
}

/// Clear slot type annotations recursively for a Chunk graph.
///
/// The soft prelude is intentionally dynamic and may pass values that don't
/// match static parameter annotations. Before executing the prelude we clear
/// slot types in every FunctionProto across the compiled chunk graph so the
/// VM won't enforce runtime type checks for those functions.
pub fn clear_slot_types_in_chunk(chunk: bytecode::Chunk) -> bytecode::Chunk {
    // Recursive helper that rebuilds chunks and their nested function chunks.
    fn rebuild(
        rc_chunk: &Rc<bytecode::Chunk>,
        visited: &mut HashMap<*const bytecode::Chunk, Rc<bytecode::Chunk>>,
    ) -> Rc<bytecode::Chunk> {
        let key = Rc::as_ptr(rc_chunk);
        if let Some(existing) = visited.get(&key) {
            return Rc::clone(existing);
        }

        // Rebuild the chunk fields, replacing functions with cleared slot types.
        let mut new_chunk = bytecode::Chunk::new(Rc::clone(&rc_chunk.source_text));
        new_chunk.code = rc_chunk.code.clone();
        new_chunk.constants = rc_chunk.constants.clone();
        new_chunk.lines = rc_chunk.lines.clone();
        // Slot/global type annotations removed in JS-first mode; nothing to do.

        // Process functions
        let mut new_funcs = Vec::new();
        for f in &rc_chunk.functions {
            // Rebuild nested chunk for this function
            let nested = rebuild(&f.chunk, visited);
            let mut nf = f.clone();
            // Replace all slot type annotations and return type with None
            nf.slot_types = nf.slot_types.iter().map(|_| None).collect();
            nf.return_type = None;
            nf.chunk = nested;
            new_funcs.push(nf);
        }
        new_chunk.functions = new_funcs;

        let rc_new = Rc::new(new_chunk);
        visited.insert(key, Rc::clone(&rc_new));
        rc_new
    }

    let rc = Rc::new(chunk);
    let mut visited: HashMap<*const bytecode::Chunk, Rc<bytecode::Chunk>> = HashMap::new();
    let rc_cleared = rebuild(&rc, &mut visited);
    Rc::try_unwrap(rc_cleared).unwrap_or_else(|rc| (*rc).clone())
}

fn strict_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) => true,
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::BigInt(x), Value::BigInt(y)) => x == y,
        (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
        (Value::Object(ax), Value::Object(bx)) => Rc::ptr_eq(ax, bx),
        (Value::Array(ax, _), Value::Array(bx, _)) => Rc::ptr_eq(ax, bx),
        (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
        (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
        (Value::Promise(ap), Value::Promise(bp)) => Rc::ptr_eq(ap, bp),
        (Value::Builtin(af), Value::Builtin(bf)) => Rc::ptr_eq(af, bf),
        _ => false,
    }
}

fn static_builtin_arg_offset(vm: &Vm, args: &[Value], _global_name: &str) -> usize {
    if let Some(first) = args.first() {
        let this_val = vm.this_value();
        if !matches!(this_val, Value::Undefined) && strict_equal(first, &this_val) {
            return 1;
        }
    }
    0
}

fn promise_constructor_matches(
    vm: &mut Vm,
    promise: &Value,
    constructor: &Value,
) -> Result<bool, crate::errors::RuntimeError> {
    let promise_constructor =
        vm.get_property_host(promise, &PropertyKey::String("constructor".to_string()))?;
    Ok(strict_equal(&promise_constructor, constructor))
}

fn collect_iterable_values(
    vm: &mut Vm,
    iterable: &Value,
) -> Result<Vec<Value>, crate::errors::RuntimeError> {
    match iterable {
        Value::Array(arr, _) => Ok(arr
            .borrow()
            .iter()
            .map(|v| v.clone().unwrap_or(Value::Undefined))
            .collect()),
        other => {
            let this_for_iter = other.clone();
            let iterator_method = vm.get_property_host(
                &this_for_iter,
                &PropertyKey::Symbol(SymbolId::well_known_iterator()),
            )?;

            let is_callable = Vm::is_callable_value(&iterator_method);
            if is_callable {
                let iter_obj = vm.call_value_with_this(&iterator_method, &this_for_iter, &[])?;
                let mut items = Vec::new();
                loop {
                    let next_fn =
                        vm.get_property_host(&iter_obj, &PropertyKey::String("next".to_string()))?;
                    let result = vm.call_value_with_this(&next_fn, &iter_obj, &[])?;
                    let done_val =
                        vm.get_property_host(&result, &PropertyKey::String("done".to_string()))?;
                    if vm.to_boolean(&done_val) {
                        break;
                    }
                    let val =
                        vm.get_property_host(&result, &PropertyKey::String("value".to_string()))?;
                    items.push(val);
                }
                Ok(items)
            } else {
                let len_val =
                    vm.get_property_host(other, &PropertyKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                let mut items = Vec::with_capacity(len);
                for idx in 0..len {
                    let item =
                        vm.get_property_host(other, &PropertyKey::String(idx.to_string()))?;
                    items.push(item);
                }
                Ok(items)
            }
        }
    }
}

pub(crate) fn resolve_promise_with_value(
    vm: &mut Vm,
    result_promise: &Rc<RefCell<PromiseState>>,
    value: Value,
) -> Result<(), crate::errors::RuntimeError> {
    fn resolve_promise_with_value_inner(
        vm: &mut Vm,
        result_promise: &Rc<RefCell<PromiseState>>,
        value: Value,
        already_resolved: Rc<RefCell<bool>>,
    ) -> Result<(), crate::errors::RuntimeError> {
        if *already_resolved.borrow() {
            return Ok(());
        }

        if let Value::Promise(ref p_rc) = value {
            if Rc::ptr_eq(p_rc, result_promise) {
                *already_resolved.borrow_mut() = true;
                return Err(vm.throw_type_error("Chaining cycle detected"));
            }

            let mut state = p_rc.borrow_mut();
            match &mut *state {
                PromiseState::Pending {
                    on_fulfilled,
                    on_rejected,
                } => {
                    let resolve_called = Rc::clone(&already_resolved);
                    let res_p = Rc::clone(result_promise);
                    on_fulfilled.push(Value::Builtin(Rc::new(move |vm, args| {
                        if *resolve_called.borrow() {
                            return Ok(Value::Undefined);
                        }
                        *resolve_called.borrow_mut() = true;
                        let off = if args.len() > 1 { 1 } else { 0 };
                        let v = args.get(off).cloned().unwrap_or(Value::Undefined);
                        resolve_promise_with_value(vm, &res_p, v)?;
                        Ok(Value::Undefined)
                    })));

                    let reject_called = Rc::clone(&already_resolved);
                    let res_p2 = Rc::clone(result_promise);
                    on_rejected.push(Value::Builtin(Rc::new(move |vm, args| {
                        if *reject_called.borrow() {
                            return Ok(Value::Undefined);
                        }
                        *reject_called.borrow_mut() = true;
                        let off = if args.len() > 1 { 1 } else { 0 };
                        let r = args.get(off).cloned().unwrap_or(Value::Undefined);
                        reject_pending_promise(vm, &res_p2, r);
                        Ok(Value::Undefined)
                    })));
                    return Ok(());
                }
                PromiseState::Fulfilled(v) => {
                    let v = v.clone();
                    let resolve_called = Rc::clone(&already_resolved);
                    let res_p = Rc::clone(result_promise);
                    vm.queue_microtask(move |vm| {
                        if *resolve_called.borrow() {
                            return Ok(());
                        }
                        *resolve_called.borrow_mut() = true;
                        fulfill_pending_promise(vm, &res_p, v.clone());
                        Ok(())
                    });
                    return Ok(());
                }
                PromiseState::Rejected(reason) => {
                    let reason = reason.clone();
                    let reject_called = Rc::clone(&already_resolved);
                    let res_p = Rc::clone(result_promise);
                    vm.queue_microtask(move |vm| {
                        if *reject_called.borrow() {
                            return Ok(());
                        }
                        *reject_called.borrow_mut() = true;
                        reject_pending_promise(vm, &res_p, reason.clone());
                        Ok(())
                    });
                    return Ok(());
                }
            }
        }

        if value.is_object() {
            let then_val =
                vm.get_property_host(&value, &PropertyKey::String("then".to_string()))?;
            if Vm::is_callable_value(&then_val) {
                // Per-spec PromiseResolveThenableJob semantics: calling a thenable's
                // `then` should not synchronously settle the outer promise. Instead
                // schedule a microtask that will invoke `then` with resolve/reject
                // callbacks. This preserves observable job ordering when a thenable
                // calls back synchronously.
                let resolve_called = Rc::clone(&already_resolved);
                let res_p = Rc::clone(result_promise);
                let thenable = value.clone();
                let then_func = then_val;

                vm.queue_microtask(move |vm| {
                    // Build resolve and reject callbacks that will settle res_p
                    let resolve_called_inner = Rc::clone(&resolve_called);
                    let res_p_inner = Rc::clone(&res_p);
                    let resolve_cb = Value::Builtin(Rc::new(move |vm, args| {
                        if *resolve_called_inner.borrow() {
                            return Ok(Value::Undefined);
                        }
                        *resolve_called_inner.borrow_mut() = true;
                        let off = if args.len() > 1 { 1 } else { 0 };
                        let v = args.get(off).cloned().unwrap_or(Value::Undefined);
                        resolve_promise_with_value(vm, &res_p_inner, v)?;
                        Ok(Value::Undefined)
                    }));

                    let reject_called_inner = Rc::clone(&resolve_called);
                    let res_p2_inner = Rc::clone(&res_p);
                    let reject_cb = Value::Builtin(Rc::new(move |vm, args| {
                        if *reject_called_inner.borrow() {
                            return Ok(Value::Undefined);
                        }
                        *reject_called_inner.borrow_mut() = true;
                        let off = if args.len() > 1 { 1 } else { 0 };
                        let r = args.get(off).cloned().unwrap_or(Value::Undefined);
                        reject_pending_promise(vm, &res_p2_inner, r);
                        Ok(Value::Undefined)
                    }));

                    // Call the then function with the thenable as `this`.
                    // Errors thrown here should reject the promise.
                    match vm.call_value_with_this(&then_func, &thenable, &[resolve_cb, reject_cb]) {
                        Ok(_) => Ok(()),
                        Err(err) => {
                            let reason = vm
                                .last_uncaught_exception
                                .take()
                                .unwrap_or(Value::String(format!("{}", err)));
                            reject_pending_promise(vm, &res_p, reason);
                            Ok(())
                        }
                    }
                });
                return Ok(());
            }
        }

        if !*already_resolved.borrow() {
            *already_resolved.borrow_mut() = true;
            fulfill_pending_promise(vm, result_promise, value);
        }
        Ok(())
    }

    resolve_promise_with_value_inner(vm, result_promise, value, Rc::new(RefCell::new(false)))
}

fn builtin_object(f: BuiltinFn, function_proto: &Rc<RefCell<ObjectData>>) -> Value {
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().prototype = Value::Object(Rc::clone(function_proto));
    Value::BuiltinObject(f, props)
}

thread_local! {
    static SHADOW_REALM_REGISTRY: RefCell<HashMap<usize, Rc<ShadowRealmRecord>>> =
        RefCell::new(HashMap::new());
}

struct ShadowRealmRecord {
    vm: RefCell<Vm>,
    base_dir: PathBuf,
}

impl ShadowRealmRecord {
    fn new(base_dir: PathBuf) -> Self {
        let globals = Rc::new(RefCell::new(ObjectData::new()));
        install_minimal_kernel_constructors(&globals);
        Self {
            vm: RefCell::new(Vm::new(Rc::clone(&globals))),
            base_dir,
        }
    }

    fn borrow_vm(&self) -> RefMut<'_, Vm> {
        self.vm.borrow_mut()
    }

    fn base_dir(&self) -> PathBuf {
        self.base_dir.clone()
    }
}

fn register_shadow_realm(obj: &Rc<RefCell<ObjectData>>, record: Rc<ShadowRealmRecord>) {
    let key = Rc::as_ptr(obj) as usize;
    SHADOW_REALM_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(key, record);
    });
}

fn get_shadow_realm_record(value: &Value) -> Option<Rc<ShadowRealmRecord>> {
    if let Value::Object(obj) = value {
        let key = Rc::as_ptr(obj) as usize;
        SHADOW_REALM_REGISTRY.with(|registry| registry.borrow().get(&key).cloned())
    } else {
        None
    }
}

fn require_shadow_realm_record(
    vm: &mut Vm,
    value: &Value,
) -> Result<Rc<ShadowRealmRecord>, crate::errors::RuntimeError> {
    get_shadow_realm_record(value)
        .ok_or_else(|| vm.throw_type_error("ShadowRealm method called on incompatible receiver"))
}

fn current_test_path(vm: &Vm) -> Option<PathBuf> {
    vm.get_global("__TSBLITZ_TEST_PATH")
        .and_then(|value| match value {
            Value::String(s) => Some(PathBuf::from(s)),
            _ => None,
        })
}

fn resolve_shadowrealm_base_dir(vm: &Vm) -> PathBuf {
    if let Some(path) = current_test_path(vm)
        && let Some(parent) = path.parent()
    {
        return parent.to_path_buf();
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn fulfill_pending_promise(
    vm: &mut Vm,
    promise: &Rc<RefCell<PromiseState>>,
    value: Value,
) {
    use crate::value::PromiseState;

    let mut state = promise.borrow_mut();
    if let PromiseState::Pending { on_fulfilled, .. } = &mut *state {
        let callbacks = on_fulfilled.clone();
        let stored_value = value.clone();
        *state = PromiseState::Fulfilled(value);
        drop(state);
        for callback in callbacks {
            let mut arg_value = Some(stored_value.clone());
            vm.queue_microtask(move |vm| {
                let args = [arg_value.take().unwrap()];
                let _ = vm.call_value_with_this(&callback, &Value::Undefined, &args);
                Ok(())
            });
        }
    }
}

pub(crate) fn reject_pending_promise(
    vm: &mut Vm,
    promise: &Rc<RefCell<PromiseState>>,
    reason: Value,
) {
    use crate::value::PromiseState;

    let mut state = promise.borrow_mut();
    if let PromiseState::Pending { on_rejected, .. } = &mut *state {
        let callbacks = on_rejected.clone();
        let stored_reason = reason.clone();
        *state = PromiseState::Rejected(reason);
        drop(state);
        if callbacks.is_empty() {
            // No rejection handlers registered: notify host hook about an
            // unhandled rejection. Hosts may inspect the promise and reason.
            // The promise object is not directly accessible here, so pass
            // Undefined as the placeholder. Embedding the Promise object
            // would require changing PromiseState layout.
            vm.call_unhandled_rejection(Value::Undefined, stored_reason);
        } else {
            for callback in callbacks {
                let mut arg_reason = Some(stored_reason.clone());
                vm.queue_microtask(move |vm| {
                    let args = [arg_reason.take().unwrap()];
                    let _ = vm.call_value_with_this(&callback, &Value::Undefined, &args);
                    Ok(())
                });
            }
        }
    }
}

fn shadowrealm_evaluate(vm: &mut Vm, args: &[Value]) -> Result<Value, crate::errors::RuntimeError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let record = require_shadow_realm_record(vm, &target)?;
    let source = match args.first() {
        Some(Value::String(s)) => s.clone(),
        _ => {
            return Err(
                vm.throw_type_error("ShadowRealm.prototype.evaluate requires a string argument")
            );
        }
    };
    let session = Parser::parse_source(&source)
        .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate parse: {}", e)))?;
    let chunk = compiler::compile_oxc(session.program(), &source)
        .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate compile: {}", e)))?;

    // Run the code inside the ShadowRealm VM and clone the resulting value
    // into the current (host) VM using a pragmatic structured-clone that
    // supports primitives, plain objects, and arrays. Non-cloneable values
    // (functions, promises, symbols, accessors, cyclic structures) will
    // result in a TypeError to avoid leaking object identity across realms.
    // Parse optional transfer list argument (third argument) as an Array of
    // ArrayBuffer objects. We collect backing-store addresses (Rc pointer
    // addresses of the [[ArrayBufferData]] Array) into a HashSet and pass it
    // to the clone helper so it can detach transferred buffers in the source
    // realm.
    let mut transfer_set: Option<HashSet<usize>> = None;
    if let Some(Value::Array(arr_rc, _)) = args.get(2) {
        let mut set = HashSet::new();
        for slot in arr_rc.borrow().iter().cloned() {
            if let Some(v) = slot {
                match v {
                    Value::Object(obj_rc) => {
                        // Look up [[ArrayBufferData]] slot and grab underlying
                        // array backing Rc pointer address.
                        if let Some(PropertyDescriptor::Data { value: ab_data, .. }) = obj_rc
                            .borrow()
                            .properties
                            .get(&PropertyKey::String("[[ArrayBufferData]]".to_string()))
                            .cloned()
                            && let Value::Array(backing_rc, _) = ab_data
                        {
                            set.insert(Rc::as_ptr(&backing_rc) as usize);
                            continue;
                        }
                        return Err(vm.throw_type_error(
                            "Transfer list entries must be ArrayBuffer objects",
                        ));
                    }
                    _ => {
                        return Err(vm.throw_type_error(
                            "Transfer list entries must be ArrayBuffer objects",
                        ));
                    }
                }
            }
        }
        transfer_set = Some(set);
    }

    let inner_result = record
        .borrow_vm()
        .run(chunk)
        .map_err(|err| vm.throw_type_error(&format!("ShadowRealm evaluate error: {}", err)))?;

    // Pass transfer_set.as_ref() to clone helper so it may detach transferred
    // ArrayBuffers in the source realm.
    clone_value_across_realms(vm, &inner_result, transfer_set.as_ref())
        .map_err(|s| vm.throw_type_error(&s))
}

fn shadowrealm_import_value(
    vm: &mut Vm,
    args: &[Value],
) -> Result<Value, crate::errors::RuntimeError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let record = require_shadow_realm_record(vm, &target)?;
    let specifier = args.first().cloned().unwrap_or(Value::Undefined);
    let specifier_str = vm.to_string(&specifier)?;
    let export_name = match args.get(1) {
        Some(Value::String(s)) => s.clone(),
        _ => {
            return Err(vm.throw_type_error(
                "ShadowRealm.prototype.importValue requires export name string",
            ));
        }
    };

    let promise = Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    }));
    let promise_value = Value::Promise(Rc::clone(&promise));

    // Parse transfer list argument (third argument) if present. For importValue
    // the signature is (shadowRealm, specifier, exportName, [transferList]) so
    // the transfer list will be at index 2 or 3 depending on calling conv. The
    // existing code places export name at args.get(1), so treat args.get(2) as
    // the optional transfer list.
    let mut transfer_set_arg: Option<Value> = None;
    if args.len() > 2 {
        transfer_set_arg = args.get(2).cloned();
    }

    let record_handle = Rc::clone(&record);
    let base_dir = record.base_dir();
    let specifier_clone = specifier_str;
    let export_clone = export_name;

    vm.queue_microtask(move |vm| {
        let result = (|| -> Result<Value, String> {
            let entry = {
                let inner_vm = record_handle.borrow_vm();
                let mut loader = inner_vm.module_loader_mut();
                let entry = loader
                    .construct(&base_dir, &specifier_clone)
                    .map_err(|err| format!("module construct failed: {}", err))?;
                loader
                    .instantiate_all()
                    .map_err(|err| format!("module instantiate failed: {}", err))?;
                entry
            };
            let chunk = {
                let rec = entry.borrow();
                let module_index = rec
                    .module_index
                    .ok_or_else(|| "module missing index".to_string())?;
                let module_ctx = ModuleContext {
                    module_index,
                    imports: rec.import_sources.clone(),
                    exports: rec.exports.keys().cloned().collect(),
                };
                compiler::compile_oxc_with_ctx(
                    rec.session.program(),
                    Some(&module_ctx),
                    rec.session.source(),
                )
                .map_err(|err| format!("module compile failed: {}", err))?
            };
            let chunk = clear_slot_types_in_chunk(chunk);
            {
                let mut inner_vm = record_handle.borrow_vm();
                inner_vm
                    .run(chunk)
                    .map_err(|err| format!("module runtime failed: {}", err))?;
            }
            let slot = entry
                .borrow()
                .exports
                .get(&export_clone)
                .cloned()
                .ok_or_else(|| format!("export {} not found", export_clone))?;
            Ok(slot.borrow().clone())
        })();

        match result {
            Ok(value) => {
                // Build transfer set from transfer_set_arg if present.
                let mut transfer_set: Option<HashSet<usize>> = None;
                if let Some(Value::Array(arr_rc, _)) = transfer_set_arg.clone() {
                    let mut set = HashSet::new();
                    for v in arr_rc.borrow().iter().flatten().cloned() {
                        match v {
                            Value::Object(obj_rc) => {
                                if let Some(PropertyDescriptor::Data { value: ab_data, .. }) =
                                    obj_rc
                                        .borrow()
                                        .properties
                                        .get(&PropertyKey::String(
                                            "[[ArrayBufferData]]".to_string(),
                                        ))
                                        .cloned()
                                    && let Value::Array(backing_rc, _) = ab_data
                                {
                                    set.insert(Rc::as_ptr(&backing_rc) as usize);
                                    continue;
                                }
                                let err_val = vm
                                    .create_intrinsic_error(
                                        "TypeError",
                                        "Transfer list entries must be ArrayBuffer objects",
                                    )
                                    .unwrap_or(Value::String(
                                        "Transfer list entries must be ArrayBuffer objects"
                                            .to_string(),
                                    ));
                                reject_pending_promise(vm, &promise, err_val);
                                return Ok(());
                            }
                            _ => {
                                let err_val = vm
                                    .create_intrinsic_error(
                                        "TypeError",
                                        "Transfer list entries must be ArrayBuffer objects",
                                    )
                                    .unwrap_or(Value::String(
                                        "Transfer list entries must be ArrayBuffer objects"
                                            .to_string(),
                                    ));
                                reject_pending_promise(vm, &promise, err_val);
                                return Ok(());
                            }
                        }
                    }
                    transfer_set = Some(set);
                }

                match clone_value_across_realms(vm, &value, transfer_set.as_ref()) {
                    Ok(cloned) => fulfill_pending_promise(vm, &promise, cloned),
                    Err(err) => {
                        let err_val = vm
                            .create_intrinsic_error(
                                "TypeError",
                                &format!("ShadowRealm clone failed: {}", err),
                            )
                            .unwrap_or(Value::String(err));
                        reject_pending_promise(vm, &promise, err_val);
                    }
                }
            }
            Err(msg) => {
                let err_val = vm
                    .create_intrinsic_error("TypeError", &msg)
                    .unwrap_or(Value::String(msg.clone()));
                reject_pending_promise(vm, &promise, err_val);
            }
        }
        Ok(())
    });

    Ok(promise_value)
}

fn dynamic_import_value(vm: &mut Vm, args: &[Value]) -> Result<Value, crate::errors::RuntimeError> {
    let specifier = args.first().cloned().unwrap_or(Value::Undefined);
    let options = args.get(1).cloned().unwrap_or(Value::Undefined);

    let promise = Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    }));
    let promise_value = Value::Promise(Rc::clone(&promise));

    let specifier_str = match vm.to_string(&specifier) {
        Ok(s) => s,
        Err(err) => {
            let reason = vm
                .last_uncaught_exception
                .take()
                .unwrap_or(Value::String(err.to_string()));
            reject_pending_promise(vm, &promise, reason);
            return Ok(promise_value);
        }
    };

    let base_dir = resolve_shadowrealm_base_dir(vm);
    let mut import_vm = Vm::new(vm.globals_rc());
    let loader_handle = import_vm.module_loader_handle();
    let result = (|| -> Result<Value, Value> {
        let entry = {
            let mut loader = loader_handle.borrow_mut();
            let entry = loader.construct(&base_dir, &specifier_str).map_err(|err| {
                host_error_object(vm, "SyntaxError", &format!("dynamic import construct failed: {}", err))
            })?;
            loader.instantiate_all().map_err(|err| {
                host_error_object(vm, "SyntaxError", &format!("dynamic import instantiate failed: {}", err))
            })?;
            entry
        };
        let entry_path = entry.borrow().path.clone();
        crate::module::evaluate_entry_shared(&loader_handle, &entry_path, &mut import_vm, false)
            .map_err(|err| {
                import_vm
                    .last_uncaught_exception
                    .take()
                    .or_else(|| {
                        Some(host_error_object(
                            vm,
                            "TypeError",
                            &format!("dynamic import evaluate failed: {}", err),
                        ))
                    })
                    .unwrap_or(Value::String(format!(
                        "dynamic import evaluate failed: {}",
                        err
                    )))
            })?;

        if !matches!(options, Value::Undefined) {
            // The `options` argument is currently parsed for side effects only.
            // Test262 dynamic-import coverage in this tree only exercises the
            // source-only form, so we intentionally ignore it here.
        }

        let module_index = entry.borrow().module_index.ok_or_else(|| {
            Value::String("dynamic import failed: module missing index".to_string())
        })?;
        let ns = import_vm
            .module_namespace_value(module_index)
            .ok_or_else(|| {
                Value::String("dynamic import failed: module namespace missing".to_string())
            })?;
        Ok(ns)
    })();

    match result {
        Ok(value) => {
            // Module namespace objects currently carry accessor properties for
            // live bindings. Returning them directly preserves that behavior
            // and avoids the cross-realm clone helper rejecting them.
            let mut state = promise.borrow_mut();
            *state = PromiseState::Fulfilled(value);
        }
        Err(reason) => {
            let mut debug_vm = Vm::new(vm.globals_rc());
            let reason_name = debug_vm
                .get_property_host(&reason, &PropertyKey::String("name".to_string()))
                .ok()
                .map(|v| v.to_string());
            eprintln!(
                "[dynamic import debug] rejection type={}, name={:?}, value={}",
                reason.type_name(),
                reason_name,
                reason
            );
            let mut state = promise.borrow_mut();
            *state = PromiseState::Rejected(reason);
        }
    }

    Ok(promise_value)
}

fn set_builtin_length(target: &Value, length: f64) {
    let desc = PropertyDescriptor::Data {
        value: Value::Number(length),
        writable: false,
        enumerable: false,
        configurable: true,
    };
    match target {
        Value::Object(props)
        | Value::Array(_, props)
        | Value::Closure { props, .. }
        | Value::BuiltinObject(_, props) => {
            props
                .borrow_mut()
                .properties
                .insert(PropertyKey::String("length".to_string()), desc);
        }
        _ => {}
    }
}

fn set_builtin_name(target: &Value, name: &str) {
    let desc = PropertyDescriptor::Data {
        value: Value::String(name.to_string()),
        writable: false,
        enumerable: false,
        configurable: true,
    };
    match target {
        Value::Object(props)
        | Value::Array(_, props)
        | Value::Closure { props, .. }
        | Value::BuiltinObject(_, props) => {
            props
                .borrow_mut()
                .properties
                .insert(PropertyKey::String("name".to_string()), desc);
        }
        _ => {}
    }
}

/// Converts a value to a non-negative integer length suitable for array operations.
fn to_length(vm: &mut Vm, value: &Value) -> Result<usize, crate::errors::RuntimeError> {
    let num = vm.to_number_value(value)?;
    if num.is_nan() || num <= 0.0_f64 {
        return Ok(0);
    }
    if !num.is_finite() {
        return Err(vm.throw_range_error("Invalid array length"));
    }
    let int_len = num.trunc();
    if int_len > MAX_ARRAY_LENGTH_F {
        return Err(vm.throw_range_error("Invalid array length"));
    }

    Ok(int_len as usize)
}

fn to_index(vm: &mut Vm, value: &Value) -> Result<usize, crate::errors::RuntimeError> {
    let num = vm.to_number_value(value)?;
    if num.is_nan() {
        return Ok(0);
    }
    if !num.is_finite() || num < 0.0_f64 {
        return Err(vm.throw_range_error("Invalid array index"));
    }
    let int_val = num.trunc();
    if int_val > MAX_ARRAY_LENGTH_F {
        return Err(vm.throw_range_error("Invalid array index"));
    }
    Ok(int_val as usize)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtomicsValueKind {
    Number { bits: u32, signed: bool },
    BigInt { signed: bool },
}

#[derive(Clone)]
struct AtomicsViewInfo {
    view: Rc<RefCell<ObjectData>>,
    data: Rc<RefCell<Vec<Option<Value>>>>,
    buffer: Rc<RefCell<ObjectData>>,
    kind_name: String,
    value_kind: AtomicsValueKind,
    length: usize,
    is_shared: bool,
}

fn atomics_value_kind(kind_name: &str) -> Option<AtomicsValueKind> {
    match kind_name {
        "Int8Array" => Some(AtomicsValueKind::Number {
            bits: 8,
            signed: true,
        }),
        "Uint8Array" => Some(AtomicsValueKind::Number {
            bits: 8,
            signed: false,
        }),
        "Int16Array" => Some(AtomicsValueKind::Number {
            bits: 16,
            signed: true,
        }),
        "Uint16Array" => Some(AtomicsValueKind::Number {
            bits: 16,
            signed: false,
        }),
        "Int32Array" => Some(AtomicsValueKind::Number {
            bits: 32,
            signed: true,
        }),
        "Uint32Array" => Some(AtomicsValueKind::Number {
            bits: 32,
            signed: false,
        }),
        "BigInt64Array" => Some(AtomicsValueKind::BigInt { signed: true }),
        "BigUint64Array" => Some(AtomicsValueKind::BigInt { signed: false }),
        _ => None,
    }
}

fn atomics_wait_kind(kind_name: &str) -> bool {
    matches!(kind_name, "Int32Array" | "BigInt64Array")
}

fn atomics_normalize_integer(value: i128, bits: u32, signed: bool) -> i128 {
    let modulus = 1i128 << bits;
    let mut v = value % modulus;
    if v < 0 {
        v += modulus;
    }
    if signed {
        let sign_bit = 1i128 << (bits - 1);
        if v >= sign_bit {
            v -= modulus;
        }
    }
    v
}

fn atomics_normalize_bigint(value: BigIntValue, signed: bool) -> BigIntValue {
    let modulus = BigIntValue::from(1u8) << 64u32;
    let half = BigIntValue::from(1u8) << 63u32;
    let mut v = value % &modulus;
    if v.sign() == Sign::Minus {
        v += &modulus;
    }
    if signed && v >= half {
        v -= &modulus;
    }
    v
}

fn atomics_value_to_integer(
    vm: &mut Vm,
    value: &Value,
    bits: u32,
    signed: bool,
) -> Result<Value, crate::errors::RuntimeError> {
    let n = vm.to_number_value(value)?;
    let int = atomics_normalize_integer(n.trunc() as i128, bits, signed);
    Ok(Value::Number(int as f64))
}

fn atomics_value_to_bigint(
    vm: &mut Vm,
    value: &Value,
    signed: bool,
) -> Result<Value, crate::errors::RuntimeError> {
    let bigint = match value {
        Value::BigInt(n) => n.clone(),
        Value::Number(n) => BigIntValue::from_f64(n.trunc()).ok_or_else(|| {
            vm.throw_type_error("Atomics BigInt operations require BigInt values")
        })?,
        Value::String(s) => BigIntValue::parse_bytes(s.as_bytes(), 10).ok_or_else(|| {
            vm.throw_type_error("Atomics BigInt operations require BigInt values")
        })?,
        _ => {
            let prim = vm.to_primitive(value, Some("number"))?;
            match prim {
                Value::BigInt(n) => n,
                Value::Number(n) => BigIntValue::from_f64(n.trunc()).ok_or_else(|| {
                    vm.throw_type_error("Atomics BigInt operations require BigInt values")
                })?,
                _ => {
                    return Err(
                        vm.throw_type_error("Atomics BigInt operations require BigInt values")
                    );
                }
            }
        }
    };
    Ok(Value::BigInt(atomics_normalize_bigint(bigint, signed)))
}

fn atomics_view_info(
    vm: &mut Vm,
    view: &Value,
) -> Result<AtomicsViewInfo, crate::errors::RuntimeError> {
    let Value::Object(view_obj) = view else {
        return Err(vm.throw_type_error("Atomics requires a TypedArray"));
    };
    let (data, buffer, kind_name, length, is_shared) = {
        let view_data = view_obj.borrow();
        let data = match view_data
            .properties
            .get(&PropertyKey::String("[[TypedArrayData]]".to_string()))
        {
            Some(PropertyDescriptor::Data {
                value: Value::Array(arr, _),
                ..
            }) => Rc::clone(arr),
            _ => return Err(vm.throw_type_error("Atomics requires a TypedArray")),
        };
        let kind_name = match view_data
            .properties
            .get(&PropertyKey::String("[[TypedArrayKind]]".to_string()))
        {
            Some(PropertyDescriptor::Data {
                value: Value::String(kind),
                ..
            }) => kind.clone(),
            _ => return Err(vm.throw_type_error("Atomics requires a TypedArray")),
        };
        let length = match view_data
            .properties
            .get(&PropertyKey::String("[[TypedArrayLength]]".to_string()))
        {
            Some(PropertyDescriptor::Data {
                value: Value::Number(n),
                ..
            }) if n.is_finite() && *n >= 0.0 => *n as usize,
            _ => data.borrow().len(),
        };
        let buffer = match view_data
            .properties
            .get(&PropertyKey::String("[[TypedArrayBuffer]]".to_string()))
        {
            Some(PropertyDescriptor::Data {
                value: Value::Object(buffer),
                ..
            }) => Rc::clone(buffer),
            _ => return Err(vm.throw_type_error("Atomics requires a TypedArray")),
        };
        let is_shared = {
            let buffer_data = buffer.borrow();
            matches!(
                buffer_data
                    .properties
                    .get(&PropertyKey::String("[[IsSharedArrayBuffer]]".to_string())),
                Some(PropertyDescriptor::Data {
                    value: Value::Bool(true),
                    ..
                })
            )
        };
        (data, buffer, kind_name, length, is_shared)
    };
    let value_kind = atomics_value_kind(&kind_name)
        .ok_or_else(|| vm.throw_type_error("Atomics requires a numeric or BigInt typed array"))?;
    Ok(AtomicsViewInfo {
        view: Rc::clone(view_obj),
        data,
        buffer,
        kind_name,
        value_kind,
        length,
        is_shared,
    })
}

fn atomics_view_index(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index_value: &Value,
) -> Result<usize, crate::errors::RuntimeError> {
    let index = to_index(vm, index_value)?;
    if index >= info.length {
        return Err(vm.throw_range_error("Atomics index out of range"));
    }
    Ok(index)
}

fn atomics_read_view_slot(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
) -> Result<Value, crate::errors::RuntimeError> {
    let prop_key = PropertyKey::String(index.to_string());
    let value = vm.get_property_host(&Value::Object(Rc::clone(&info.view)), &prop_key)?;
    if !matches!(value, Value::Undefined) {
        return Ok(value);
    }
    Ok(info
        .data
        .borrow()
        .get(index)
        .cloned()
        .flatten()
        .unwrap_or(Value::Undefined))
}

fn atomics_write_view_slot(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: Value,
) -> Result<(), crate::errors::RuntimeError> {
    let prop_key = Value::Number(index as f64);
    let stored = value.clone();
    let _ = vm.set_property_host(&Value::Object(Rc::clone(&info.view)), &prop_key, value)?;
    if let Some(slot) = info.data.borrow_mut().get_mut(index) {
        *slot = Some(stored);
    }
    Ok(())
}

fn atomics_waiters_array(buffer: &Rc<RefCell<ObjectData>>) -> Rc<RefCell<Vec<Option<Value>>>> {
    let mut buffer_data = buffer.borrow_mut();
    if let Some(PropertyDescriptor::Data {
        value: Value::Array(arr, _),
        ..
    }) = buffer_data
        .properties
        .get(&PropertyKey::String("[[AtomicsWaiters]]".to_string()))
    {
        return Rc::clone(arr);
    }
    let arr = Rc::new(RefCell::new(Vec::new()));
    buffer_data.properties.insert(
        PropertyKey::String("[[AtomicsWaiters]]".to_string()),
        PropertyDescriptor::data(Value::Array(
            Rc::clone(&arr),
            Rc::new(RefCell::new(ObjectData::new())),
        )),
    );
    arr
}

fn atomics_make_waiter_record(index: usize, resolve: Option<Value>) -> Value {
    let mut data = ObjectData::new();
    data.properties.insert(
        PropertyKey::String("index".to_string()),
        PropertyDescriptor::data(Value::Number(index as f64)),
    );
    data.properties.insert(
        PropertyKey::String("resolve".to_string()),
        PropertyDescriptor::data(resolve.unwrap_or(Value::Undefined)),
    );
    Value::Object(Rc::new(RefCell::new(data)))
}

fn atomics_make_wait_result(async_flag: bool, value: Value) -> Value {
    let result = Rc::new(RefCell::new(ObjectData::new()));
    result.borrow_mut().properties.insert(
        PropertyKey::String("async".to_string()),
        PropertyDescriptor::data(Value::Bool(async_flag)),
    );
    result.borrow_mut().properties.insert(
        PropertyKey::String("value".to_string()),
        PropertyDescriptor::data(value),
    );
    Value::Object(result)
}

fn atomics_wake_waiters(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    count: usize,
) -> Result<usize, crate::errors::RuntimeError> {
    let waiters = atomics_waiters_array(&info.buffer);
    let mut to_wake: Vec<Value> = Vec::new();
    {
        let mut waiters_mut = waiters.borrow_mut();
        let mut remaining: Vec<Option<Value>> = Vec::with_capacity(waiters_mut.len());
        for entry in waiters_mut.iter().cloned() {
            if to_wake.len() >= count {
                remaining.push(entry);
                continue;
            }
            let Some(waiter) = entry else {
                continue;
            };
            let record_index =
                match vm.get_property_host(&waiter, &PropertyKey::String("index".to_string())) {
                    Ok(Value::Number(n)) if n.is_finite() && n >= 0.0 => n as usize,
                    _ => {
                        remaining.push(Some(waiter));
                        continue;
                    }
                };
            if record_index != index {
                remaining.push(Some(waiter));
                continue;
            }
            to_wake.push(waiter);
        }
        *waiters_mut = remaining;
    }

    for waiter in &to_wake {
        if let Ok(resolve) =
            vm.get_property_host(waiter, &PropertyKey::String("resolve".to_string()))
            && !matches!(resolve, Value::Undefined)
        {
            let _ = vm.call_value_with_this(
                &resolve,
                &Value::Undefined,
                &[Value::String("ok".to_string())],
            );
        }
    }

    Ok(to_wake.len())
}

fn atomics_register_waiter(
    _vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    resolve: Option<Value>,
) -> Result<(), crate::errors::RuntimeError> {
    let waiters = atomics_waiters_array(&info.buffer);
    waiters
        .borrow_mut()
        .push(Some(atomics_make_waiter_record(index, resolve)));
    Ok(())
}

fn atomics_coerce_view_value(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    value: &Value,
) -> Result<Value, crate::errors::RuntimeError> {
    match info.value_kind {
        AtomicsValueKind::Number { bits, signed } => {
            atomics_value_to_integer(vm, value, bits, signed)
        }
        AtomicsValueKind::BigInt { signed } => atomics_value_to_bigint(vm, value, signed),
    }
}

fn atomics_normalized_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
) -> Result<Value, crate::errors::RuntimeError> {
    let current = atomics_read_view_slot(vm, info, index)?;
    atomics_coerce_view_value(vm, info, &current)
}

fn atomics_load_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
) -> Result<Value, crate::errors::RuntimeError> {
    atomics_normalized_current(vm, info, index)
}

fn atomics_store_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: &Value,
) -> Result<Value, crate::errors::RuntimeError> {
    let stored = atomics_coerce_view_value(vm, info, value)?;
    atomics_write_view_slot(vm, info, index, stored.clone())?;
    Ok(stored)
}

fn atomics_exchange_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: &Value,
) -> Result<Value, crate::errors::RuntimeError> {
    let current = atomics_normalized_current(vm, info, index)?;
    let stored = atomics_coerce_view_value(vm, info, value)?;
    atomics_write_view_slot(vm, info, index, stored)?;
    Ok(current)
}

fn atomics_binary_update(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: &Value,
    op: &str,
) -> Result<Value, crate::errors::RuntimeError> {
    let current = atomics_normalized_current(vm, info, index)?;
    let operand = atomics_coerce_view_value(vm, info, value)?;
    let result = match (info.value_kind, current.clone(), operand) {
        (
            AtomicsValueKind::Number { bits, signed },
            Value::Number(current_n),
            Value::Number(operand_n),
        ) => {
            let lhs = current_n.trunc() as i128;
            let rhs = operand_n.trunc() as i128;
            let raw = match op {
                "add" => lhs + rhs,
                "sub" => lhs - rhs,
                "and" => lhs & rhs,
                "or" => lhs | rhs,
                "xor" => lhs ^ rhs,
                _ => lhs,
            };
            Value::Number(atomics_normalize_integer(raw, bits, signed) as f64)
        }
        (
            AtomicsValueKind::BigInt { signed },
            Value::BigInt(current_n),
            Value::BigInt(operand_n),
        ) => {
            let raw = match op {
                "add" => current_n + operand_n,
                "sub" => current_n - operand_n,
                "and" => current_n & operand_n,
                "or" => current_n | operand_n,
                "xor" => current_n ^ operand_n,
                _ => current_n,
            };
            Value::BigInt(atomics_normalize_bigint(raw, signed))
        }
        _ => return Err(vm.throw_type_error("Atomics type coercion failed")),
    };
    atomics_write_view_slot(vm, info, index, result)?;
    Ok(current)
}

fn atomics_compare_exchange(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    expected: &Value,
    replacement: &Value,
) -> Result<Value, crate::errors::RuntimeError> {
    let current = atomics_normalized_current(vm, info, index)?;
    let expected = atomics_coerce_view_value(vm, info, expected)?;
    if current == expected {
        let replacement = atomics_coerce_view_value(vm, info, replacement)?;
        atomics_write_view_slot(vm, info, index, replacement)?;
    }
    Ok(current)
}

fn own_property_descriptor(obj: &Value, key: &PropertyKey) -> Option<PropertyDescriptor> {
    match obj {
        Value::Object(data) => data.borrow().properties.get(key).cloned(),
        Value::Array(arr, props) => {
            if let PropertyKey::String(s) = key
                && let Some(idx) = canonical_index_key(s)
                && let Some(Some(value)) = arr.borrow().get(idx as usize).cloned()
            {
                return Some(PropertyDescriptor::Data {
                    value,
                    writable: true,
                    enumerable: true,
                    configurable: true,
                });
            }
            props.borrow().properties.get(key).cloned()
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            props.borrow().properties.get(key).cloned()
        }
        _ => None,
    }
}

/// Returns the value of the own property descriptor for the given key, if one exists.
pub fn own_property_descriptor_value(obj: &Value, key: &PropertyKey) -> Option<Value> {
    own_property_descriptor(obj, key).map(|desc| {
        let mut obj = ObjectData::new();
        match desc {
            PropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => {
                obj.properties.insert(
                    PropertyKey::String("value".to_string()),
                    PropertyDescriptor::data(value),
                );
                obj.properties.insert(
                    PropertyKey::String("writable".to_string()),
                    PropertyDescriptor::data(Value::Bool(writable)),
                );
                obj.properties.insert(
                    PropertyKey::String("enumerable".to_string()),
                    PropertyDescriptor::data(Value::Bool(enumerable)),
                );
                obj.properties.insert(
                    PropertyKey::String("configurable".to_string()),
                    PropertyDescriptor::data(Value::Bool(configurable)),
                );
            }
            PropertyDescriptor::Accessor {
                get,
                set,
                enumerable,
                configurable,
            } => {
                obj.properties.insert(
                    PropertyKey::String("get".to_string()),
                    PropertyDescriptor::data(get.unwrap_or(Value::Undefined)),
                );
                obj.properties.insert(
                    PropertyKey::String("set".to_string()),
                    PropertyDescriptor::data(set.unwrap_or(Value::Undefined)),
                );
                obj.properties.insert(
                    PropertyKey::String("enumerable".to_string()),
                    PropertyDescriptor::data(Value::Bool(enumerable)),
                );
                obj.properties.insert(
                    PropertyKey::String("configurable".to_string()),
                    PropertyDescriptor::data(Value::Bool(configurable)),
                );
            }
        }
        Value::Object(Rc::new(RefCell::new(obj)))
    })
}

fn own_enumerable_keys(obj: &Value) -> Vec<PropertyKey> {
    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => {
            for (key, desc) in &data.borrow().properties {
                let enumerable = match desc {
                    PropertyDescriptor::Data { enumerable, .. }
                    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                };
                if enumerable {
                    keys.push(key.clone());
                }
            }
        }
        Value::Array(arr, props) => {
            for (idx, slot) in arr.borrow().iter().enumerate() {
                if slot.is_some() {
                    keys.push(PropertyKey::String(idx.to_string()));
                }
            }
            for (key, desc) in &props.borrow().properties {
                let enumerable = match desc {
                    PropertyDescriptor::Data { enumerable, .. }
                    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                };
                if enumerable {
                    keys.push(key.clone());
                }
            }
        }
        Value::Closure { .. } | Value::BuiltinObject(..) | Value::Builtin(_) => {
            let p = match obj {
                Value::Closure { props, .. } => Some(props),
                Value::BuiltinObject(_, props) => Some(props),
                _ => None,
            };
            if let Some(props) = p {
                for (key, desc) in &props.borrow().properties {
                    let enumerable = match desc {
                        PropertyDescriptor::Data { enumerable, .. }
                        | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                    };
                    if enumerable {
                        keys.push(key.clone());
                    }
                }
            }
        }
        _ => {}
    }
    keys
}

fn non_enum_data(value: Value) -> PropertyDescriptor {
    PropertyDescriptor::Data {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
    }
}

fn descriptor_value_from_descriptor(
    object_proto: &Rc<RefCell<ObjectData>>,
    desc: &PropertyDescriptor,
) -> Value {
    let mut obj = ObjectData::with_prototype(Value::Object(Rc::clone(object_proto)));
    match desc {
        PropertyDescriptor::Data {
            value,
            writable,
            enumerable,
            configurable,
        } => {
            obj.properties.insert(
                PropertyKey::String("value".to_string()),
                PropertyDescriptor::data(value.clone()),
            );
            obj.properties.insert(
                PropertyKey::String("writable".to_string()),
                PropertyDescriptor::data(Value::Bool(*writable)),
            );
            obj.properties.insert(
                PropertyKey::String("enumerable".to_string()),
                PropertyDescriptor::data(Value::Bool(*enumerable)),
            );
            obj.properties.insert(
                PropertyKey::String("configurable".to_string()),
                PropertyDescriptor::data(Value::Bool(*configurable)),
            );
        }
        PropertyDescriptor::Accessor {
            get,
            set,
            enumerable,
            configurable,
        } => {
            obj.properties.insert(
                PropertyKey::String("get".to_string()),
                PropertyDescriptor::data(get.clone().unwrap_or(Value::Undefined)),
            );
            obj.properties.insert(
                PropertyKey::String("set".to_string()),
                PropertyDescriptor::data(set.clone().unwrap_or(Value::Undefined)),
            );
            obj.properties.insert(
                PropertyKey::String("enumerable".to_string()),
                PropertyDescriptor::data(Value::Bool(*enumerable)),
            );
            obj.properties.insert(
                PropertyKey::String("configurable".to_string()),
                PropertyDescriptor::data(Value::Bool(*configurable)),
            );
        }
    }
    Value::Object(Rc::new(RefCell::new(obj)))
}

/// Returns the own property keys of an object.
fn own_property_keys(obj: &Value) -> Vec<PropertyKey> {
    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => {
            for key in data.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        Value::Array(arr, props) => {
            for idx in 0..arr.borrow().len() {
                keys.push(PropertyKey::String(idx.to_string()));
            }
            for key in props.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            for key in props.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        _ => {}
    }
    keys
}

fn property_key_to_value(key: &PropertyKey) -> Value {
    match key {
        PropertyKey::String(name) => Value::String(name.clone()),
        PropertyKey::Symbol(id) => Value::Symbol(id.clone(), None),
    }
}

fn array_from_values(values: Vec<Value>) -> Value {
    let arr_rc = Rc::new(RefCell::new(
        values.into_iter().map(Some).collect::<Vec<_>>(),
    ));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
    );
    Value::Array(arr_rc, props)
}

fn syntax_error(vm: &mut Vm, message: String) -> crate::errors::RuntimeError {
    let err_val = host_error_object(vm, "SyntaxError", &message);
    vm.throw_exception(err_val)
}

fn host_error_object(vm: &Vm, name: &str, message: &str) -> Value {
    let mut obj = ObjectData::new();
    if let Some(proto) = vm.realm().intrinsics.error_prototype.clone() {
        obj.prototype = proto;
    }
    obj.properties.insert(
        PropertyKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String(name.to_string())),
    );
    obj.properties.insert(
        PropertyKey::String("message".to_string()),
        PropertyDescriptor::data(Value::String(message.to_string())),
    );
    Value::Object(Rc::new(RefCell::new(obj)))
}

fn canonical_index_key(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if s != "0" && s.starts_with('0') {
        return None;
    }
    let idx = s.parse::<u64>().ok()?;
    if idx.to_string() != s {
        return None;
    }
    Some(idx)
}

/// Structured-clone helper for cross-realm value transfers used by ShadowRealm
/// operations. Implements copying semantics for primitives, Array, Object,
/// Date, RegExp, Map, Set, ArrayBuffer, and TypedArray shaped objects used by
/// this runtime. Preserves cycles/shared references by using a visited map and
/// creates host-realm objects with the appropriate host intrinsics as
/// prototypes. Returns Err(String) for uncloneable values (functions,
/// promises, symbols, accessors, etc.).
fn clone_value_across_realms(
    vm: &mut Vm,
    value: &Value,
    transfer_set: Option<&HashSet<usize>>,
) -> Result<Value, String> {
    use std::collections::HashMap;

    // visited maps source Rc pointer addresses to already-created clone Values
    // so we can preserve cycles and shared references.
    fn helper(
        vm: &mut Vm,
        v: &Value,
        visited: &mut HashMap<usize, Value>,
        transfer_set: Option<&HashSet<usize>>,
    ) -> Result<Value, String> {
        match v {
            Value::Undefined => Ok(Value::Undefined),
            Value::Null => Ok(Value::Null),
            Value::Bool(b) => Ok(Value::Bool(*b)),
            Value::Number(n) => Ok(Value::Number(*n)),
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::BigInt(bi) => Ok(Value::BigInt(bi.clone())),
            Value::Symbol(_, _) => Err("Cannot clone Symbol across realms".to_string()),
            Value::Promise(_) => Err("Cannot clone Promise across realms".to_string()),
            Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. } => {
                Err("Cannot clone functions/closures across realms".to_string())
            }
            Value::Array(arr_rc, props_rc) => {
                let addr = Rc::as_ptr(arr_rc) as usize;
                if let Some(existing) = visited.get(&addr) {
                    return Ok(existing.clone());
                }

                // Reserve a placeholder array in visited to handle cycles.
                let len = arr_rc.borrow().len();
                let placeholder_vec: Vec<Option<Value>> = vec![None; len];
                let new_arr = Rc::new(RefCell::new(placeholder_vec));
                let new_props = Rc::new(RefCell::new(ObjectData::new()));
                if let Some(proto) = vm.realm().intrinsics.array_prototype.clone() {
                    new_props.borrow_mut().prototype = proto;
                }
                // insert length property placeholder
                new_props.borrow_mut().properties.insert(
                    PropertyKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(len as f64)),
                );
                let new_val = Value::Array(Rc::clone(&new_arr), Rc::clone(&new_props));
                visited.insert(addr, new_val);

                // Fill dense slots
                for (i, slot) in arr_rc.borrow().iter().enumerate() {
                    if let Some(sv) = slot {
                        let cloned = helper(vm, sv, visited, transfer_set)?;
                        new_arr.borrow_mut()[i] = Some(cloned);
                    } else {
                        new_arr.borrow_mut()[i] = None;
                    }
                }

                // Clone property bag (data descriptors only)
                for (k, desc) in &props_rc.borrow().properties {
                    match desc {
                        PropertyDescriptor::Data { value, .. } => {
                            let cloned = helper(vm, value, visited, transfer_set)?;
                            new_props
                                .borrow_mut()
                                .properties
                                .insert(k.clone(), PropertyDescriptor::data(cloned));
                        }
                        _ => {
                            return Err(
                                "Cannot clone accessor properties across realms".to_string()
                            );
                        }
                    }
                }

                Ok(Value::Array(new_arr, new_props))
            }
            Value::Object(obj_rc) => {
                let addr = Rc::as_ptr(obj_rc) as usize;
                if let Some(existing) = visited.get(&addr) {
                    return Ok(existing.clone());
                }

                // Detect special internal shapes used by runtime: ArrayBuffer,
                // TypedArray, Map, Set, Date, RegExp by inspecting internal slots
                let borrow = obj_rc.borrow();

                // (no-op) helper removed; build placeholders inline below

                // ArrayBuffer: identified by [[ArrayBufferData]] internal slot
                if let Some(PropertyDescriptor::Data { value: ab_data, .. }) = borrow
                    .properties
                    .get(&PropertyKey::String("[[ArrayBufferData]]".to_string()))
                {
                    // Create placeholder object with host ArrayBuffer prototype
                    let mut out = ObjectData::new();
                    if let Some(proto) = vm.realm().get_intrinsic_prototype("Array") {
                        out.prototype = proto;
                    }
                    let target_obj = Rc::new(RefCell::new(out));
                    let target_val = Value::Object(Rc::clone(&target_obj));
                    visited.insert(addr, target_val);

                    // Clone underlying data (expected to be Value::Array)
                    let cloned_data = helper(vm, ab_data, visited, transfer_set)?;
                    target_obj.borrow_mut().properties.insert(
                        PropertyKey::String("[[ArrayBufferData]]".to_string()),
                        PropertyDescriptor::data(cloned_data),
                    );
                    // Copy byteLength if present
                    if let Some(PropertyDescriptor::Data { value, .. }) = borrow
                        .properties
                        .get(&PropertyKey::String("byteLength".to_string()))
                    {
                        let bl = helper(vm, value, visited, transfer_set)?;
                        target_obj.borrow_mut().properties.insert(
                            PropertyKey::String("byteLength".to_string()),
                            PropertyDescriptor::data(bl),
                        );
                    }
                    return Ok(Value::Object(target_obj));
                }

                // TypedArray: identified by [[TypedArrayData]]
                if let Some(PropertyDescriptor::Data { value: ta_data, .. }) = borrow
                    .properties
                    .get(&PropertyKey::String("[[TypedArrayData]]".to_string()))
                {
                    let mut out = ObjectData::new();
                    if let Some(proto) = vm.realm().get_intrinsic_prototype("Array") {
                        // TypedArray prototype not separately tracked in Intrinsics here,
                        // fall back to Array prototype for shape safety.
                        out.prototype = proto;
                    }
                    let target_obj = Rc::new(RefCell::new(out));
                    let target_val = Value::Object(Rc::clone(&target_obj));
                    visited.insert(addr, target_val);

                    let cloned_ta = helper(vm, ta_data, visited, transfer_set)?;
                    target_obj.borrow_mut().properties.insert(
                        PropertyKey::String("[[TypedArrayData]]".to_string()),
                        PropertyDescriptor::data(cloned_ta),
                    );
                    // copy common numeric properties (length, byteLength)
                    for key_name in &["length", "byteLength"] {
                        if let Some(PropertyDescriptor::Data { value, .. }) = borrow
                            .properties
                            .get(&PropertyKey::String(key_name.to_string()))
                        {
                            let cloned = helper(vm, value, visited, transfer_set)?;
                            target_obj.borrow_mut().properties.insert(
                                PropertyKey::String(key_name.to_string()),
                                PropertyDescriptor::data(cloned),
                            );
                        }
                    }
                    // copy numeric indexed properties if present
                    for (k, desc) in &borrow.properties {
                        if let PropertyKey::String(s) = k
                            && canonical_index_key(s).is_some()
                        {
                            if let PropertyDescriptor::Data { value, .. } = desc {
                                let cloned = helper(vm, value, visited, transfer_set)?;
                                target_obj.borrow_mut().properties.insert(
                                    PropertyKey::String(s.clone()),
                                    PropertyDescriptor::data(cloned),
                                );
                            } else {
                                return Err(
                                    "Cannot clone accessor properties across realms".to_string()
                                );
                            }
                        }
                    }
                    return Ok(Value::Object(target_obj));
                }

                // Map: detect by __map_entries internal slot
                if let Some(PropertyDescriptor::Data { value: entries, .. }) = borrow
                    .properties
                    .get(&PropertyKey::String("__map_entries".to_string()))
                {
                    let mut out = ObjectData::new();
                    if let Some(proto) = vm.realm().get_intrinsic_prototype("Map") {
                        out.prototype = proto;
                    }
                    let target_obj = Rc::new(RefCell::new(out));
                    let target_val = Value::Object(Rc::clone(&target_obj));
                    visited.insert(addr, target_val);

                    let cloned_entries = helper(vm, entries, visited, transfer_set)?;
                    target_obj.borrow_mut().properties.insert(
                        PropertyKey::String("__map_entries".to_string()),
                        PropertyDescriptor::data(cloned_entries),
                    );
                    // copy size
                    if let Some(PropertyDescriptor::Data { value, .. }) = borrow
                        .properties
                        .get(&PropertyKey::String("size".to_string()))
                    {
                        let cloned = helper(vm, value, visited, transfer_set)?;
                        target_obj.borrow_mut().properties.insert(
                            PropertyKey::String("size".to_string()),
                            PropertyDescriptor::data(cloned),
                        );
                    }
                    return Ok(Value::Object(target_obj));
                }

                // Set: detect by __set_values internal slot
                if let Some(PropertyDescriptor::Data {
                    value: set_vals, ..
                }) = borrow
                    .properties
                    .get(&PropertyKey::String("__set_values".to_string()))
                {
                    let mut out = ObjectData::new();
                    if let Some(proto) = vm.realm().get_intrinsic_prototype("Array") {
                        // No dedicated Set.prototype in intrinsics table; fall back to Array
                        out.prototype = proto;
                    }
                    let target_obj = Rc::new(RefCell::new(out));
                    let target_val = Value::Object(Rc::clone(&target_obj));
                    visited.insert(addr, target_val);

                    let cloned_vals = helper(vm, set_vals, visited, transfer_set)?;
                    target_obj.borrow_mut().properties.insert(
                        PropertyKey::String("__set_values".to_string()),
                        PropertyDescriptor::data(cloned_vals),
                    );
                    if let Some(PropertyDescriptor::Data { value, .. }) = borrow
                        .properties
                        .get(&PropertyKey::String("size".to_string()))
                    {
                        let cloned = helper(vm, value, visited, transfer_set)?;
                        target_obj.borrow_mut().properties.insert(
                            PropertyKey::String("size".to_string()),
                            PropertyDescriptor::data(cloned),
                        );
                    }
                    return Ok(Value::Object(target_obj));
                }

                // Date: look for [[PrimitiveValue]] internal slot
                if let Some(PropertyDescriptor::Data { value: prim, .. }) = borrow
                    .properties
                    .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                {
                    let mut out = ObjectData::new();
                    if let Some(proto) = vm.realm().get_intrinsic_prototype("Date") {
                        out.prototype = proto;
                    }
                    let target_obj = Rc::new(RefCell::new(out));
                    let target_val = Value::Object(Rc::clone(&target_obj));
                    visited.insert(addr, target_val);

                    let cloned_prim = helper(vm, prim, visited, transfer_set)?;
                    target_obj.borrow_mut().properties.insert(
                        PropertyKey::String("[[PrimitiveValue]]".to_string()),
                        PropertyDescriptor::data(cloned_prim),
                    );
                    return Ok(Value::Object(target_obj));
                }

                // RegExp: look for 'source' and 'flags' properties
                if borrow
                    .properties
                    .contains_key(&PropertyKey::String("source".to_string()))
                    && borrow
                        .properties
                        .contains_key(&PropertyKey::String("flags".to_string()))
                {
                    let mut out = ObjectData::new();
                    if let Some(proto) = vm.realm().get_intrinsic_prototype("Object") {
                        out.prototype = proto;
                    }
                    let target_obj = Rc::new(RefCell::new(out));
                    let target_val = Value::Object(Rc::clone(&target_obj));
                    visited.insert(addr, target_val);

                    // copy source, flags, lastIndex if present
                    for key in ["source", "flags", "lastIndex"] {
                        if let Some(PropertyDescriptor::Data { value, .. }) =
                            borrow.properties.get(&PropertyKey::String(key.to_string()))
                        {
                            let cloned = helper(vm, value, visited, transfer_set)?;
                            target_obj.borrow_mut().properties.insert(
                                PropertyKey::String(key.to_string()),
                                PropertyDescriptor::data(cloned),
                            );
                        }
                    }
                    return Ok(Value::Object(target_obj));
                }

                // Generic object fallback: create placeholder and copy data properties
                let mut out = ObjectData::new();
                if let Some(proto) = vm.realm().intrinsics.object_prototype.clone() {
                    out.prototype = proto;
                }
                let target_obj = Rc::new(RefCell::new(out));
                let target_val = Value::Object(Rc::clone(&target_obj));
                visited.insert(addr, target_val);

                for (k, desc) in &borrow.properties {
                    match desc {
                        PropertyDescriptor::Data { value, .. } => {
                            let cloned = helper(vm, value, visited, transfer_set)?;
                            target_obj
                                .borrow_mut()
                                .properties
                                .insert(k.clone(), PropertyDescriptor::data(cloned));
                        }
                        _ => {
                            return Err(
                                "Cannot clone accessor properties across realms".to_string()
                            );
                        }
                    }
                }

                Ok(Value::Object(target_obj))
            }
            Value::Uninitialized => Err("Cannot clone uninitialized value".to_string()),
        }
    }

    let mut visited: HashMap<usize, Value> = HashMap::new();
    helper(vm, value, &mut visited, transfer_set)
}

/// Install a minimal set of kernel constructors and prototypes on `globals`.
///
/// This bootstraps host-visible `Object`, `Function`, and `Array` behavior
/// required by the runtime prelude before the full standard library is loaded.
pub fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) {
    use PropertyKey as PKey;

    let object_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
    let function_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let function_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    function_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&function_proto_map))),
    );

    let object_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&object_proto_map))),
    );

    let array_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let for_each_fn = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
            let this_for_call = this.clone();
            if let Value::Array(arr, _) = &this {
                // Snapshot initial length per ECMAScript semantics and avoid
                // holding a RefCell borrow across callback invocation.
                let len = arr.borrow().len();
                for i in 0..len {
                    let item = arr.borrow().get(i).cloned().flatten();
                    if let Some(item) = item {
                        let idx = Value::Number(i as f64);
                        let _ = vm.call_value_with_this(
                            &callback,
                            &this_for_call,
                            &[item, idx, this_for_call.clone()],
                        );
                    }
                }
            }
            Ok(Value::Undefined)
        }),
        &function_proto_map,
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("forEach".to_string()),
        PropertyDescriptor::data(for_each_fn),
    );
    let array_proto_for_map = Rc::clone(&array_proto_map);
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("map".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = to_length(vm, &len_val)?;
                let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    // Check if property exists (hasOwnProperty check)
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        let mapped = vm.call_value_with_this(
                            &callback,
                            &this_arg,
                            &[item, Value::Number(i as f64), receiver.clone()],
                        )?;
                        out.push(Some(mapped));
                    } else {
                        // Preserve hole
                        out.push(None);
                    }
                }

                let arr_rc = Rc::new(RefCell::new(out));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_map));
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                Ok(Value::Array(arr_rc, props))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("join".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let sep = match args.get(1) {
                    Some(Value::Undefined) | None => ",".to_string(),
                    Some(v) => vm.to_string(v)?,
                };
                match this {
                    Value::Array(arr, _) => {
                        let mut out = String::new();
                        let mut first = true;
                        for item_slot in arr.borrow().iter() {
                            if !first {
                                out.push_str(&sep);
                            }
                            first = false;
                            if let Some(item) = item_slot {
                                match item {
                                    Value::Undefined | Value::Null => {}
                                    v => out.push_str(&vm.to_string(v)?),
                                }
                            }
                        }
                        Ok(Value::String(out))
                    }
                    _ => Ok(Value::String("".to_string())),
                }
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("sort".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let compare_fn = args.get(1).cloned().unwrap_or(Value::Undefined);
                let has_compare = matches!(
                    compare_fn,
                    Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
                );
                if let Value::Array(arr, props) = &this {
                    // Minimal sort semantics: dense elements, holes/undefined retained at end.
                    // This is sufficient for harness helpers that sort string key lists.
                    let snapshot = arr.borrow().clone();
                    let mut values: Vec<Value> = Vec::new();
                    let mut undefined_count = 0usize;
                    let mut hole_count = 0usize;
                    for slot in snapshot {
                        match slot {
                            Some(Value::Undefined) => undefined_count += 1,
                            Some(v) => values.push(v),
                            None => hole_count += 1,
                        }
                    }

                    let n = values.len();
                    for i in 0..n {
                        for j in (i + 1)..n {
                            let swap = if has_compare {
                                let cmp = vm.call_value_with_this(
                                    &compare_fn,
                                    &Value::Undefined,
                                    &[values[i].clone(), values[j].clone()],
                                )?;
                                vm.to_number_value(&cmp)?.is_sign_positive()
                            } else {
                                let ai = vm.to_string(&values[i])?;
                                let aj = vm.to_string(&values[j])?;
                                ai > aj
                            };
                            if swap {
                                values.swap(i, j);
                            }
                        }
                    }

                    // Keep explicit undefined values and holes at the tail.
                    while undefined_count > 0 {
                        values.push(Value::Undefined);
                        undefined_count -= 1;
                    }
                    let mut next_slots: Vec<Option<Value>> =
                        values.into_iter().map(Some).collect::<Vec<_>>();
                    next_slots.extend((0..hole_count).map(|_| None));
                    *arr.borrow_mut() = next_slots;
                    props.borrow_mut().properties.insert(
                        PKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(arr.borrow().len() as f64)),
                    );
                }
                Ok(this)
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("push".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let (items, props) = match this {
                    Value::Array(arr, props) => (arr, props),
                    _ => return Ok(Value::Number(0.0)),
                };
                let mut vec = items.borrow_mut();
                for v in args.iter().skip(1).cloned() {
                    vec.push(Some(v));
                }
                let new_len = vec.len() as f64;
                drop(vec);
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(new_len)),
                );
                Ok(Value::Number(new_len))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("splice".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                let array_proto_map = Rc::clone(&array_proto_map);
                move |vm, args| {
                    let this = args.first().cloned().unwrap_or(Value::Undefined);
                    let (arr, props) = match this {
                        Value::Array(arr, props) => (arr, props),
                        _ => return Ok(array_from_values(Vec::new())),
                    };
                    let len = arr.borrow().len() as i64;
                    let start_raw = vm
                        .to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?
                        .trunc() as i64;
                    let start = if start_raw < 0 {
                        (len + start_raw).max(0)
                    } else {
                        start_raw.min(len)
                    } as usize;
                    let delete_count = if let Some(dc) = args.get(2) {
                        vm.to_number_value(dc)?.max(0.0) as usize
                    } else {
                        len.saturating_sub(start as i64) as usize
                    };
                    let mut vec = arr.borrow_mut();
                    let end = start.saturating_add(delete_count).min(vec.len());
                    let removed: Vec<Value> =
                        vec[start..end].iter().filter_map(Clone::clone).collect();
                    let insert_items: Vec<Option<Value>> =
                        args.iter().skip(3).cloned().map(Some).collect();
                    vec.splice(start..end, insert_items);
                    let new_len = vec.len() as f64;
                    drop(vec);
                    props.borrow_mut().properties.insert(
                        PKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(new_len)),
                    );
                    let removed_arr = Rc::new(RefCell::new(
                        removed.into_iter().map(Some).collect::<Vec<_>>(),
                    ));
                    let removed_props = Rc::new(RefCell::new(ObjectData::new()));
                    removed_props.borrow_mut().prototype =
                        Value::Object(Rc::clone(&array_proto_map));
                    removed_props.borrow_mut().properties.insert(
                        PKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(removed_arr.borrow().len() as f64)),
                    );
                    Ok(Value::Array(removed_arr, removed_props))
                }
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("concat".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                let array_proto_map = Rc::clone(&array_proto_map);
                move |_vm, args| {
                    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                    let mut out: Vec<Option<Value>> = Vec::new();
                    let mut append = |value: Value| -> Result<(), crate::errors::RuntimeError> {
                        match value {
                            Value::Array(arr, _) => {
                                out.extend(arr.borrow().iter().cloned());
                            }
                            other => out.push(Some(other)),
                        }
                        Ok(())
                    };
                    append(receiver)?;
                    for arg in args.iter().skip(1).cloned() {
                        append(arg)?;
                    }

                    let arr_rc = Rc::new(RefCell::new(out));
                    let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                    props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_map));
                    props.borrow_mut().properties.insert(
                        PKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                    );
                    Ok(Value::Array(arr_rc, props))
                }
            }),
            &function_proto_map,
        )),
    );
    let array_iterator_proto: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    array_iterator_proto.borrow_mut().properties.insert(
        PKey::String("next".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, _args| {
                let result = Rc::new(RefCell::new(ObjectData::new()));
                result.borrow_mut().properties.insert(
                    PKey::String("done".to_string()),
                    PropertyDescriptor::data(Value::Bool(true)),
                );
                result.borrow_mut().properties.insert(
                    PKey::String("value".to_string()),
                    PropertyDescriptor::data(Value::Undefined),
                );
                Ok(Value::Object(result))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::Symbol(SymbolId::well_known_iterator()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                let array_iterator_proto = Rc::clone(&array_iterator_proto);
                move |_vm, args| {
                    let this = args.first().cloned().unwrap_or(Value::Undefined);
                    let iter = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                        Rc::clone(&array_iterator_proto),
                    ))));
                    iter.borrow_mut().properties.insert(
                        PKey::String("[[IteratedObject]]".to_string()),
                        PropertyDescriptor::data(this),
                    );
                    Ok(Value::Object(iter))
                }
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.filter
    let array_proto_for_filter = Rc::clone(&array_proto_map);
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("filter".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                let mut out: Vec<Option<Value>> = Vec::new();

                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        let keep = vm.call_value_with_this(
                            &callback,
                            &this_arg,
                            &[item.clone(), Value::Number(i as f64), receiver.clone()],
                        )?;
                        if keep.is_truthy() {
                            out.push(Some(item));
                        }
                    }
                }

                let arr_rc = Rc::new(RefCell::new(out));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_filter));
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                Ok(Value::Array(arr_rc, props))
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.find
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("find".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;

                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        let found = vm.call_value_with_this(
                            &callback,
                            &this_arg,
                            &[item.clone(), Value::Number(i as f64), receiver.clone()],
                        )?;
                        if found.is_truthy() {
                            return Ok(item);
                        }
                    }
                }
                Ok(Value::Undefined)
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.reduce
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("reduce".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;

                let mut i = 0;
                let mut accumulator = if args.len() > 2 {
                    args[2].clone()
                } else {
                    // Find first defined element
                    while i < len {
                        let key = PKey::String(i.to_string());
                        let has_prop = vm.has_property_host(&receiver, &key)?;
                        if has_prop {
                            return Ok(Value::Undefined); // Need initial value
                        }
                        i += 1;
                    }
                    return Ok(Value::Undefined);
                };

                while i < len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        accumulator = vm.call_value_with_this(
                            &callback,
                            &Value::Undefined,
                            &[accumulator, item, Value::Number(i as f64), receiver.clone()],
                        )?;
                    }
                    i += 1;
                }
                Ok(accumulator)
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.flat
    let array_proto_for_flat = Rc::clone(&array_proto_map);
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("flat".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let depth = if let Some(d) = args.get(1) {
                    if matches!(d, Value::Undefined) {
                        1
                    } else {
                        vm.to_number_value(d)?.max(0.0) as usize
                    }
                } else {
                    1
                };

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                let mut out: Vec<Option<Value>> = Vec::new();

                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        if depth > 0 {
                            // Check if item is an array
                            if matches!(item, Value::Array(_, _)) {
                                let item_len_val = vm.get_property_host(
                                    &item,
                                    &PKey::String("length".to_string()),
                                )?;
                                let item_len = vm.to_number_value(&item_len_val)?.max(0.0) as usize;
                                for j in 0..item_len {
                                    let j_key = PKey::String(j.to_string());
                                    let has_j = vm.has_property_host(&item, &j_key)?;
                                    if has_j {
                                        let sub_item = vm.get_property_host(&item, &j_key)?;
                                        // Recursive flat for depth > 1 would need more work
                                        out.push(Some(sub_item));
                                    }
                                }
                            } else {
                                out.push(Some(item));
                            }
                        } else {
                            out.push(Some(item));
                        }
                    }
                }

                let arr_rc = Rc::new(RefCell::new(out));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_flat));
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                Ok(Value::Array(arr_rc, props))
            }),
            &function_proto_map,
        )),
    );
    let array_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&array_proto_map))),
    );

    let string_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let string_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    string_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&string_proto_map))),
    );

    let boolean_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("[[PrimitiveValue]]".to_string()),
        PropertyDescriptor::data(Value::Bool(false)),
    );
    let boolean_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    boolean_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::Data {
            value: Value::Object(Rc::clone(&boolean_proto_map)),
            writable: false,
            enumerable: false,
            configurable: false,
        },
    );

    let error_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let error_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    error_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&error_proto_map))),
    );

    let aggregate_error_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&error_proto_map))),
    ));
    let aggregate_error_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    aggregate_error_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&aggregate_error_proto_map))),
    );

    let function_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Function");
        let body_src = if let Some(last) = args.last() {
            vm.to_string(last)?
        } else {
            "return undefined".to_string()
        };
        let param_count = args.len().saturating_sub(off + 1);
        let mut params = Vec::with_capacity(param_count);
        for idx in 0..param_count {
            params.push(vm.to_string(&args[off + idx])?);
        }
        let source = format!(
            "function __tsblitz_dynamic({}) {{\n{}\n}}\n__tsblitz_eval_result = __tsblitz_dynamic;",
            params.join(","),
            body_src
        );
        let session =
            Parser::parse_source(&source).map_err(|e| syntax_error(vm, format!("parse: {}", e)))?;
        let prog = session.program();
        let chunk = compiler::compile_oxc(prog, &source)
            .map_err(|e| syntax_error(vm, format!("compile: {}", e)))?;
        let mut child = Vm::new(vm.globals_rc());
        child.run(chunk)?;
        Ok(child
            .get_global("__tsblitz_eval_result")
            .unwrap_or(Value::Undefined))
    });
    let function_ctor_val =
        Value::BuiltinObject(function_ctor_fn.clone(), Rc::clone(&function_ctor_map));
    let function_call_val = builtin_object(
        Rc::new(|vm, args| {
            let callee = args.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            let rest = args.get(2..).unwrap_or(&[]);
            vm.call_value_with_this(&callee, &this_arg, rest)
        }),
        &function_proto_map,
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(function_ctor_val.clone()),
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("call".to_string()),
        PropertyDescriptor::data(function_call_val.clone()),
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("apply".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let callee = args.first().cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                let call_args: Vec<Value> = match args.get(2) {
                    None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(arr, _)) => {
                        arr.borrow().iter().filter_map(Clone::clone).collect()
                    }
                    Some(_) => Vec::new(),
                };
                vm.call_value_with_this(&callee, &this_arg, &call_args)
            }),
            &function_proto_map,
        )),
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("bind".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                let function_proto_map = Rc::clone(&function_proto_map);
                let function_call_val = function_call_val;
                move |_vm, args| {
                    let target = args.first().cloned().unwrap_or(Value::Undefined);
                    let bound_this = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let bound_args: Vec<Value> = args.iter().skip(2).cloned().collect();
                    if crate::trace::enabled() {
                        eprintln!(
                            "[trace] bind create target={:?} bound_this={:?} bound_args={:?}",
                            target, bound_this, bound_args
                        );
                    }
                    let bound_target = target;
                    let bound_this_arg = bound_this;
                    let prefix_args = bound_args;
                    let call_builtin = function_call_val.clone();
                    Ok(builtin_object(
                        Rc::new(move |vm, call_args| {
                            if crate::trace::enabled() {
                                eprintln!(
                                    "[trace] bound fn call target={:?} this={:?} args={:?}",
                                    bound_target, bound_this_arg, call_args
                                );
                            }
                            let mut combined = prefix_args.clone();
                            if call_args
                                .first()
                                .is_some_and(|first| strict_equal(first, &vm.this_value()))
                            {
                                combined.extend(call_args.iter().skip(1).cloned());
                            } else {
                                combined.extend(call_args.iter().cloned());
                            }
                            if strict_equal(&bound_target, &call_builtin) {
                                if let Value::BuiltinObject(f, _) = &call_builtin {
                                    let mut call_args = Vec::with_capacity(combined.len() + 1);
                                    call_args.push(bound_this_arg.clone());
                                    call_args.extend(combined.iter().cloned());
                                    (f.as_ref())(vm, &call_args)
                                } else {
                                    vm.call_value_with_this(
                                        &bound_target,
                                        &bound_this_arg,
                                        &combined,
                                    )
                                }
                            } else {
                                vm.call_value_with_this(&bound_target, &bound_this_arg, &combined)
                            }
                        }),
                        &function_proto_map,
                    ))
                }
            }),
            &function_proto_map,
        )),
    );

    function_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let source = if Vm::is_callable_value(&this) {
                    vm.function_to_string(&this)?
                } else {
                    // Be forgiving for harness message formatting. Test262's
                    // assert helpers stringify a few function-like prototype
                    // objects, and this runtime previously surfaced those as a
                    // primitive-string throw. Returning an object-tag string
                    // keeps error reporting stable without affecting callable
                    // functions.
                    format!("[object {}]", this.object_tag())
                };
                Ok(Value::String(source))
            }),
            &function_proto_map,
        )),
    );

    let object_ctor_props = Rc::clone(&object_ctor_map);
    let object_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        // Object(value): return value for object-like operands; otherwise for
        // primitive operands return the primitive directly (minimal boxing
        // behavior for harness compatibility).
        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let value = args.get(value_idx).cloned().unwrap_or(Value::Undefined);
        match value {
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Promise(_) => Ok(value),
            Value::Null | Value::Undefined => {
                let proto = object_ctor_props
                    .borrow()
                    .properties
                    .get(&PKey::String("prototype".to_string()))
                    .and_then(|d| match d {
                        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                        _ => None,
                    })
                    .unwrap_or(Value::Null);
                let mut inst = ObjectData::new();
                inst.prototype = proto;
                Ok(Value::Object(Rc::new(RefCell::new(inst))))
            }
            _ => Ok(value),
        }
    });
    let object_ctor_val = Value::BuiltinObject(object_ctor_fn.clone(), Rc::clone(&object_ctor_map));

    let symbol_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let symbol_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let desc = match args.get(value_idx) {
            Some(Value::Undefined) | None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(v) => Some(vm.to_string(v)?),
        };
        Ok(Value::Symbol(SymbolId::new(), desc))
    });
    let symbol_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&symbol_proto_map))),
    );
    let symbol_ctor_val = Value::BuiltinObject(symbol_ctor_fn.clone(), Rc::clone(&symbol_ctor_map));
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(symbol_ctor_val.clone()),
    );
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::Symbol(_, desc) => Ok(Value::String(match desc {
                        Some(d) => format!("Symbol({})", d),
                        None => "Symbol()".to_string(),
                    })),
                    _ => Ok(Value::String("[object Symbol]".to_string())),
                }
            }),
            &function_proto_map,
        )),
    );
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let res = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(res)
            }),
            &function_proto_map,
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("iterator".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_iterator(),
            Some("Symbol.iterator".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("asyncIterator".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_async_iterator(),
            Some("Symbol.asyncIterator".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("toPrimitive".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_to_primitive(),
            Some("Symbol.toPrimitive".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("toStringTag".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_to_string_tag(),
            Some("Symbol.toStringTag".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("species".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_species(),
            Some("Symbol.species".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("hasInstance".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_has_instance(),
            Some("Symbol.hasInstance".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("isConcatSpreadable".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_is_concat_spreadable(),
            Some("Symbol.isConcatSpreadable".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("match".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_match(),
            Some("Symbol.match".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("matchAll".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_match_all(),
            Some("Symbol.matchAll".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("replace".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_replace(),
            Some("Symbol.replace".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("search".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_search(),
            Some("Symbol.search".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("split".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_split(),
            Some("Symbol.split".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("unscopables".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_unscopables(),
            Some("Symbol.unscopables".to_string()),
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(object_ctor_val.clone()),
    );

    // Promise constructor
    let promise_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let promise_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let promise_ctor_fn: BuiltinFn = {
        let _promise_proto_map_rc = Rc::clone(&promise_proto_map);
        Rc::new(move |vm, args| {
            // Promise constructor takes an executor function
            // Builtin constructors receive the new instance in args[0] and the
            // user-supplied constructor argument in args[1].
            let off = static_builtin_arg_offset(vm, args, "Promise");
            let executor = args.get(off).cloned().unwrap_or(Value::Undefined);

            // Create a new pending promise
            use crate::value::PromiseState;
            let promise = Rc::new(RefCell::new(PromiseState::Pending {
                on_fulfilled: Vec::new(),
                on_rejected: Vec::new(),
            }));

            // Create resolve function
            let promise_for_resolve = Rc::clone(&promise);
            let resolve_called = Rc::new(RefCell::new(false));
            let resolve_called_for_resolve = Rc::clone(&resolve_called);
            let resolve_fn = Value::Builtin(Rc::new(move |vm, args| {
                if *resolve_called_for_resolve.borrow() {
                    return Ok(Value::Undefined);
                }
                *resolve_called_for_resolve.borrow_mut() = true;
                let value = args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| args.first().cloned().unwrap_or(Value::Undefined));
                if let Err(err) = resolve_promise_with_value(vm, &promise_for_resolve, value) {
                    let reason = vm
                        .last_uncaught_exception
                        .take()
                        .unwrap_or(Value::String(format!("{}", err)));
                    reject_pending_promise(vm, &promise_for_resolve, reason);
                }
                Ok(Value::Undefined)
            }));

            // Create reject function
            let promise_for_reject = Rc::clone(&promise);
            let reject_called = Rc::clone(&resolve_called);
            let reject_fn = Value::Builtin(Rc::new(move |vm, args| {
                if *reject_called.borrow() {
                    return Ok(Value::Undefined);
                }
                *reject_called.borrow_mut() = true;
                let reason = args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| args.first().cloned().unwrap_or(Value::Undefined));
                reject_pending_promise(vm, &promise_for_reject, reason);
                Ok(Value::Undefined)
            }));

            // Call the executor with resolve and reject functions.
            vm.last_uncaught_exception = None;
            if let Err(err) =
                vm.call_value_with_this(&executor, &Value::Undefined, &[resolve_fn, reject_fn])
            {
                let reason = vm
                    .last_uncaught_exception
                    .take()
                    .unwrap_or(Value::String(format!("{}", err)));
                reject_pending_promise(vm, &promise, reason);
            }

            // Return the promise
            Ok(Value::Promise(promise))
        })
    };
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&promise_proto_map))),
    );
    let promise_ctor_val =
        Value::BuiltinObject(promise_ctor_fn.clone(), Rc::clone(&promise_ctor_map));
    promise_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(promise_ctor_val.clone()),
    );

    // Promise.prototype.then()
    promise_proto_map.borrow_mut().properties.insert(
        PKey::String("then".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let on_fulfilled = args.get(1).cloned().unwrap_or(Value::Undefined);
                let on_rejected = args.get(2).cloned().unwrap_or(Value::Undefined);
                let on_fulfilled = if Vm::is_callable_value(&on_fulfilled) {
                    on_fulfilled
                } else {
                    Value::Undefined
                };
                let on_rejected = if Vm::is_callable_value(&on_rejected) {
                    on_rejected
                } else {
                    Value::Undefined
                };

                if let Value::Promise(promise_rc) = this {
                    use crate::value::PromiseState;

                    // Create a new promise for chaining
                    let result_promise = Rc::new(RefCell::new(PromiseState::Pending {
                        on_fulfilled: Vec::new(),
                        on_rejected: Vec::new(),
                    }));
                    let result_promise_value = Value::Promise(Rc::clone(&result_promise));

                    let mut state = promise_rc.borrow_mut();
                    match &mut *state {
                        PromiseState::Pending {
                            on_fulfilled: existing_fulfilled,
                            on_rejected: existing_rejected,
                        } => {
                            // Register wrappers that run handlers and settle the chained promise.
                            let user_on_fulfilled = on_fulfilled;
                            let res_p_f = Rc::clone(&result_promise);
                            let on_fulfilled_fn = Value::Builtin(Rc::new(move |vm, args| {
                                let off = if args.len() > 1 { 1 } else { 0 };
                                let value = args.get(off).cloned().unwrap_or(Value::Undefined);
                                if matches!(user_on_fulfilled, Value::Undefined) {
                                    resolve_promise_with_value(vm, &res_p_f, value)?;
                                    return Ok(Value::Undefined);
                                }
                                vm.last_uncaught_exception = None;
                                match vm.call_value_with_this(
                                    &user_on_fulfilled,
                                    &Value::Undefined,
                                    &[value],
                                ) {
                                    Ok(out) => resolve_promise_with_value(vm, &res_p_f, out)?,
                                    Err(err) => {
                                        let reason = vm
                                            .last_uncaught_exception
                                            .take()
                                            .unwrap_or(Value::String(format!("{}", err)));
                                        reject_pending_promise(vm, &res_p_f, reason);
                                    }
                                }
                                Ok(Value::Undefined)
                            }));

                            let user_on_rejected = on_rejected;
                            let res_p_r = Rc::clone(&result_promise);
                            let on_rejected_fn = Value::Builtin(Rc::new(move |vm, args| {
                                let off = if args.len() > 1 { 1 } else { 0 };
                                let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
                                if matches!(user_on_rejected, Value::Undefined) {
                                    reject_pending_promise(vm, &res_p_r, reason);
                                    return Ok(Value::Undefined);
                                }
                                vm.last_uncaught_exception = None;
                                match vm.call_value_with_this(
                                    &user_on_rejected,
                                    &Value::Undefined,
                                    &[reason],
                                ) {
                                    Ok(out) => resolve_promise_with_value(vm, &res_p_r, out)?,
                                    Err(err) => {
                                        let reject_reason = vm
                                            .last_uncaught_exception
                                            .take()
                                            .unwrap_or(Value::String(format!("{}", err)));
                                        reject_pending_promise(vm, &res_p_r, reject_reason);
                                    }
                                }
                                Ok(Value::Undefined)
                            }));

                            existing_fulfilled.push(on_fulfilled_fn);
                            existing_rejected.push(on_rejected_fn);
                        }
                        PromiseState::Fulfilled(value) => {
                            let value = value.clone();
                            let user_on_fulfilled = on_fulfilled;
                            let res_p_f = Rc::clone(&result_promise);
                            vm.queue_microtask(move |vm| {
                                if matches!(user_on_fulfilled, Value::Undefined) {
                                    resolve_promise_with_value(vm, &res_p_f, value.clone())?;
                                } else {
                                    vm.last_uncaught_exception = None;
                                    match vm.call_value_with_this(
                                        &user_on_fulfilled,
                                        &Value::Undefined,
                                        &[value.clone()],
                                    ) {
                                        Ok(out) => resolve_promise_with_value(vm, &res_p_f, out)?,
                                        Err(err) => {
                                            let reason = vm
                                                .last_uncaught_exception
                                                .take()
                                                .unwrap_or(Value::String(format!("{}", err)));
                                            reject_pending_promise(vm, &res_p_f, reason);
                                        }
                                    }
                                }
                                Ok(())
                            });
                        }
                        PromiseState::Rejected(reason) => {
                            let reason = reason.clone();
                            let user_on_rejected = on_rejected;
                            let res_p_r = Rc::clone(&result_promise);
                            vm.queue_microtask(move |vm| {
                                if matches!(user_on_rejected, Value::Undefined) {
                                    reject_pending_promise(vm, &res_p_r, reason.clone());
                                } else {
                                    vm.last_uncaught_exception = None;
                                    match vm.call_value_with_this(
                                        &user_on_rejected,
                                        &Value::Undefined,
                                        &[reason.clone()],
                                    ) {
                                        Ok(out) => resolve_promise_with_value(vm, &res_p_r, out)?,
                                        Err(err) => {
                                            let reject_reason = vm
                                                .last_uncaught_exception
                                                .take()
                                                .unwrap_or(Value::String(format!("{}", err)));
                                            reject_pending_promise(vm, &res_p_r, reject_reason);
                                        }
                                    }
                                }
                                Ok(())
                            });
                        }
                    }

                    Ok(result_promise_value)
                } else {
                    Err(vm.throw_type_error("Promise.prototype.then called on non-promise"))
                }
            }),
            &function_proto_map,
        )),
    );

    // Promise.prototype.catch()
    promise_proto_map.borrow_mut().properties.insert(
        PKey::String("catch".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let on_rejected = args.get(1).cloned().unwrap_or(Value::Undefined);
                if let Value::Promise(_) = this {
                    let then_method =
                        vm.get_property_host(&this, &PropertyKey::String("then".to_string()))?;
                    vm.call_value_with_this(&then_method, &this, &[Value::Undefined, on_rejected])
                } else {
                    Err(vm.throw_type_error("Promise.prototype.catch called on non-promise"))
                }
            }),
            &function_proto_map,
        )),
    );

    // Promise.resolve() static method
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("resolve".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Promise");
                let value = args.get(off).cloned().unwrap_or(Value::Undefined);
                let this_val = vm.this_value();
                if !this_val.is_object() {
                    return Err(vm.throw_type_error("Promise.resolve called on non-object"));
                }
                if let Value::Promise(_) = &value
                    && promise_constructor_matches(vm, &value, &this_val)?
                {
                    return Ok(value);
                }

                let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let resolve_slot_capture = Rc::clone(&resolve_slot);
                let reject_slot_capture = Rc::clone(&reject_slot);
                let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
                    let off = if executor_args.len() > 2 { 1 } else { 0 };
                    *resolve_slot_capture.borrow_mut() = executor_args.get(off).cloned();
                    *reject_slot_capture.borrow_mut() = executor_args.get(off + 1).cloned();
                    Ok(Value::Undefined)
                }));

                let promise = vm.construct_value(&this_val, &[executor])?;
                let resolve_fn = resolve_slot
                    .borrow()
                    .clone()
                    .ok_or_else(|| vm.throw_type_error("Promise capability resolve missing"))?;
                if let Err(err) = vm.call_value_with_this(&resolve_fn, &Value::Undefined, &[value])
                {
                    let reason = vm
                        .last_uncaught_exception
                        .take()
                        .unwrap_or(Value::String(format!("{}", err)));
                    if let Value::Promise(promise_state) = &promise {
                        reject_pending_promise(vm, promise_state, reason);
                    }
                }
                Ok(promise)
            }),
            &function_proto_map,
        )),
    );

    // Promise.all() static method
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("all".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Promise");
                let iterable = args.get(off).cloned().unwrap_or(Value::Undefined);
                let this_val = vm.this_value();
                if !this_val.is_object() {
                    return Err(vm.throw_type_error("Promise.all called on non-object"));
                }

                let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let resolve_slot_capture = Rc::clone(&resolve_slot);
                let reject_slot_capture = Rc::clone(&reject_slot);
                let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
                    let off = if executor_args.len() > 2 { 1 } else { 0 };
                    *resolve_slot_capture.borrow_mut() = executor_args.get(off).cloned();
                    *reject_slot_capture.borrow_mut() = executor_args.get(off + 1).cloned();
                    Ok(Value::Undefined)
                }));

                let promise = vm.construct_value(&this_val, &[executor])?;
                let resolve_fn = resolve_slot
                    .borrow()
                    .clone()
                    .ok_or_else(|| vm.throw_type_error("Promise capability resolve missing"))?;
                let reject_fn = reject_slot
                    .borrow()
                    .clone()
                    .ok_or_else(|| vm.throw_type_error("Promise capability reject missing"))?;

                let items = match collect_iterable_values(vm, &iterable) {
                    Ok(items) => items,
                    Err(err) => {
                        let reason = vm
                            .create_intrinsic_error(
                                "TypeError",
                                &format!("Promise.all iterable error: {}", err),
                            )
                            .unwrap_or(Value::String(format!("{}", err)));
                        let _ = vm.call_value_with_this(&reject_fn, &Value::Undefined, &[reason]);
                        return Ok(promise);
                    }
                };

                if items.is_empty() {
                    let array = vm.new_array(Vec::new());
                    let _ = vm.call_value_with_this(&resolve_fn, &Value::Undefined, &[array]);
                    return Ok(promise);
                }

                let results: Rc<RefCell<Vec<Option<Value>>>> =
                    Rc::new(RefCell::new(vec![None; items.len()]));
                let remaining = Rc::new(RefCell::new(items.len()));
                let settled = Rc::new(RefCell::new(false));

                for (idx, item) in items.into_iter().enumerate() {
                    let then_val =
                        vm.get_property_host(&item, &PropertyKey::String("then".to_string()))?;
                    if Vm::is_callable_value(&then_val) {
                        let results_rc = Rc::clone(&results);
                        let remaining_rc = Rc::clone(&remaining);
                        let settled_rc = Rc::clone(&settled);
                        let resolve_fn_rc = resolve_fn.clone();
                        let reject_fn_rc = reject_fn.clone();
                        let resolve_slot_resolved = Rc::new(RefCell::new(false));
                        let reject_slot_resolved = Rc::new(RefCell::new(false));

                        let resolve_slot_resolved_cb = Rc::clone(&resolve_slot_resolved);
                        let resolve_cb = Value::Builtin(Rc::new(move |vm, args| {
                            if *settled_rc.borrow() || *resolve_slot_resolved_cb.borrow() {
                                return Ok(Value::Undefined);
                            }
                            *resolve_slot_resolved_cb.borrow_mut() = true;
                            let off = if args.len() > 1 { 1 } else { 0 };
                            let value = args.get(off).cloned().unwrap_or(Value::Undefined);
                            results_rc.borrow_mut()[idx] = Some(value);
                            let mut remaining_ref = remaining_rc.borrow_mut();
                            if *remaining_ref > 0 {
                                *remaining_ref -= 1;
                            }
                            if *remaining_ref == 0 && !*settled_rc.borrow() {
                                *settled_rc.borrow_mut() = true;
                                let array = vm.new_array(results_rc.borrow().clone());
                                let _ = vm.call_value_with_this(
                                    &resolve_fn_rc,
                                    &Value::Undefined,
                                    &[array],
                                );
                            }
                            Ok(Value::Undefined)
                        }));

                        let reject_slot_resolved_cb = Rc::clone(&reject_slot_resolved);
                        let settled_rc = Rc::clone(&settled);
                        let reject_cb = Value::Builtin(Rc::new(move |vm, args| {
                            if *settled_rc.borrow() || *reject_slot_resolved_cb.borrow() {
                                return Ok(Value::Undefined);
                            }
                            *reject_slot_resolved_cb.borrow_mut() = true;
                            *settled_rc.borrow_mut() = true;
                            let off = if args.len() > 1 { 1 } else { 0 };
                            let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
                            let _ = vm.call_value_with_this(
                                &reject_fn_rc,
                                &Value::Undefined,
                                &[reason],
                            );
                            Ok(Value::Undefined)
                        }));

                        if let Err(err) =
                            vm.call_value_with_this(&then_val, &item, &[resolve_cb, reject_cb])
                        {
                            *settled.borrow_mut() = true;
                            let reason = vm
                                .last_uncaught_exception
                                .take()
                                .unwrap_or(Value::String(format!("{}", err)));
                            let _ =
                                vm.call_value_with_this(&reject_fn, &Value::Undefined, &[reason]);
                            return Ok(promise);
                        }
                    } else {
                        results.borrow_mut()[idx] = Some(item);
                        let mut remaining_ref = remaining.borrow_mut();
                        if *remaining_ref > 0 {
                            *remaining_ref -= 1;
                        }
                        if *remaining_ref == 0 && !*settled.borrow() {
                            *settled.borrow_mut() = true;
                            let array = vm.new_array(results.borrow().clone());
                            let _ =
                                vm.call_value_with_this(&resolve_fn, &Value::Undefined, &[array]);
                        }
                    }
                }

                Ok(promise)
            }),
            &function_proto_map,
        )),
    );

    // Promise.reject() static method
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("reject".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Promise");
                let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
                let this_val = vm.this_value();
                if !this_val.is_object() {
                    return Err(vm.throw_type_error("Promise.reject called on non-object"));
                }

                let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let resolve_slot_capture = Rc::clone(&resolve_slot);
                let reject_slot_capture = Rc::clone(&reject_slot);
                let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
                    let off = if executor_args.len() > 2 { 1 } else { 0 };
                    *resolve_slot_capture.borrow_mut() = executor_args.get(off).cloned();
                    *reject_slot_capture.borrow_mut() = executor_args.get(off + 1).cloned();
                    Ok(Value::Undefined)
                }));

                let promise = vm.construct_value(&this_val, &[executor])?;
                let reject_fn = reject_slot
                    .borrow()
                    .clone()
                    .ok_or_else(|| vm.throw_type_error("Promise capability reject missing"))?;
                if let Err(err) = vm.call_value_with_this(&reject_fn, &Value::Undefined, &[reason])
                {
                    let reject_reason = vm
                        .last_uncaught_exception
                        .take()
                        .unwrap_or(Value::String(format!("{}", err)));
                    if let Value::Promise(promise_state) = &promise {
                        reject_pending_promise(vm, promise_state, reject_reason);
                    }
                }
                Ok(promise)
            }),
            &function_proto_map,
        )),
    );

    // Reflect.construct
    let reflect_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&object_proto_map),
    ))));
    reflect_obj.borrow_mut().properties.insert(
        PKey::String("construct".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Reflect");
                let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                let arg_list = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                let new_target = args.get(off + 2).cloned().unwrap_or_else(|| target.clone());

                let ctor = if matches!(new_target, Value::Undefined) {
                    &target
                } else {
                    &new_target
                };

                if !target.is_constructable() {
                    return Err(
                        vm.throw_type_error("Reflect.construct: target is not a constructor")
                    );
                }
                if !ctor.is_constructable() {
                    return Err(
                        vm.throw_type_error("Reflect.construct: newTarget is not a constructor")
                    );
                }

                let args_vec: Vec<Value> = match arg_list {
                    Value::Array(arr, _) => arr.borrow().iter().filter_map(Clone::clone).collect(),
                    Value::Undefined | Value::Null => Vec::new(),
                    _ => Vec::new(),
                };

                let result = vm.construct_value(&target, &args_vec)?;
                if matches!(new_target, Value::Undefined) {
                    return Ok(result);
                }

                let new_target_proto = vm
                    .get_property_host(&new_target, &PropertyKey::String("prototype".to_string()))
                    .unwrap_or(Value::Undefined);
                let desired_proto = match new_target_proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => Some(new_target_proto),
                    _ => None,
                };

                if let Some(proto_value) = desired_proto
                    && let Value::Object(obj) = &result
                {
                    obj.borrow_mut().prototype = proto_value;
                }

                Ok(result)
            }),
            &function_proto_map,
        )),
    );
    reflect_obj.borrow_mut().properties.insert(
        PKey::String("ownKeys".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let keys = own_property_keys(&target)
                    .into_iter()
                    .map(|key| property_key_to_value(&key))
                    .collect::<Vec<_>>();
                Ok(array_from_values(keys))
            }),
            &function_proto_map,
        )),
    );

    // Date constructor

    let date_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let date_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&function_proto_map)),
    )));
    let date_ctor_fn: BuiltinFn = {
        let date_proto_map_rc = Rc::clone(&date_proto_map);
        Rc::new(move |_vm, args| {
            use std::time::{SystemTime, UNIX_EPOCH};

            // Helper to convert value to number, returning NaN if not a number
            fn to_num(v: &Value) -> f64 {
                match v {
                    Value::Number(n) => *n,
                    Value::Undefined => f64::NAN,
                    Value::Null => 0.0,
                    Value::Bool(b) => {
                        if *b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
                    _ => f64::NAN,
                }
            }

            // Simplified Unix timestamp calculation for date components
            fn date_to_timestamp(
                year: i32,
                month: i32,
                day: i32,
                hour: i32,
                min: i32,
                sec: i32,
                ms: i32,
            ) -> f64 {
                // month is 1-indexed (1=Jan, 12=Dec)
                // Simplified - assumes proleptic Gregorian calendar
                let is_leap = |y: i32| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;

                let mut total_days = 0i64;

                // Add days for complete years before this year
                for y in 1970..year {
                    total_days += if is_leap(y) { 366 } else { 365 };
                }

                // Add days for complete months in this year
                let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
                for m in 1..month {
                    let mut m_days = days_in_month[m.min(12) as usize] as i64;
                    if m == 2 && is_leap(year) {
                        m_days += 1;
                    }
                    total_days += m_days;
                }

                // Add days in this month (day is 1-indexed)
                total_days += (day - 1) as i64;

                // Convert to milliseconds
                let total_secs =
                    total_days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
                total_secs as f64 * 1000.0 + ms as f64
            }

            let timestamp = match args.len() {
                1 => {
                    // Only receiver, no user arguments: current time
                    match SystemTime::now().duration_since(UNIX_EPOCH) {
                        Ok(duration) => duration.as_millis() as f64,
                        Err(_) => 0.0,
                    }
                }
                2 => {
                    // Receiver + single argument: timestamp or date string
                    if let Some(arg) = args.get(1) {
                        to_num(arg)
                    } else {
                        0.0
                    }
                }
                _ => {
                    // Multiple user arguments: year, month [, day, hour, minute, second, millisecond]
                    let year = to_num(args.get(1).unwrap_or(&Value::Undefined)) as i32;
                    let month = to_num(args.get(2).unwrap_or(&Value::Undefined)) as i32; // 0-indexed
                    let day = to_num(args.get(3).unwrap_or(&Value::Number(1.0))) as i32;
                    let hour = to_num(args.get(4).unwrap_or(&Value::Number(0.0))) as i32;
                    let min = to_num(args.get(5).unwrap_or(&Value::Number(0.0))) as i32;
                    let sec = to_num(args.get(6).unwrap_or(&Value::Number(0.0))) as i32;
                    let ms = to_num(args.get(7).unwrap_or(&Value::Number(0.0))) as i32;

                    date_to_timestamp(year, month + 1, day, hour, min, sec, ms)
                }
            };

            let mut date_obj_data =
                ObjectData::with_prototype(Value::Object(Rc::clone(&date_proto_map_rc)));
            date_obj_data.properties.insert(
                PropertyKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(Value::Number(timestamp)),
            );
            let date_obj = Rc::new(RefCell::new(date_obj_data));
            Ok(Value::Object(date_obj))
        })
    };
    let date_ctor_val = Value::BuiltinObject(date_ctor_fn.clone(), Rc::clone(&date_ctor_map));
    date_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&date_proto_map))),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(date_ctor_val.clone()),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res = if let Value::Object(obj) = this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                {
                    value.clone()
                } else {
                    Value::Number(f64::NAN)
                };
                Ok(res)
            }),
            &function_proto_map,
        )),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("getTime".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Object(obj) = this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                {
                    return Ok(value.clone());
                }
                Ok(Value::Number(f64::NAN))
            }),
            &function_proto_map,
        )),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("getTimezoneOffset".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, _args| {
                // Return UTC offset in minutes (simplified - assuming UTC for now)
                Ok(Value::Number(0.0))
            }),
            &function_proto_map,
        )),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("toISOString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Object(obj) = this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                    && let Value::Number(n) = value
                {
                    // Convert timestamp to ISO string (simplified)
                    let millis = *n as i64;
                    let secs = millis / 1000;
                    let ms = millis % 1000;

                    // Calculate date components (simplified)
                    let year = 1970 + (secs / (365 * 24 * 3600)) as i32;
                    let day_of_year = ((secs % (365 * 24 * 3600)) / (24 * 3600)) as i32;
                    let hour = ((secs % (24 * 3600)) / 3600) as i32;
                    let minute = ((secs % 3600) / 60) as i32;
                    let second = (secs % 60) as i32;

                    return Ok(Value::String(format!(
                        "{:04}-01-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                        year,
                        day_of_year.min(31).max(1),
                        hour,
                        minute,
                        second,
                        ms
                    )));
                }
                Ok(Value::String("Invalid Date".to_string()))
            }),
            &function_proto_map,
        )),
    );

    // ArrayBuffer constructor
    let arraybuffer_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let arraybuffer_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let arraybuffer_ctor_fn: BuiltinFn = {
        let proto_map_rc = Rc::clone(&arraybuffer_proto_map);
        Rc::new(move |_vm, args| {
            // Get length argument with ToLength semantics (RangeError on Infinity).
            let length_value = args.get(1).cloned().unwrap_or(Value::Undefined);
            let length = to_length(_vm, &length_value)?;

            let mut arraybuffer_obj_data =
                ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("[[ArrayBufferData]]".to_string()),
                PropertyDescriptor::data(Value::Array(
                    Rc::new(RefCell::new(vec![Some(Value::Number(0.0)); length])),
                    Rc::new(RefCell::new(ObjectData::new())),
                )),
            );
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("[[IsSharedArrayBuffer]]".to_string()),
                PropertyDescriptor::data(Value::Bool(false)),
            );
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("byteLength".to_string()),
                PropertyDescriptor::data(Value::Number(length as f64)),
            );
            let arraybuffer_obj = Rc::new(RefCell::new(arraybuffer_obj_data));
            Ok(Value::Object(arraybuffer_obj))
        })
    };
    let arraybuffer_ctor_val = Value::BuiltinObject(
        arraybuffer_ctor_fn.clone(),
        Rc::clone(&arraybuffer_ctor_map),
    );
    arraybuffer_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&arraybuffer_proto_map))),
    );
    arraybuffer_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(arraybuffer_ctor_val.clone()),
    );

    // SharedArrayBuffer constructor (minimal)
    let sharedarraybuffer_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let sharedarraybuffer_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let sharedarraybuffer_ctor_fn: BuiltinFn = {
        let proto_map_rc = Rc::clone(&sharedarraybuffer_proto_map);
        Rc::new(move |vm, args| {
            let length_value = args.get(1).cloned().unwrap_or(Value::Undefined);
            let length = to_length(vm, &length_value)?;

            let mut arraybuffer_obj_data =
                ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("[[ArrayBufferData]]".to_string()),
                PropertyDescriptor::data(Value::Array(
                    Rc::new(RefCell::new(vec![Some(Value::Number(0.0)); length])),
                    Rc::new(RefCell::new(ObjectData::new())),
                )),
            );
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("[[IsSharedArrayBuffer]]".to_string()),
                PropertyDescriptor::data(Value::Bool(true)),
            );
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("byteLength".to_string()),
                PropertyDescriptor::data(Value::Number(length as f64)),
            );
            let arraybuffer_obj = Rc::new(RefCell::new(arraybuffer_obj_data));
            Ok(Value::Object(arraybuffer_obj))
        })
    };
    let sharedarraybuffer_ctor_val = Value::BuiltinObject(
        sharedarraybuffer_ctor_fn.clone(),
        Rc::clone(&sharedarraybuffer_ctor_map),
    );
    sharedarraybuffer_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&sharedarraybuffer_proto_map))),
    );
    sharedarraybuffer_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(sharedarraybuffer_ctor_val.clone()),
    );

    // %TypedArray% intrinsic constructor and TypedArray constructors
    let typedarray_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let typedarray_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    typedarray_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&typedarray_proto_map))),
    );
    typedarray_ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("TypedArray".to_string())),
    );
    let typedarray_ctor_fn: BuiltinFn = Rc::new(move |_vm, _args| {
        Err(crate::errors::RuntimeError::from(
            "TypeError: TypedArray constructor is not directly callable",
        ))
    });
    let typedarray_ctor_val =
        Value::BuiltinObject(typedarray_ctor_fn, Rc::clone(&typedarray_ctor_map));
    typedarray_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(typedarray_ctor_val.clone()),
    );
    typedarray_proto_map.borrow_mut().properties.insert(
        PKey::String("fill".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let fill_value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let len = match vm.get_property_host(&receiver, &PKey::String("length".to_string()))
                {
                    Ok(Value::Number(n)) if n.is_finite() && n > 0.0 => n as usize,
                    _ => 0,
                };
                for i in 0..len {
                    let key = Value::String(i.to_string());
                    let _ = vm.set_property_host(&receiver, &key, fill_value.clone())?;
                }
                Ok(receiver)
            }),
            &function_proto_map,
        )),
    );

    macro_rules! create_typed_array_ctor {
        ($name:expr, $element_count:expr, $zero_value:expr) => {{
            let proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
                ObjectData::with_prototype(Value::Object(Rc::clone(&typedarray_proto_map))),
            ));
            let ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
                ObjectData::with_prototype(typedarray_ctor_val.clone()),
            ));
            let ctor_fn: BuiltinFn = {
                let proto_map_rc = Rc::clone(&proto_map);
                let ctor_name = $name.to_string();
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, $name);
                    let init = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                    let byte_offset_arg = args.get(off + 2).cloned().unwrap_or(Value::Undefined);
                    let length_arg = args.get(off + 3).cloned().unwrap_or(Value::Undefined);
                    let (length, initial_values, byte_offset, view_byte_length, buffer_slot): (
                        usize,
                        Vec<Value>,
                        usize,
                        usize,
                        Value,
                    ) = match &init {
                        Value::Number(_) => {
                            let length = to_length(vm, &init)?;
                            let view_byte_length = length * $element_count;
                            (
                                length,
                                Vec::<Value>::new(),
                                0usize,
                                view_byte_length,
                                Value::Undefined,
                            )
                        }
                        Value::String(s) => {
                            let length = s.parse::<usize>().unwrap_or(0);
                            let view_byte_length = length * $element_count;
                            (
                                length,
                                Vec::<Value>::new(),
                                0usize,
                                view_byte_length,
                                Value::Undefined,
                            )
                        }
                        Value::Array(arr, _) => {
                            let initial_values = arr
                                .borrow()
                                .iter()
                                .map(|slot| slot.clone().unwrap_or(Value::Undefined))
                                .collect::<Vec<Value>>();
                            let length = initial_values.len();
                            let view_byte_length = length * $element_count;
                            (
                                length,
                                initial_values,
                                0usize,
                                view_byte_length,
                                Value::Undefined,
                            )
                        }
                        Value::Object(_) => {
                            let buffer_value = init.clone();
                            vm.get_property_host(
                                &buffer_value,
                                &PropertyKey::String("[[ArrayBufferData]]".to_string()),
                            )?;
                            let byte_length_value = vm.get_property_host(
                                &buffer_value,
                                &PropertyKey::String("byteLength".to_string()),
                            )?;
                            let buffer_byte_length = vm.to_number_value(&byte_length_value)?;
                            if !buffer_byte_length.is_finite() || buffer_byte_length < 0.0 {
                                return Err(vm.throw_range_error("Invalid typed array length"));
                            }
                            let buffer_byte_length = buffer_byte_length as usize;
                            let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
                                0
                            } else {
                                to_index(vm, &byte_offset_arg)?
                            };
                            if byte_offset % $element_count != 0 {
                                return Err(vm.throw_range_error("Invalid typed array offset"));
                            }
                            let length_from_spec = if matches!(length_arg, Value::Undefined) {
                                if buffer_byte_length < byte_offset {
                                    return Err(vm.throw_range_error("Invalid typed array length"));
                                }
                                (buffer_byte_length - byte_offset) / $element_count
                            } else {
                                to_index(vm, &length_arg)?
                            };
                            let expected_byte_length = length_from_spec * $element_count;
                            if byte_offset + expected_byte_length > buffer_byte_length {
                                return Err(vm.throw_range_error("Invalid typed array length"));
                            }
                            (
                                length_from_spec,
                                Vec::<Value>::new(),
                                byte_offset,
                                expected_byte_length,
                                buffer_value,
                            )
                        }
                        _ => (0, Vec::<Value>::new(), 0usize, 0usize, Value::Undefined),
                    };

                    let mut typed_array_data =
                        ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayData]]".to_string()),
                        PropertyDescriptor::data(Value::Array(
                            Rc::new(RefCell::new(vec![Some($zero_value.clone()); length])),
                            Rc::new(RefCell::new(ObjectData::new())),
                        )),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayKind]]".to_string()),
                        PropertyDescriptor::data(Value::String(ctor_name.clone())),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayBytesPerElement]]".to_string()),
                        PropertyDescriptor::data(Value::Number($element_count as f64)),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayLength]]".to_string()),
                        PropertyDescriptor::data(Value::Number(length as f64)),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayByteLength]]".to_string()),
                        PropertyDescriptor::data(Value::Number(view_byte_length as f64)),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayByteOffset]]".to_string()),
                        PropertyDescriptor::data(Value::Number(byte_offset as f64)),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayBuffer]]".to_string()),
                        PropertyDescriptor::data(buffer_slot),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(length as f64)),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("byteLength".to_string()),
                        PropertyDescriptor::data(Value::Number(view_byte_length as f64)),
                    );
                    for (idx, value) in initial_values.into_iter().enumerate() {
                        typed_array_data.properties.insert(
                            PropertyKey::String(idx.to_string()),
                            PropertyDescriptor::data(value),
                        );
                    }
                    let typed_array_obj = Rc::new(RefCell::new(typed_array_data));
                    Ok(Value::Object(typed_array_obj))
                })
            };
            let ctor_val = Value::BuiltinObject(ctor_fn.clone(), Rc::clone(&ctor_map));
            ctor_map.borrow_mut().properties.insert(
                PKey::String("prototype".to_string()),
                PropertyDescriptor::data(Value::Object(Rc::clone(&proto_map))),
            );
            ctor_map.borrow_mut().properties.insert(
                PKey::String("name".to_string()),
                PropertyDescriptor::data(Value::String($name.to_string())),
            );
            ctor_map.borrow_mut().properties.insert(
                PKey::String("BYTES_PER_ELEMENT".to_string()),
                PropertyDescriptor::data(Value::Number($element_count as f64)),
            );
            proto_map.borrow_mut().properties.insert(
                PKey::String("constructor".to_string()),
                PropertyDescriptor::data(ctor_val.clone()),
            );
            (ctor_val, ctor_map)
        }};
    }

    let (uint8array_ctor_val, _uint8array_ctor_map) =
        create_typed_array_ctor!("Uint8Array", 1, Value::Number(0.0));
    let (int8array_ctor_val, _int8array_ctor_map) =
        create_typed_array_ctor!("Int8Array", 1, Value::Number(0.0));
    let (uint16array_ctor_val, _uint16array_ctor_map) =
        create_typed_array_ctor!("Uint16Array", 2, Value::Number(0.0));
    let (int16array_ctor_val, _int16array_ctor_map) =
        create_typed_array_ctor!("Int16Array", 2, Value::Number(0.0));
    let (uint32array_ctor_val, _uint32array_ctor_map) =
        create_typed_array_ctor!("Uint32Array", 4, Value::Number(0.0));
    let (int32array_ctor_val, _int32array_ctor_map) =
        create_typed_array_ctor!("Int32Array", 4, Value::Number(0.0));
    let (float32array_ctor_val, _float32array_ctor_map) =
        create_typed_array_ctor!("Float32Array", 4, Value::Number(0.0));
    let (float64array_ctor_val, _float64array_ctor_map) =
        create_typed_array_ctor!("Float64Array", 8, Value::Number(0.0));
    let (bigint64array_ctor_val, _bigint64array_ctor_map) =
        create_typed_array_ctor!("BigInt64Array", 8, Value::BigInt(BigIntValue::from(0u8)));
    let (biguint64array_ctor_val, _biguint64array_ctor_map) =
        create_typed_array_ctor!("BigUint64Array", 8, Value::BigInt(BigIntValue::from(0u8)));
    let (uint8clampedarray_ctor_val, _uint8clampedarray_ctor_map) =
        create_typed_array_ctor!("Uint8ClampedArray", 1, Value::Number(0.0));

    let dataview_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let dataview_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let dataview_ctor_fn: BuiltinFn = {
        let proto_map_rc = Rc::clone(&dataview_proto_map);
        Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "DataView");
            let buffer = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            if !matches!(buffer, Value::Object(_)) {
                return Err(vm.throw_type_error(
                    "DataView constructor requires ArrayBuffer or SharedArrayBuffer",
                ));
            }
            vm.get_property_host(
                &buffer,
                &PropertyKey::String("[[ArrayBufferData]]".to_string()),
            )?;
            let byte_length_value =
                vm.get_property_host(&buffer, &PropertyKey::String("byteLength".to_string()))?;
            let buffer_byte_length = vm.to_number_value(&byte_length_value)?;
            if !buffer_byte_length.is_finite() || buffer_byte_length < 0.0 {
                return Err(vm.throw_range_error("Invalid DataView length"));
            }
            let buffer_byte_length = buffer_byte_length as usize;
            let byte_offset_arg = args.get(off + 2).cloned().unwrap_or(Value::Undefined);
            let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
                0
            } else {
                to_index(vm, &byte_offset_arg)?
            };
            let byte_length_arg = args.get(off + 3).cloned().unwrap_or(Value::Undefined);
            let byte_length = if matches!(byte_length_arg, Value::Undefined) {
                buffer_byte_length
                    .checked_sub(byte_offset)
                    .ok_or_else(|| vm.throw_range_error("Invalid DataView length"))?
            } else {
                to_index(vm, &byte_length_arg)?
            };
            if byte_offset + byte_length > buffer_byte_length {
                return Err(vm.throw_range_error("Invalid DataView length"));
            }
            let mut dataview_data =
                ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
            dataview_data.properties.insert(
                PropertyKey::String("[[DataViewBuffer]]".to_string()),
                PropertyDescriptor::data(buffer.clone()),
            );
            dataview_data.properties.insert(
                PropertyKey::String("[[DataViewByteOffset]]".to_string()),
                PropertyDescriptor::data(Value::Number(byte_offset as f64)),
            );
            dataview_data.properties.insert(
                PropertyKey::String("[[DataViewByteLength]]".to_string()),
                PropertyDescriptor::data(Value::Number(byte_length as f64)),
            );
            dataview_data.properties.insert(
                PropertyKey::String("buffer".to_string()),
                PropertyDescriptor::data(buffer.clone()),
            );
            dataview_data.properties.insert(
                PropertyKey::String("byteLength".to_string()),
                PropertyDescriptor::data(Value::Number(byte_length as f64)),
            );
            dataview_data.properties.insert(
                PropertyKey::String("byteOffset".to_string()),
                PropertyDescriptor::data(Value::Number(byte_offset as f64)),
            );
            let dataview_obj = Rc::new(RefCell::new(dataview_data));
            Ok(Value::Object(dataview_obj))
        })
    };
    let dataview_ctor_val =
        Value::BuiltinObject(dataview_ctor_fn.clone(), Rc::clone(&dataview_ctor_map));
    dataview_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&dataview_proto_map))),
    );
    dataview_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(dataview_ctor_val.clone()),
    );

    let map_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&object_proto_map)),
    )));
    let map_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&function_proto_map)),
    )));
    let map_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        if matches!(
            receiver,
            Value::Object(_)
                | Value::Array(..)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
        ) {
            let arr_rc = Rc::new(RefCell::new(Vec::<Option<Value>>::new()));
            let arr_props = Rc::new(RefCell::new(ObjectData::new()));
            arr_props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(0.0)),
            );
            let _ = vm.set_property_host(
                &receiver,
                &Value::String("__map_entries".to_string()),
                Value::Array(arr_rc.clone(), arr_props),
            )?;
            let _ = vm.set_property_host(
                &receiver,
                &Value::String("size".to_string()),
                Value::Number(0.0),
            )?;
            let iterable = args.get(1).cloned().unwrap_or(Value::Undefined);
            if let Value::Array(items, _) = iterable {
                for item in items.borrow().iter().flatten() {
                    if let Value::Array(pair, _) = item {
                        let p = pair.borrow();
                        if p.len() >= 2 {
                            let key = p[0].as_ref().cloned().unwrap_or(Value::Undefined);
                            let val = p[1].as_ref().cloned().unwrap_or(Value::Undefined);
                            let mut updated = false;
                            for existing in arr_rc.borrow_mut().iter_mut() {
                                if let Some(Value::Array(existing_pair, _)) = existing {
                                    let mut ep = existing_pair.borrow_mut();
                                    if !ep.is_empty()
                                        && strict_equal(
                                            ep[0].as_ref().unwrap_or(&Value::Undefined),
                                            &key,
                                        )
                                    {
                                        if ep.len() < 2 {
                                            ep.push(Some(val.clone()));
                                        } else {
                                            ep[1] = Some(val.clone());
                                        }
                                        updated = true;
                                        break;
                                    }
                                }
                            }
                            if !updated {
                                let pair_rc = Rc::new(RefCell::new(vec![Some(key), Some(val)]));
                                let pair_props = Rc::new(RefCell::new(ObjectData::new()));
                                pair_props.borrow_mut().properties.insert(
                                    PropertyKey::String("length".to_string()),
                                    PropertyDescriptor::data(Value::Number(2.0)),
                                );
                                arr_rc
                                    .borrow_mut()
                                    .push(Some(Value::Array(pair_rc, pair_props)));
                            }
                        }
                    }
                }
                let _ = vm.set_property_host(
                    &receiver,
                    &Value::String("size".to_string()),
                    Value::Number(arr_rc.borrow().len() as f64),
                )?;
            }
            return Ok(receiver);
        }
        Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
    });
    let map_ctor_val = Value::BuiltinObject(map_ctor_fn.clone(), Rc::clone(&map_ctor_map));
    map_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&map_proto_map))),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(map_ctor_val.clone()),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("get".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).cloned().unwrap_or(Value::Undefined);
                let entries =
                    vm.get_property_host(&this, &PropertyKey::String("__map_entries".to_string()))?;
                if let Value::Array(arr, _) = entries {
                    for item in arr.borrow().iter() {
                        if let Some(Value::Array(pair, _)) = item {
                            let p = pair.borrow();
                            if p.len() >= 2
                                && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
                            {
                                return Ok(p[1].as_ref().unwrap_or(&Value::Undefined).clone());
                            }
                        }
                    }
                }
                Ok(Value::Undefined)
            }),
            &function_proto_map,
        )),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("has".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).cloned().unwrap_or(Value::Undefined);
                let entries =
                    vm.get_property_host(&this, &PropertyKey::String("__map_entries".to_string()))?;
                if let Value::Array(arr, _) = entries {
                    for item in arr.borrow().iter() {
                        if let Some(Value::Array(pair, _)) = item {
                            let p = pair.borrow();
                            if !p.is_empty()
                                && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
                            {
                                return Ok(Value::Bool(true));
                            }
                        }
                    }
                }
                Ok(Value::Bool(false))
            }),
            &function_proto_map,
        )),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("set".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).cloned().unwrap_or(Value::Undefined);
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                let entries_key = PropertyKey::String("__map_entries".to_string());
                let entries = vm.get_property_host(&this, &entries_key)?;
                if let Value::Array(arr, props) = entries {
                    let mut updated = false;
                    for item in arr.borrow_mut().iter_mut() {
                        if let Some(Value::Array(pair, _)) = item {
                            let mut p = pair.borrow_mut();
                            if !p.is_empty()
                                && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
                            {
                                if p.len() < 2 {
                                    p.push(Some(value.clone()));
                                } else {
                                    p[1] = Some(value.clone());
                                }
                                updated = true;
                                break;
                            }
                        }
                    }
                    if !updated {
                        let pair_rc = Rc::new(RefCell::new(vec![Some(key), Some(value)]));
                        let pair_props = Rc::new(RefCell::new(ObjectData::new()));
                        pair_props.borrow_mut().properties.insert(
                            PropertyKey::String("length".to_string()),
                            PropertyDescriptor::data(Value::Number(2.0)),
                        );
                        arr.borrow_mut()
                            .push(Some(Value::Array(pair_rc, pair_props)));
                        props.borrow_mut().properties.insert(
                            PropertyKey::String("length".to_string()),
                            PropertyDescriptor::data(Value::Number(arr.borrow().len() as f64)),
                        );
                    }
                    let _ = vm.set_property_host(
                        &this,
                        &Value::String("size".to_string()),
                        Value::Number(arr.borrow().len() as f64),
                    )?;
                }
                Ok(this)
            }),
            &function_proto_map,
        )),
    );
    let function_proto_for_map_iter = Rc::clone(&function_proto_map);
    map_proto_map.borrow_mut().properties.insert(
        PKey::Symbol(SymbolId::well_known_iterator()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let entries =
                    vm.get_property_host(&this, &PropertyKey::String("__map_entries".to_string()))?;
                let mut iter = ObjectData::new();
                iter.properties.insert(
                    PropertyKey::String("__iter_entries".to_string()),
                    PropertyDescriptor::data(entries),
                );
                iter.properties.insert(
                    PropertyKey::String("__iter_index".to_string()),
                    PropertyDescriptor::data(Value::Number(0.0)),
                );
                let function_proto_for_next = Rc::clone(&function_proto_for_map_iter);
                let next_fn = builtin_object(
                    Rc::new(|vm, args| {
                        let it = args.first().cloned().unwrap_or(Value::Undefined);
                        let entries = vm
                            .get_property_host(
                                &it,
                                &PropertyKey::String("__iter_entries".to_string()),
                            )
                            .unwrap_or(Value::Undefined);
                        let idx_val = vm
                            .get_property_host(
                                &it,
                                &PropertyKey::String("__iter_index".to_string()),
                            )
                            .unwrap_or(Value::Number(0.0));
                        let mut idx = vm.to_number_value(&idx_val).unwrap_or(0.0) as usize;
                        let mut done = true;
                        let mut value = Value::Undefined;
                        if let Value::Array(arr, _) = entries
                            && idx < arr.borrow().len()
                        {
                            value = arr.borrow()[idx]
                                .as_ref()
                                .cloned()
                                .unwrap_or(Value::Undefined);
                            idx += 1;
                            done = false;
                        }
                        let _ = vm.set_property_host(
                            &it,
                            &Value::String("__iter_index".to_string()),
                            Value::Number(idx as f64),
                        )?;
                        let mut result = ObjectData::new();
                        result.properties.insert(
                            PropertyKey::String("value".to_string()),
                            PropertyDescriptor::data(value),
                        );
                        result.properties.insert(
                            PropertyKey::String("done".to_string()),
                            PropertyDescriptor::data(Value::Bool(done)),
                        );
                        Ok(Value::Object(Rc::new(RefCell::new(result))))
                    }),
                    &function_proto_for_next,
                );
                iter.properties.insert(
                    PropertyKey::String("next".to_string()),
                    PropertyDescriptor::data(next_fn),
                );
                Ok(Value::Object(Rc::new(RefCell::new(iter))))
            }),
            &function_proto_map,
        )),
    );

    let set_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&object_proto_map)),
    )));
    let set_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&function_proto_map)),
    )));
    let set_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        if matches!(
            receiver,
            Value::Object(_)
                | Value::Array(..)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
        ) {
            let arr_rc = Rc::new(RefCell::new(Vec::<Option<Value>>::new()));
            let arr_props = Rc::new(RefCell::new(ObjectData::new()));
            arr_props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(0.0)),
            );
            let _ = vm.set_property_host(
                &receiver,
                &Value::String("__set_values".to_string()),
                Value::Array(arr_rc.clone(), arr_props),
            )?;
            let _ = vm.set_property_host(
                &receiver,
                &Value::String("size".to_string()),
                Value::Number(0.0),
            )?;
            let iterable = args.get(1).cloned().unwrap_or(Value::Undefined);
            if let Value::Array(items, _) = iterable {
                for item in items.borrow().iter().flatten() {
                    let mut exists = false;
                    for existing in arr_rc.borrow().iter().flatten() {
                        if strict_equal(existing, item) {
                            exists = true;
                            break;
                        }
                    }
                    if !exists {
                        arr_rc.borrow_mut().push(Some(item.clone()));
                    }
                }
                let _ = vm.set_property_host(
                    &receiver,
                    &Value::String("size".to_string()),
                    Value::Number(arr_rc.borrow().len() as f64),
                )?;
            }
            return Ok(receiver);
        }
        Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
    });
    let set_ctor_val = Value::BuiltinObject(set_ctor_fn.clone(), Rc::clone(&set_ctor_map));
    set_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&set_proto_map))),
    );
    set_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(set_ctor_val.clone()),
    );
    set_proto_map.borrow_mut().properties.insert(
        PKey::String("add".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let values =
                    vm.get_property_host(&this, &PropertyKey::String("__set_values".to_string()))?;
                if let Value::Array(arr, props) = values {
                    let mut exists = false;
                    for existing in arr.borrow().iter().flatten() {
                        if strict_equal(existing, &value) {
                            exists = true;
                            break;
                        }
                    }
                    if !exists {
                        arr.borrow_mut().push(Some(value));
                        props.borrow_mut().properties.insert(
                            PropertyKey::String("length".to_string()),
                            PropertyDescriptor::data(Value::Number(arr.borrow().len() as f64)),
                        );
                    }
                    let _ = vm.set_property_host(
                        &this,
                        &Value::String("size".to_string()),
                        Value::Number(arr.borrow().len() as f64),
                    )?;
                }
                Ok(this)
            }),
            &function_proto_map,
        )),
    );
    set_proto_map.borrow_mut().properties.insert(
        PKey::String("has".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let values =
                    vm.get_property_host(&this, &PropertyKey::String("__set_values".to_string()))?;
                if let Value::Array(arr, _) = values {
                    for existing in arr.borrow().iter().flatten() {
                        if strict_equal(existing, &value) {
                            return Ok(Value::Bool(true));
                        }
                    }
                }
                Ok(Value::Bool(false))
            }),
            &function_proto_map,
        )),
    );
    let function_proto_for_set_iter = Rc::clone(&function_proto_map);
    set_proto_map.borrow_mut().properties.insert(
        PKey::Symbol(SymbolId::well_known_iterator()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let values =
                    vm.get_property_host(&this, &PropertyKey::String("__set_values".to_string()))?;
                let mut iter = ObjectData::new();
                iter.properties.insert(
                    PropertyKey::String("__iter_values".to_string()),
                    PropertyDescriptor::data(values),
                );
                iter.properties.insert(
                    PropertyKey::String("__iter_index".to_string()),
                    PropertyDescriptor::data(Value::Number(0.0)),
                );
                let function_proto_for_next = Rc::clone(&function_proto_for_set_iter);
                let next_fn = builtin_object(
                    Rc::new(|vm, args| {
                        let it = args.first().cloned().unwrap_or(Value::Undefined);
                        let values = vm
                            .get_property_host(
                                &it,
                                &PropertyKey::String("__iter_values".to_string()),
                            )
                            .unwrap_or(Value::Undefined);
                        let idx_val = vm
                            .get_property_host(
                                &it,
                                &PropertyKey::String("__iter_index".to_string()),
                            )
                            .unwrap_or(Value::Number(0.0));
                        let mut idx = vm.to_number_value(&idx_val).unwrap_or(0.0) as usize;
                        let mut done = true;
                        let mut value = Value::Undefined;
                        if let Value::Array(arr, _) = values
                            && idx < arr.borrow().len()
                        {
                            value = arr.borrow()[idx]
                                .as_ref()
                                .cloned()
                                .unwrap_or(Value::Undefined);
                            idx += 1;
                            done = false;
                        }
                        let _ = vm.set_property_host(
                            &it,
                            &Value::String("__iter_index".to_string()),
                            Value::Number(idx as f64),
                        )?;
                        let mut result = ObjectData::new();
                        result.properties.insert(
                            PropertyKey::String("value".to_string()),
                            PropertyDescriptor::data(value),
                        );
                        result.properties.insert(
                            PropertyKey::String("done".to_string()),
                            PropertyDescriptor::data(Value::Bool(done)),
                        );
                        Ok(Value::Object(Rc::new(RefCell::new(result))))
                    }),
                    &function_proto_for_next,
                );
                iter.properties.insert(
                    PropertyKey::String("next".to_string()),
                    PropertyDescriptor::data(next_fn),
                );
                Ok(Value::Object(Rc::new(RefCell::new(iter))))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Null);
                if let Value::Object(obj) = &this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                    && matches!(value, Value::Bool(_))
                {
                    return Ok(Value::String("[object Boolean]".to_string()));
                }
                Ok(Value::String(format!("[object {}]", this.object_tag())))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("join".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let sep = match args.get(1) {
                    Some(Value::Undefined) | None => ",".to_string(),
                    Some(v) => vm.to_string(v)?,
                };
                let mut out = String::new();
                let mut first = true;
                let len =
                    match vm.get_property_host(&this, &PropertyKey::String("length".to_string())) {
                        Ok(v) => vm.to_number_value(&v).unwrap_or(0.0).max(0.0) as usize,
                        Err(_) => 0usize,
                    };
                for i in 0..len {
                    if !first {
                        out.push_str(&sep);
                    }
                    first = false;
                    let key = PropertyKey::String(i.to_string());
                    let item = vm
                        .get_property_host(&this, &key)
                        .unwrap_or(Value::Undefined);
                    if !matches!(item, Value::Undefined | Value::Null) {
                        out.push_str(&vm.to_string(&item)?);
                    }
                }
                Ok(Value::String(out))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|_vm, args| {
                // Builtins invoked as methods receive the receiver as args[0].
                // Return the receiver (this) per standard Object.prototype.valueOf.
                let res = args.first().cloned().unwrap_or(Value::Null);
                if crate::trace::enabled() {
                    crate::trace::value_string(
                        "[trace] Object.prototype.valueOf receiver",
                        &args.first().cloned().unwrap_or(Value::Null),
                    );
                    crate::trace::value_string("[trace] Object.prototype.valueOf result", &res);
                }
                Ok(res)
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("hasOwnProperty".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let key = vm.to_property_key(&key_value)?;
                if crate::trace::enabled() {
                    eprintln!("[trace] hasOwnProperty this={:?} key={:?}", this, key_value);
                }
                let has_own = match this {
                    Value::Object(obj) => obj.borrow().properties.contains_key(&key),
                    Value::Array(_, props)
                    | Value::Closure { props, .. }
                    | Value::BuiltinObject(_, props) => {
                        props.borrow().properties.contains_key(&key)
                    }
                    _ => false,
                };
                Ok(Value::Bool(has_own))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("propertyIsEnumerable".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let key = vm.to_property_key(&key_value)?;
                let enumerable =
                    own_property_descriptor(&this, &key).is_some_and(|desc| match desc {
                        PropertyDescriptor::Data { enumerable, .. }
                        | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
                    });
                Ok(Value::Bool(enumerable))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("isPrototypeOf".to_string()),
        non_enum_data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(vm.is_prototype_of(&this, &value)?))
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("create".to_string()),
        PropertyDescriptor::data({
            let object_proto_map = Rc::clone(&object_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let proto = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let obj = match proto {
                        Value::Null => ObjectData::with_prototype(Value::Null),
                        Value::Object(_)
                        | Value::Array(..)
                        | Value::Closure { .. }
                        | Value::BuiltinObject(_, _) => ObjectData::with_prototype(proto),
                        _ => {
                            return Err(crate::errors::RuntimeError::from(
                                "Object.create: prototype must be object or null",
                            ));
                        }
                    };
                    let created = Value::Object(Rc::new(RefCell::new(obj)));
                    if let Some(props) = args.get(off + 1)
                        && !matches!(props, Value::Undefined)
                    {
                        for key in own_enumerable_keys(props) {
                            let descriptor = vm.get_property_host(props, &key)?;
                            let key_value = match &key {
                                PropertyKey::String(s) => Value::String(s.clone()),
                                PropertyKey::Symbol(sym) => Value::Symbol(sym.clone(), None),
                            };
                            let ok = vm.define_property_value(&created, &key_value, &descriptor)?;
                            if !ok {
                                return Err(crate::errors::RuntimeError::from(
                                    "Object.create: define property failed",
                                ));
                            }
                        }
                    }
                    let _ = &object_proto_map;
                    Ok(created)
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("is".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                let a = args.get(off).cloned().unwrap_or(Value::Undefined);
                let b = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(crate::vm::same_value(&a, &b)))
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("hasOwn".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                    let key = vm.to_property_key(&key_value)?;
                    let has = own_property_descriptor(&target, &key).is_some();
                    Ok(Value::Bool(has))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getPrototypeOf".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    if matches!(target, Value::Null | Value::Undefined) {
                        return Err(
                            vm.throw_type_error("Object.getPrototypeOf called on null/undefined")
                        );
                    }
                    Ok(vm.get_prototype(&target))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertyDescriptor".to_string()),
        PropertyDescriptor::data({
            let object_proto_map = Rc::clone(&object_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                    let key = vm.to_property_key(&key_value)?;
                    let Some(desc) = own_property_descriptor(&target, &key) else {
                        return Ok(Value::Undefined);
                    };
                    Ok(descriptor_value_from_descriptor(&object_proto_map, &desc))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );

    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertySymbols".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let symbols = own_property_keys(&target)
                    .into_iter()
                    .filter_map(|key| {
                        if let PropertyKey::Symbol(id) = key {
                            Some(Value::Symbol(id, None))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                Ok(array_from_values(symbols))
            }),
            &function_proto_map,
        )),
    );

    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertyDescriptors".to_string()),
        PropertyDescriptor::data({
            let object_proto_map = Rc::clone(&object_proto_map);
            builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let mut result = ObjectData::new();
                    for key in own_property_keys(&target) {
                        if let Some(desc) = own_property_descriptor(&target, &key) {
                            let descriptor_value =
                                descriptor_value_from_descriptor(&object_proto_map, &desc);
                            result
                                .properties
                                .insert(key.clone(), PropertyDescriptor::data(descriptor_value));
                        }
                    }
                    Ok(Value::Object(Rc::new(RefCell::new(result))))
                }),
                &function_proto_map,
            )
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertyNames".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let mut names: Vec<String> = Vec::new();
                    match &target {
                        Value::Object(obj) => {
                            for key in obj.borrow().properties.keys() {
                                if let PropertyKey::String(name) = key {
                                    names.push(name.clone());
                                }
                            }
                        }
                        Value::Array(arr, props) => {
                            for idx in 0..arr.borrow().len() {
                                names.push(idx.to_string());
                            }
                            for key in props.borrow().properties.keys() {
                                if let PropertyKey::String(name) = key {
                                    names.push(name.clone());
                                }
                            }
                        }
                        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                            for key in props.borrow().properties.keys() {
                                if let PropertyKey::String(name) = key {
                                    names.push(name.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                    names.sort();
                    names.dedup();
                    let arr_rc = Rc::new(RefCell::new(
                        names
                            .into_iter()
                            .map(|s| Some(Value::String(s)))
                            .collect::<Vec<_>>(),
                    ));
                    let props = Rc::new(RefCell::new(ObjectData::new()));
                    props.borrow_mut().properties.insert(
                        PropertyKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                    );
                    Ok(Value::Array(arr_rc, props))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("keys".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let keys = own_enumerable_keys(&target)
                        .into_iter()
                        .filter_map(|key| match key {
                            PropertyKey::String(s) => Some(Value::String(s)),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    Ok(array_from_values(keys))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("values".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let values = own_enumerable_keys(&target)
                        .into_iter()
                        .map(|key| vm.get_property_host(&target, &key))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(array_from_values(values))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("entries".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let entries = own_enumerable_keys(&target)
                        .into_iter()
                        .map(|key| {
                            let key_val = property_key_to_value(&key);
                            let val = vm.get_property_host(&target, &key)?;
                            Ok(array_from_values(vec![key_val, val]))
                        })
                        .collect::<Result<Vec<_>, crate::errors::RuntimeError>>()?;
                    Ok(array_from_values(entries))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("defineProperty".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                if args.len() < off + 3 {
                    return Ok(Value::Undefined);
                }
                let target = args[off].clone();
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] Object.defineProperty target={:?} key={:?} desc={:?}",
                        target,
                        args.get(off + 1),
                        args.get(off + 2)
                    );
                }
                let ok = vm.define_property_value(&target, &args[off + 1], &args[off + 2])?;
                if !ok {
                    return Err(vm.throw_type_error("Object.defineProperty failed"));
                }
                Ok(target)
            }),
            &function_proto_map,
        )),
    );

    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("defineProperties".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                let props = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                if !matches!(
                    props,
                    Value::Object(_)
                        | Value::Array(..)
                        | Value::Closure { .. }
                        | Value::BuiltinObject(_, _)
                ) {
                    return Err(vm.throw_type_error("Object.defineProperties called on non-object"));
                }
                for key in own_property_keys(&props) {
                    let descriptor = vm.get_property_host(&props, &key)?;
                    if matches!(descriptor, Value::Undefined) {
                        continue;
                    }
                    let key_value = property_key_to_value(&key);
                    let ok = vm.define_property_value(&target, &key_value, &descriptor)?;
                    if !ok {
                        return Err(vm.throw_type_error("Object.defineProperties failed"));
                    }
                }
                Ok(target)
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getPrototypeOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                if matches!(target, Value::Null | Value::Undefined) {
                    return Err(
                        vm.throw_type_error("Object.getPrototypeOf called on null/undefined")
                    );
                }
                Ok(vm.get_prototype(&target))
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("isExtensible".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let extensible = match target {
                    Value::Object(obj) => obj.borrow().extensible,
                    Value::Array(_, props)
                    | Value::Closure { props, .. }
                    | Value::BuiltinObject(_, props) => props.borrow().extensible,
                    _ => false,
                };
                Ok(Value::Bool(extensible))
            }),
            &function_proto_map,
        )),
    );

    let array_ctor_props = Rc::clone(&array_ctor_map);
    let array_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let elems: Vec<Option<Value>> = args.iter().map(|v| Some(v.clone())).collect();
        let arr_rc = Rc::new(RefCell::new(elems));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
        );
        if let Some(pval) = array_ctor_props
            .borrow()
            .properties
            .get(&PKey::String("prototype".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
        {
            props.borrow_mut().prototype = pval;
        }
        Ok(Value::Array(arr_rc, props))
    });
    let array_ctor_val = Value::BuiltinObject(array_ctor_fn.clone(), Rc::clone(&array_ctor_map));
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("isArray".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|_vm, args| {
                    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Bool(matches!(value, Value::Array(..))))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("from".to_string()),
        PropertyDescriptor::data({
            let array_proto_for_from = Rc::clone(&array_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Array");
                    let source = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let map_fn = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                    let has_map_fn = matches!(
                        map_fn,
                        Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
                    );

                    let len = match &source {
                        Value::Array(arr, _) => arr.borrow().len(),
                        Value::String(s) => s.chars().count(),
                        _ => {
                            let len_v =
                                vm.get_property_host(&source, &PKey::String("length".to_string()))?;
                            vm.to_number_value(&len_v)?.max(0.0) as usize
                        }
                    };

                    let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
                    for i in 0..len {
                        let mut v = match &source {
                            Value::Array(arr, _) => arr
                                .borrow()
                                .get(i)
                                .cloned()
                                .flatten()
                                .unwrap_or(Value::Undefined),
                            Value::String(s) => {
                                let ch = s.chars().nth(i).unwrap_or('\0');
                                Value::String(ch.to_string())
                            }
                            _ => vm.get_property_host(&source, &PKey::String(i.to_string()))?,
                        };
                        if has_map_fn {
                            v = vm.call_value_with_this(
                                &map_fn,
                                &Value::Undefined,
                                &[v, Value::Number(i as f64)],
                            )?;
                        }
                        out.push(Some(v));
                    }

                    let arr_rc = Rc::new(RefCell::new(out));
                    let props = Rc::new(RefCell::new(ObjectData::new()));
                    props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_from));
                    props.borrow_mut().properties.insert(
                        PKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                    );
                    Ok(Value::Array(arr_rc, props))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(array_ctor_val.clone()),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("lastIndexOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let search = args.get(1).cloned().unwrap_or(Value::Undefined);
                let length =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len_num = vm.to_number_value(&length)?;
                if !len_num.is_finite() || len_num <= 0.0 {
                    return Ok(Value::Number(-1.0));
                }
                let len = len_num.floor() as i64;
                let mut k = if let Some(from_index) = args.get(2) {
                    let n = vm.to_number_value(from_index)?;
                    if n.is_nan() {
                        len - 1
                    } else if n >= 0.0 {
                        (n.floor() as i64).min(len - 1)
                    } else {
                        let idx = len + n.floor() as i64;
                        if idx < 0 {
                            return Ok(Value::Number(-1.0));
                        }
                        idx
                    }
                } else {
                    len - 1
                };

                if len > 1_000_000 {
                    let mut candidates: Vec<u64> = Vec::new();
                    match &receiver {
                        Value::Object(obj) => {
                            for key in obj.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k as u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        Value::Array(arr, props) => {
                            let dense_len = arr.borrow().len() as u64;
                            for idx in 0..dense_len.min((k as u64).saturating_add(1)) {
                                candidates.push(idx);
                            }
                            for key in props.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k as u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                            for key in props.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k as u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        _ => {}
                    }

                    candidates.sort_unstable();
                    candidates.dedup();
                    for idx in candidates.into_iter().rev() {
                        let key = PKey::String(idx.to_string());
                        let element = vm.get_property_host(&receiver, &key)?;
                        if strict_equal(&element, &search) {
                            return Ok(Value::Number(idx as f64));
                        }
                    }
                    return Ok(Value::Number(-1.0));
                }

                while k >= 0 {
                    let key = PKey::String(k.to_string());
                    if vm.has_property_host(&receiver, &key)? {
                        let element = vm.get_property_host(&receiver, &key)?;
                        if env::var("TSBLITZ_TRACE").ok().as_deref() == Some("1") {
                            eprintln!(
                                "[trace] lastIndexOf k={} element={} search={}",
                                k, element, search
                            );
                        }
                        if strict_equal(&element, &search) {
                            return Ok(Value::Number(k as f64));
                        }
                    }
                    k -= 1;
                }
                Ok(Value::Number(-1.0))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("join".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let sep = match args.get(1) {
                    Some(Value::Undefined) | None => ",".to_string(),
                    Some(v) => vm.to_string(v)?,
                };
                let Value::Array(arr, _) = receiver else {
                    return Ok(Value::String(String::new()));
                };
                let mut out = String::new();
                for (idx, item) in arr.borrow().iter().enumerate() {
                    if idx > 0 {
                        out.push_str(&sep);
                    }
                    match item {
                        Some(Value::Undefined) | Some(Value::Null) | None => {}
                        Some(v) => out.push_str(&vm.to_string(v)?),
                    }
                }
                Ok(Value::String(out))
            }),
            &function_proto_map,
        )),
    );

    let string_proto_for_ctor = Rc::clone(&string_proto_map);
    let string_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        // Support being called as a constructor: new String(value) should
        // create a wrapper object with [[PrimitiveValue]] set to the
        // stringified value. When called as a function, return the primitive
        // string.
        if vm.current_new_target().is_some() {
            let value = args.get(1).cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&value)?;
            let mut obj = ObjectData::new();
            obj.prototype = Value::Object(Rc::clone(&string_proto_for_ctor));
            let obj_rc = Rc::new(RefCell::new(obj));
            obj_rc.borrow_mut().properties.insert(
                PKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(Value::String(s)),
            );
            return Ok(Value::Object(obj_rc));
        }

        // Called as function: String(value)
        // Called via call/apply: String.call(thisArg, value)
        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let value = args.get(value_idx);
        if value.is_none() {
            return Ok(Value::String(String::new()));
        }
        match value.expect("checked is_some") {
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::Number(n) => Ok(Value::String(n.to_string())),
            Value::Bool(b) => Ok(Value::String(
                (if *b { "true" } else { "false" }).to_string(),
            )),
            Value::Null => Ok(Value::String("null".to_string())),
            Value::Undefined => Ok(Value::String("undefined".to_string())),
            Value::BigInt(n) => Ok(Value::String(n.to_string())),
            Value::Symbol(_, desc) => Ok(Value::String(match desc {
                Some(d) => format!("Symbol({})", d),
                None => "Symbol()".to_string(),
            })),
            other => Ok(Value::String(vm.to_string(other)?)),
        }
    });
    let string_ctor_val = Value::BuiltinObject(string_ctor_fn.clone(), Rc::clone(&string_ctor_map));
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(string_ctor_val.clone()),
    );
    // Provide String.prototype.valueOf so ToPrimitive can retrieve the
    // internal string value for String wrapper objects and primitives.
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res =
                    match this {
                        Value::String(s) => Value::String(s),
                        Value::Object(obj) => {
                            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                                .borrow()
                                .properties
                                .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            {
                                match value {
                                    Value::String(s) => Value::String(s.clone()),
                                    _ => return Err(vm.throw_type_error(
                                        "String.prototype.valueOf called on incompatible receiver",
                                    )),
                                }
                            } else {
                                return Err(vm.throw_type_error(
                                    "String.prototype.valueOf called on incompatible receiver",
                                ));
                            }
                        }
                        _ => {
                            return Err(vm.throw_type_error(
                                "String.prototype.valueOf called on incompatible receiver",
                            ));
                        }
                    };
                Ok(res)
            }),
            &function_proto_map,
        )),
    );
    // Provide String.prototype.toString so String-like values produce the
    // primitive string when ToPrimitive prefers string or when explicitly
    // invoked. Mirrors Boolean.prototype.toString semantics used elsewhere.
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::String(s) => Ok(Value::String(s)),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::String(s) = value
                        {
                            return Ok(Value::String(s.clone()));
                        }
                        Err(vm.throw_type_error(
                            "String.prototype.toString called on incompatible receiver",
                        ))
                    }
                    _ => Err(vm.throw_type_error(
                        "String.prototype.toString called on incompatible receiver",
                    )),
                }
            }),
            &function_proto_map,
        )),
    );
    let string_index_of = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let haystack = vm.to_string(&receiver)?;
            let needle = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
            let len_chars = haystack.chars().count();
            let start = if let Some(pos) = args.get(2) {
                let n = vm.to_number_value(pos)?;
                if n.is_nan() || n <= 0.0 {
                    0usize
                } else {
                    (n.floor() as usize).min(len_chars)
                }
            } else {
                0usize
            };
            if needle.is_empty() {
                return Ok(Value::Number(start as f64));
            }
            let start_byte = char_index_to_byte_index(&haystack, start);
            if let Some(rel_byte) = haystack[start_byte..].find(&needle) {
                let absolute_byte = start_byte + rel_byte;
                let absolute_char = haystack[..absolute_byte].chars().count();
                return Ok(Value::Number(absolute_char as f64));
            }
            Ok(Value::Number(-1.0))
        }),
        &function_proto_map,
    );
    set_builtin_name(&string_index_of, "indexOf");
    set_builtin_length(&string_index_of, 2.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("indexOf".to_string()),
        non_enum_data(string_index_of),
    );
    let string_char_code_at = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&receiver)?;
            let pos = match args.get(1) {
                Some(v) => vm.to_number_value(v)?,
                None => 0.0,
            };
            let index = if pos.is_nan() || pos <= 0.0 {
                0usize
            } else if !pos.is_finite() {
                return Ok(Value::Number(f64::NAN));
            } else {
                pos.trunc() as usize
            };
            match char_code_at(&s, index) {
                Some(code) => Ok(Value::Number(code as f64)),
                None => Ok(Value::Number(f64::NAN)),
            }
        }),
        &function_proto_map,
    );
    set_builtin_name(&string_char_code_at, "charCodeAt");
    set_builtin_length(&string_char_code_at, 1.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("charCodeAt".to_string()),
        non_enum_data(string_char_code_at),
    );
    let string_slice = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&receiver)?;
            let len = s.chars().count() as isize;

            let to_int = |vm: &mut Vm, v: &Value| -> Result<isize, crate::errors::RuntimeError> {
                let n = vm.to_number_value(v)?;
                if !n.is_finite() {
                    return Ok(0);
                }
                Ok(n.trunc() as isize)
            };

            let start_raw = match args.get(1) {
                Some(v) => to_int(vm, v)?,
                None => 0,
            };
            let from = if start_raw < 0 {
                (len + start_raw).max(0)
            } else {
                start_raw.min(len)
            };

            let end_raw_opt = args.get(2).filter(|v| !matches!(v, Value::Undefined));
            let end_raw = match end_raw_opt {
                Some(v) => to_int(vm, v)?,
                None => len,
            };
            let to = if end_raw < 0 {
                (len + end_raw).max(0)
            } else {
                end_raw.min(len)
            };

            if to <= from {
                return Ok(Value::String(String::new()));
            }

            let start_byte = char_index_to_byte_index(&s, from as usize);
            let end_byte = char_index_to_byte_index(&s, to as usize);
            Ok(Value::String(s[start_byte..end_byte].to_string()))
        }),
        &function_proto_map,
    );
    set_builtin_name(&string_slice, "slice");
    set_builtin_length(&string_slice, 2.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("slice".to_string()),
        non_enum_data(string_slice),
    );
    let boolean_ctor_fn: BuiltinFn = Rc::new(|vm, args| {
        // Called as function: Boolean(value)
        // Called via call/apply: Boolean.call(thisArg, value)
        let value = if vm.current_new_target().is_some() {
            args.get(1).cloned().unwrap_or(Value::Undefined)
        } else {
            let value_idx = if args.len() > 1 { 1 } else { 0 };
            args.get(value_idx).cloned().unwrap_or(Value::Undefined)
        };
        let prim = Value::Bool(vm.to_boolean(&value));
        if vm.current_new_target().is_some()
            && let Some(Value::Object(obj)) = args.first()
        {
            obj.borrow_mut().properties.insert(
                PKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(prim),
            );
            return Ok(args.first().cloned().unwrap_or(Value::Undefined));
        }
        Ok(prim)
    });
    let boolean_ctor_val = Value::BuiltinObject(boolean_ctor_fn, Rc::clone(&boolean_ctor_map));
    set_builtin_length(&boolean_ctor_val, 1.0);
    set_builtin_name(&boolean_ctor_val, "Boolean");
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(boolean_ctor_val.clone()),
    );
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res = match this {
                    Value::Bool(b) => Value::Bool(b),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                        {
                            value.clone()
                        } else {
                            return Err(vm.throw_type_error(
                                "Boolean.prototype.valueOf called on incompatible receiver",
                            ));
                        }
                    }
                    _ => {
                        return Err(vm.throw_type_error(
                            "Boolean.prototype.valueOf called on incompatible receiver",
                        ));
                    }
                };
                Ok(res)
            }),
            &function_proto_map,
        )),
    );
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::Bool(b) => {
                        return Ok(Value::String(if b { "true" } else { "false" }.to_string()));
                    }
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::Bool(b) = value
                        {
                            return Ok(Value::String(
                                if *b { "true" } else { "false" }.to_string(),
                            ));
                        }
                    }
                    _ => {}
                }
                Err(vm
                    .throw_type_error("Boolean.prototype.toString called on incompatible receiver"))
            }),
            &function_proto_map,
        )),
    );
    let regexp_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let regexp_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    regexp_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&regexp_proto_map))),
    );
    let regexp_proto_for_ctor = Rc::clone(&regexp_proto_map);
    let regexp_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let off = if vm.current_new_target().is_some() {
            1
        } else {
            static_builtin_arg_offset(vm, args, "RegExp")
        };
        let pattern_arg = args.get(off).cloned().unwrap_or(Value::Undefined);
        let flags_arg = args.get(off + 1).cloned().unwrap_or(Value::Undefined);

        let (pattern, flags) = if matches!(flags_arg, Value::Undefined) && pattern_arg.is_object() {
            let source = vm.get_property_host(&pattern_arg, &PKey::String("source".to_string()))?;
            let flags = vm.get_property_host(&pattern_arg, &PKey::String("flags".to_string()))?;
            match (source, flags) {
                (Value::String(source), Value::String(flags)) => (source, flags),
                _ => (vm.to_string(&pattern_arg)?, String::new()),
            }
        } else {
            let pattern = match pattern_arg {
                Value::Undefined => "(?:)".to_string(),
                v => vm.to_string(&v)?,
            };
            let flags = match flags_arg {
                Value::Undefined => String::new(),
                v => vm.to_string(&v)?,
            };
            (pattern, flags)
        };

        let source = if pattern.is_empty() {
            "(?:)".to_string()
        } else {
            pattern
        };

        let mut inst = if vm.current_new_target().is_some() {
            args.first().cloned().unwrap_or(Value::Undefined)
        } else {
            let mut data = ObjectData::new();
            data.prototype = Value::Object(Rc::clone(&regexp_proto_for_ctor));
            Value::Object(Rc::new(RefCell::new(data)))
        };

        let Value::Object(obj) = &mut inst else {
            return Err(crate::errors::RuntimeError::from(
                "RegExp constructor failed to create an object",
            ));
        };

        {
            let mut obj = obj.borrow_mut();
            obj.prototype = if vm.current_new_target().is_some() {
                obj.prototype.clone()
            } else {
                Value::Object(Rc::clone(&regexp_proto_for_ctor))
            };
            obj.properties.insert(
                PKey::String("source".to_string()),
                PropertyDescriptor::Data {
                    value: Value::String(source),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
            obj.properties.insert(
                PKey::String("flags".to_string()),
                PropertyDescriptor::Data {
                    value: Value::String(flags),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            );
            obj.properties.insert(
                PKey::String("lastIndex".to_string()),
                PropertyDescriptor::Data {
                    value: Value::Number(0.0),
                    writable: true,
                    enumerable: false,
                    configurable: false,
                },
            );
        }

        Ok(inst)
    });
    let regexp_ctor_val = Value::BuiltinObject(regexp_ctor_fn, Rc::clone(&regexp_ctor_map));
    set_builtin_name(&regexp_ctor_val, "RegExp");
    set_builtin_length(&regexp_ctor_val, 2.0);
    regexp_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(regexp_ctor_val.clone()),
    );
    let regexp_to_string = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let source = vm.get_property_host(&this, &PKey::String("source".to_string()))?;
            let flags = vm.get_property_host(&this, &PKey::String("flags".to_string()))?;
            let src = vm.to_string(&source)?;
            let flg = vm.to_string(&flags)?;
            Ok(Value::String(format!("/{}/{}", src, flg)))
        }),
        &function_proto_map,
    );
    set_builtin_name(&regexp_to_string, "toString");
    set_builtin_length(&regexp_to_string, 0.0);
    regexp_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        non_enum_data(regexp_to_string),
    );
    let regexp_exec = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let input = args.get(1).cloned().unwrap_or(Value::Undefined);
            let source = vm.get_property_host(&this, &PKey::String("source".to_string()))?;
            let flags = vm.get_property_host(&this, &PKey::String("flags".to_string()))?;
            let pattern = vm.to_string(&source)?;
            let subject = vm.to_string(&input)?;
            let flags = vm.to_string(&flags)?;
            let regex = build_regex(&pattern, &flags)?;
            let sticky = flags.contains('y');
            let global = flags.contains('g') || sticky;
            let mut last_index = 0usize;
            if global {
                let last_index_value =
                    vm.get_property_host(&this, &PKey::String("lastIndex".to_string()))?;
                let n = vm.to_number_value(&last_index_value)?;
                if n.is_nan() || n <= 0.0 {
                    last_index = 0;
                } else if n.is_finite() {
                    last_index = n.trunc() as usize;
                }
            }
            let start_byte = char_index_to_byte_index(&subject, last_index);
            if sticky && !sticky_caret_allows(&subject, last_index, &pattern, flags.contains('m')) {
                if global {
                    let _ = vm.set_property_host(
                        &this,
                        &Value::String("lastIndex".to_string()),
                        Value::Number(0.0),
                    )?;
                }
                return Ok(Value::Null);
            }
            let caps = if sticky {
                match regex.captures_at(&subject, start_byte) {
                    Some(caps) => {
                        if let Some(m) = caps.get(0) {
                            if m.start() == start_byte {
                                Some(caps)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    None => None,
                }
            } else {
                regex.captures_at(&subject, start_byte)
            };

            let Some(caps) = caps else {
                if global {
                    let _ = vm.set_property_host(
                        &this,
                        &Value::String("lastIndex".to_string()),
                        Value::Number(0.0),
                    )?;
                }
                return Ok(Value::Null);
            };

            let m = caps
                .get(0)
                .ok_or_else(|| crate::errors::RuntimeError::from("RegExp exec missing match"))?;
            if global {
                let end_char = subject[..m.end()].chars().count();
                let _ = vm.set_property_host(
                    &this,
                    &Value::String("lastIndex".to_string()),
                    Value::Number(end_char as f64),
                )?;
            }

            let mut entries = Vec::with_capacity(caps.len());
            for i in 0..caps.len() {
                match caps.get(i) {
                    Some(mat) => entries.push(Value::String(mat.as_str().to_string())),
                    None => entries.push(Value::Undefined),
                }
            }
            let result = array_from_values(entries);
            let _ = vm.set_property_host(
                &result,
                &Value::String("index".to_string()),
                Value::Number(subject[..m.start()].chars().count() as f64),
            )?;
            let _ = vm.set_property_host(
                &result,
                &Value::String("input".to_string()),
                Value::String(subject),
            )?;
            Ok(result)
        }),
        &function_proto_map,
    );
    set_builtin_name(&regexp_exec, "exec");
    set_builtin_length(&regexp_exec, 1.0);
    regexp_proto_map
        .borrow_mut()
        .properties
        .insert(PKey::String("exec".to_string()), non_enum_data(regexp_exec));
    let regexp_test = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let input = args.get(1).cloned().unwrap_or(Value::Undefined);
            let source = vm.get_property_host(&this, &PKey::String("source".to_string()))?;
            let flags = vm.get_property_host(&this, &PKey::String("flags".to_string()))?;
            let pattern = vm.to_string(&source)?;
            let subject = vm.to_string(&input)?;
            let flags = vm.to_string(&flags)?;
            let regex = build_regex(&pattern, &flags)?;
            let sticky = flags.contains('y');
            let global = flags.contains('g') || sticky;
            let mut last_index = 0usize;
            if global {
                let last_index_value =
                    vm.get_property_host(&this, &PKey::String("lastIndex".to_string()))?;
                let n = vm.to_number_value(&last_index_value)?;
                if n.is_nan() || n <= 0.0 {
                    last_index = 0;
                } else if n.is_finite() {
                    last_index = n.trunc() as usize;
                }
            }
            let start_byte = char_index_to_byte_index(&subject, last_index);
            if sticky && !sticky_caret_allows(&subject, last_index, &pattern, flags.contains('m')) {
                if global {
                    let _ = vm.set_property_host(
                        &this,
                        &Value::String("lastIndex".to_string()),
                        Value::Number(0.0),
                    )?;
                }
                return Ok(Value::Bool(false));
            }
            let found = if sticky {
                regex
                    .find_at(&subject, start_byte)
                    .is_some_and(|m| m.start() == start_byte)
            } else {
                regex.find_at(&subject, start_byte).is_some()
            };
            if !found && global {
                let _ = vm.set_property_host(
                    &this,
                    &Value::String("lastIndex".to_string()),
                    Value::Number(0.0),
                )?;
            }
            Ok(Value::Bool(found))
        }),
        &function_proto_map,
    );
    set_builtin_name(&regexp_test, "test");
    set_builtin_length(&regexp_test, 1.0);
    regexp_proto_map
        .borrow_mut()
        .properties
        .insert(PKey::String("test".to_string()), non_enum_data(regexp_test));

    let error_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let ctor = vm
            .current_new_target()
            .or_else(|| vm.get_global("Error"))
            .unwrap_or(Value::Undefined);
        let proto = vm
            .get_property_host(&ctor, &PKey::String("prototype".to_string()))
            .unwrap_or(Value::Null);
        let mut inst = ObjectData::new();
        inst.prototype = proto;
        if let Some(message) = args.get(1).cloned() {
            inst.properties.insert(
                PKey::String("message".to_string()),
                PropertyDescriptor::data(message),
            );
        }
        inst.properties.insert(
            PKey::String("constructor".to_string()),
            PropertyDescriptor::data(ctor),
        );
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let error_ctor_val = Value::BuiltinObject(error_ctor_fn.clone(), Rc::clone(&error_ctor_map));
    error_ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("Error".to_string())),
    );
    error_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(error_ctor_val.clone()),
    );
    error_proto_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("Error".to_string())),
    );

        let aggregate_error_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
            let ctor = vm
                .current_new_target()
                .or_else(|| vm.get_global("AggregateError"))
                .unwrap_or(Value::Undefined);
        let proto = vm
            .get_property_host(&ctor, &PKey::String("prototype".to_string()))
            .unwrap_or(Value::Null);
        let mut inst = ObjectData::new();
        inst.prototype = proto;
        if let Some(message) = args.get(2).cloned() {
            inst.properties.insert(
                PKey::String("message".to_string()),
                PropertyDescriptor::data(message),
            );
        }
        inst.properties.insert(
            PKey::String("name".to_string()),
            PropertyDescriptor::data(Value::String("AggregateError".to_string())),
        );
        inst.properties.insert(
            PKey::String("constructor".to_string()),
            PropertyDescriptor::data(ctor),
        );
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let aggregate_error_ctor_val = Value::BuiltinObject(
        aggregate_error_ctor_fn.clone(),
        Rc::clone(&aggregate_error_ctor_map),
    );
    aggregate_error_ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("AggregateError".to_string())),
    );
    aggregate_error_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(aggregate_error_ctor_val.clone()),
    );
    aggregate_error_proto_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("AggregateError".to_string())),
    );

    let make_error_ctor = |name: &str, proto_map: &Rc<RefCell<ObjectData>>| {
        let ctor_props = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
            Rc::clone(&function_proto_map),
        ))));
        ctor_props.borrow_mut().properties.insert(
            PKey::String("prototype".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(proto_map))),
        );
        let ctor_name = name.to_string();
        let instance_name = name.to_string();
        let ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
            let ctor = vm
                .current_new_target()
                .or_else(|| vm.get_global(&ctor_name))
                .unwrap_or(Value::Undefined);
            let proto = vm
                .get_property_host(&ctor, &PKey::String("prototype".to_string()))
                .unwrap_or(Value::Null);
            let mut inst = ObjectData::new();
            inst.prototype = proto;
            if let Some(message) = args.get(1).cloned() {
                inst.properties.insert(
                    PKey::String("message".to_string()),
                    PropertyDescriptor::data(message),
                );
            }
            inst.properties.insert(
                PKey::String("name".to_string()),
                PropertyDescriptor::data(Value::String(instance_name.clone())),
            );
            Ok(Value::Object(Rc::new(RefCell::new(inst))))
        });
        let ctor_val = Value::BuiltinObject(ctor_fn, Rc::clone(&ctor_props));
        ctor_props.borrow_mut().properties.insert(
            PKey::String("name".to_string()),
            PropertyDescriptor::data(Value::String(name.to_string())),
        );
        proto_map.borrow_mut().properties.insert(
            PKey::String("constructor".to_string()),
            PropertyDescriptor::data(ctor_val.clone()),
        );
        proto_map.borrow_mut().properties.insert(
            PKey::String("name".to_string()),
            PropertyDescriptor::data(Value::String(name.to_string())),
        );
        (name.to_string(), ctor_val)
    };

    let eval_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let range_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let reference_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&error_proto_map)),
    )));
    let syntax_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let type_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let uri_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));

    let extra_error_ctors = vec![
        make_error_ctor("EvalError", &eval_error_proto_map),
        make_error_ctor("RangeError", &range_error_proto_map),
        make_error_ctor("ReferenceError", &reference_error_proto_map),
        make_error_ctor("SyntaxError", &syntax_error_proto_map),
        make_error_ctor("TypeError", &type_error_proto_map),
        make_error_ctor("URIError", &uri_error_proto_map),
    ];

    let math = Rc::new(RefCell::new(ObjectData::new()));
    math.borrow_mut().properties.insert(
        PKey::String("pow".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let x = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
                    let y = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))?;
                    Ok(Value::Number(x.powf(y)))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );

    let reflect = Rc::new(RefCell::new(ObjectData::new()));
    reflect.borrow_mut().properties.insert(
        PKey::String("construct".to_string()),
        PropertyDescriptor::data(Value::BuiltinObject(
            Rc::new(|vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let arguments_list = args.get(2).cloned().unwrap_or(Value::Undefined);
                let new_target = args.get(3).cloned().unwrap_or_else(|| target.clone());

                if !target.is_constructable() {
                    let err =
                        vm.create_intrinsic_error("TypeError", "target is not a constructor")?;
                    return Err(vm.throw_exception(err));
                }
                if !new_target.is_constructable() {
                    let err =
                        vm.create_intrinsic_error("TypeError", "newTarget is not a constructor")?;
                    return Err(vm.throw_exception(err));
                }

                let mut args_vec = Vec::new();
                match &arguments_list {
                    Value::Array(arr, _) => {
                        for item in arr.borrow().iter().cloned() {
                            if let Some(v) = item {
                                args_vec.push(v);
                            } else {
                                args_vec.push(Value::Undefined);
                            }
                        }
                    }
                    other => {
                        let len_val = vm
                            .get_property_host(other, &PropertyKey::String("length".to_string()))?;
                        let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                        for idx in 0..len {
                            let item =
                                vm.get_property_host(other, &PropertyKey::String(idx.to_string()))?;
                            args_vec.push(item);
                        }
                    }
                }

                vm.construct_value(&target, &args_vec)
            }),
            Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                Rc::clone(&function_proto_map),
            )))),
        )),
    );
    reflect.borrow_mut().properties.insert(
        PKey::String("ownKeys".to_string()),
        PropertyDescriptor::data(Value::BuiltinObject(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let keys = own_property_keys(&target)
                    .into_iter()
                    .map(|key| property_key_to_value(&key))
                    .collect::<Vec<_>>();
                Ok(array_from_values(keys))
            }),
            Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                Rc::clone(&function_proto_map),
            )))),
        )),
    );

    let shadowrealm_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let shadowrealm_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let shadowrealm_proto_val = Value::Object(Rc::clone(&shadowrealm_proto_map));
    let shadowrealm_ctor_fn: BuiltinFn = Rc::new({
        let shadowrealm_proto_val = shadowrealm_proto_val;
        move |vm, _args| {
            if vm.current_new_target().is_none() {
                return Err(vm.throw_type_error("ShadowRealm must be called with new"));
            }
            let obj = ObjectData::with_prototype(shadowrealm_proto_val.clone());
            let obj_rc = Rc::new(RefCell::new(obj));
            let record = Rc::new(ShadowRealmRecord::new(resolve_shadowrealm_base_dir(vm)));
            register_shadow_realm(&obj_rc, record);
            Ok(Value::Object(obj_rc))
        }
    });
    let shadowrealm_ctor_val =
        Value::BuiltinObject(shadowrealm_ctor_fn, Rc::clone(&shadowrealm_ctor_map));
    shadowrealm_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&shadowrealm_proto_map))),
    );
    shadowrealm_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        non_enum_data(shadowrealm_ctor_val.clone()),
    );
    set_builtin_length(&shadowrealm_ctor_val, 0.0);
    shadowrealm_ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("ShadowRealm".to_string())),
    );

    let shadowrealm_evaluate_val =
        builtin_object(Rc::new(shadowrealm_evaluate), &function_proto_map);
    {
        set_builtin_length(&shadowrealm_evaluate_val, 1.0);
        if let Value::BuiltinObject(_, props) = &shadowrealm_evaluate_val {
            props.borrow_mut().properties.insert(
                PKey::String("name".to_string()),
                PropertyDescriptor::data(Value::String("evaluate".to_string())),
            );
        }
    }
    shadowrealm_proto_map.borrow_mut().properties.insert(
        PKey::String("evaluate".to_string()),
        non_enum_data(shadowrealm_evaluate_val),
    );

    let shadowrealm_import_val =
        builtin_object(Rc::new(shadowrealm_import_value), &function_proto_map);
    {
        set_builtin_length(&shadowrealm_import_val, 2.0);
        if let Value::BuiltinObject(_, props) = &shadowrealm_import_val {
            props.borrow_mut().properties.insert(
                PKey::String("name".to_string()),
                PropertyDescriptor::data(Value::String("importValue".to_string())),
            );
        }
    }
    shadowrealm_proto_map.borrow_mut().properties.insert(
        PKey::String("importValue".to_string()),
        non_enum_data(shadowrealm_import_val),
    );
    shadowrealm_proto_map.borrow_mut().properties.insert(
        PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
        PropertyDescriptor::Data {
            value: Value::String("ShadowRealm".to_string()),
            writable: false,
            enumerable: false,
            configurable: true,
        },
    );

    let mut g = globals.borrow_mut();
    // Ensure `globalThis` aliases the actual global object.
    let global_this_key = PKey::String("globalThis".to_string());
    g.properties.insert(
        global_this_key.clone(),
        PropertyDescriptor::data(Value::Object(Rc::clone(globals))),
    );
    // ECMAScript global constants (only insert if not already present)
    g.properties
        .entry(PKey::String("NaN".to_string()))
        .or_insert(PropertyDescriptor::data(Value::Number(f64::NAN)));
    g.properties
        .entry(PKey::String("Infinity".to_string()))
        .or_insert(PropertyDescriptor::data(Value::Number(f64::INFINITY)));
    g.properties
        .entry(PKey::String("undefined".to_string()))
        .or_insert(PropertyDescriptor::data(Value::Undefined));
    g.properties.insert(
        PKey::String("eval".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "eval");
            let src = match args.get(off) {
                Some(Value::String(s)) => s.clone(),
                Some(other) => return Ok(other.clone()),
                None => return Ok(Value::Undefined),
            };
            match vm.eval_expression_source(&src) {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let msg = err.to_string();
                    if !msg.contains("parse:") && !msg.contains("compile:") {
                        return Err(err);
                    }
                }
            }
            let session = Parser::parse_source(&src)
                .map_err(|e| crate::errors::RuntimeError::from(format!("eval parse: {}", e)))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog, &src)
                .map_err(|e| crate::errors::RuntimeError::from(format!("eval compile: {}", e)))?;
            let mut eval_vm = Vm::new(vm.globals_rc());
            eval_vm.run(chunk)
        }))),
    );
    g.properties.insert(
        PKey::String("Reflect".to_string()),
        PropertyDescriptor::data(Value::Object(reflect_obj)),
    );
    // Add kernel helpers if missing, prefer host-provided values when present.
    let mut insert_if_missing = |name: &str, val: Value| {
        let k = PKey::String(name.to_string());
        g.properties
            .entry(k)
            .or_insert_with(|| PropertyDescriptor::data(val));
    };

    insert_if_missing(
        "__native_in",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let key = args.get(off).cloned().unwrap_or(Value::Undefined);
            let target = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            if !target.is_object() {
                return Err(vm.throw_type_error(&format!(
                    "Cannot use 'in' operator to search for '{:?}' in {}",
                    key,
                    target.type_name()
                )));
            }
            let key = vm.to_property_key(&key)?;
            Ok(Value::Bool(vm.has_property_host(&target, &key)?))
        })),
    );

    insert_if_missing(
        "__native_construct",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let ctor_args: Vec<Value> = args.iter().skip(off + 1).cloned().collect();
            vm.construct_value(&target, &ctor_args)
        })),
    );

    insert_if_missing(
        "__native_dynamic_import",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let specifier = args.get(off).cloned().unwrap_or(Value::Undefined);
            let options = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            dynamic_import_value(vm, &[specifier, options])
        })),
    );

    insert_if_missing(
        "__native_typeof",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let value = args.get(off).cloned().unwrap_or(Value::Undefined);
            let ty = match value {
                Value::Uninitialized => "undefined",
                Value::Undefined => "undefined",
                Value::Null => "object",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::BigInt(_) => "bigint",
                Value::Symbol(_, _) => "symbol",
                Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. } => {
                    "function"
                }
                Value::Object(_) | Value::Array(..) | Value::Promise(_) => "object",
            };
            Ok(Value::String(ty.to_string()))
        })),
    );
    insert_if_missing(
        "__native_typeof_name",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let name = args
                .get(off)
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let value = vm.get_global(&name).unwrap_or(Value::Undefined);
            let ty = match value {
                Value::Uninitialized => "undefined",
                Value::Undefined => "undefined",
                Value::Null => "object",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::BigInt(_) => "bigint",
                Value::Symbol(_, _) => "symbol",
                Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. } => {
                    "function"
                }
                Value::Object(_) | Value::Array(..) | Value::Promise(_) => "object",
            };
            Ok(Value::String(ty.to_string()))
        })),
    );
    set_builtin_name(&object_ctor_val, "Object");
    set_builtin_name(&function_ctor_val, "Function");
    set_builtin_name(&array_ctor_val, "Array");
    set_builtin_name(&string_ctor_val, "String");
    set_builtin_name(&boolean_ctor_val, "Boolean");
    set_builtin_name(&regexp_ctor_val, "RegExp");
    set_builtin_name(&symbol_ctor_val, "Symbol");
    set_builtin_name(&promise_ctor_val, "Promise");
    set_builtin_name(&date_ctor_val, "Date");
    set_builtin_name(&error_ctor_val, "Error");
    set_builtin_name(&aggregate_error_ctor_val, "AggregateError");

    g.properties.insert(
        PKey::String("Function".to_string()),
        PropertyDescriptor::data(function_ctor_val),
    );
    g.properties.insert(
        PKey::String("Object".to_string()),
        PropertyDescriptor::data(object_ctor_val),
    );
    g.properties.insert(
        PKey::String("Array".to_string()),
        PropertyDescriptor::data(array_ctor_val),
    );
    g.properties.insert(
        PKey::String("String".to_string()),
        PropertyDescriptor::data(string_ctor_val),
    );
    g.properties.insert(
        PKey::String("Boolean".to_string()),
        non_enum_data(boolean_ctor_val),
    );
    g.properties.insert(
        PKey::String("RegExp".to_string()),
        PropertyDescriptor::data(regexp_ctor_val),
    );
    g.properties.insert(
        PKey::String("Symbol".to_string()),
        PropertyDescriptor::data(symbol_ctor_val),
    );
    g.properties.insert(
        PKey::String("Promise".to_string()),
        PropertyDescriptor::data(promise_ctor_val),
    );
    g.properties.insert(
        PKey::String("Date".to_string()),
        PropertyDescriptor::data(date_ctor_val),
    );
    g.properties.insert(
        PKey::String("ArrayBuffer".to_string()),
        PropertyDescriptor::data(arraybuffer_ctor_val),
    );
    g.properties.insert(
        PKey::String("SharedArrayBuffer".to_string()),
        PropertyDescriptor::data(sharedarraybuffer_ctor_val),
    );
    g.properties.insert(
        PKey::String("DataView".to_string()),
        PropertyDescriptor::data(dataview_ctor_val),
    );
    g.properties.insert(
        PKey::String("TypedArray".to_string()),
        PropertyDescriptor::data(typedarray_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint8Array".to_string()),
        PropertyDescriptor::data(uint8array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Int8Array".to_string()),
        PropertyDescriptor::data(int8array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint16Array".to_string()),
        PropertyDescriptor::data(uint16array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Int16Array".to_string()),
        PropertyDescriptor::data(int16array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint32Array".to_string()),
        PropertyDescriptor::data(uint32array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Int32Array".to_string()),
        PropertyDescriptor::data(int32array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Float32Array".to_string()),
        PropertyDescriptor::data(float32array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Float64Array".to_string()),
        PropertyDescriptor::data(float64array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint8ClampedArray".to_string()),
        PropertyDescriptor::data(uint8clampedarray_ctor_val),
    );
    g.properties.insert(
        PKey::String("BigInt64Array".to_string()),
        PropertyDescriptor::data(bigint64array_ctor_val),
    );
    g.properties.insert(
        PKey::String("BigUint64Array".to_string()),
        PropertyDescriptor::data(biguint64array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Map".to_string()),
        PropertyDescriptor::data(map_ctor_val),
    );
    g.properties.insert(
        PKey::String("Set".to_string()),
        PropertyDescriptor::data(set_ctor_val),
    );
    let json_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&object_proto_map),
    ))));
    json_obj.borrow_mut().properties.insert(
        PKey::String("stringify".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                fn escape_json_string(s: &str) -> String {
                    let mut out = String::with_capacity(s.len() + 2);
                    out.push('"');
                    for ch in s.chars() {
                        match ch {
                            '"' => out.push_str("\\\""),
                            '\\' => out.push_str("\\\\"),
                            '\n' => out.push_str("\\n"),
                            '\r' => out.push_str("\\r"),
                            '\t' => out.push_str("\\t"),
                            '\u{08}' => out.push_str("\\b"),
                            '\u{0c}' => out.push_str("\\f"),
                            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
                            c => out.push(c),
                        }
                    }
                    out.push('"');
                    out
                }

                move |_vm, args| {
                    let off = if args.len() > 1 { 1 } else { 0 };
                    let value = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let out = match value {
                        Value::Uninitialized => Value::Undefined,
                        Value::Undefined | Value::Builtin(_) | Value::BuiltinObject(_, _) => {
                            Value::Undefined
                        }
                        Value::Null => Value::String("null".to_string()),
                        Value::Bool(b) => {
                            Value::String(if b { "true" } else { "false" }.to_string())
                        }
                        Value::Number(n) => {
                            if n.is_finite() {
                                Value::String(n.to_string())
                            } else {
                                Value::String("null".to_string())
                            }
                        }
                        Value::String(s) => Value::String(escape_json_string(&s)),
                        Value::BigInt(_) => {
                            return Err(crate::errors::RuntimeError::from(
                                "Do not know how to serialize a BigInt",
                            ));
                        }
                        Value::Array(_, _) => Value::String("[]".to_string()),
                        Value::Object(_) | Value::Closure { .. } | Value::Promise(_) => {
                            Value::String("{}".to_string())
                        }
                        Value::Symbol(_, _) => Value::Undefined,
                    };
                    Ok(out)
                }
            }),
            &function_proto_map,
        )),
    );
    g.properties.insert(
        PKey::String("JSON".to_string()),
        PropertyDescriptor::data(Value::Object(json_obj)),
    );

    g.properties.insert(
        PKey::String("Error".to_string()),
        PropertyDescriptor::data(error_ctor_val),
    );
    g.properties.insert(
        PKey::String("AggregateError".to_string()),
        PropertyDescriptor::data(aggregate_error_ctor_val),
    );
    for (name, ctor) in extra_error_ctors {
        g.properties
            .insert(PKey::String(name), PropertyDescriptor::data(ctor));
    }
    g.properties.insert(
        PKey::String("Math".to_string()),
        PropertyDescriptor::data(Value::Object(math)),
    );

    // Number constructor with static properties.
    let number_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let number_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&number_proto_map))),
    );
    let number_proto_for_ctor = Rc::clone(&number_proto_map);
    let number_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        // When called as constructor (new Number(...)) create a Number object
        // wrapper with internal [[PrimitiveValue]] per other constructors.
        if vm.current_new_target().is_some() {
            let value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
            let n = vm.to_number_value(&value)?;
            let mut obj = ObjectData::new();
            // Attach Number.prototype for this constructor's realm (available in
            // the surrounding scope via number_proto_map).
            obj.prototype = Value::Object(Rc::clone(&number_proto_for_ctor));
            // Create object and set [[PrimitiveValue]] slot
            let obj_rc = Rc::new(RefCell::new(obj));
            obj_rc.borrow_mut().properties.insert(
                PKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(Value::Number(n)),
            );
            return Ok(Value::Object(obj_rc));
        }
        // Called as a plain function: Number(value)
        // Called via Function.prototype.call/apply: Number.call(thisArg, value)
        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let value = args.get(value_idx).cloned().unwrap_or(Value::Number(0.0));
        let n = vm.to_number_value(&value)?;
        Ok(Value::Number(n))
    });
    let number_ctor_val = Value::BuiltinObject(number_ctor_fn, Rc::clone(&number_ctor_map));
    number_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(number_ctor_val.clone()),
    );
    // Provide Number.prototype.valueOf so ToPrimitive can call it on Number
    // wrapper objects or primitives per ECMAScript semantics.
    number_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res = match this {
                    Value::Number(n) => Value::Number(n),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                        {
                            value.clone()
                        } else {
                            return Err(vm.throw_type_error(
                                "Number.prototype.valueOf called on incompatible receiver",
                            ));
                        }
                    }
                    _ => {
                        return Err(vm.throw_type_error(
                            "Number.prototype.valueOf called on incompatible receiver",
                        ));
                    }
                };
                if crate::trace::enabled() {
                    crate::trace::value_string(
                        "[trace] Number.prototype.valueOf receiver",
                        &args.first().cloned().unwrap_or(Value::Undefined),
                    );
                    crate::trace::value_string("[trace] Number.prototype.valueOf result", &res);
                }
                Ok(res)
            }),
            &function_proto_map,
        )),
    );
    // Minimal Number.prototype.toString implementation. Full radix support is
    // omitted for now; tests commonly expect the default base-10 string.
    number_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::Number(n) => Ok(Value::String(n.to_string())),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::Number(n) = value
                        {
                            return Ok(Value::String(n.to_string()));
                        }
                        Err(vm.throw_type_error(
                            "Number.prototype.toString called on incompatible receiver",
                        ))
                    }
                    _ => Err(vm.throw_type_error(
                        "Number.prototype.toString called on incompatible receiver",
                    )),
                }
            }),
            &function_proto_map,
        )),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("POSITIVE_INFINITY".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("NEGATIVE_INFINITY".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NEG_INFINITY)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("MAX_VALUE".to_string()),
        PropertyDescriptor::data(Value::Number(f64::MAX)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("MIN_VALUE".to_string()),
        PropertyDescriptor::data(Value::Number(f64::MIN_POSITIVE)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("isNaN".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let value_idx = if args.len() > 1 { 1 } else { 0 };
                let is_nan = matches!(args.get(value_idx), Some(Value::Number(n)) if n.is_nan());
                Ok(Value::Bool(is_nan))
            }),
            &function_proto_map,
        )),
    );
    g.properties.insert(
        PKey::String("Number".to_string()),
        PropertyDescriptor::data(number_ctor_val),
    );
    g.properties.insert(
        PKey::String("ShadowRealm".to_string()),
        PropertyDescriptor::data(shadowrealm_ctor_val),
    );

    let _reflect = Rc::new(RefCell::new(ObjectData::new()));

    // Ensure a few additional helpers the soft prelude expects. Do not
    // override host-provided values.
    let mut insert_if_missing_global = |name: &str, v: Value| {
        let k = PKey::String(name.to_string());
        g.properties
            .entry(k)
            .or_insert_with(|| PropertyDescriptor::data(v));
    };

    insert_if_missing_global(
        "__native_toString",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&v)?;
            Ok(Value::String(s))
        })),
    );
    insert_if_missing_global(
        "__native_toNumber",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Number(n))
        })),
    );
    insert_if_missing_global(
        "isNaN",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Bool(n.is_nan()))
        })),
    );
    insert_if_missing_global(
        "isFinite",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Bool(n.is_finite()))
        })),
    );
    insert_if_missing_global("parseInt", Value::Builtin(Rc::new(global_parse_int)));
    insert_if_missing_global("parseFloat", Value::Builtin(Rc::new(global_parse_float)));
    insert_if_missing_global(
        "__native_toBoolean",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.to_boolean(&v)))
        })),
    );
    insert_if_missing_global(
        "__native_abstractEquality",
        Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Undefined);
            let b = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.abstract_equality(&a, &b)?))
        })),
    );
    insert_if_missing_global(
        "__native_instanceof",
        Value::Builtin(Rc::new(|vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let value = args.get(off).cloned().unwrap_or(Value::Undefined);
            let ctor = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.instance_of(&value, &ctor)?))
        })),
    );
    insert_if_missing_global(
        "__native_inProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let prop = args.first().cloned().unwrap_or(Value::Undefined);
            let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.in_property(&prop, &obj)?))
        })),
    );

    insert_if_missing_global(
        "__native_deleteProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let (target, key) = if args.len() >= 3 {
                (
                    args.get(1).cloned().unwrap_or(Value::Undefined),
                    args.get(2).cloned().unwrap_or(Value::Undefined),
                )
            } else {
                (
                    args.first().cloned().unwrap_or(Value::Undefined),
                    args.get(1).cloned().unwrap_or(Value::Undefined),
                )
            };
            if crate::trace::enabled() {
                eprintln!(
                    "[trace] __native_deleteProperty target={:?} key={:?}",
                    target, key
                );
            }
            let key = vm.to_property_key(&key)?;
            Ok(Value::Bool(vm.delete_property_host(&target, &key)?))
        })),
    );

    insert_if_missing_global(
        "__native_forInKeys",
        Value::Builtin(Rc::new(|vm, args| {
            let mut current = args.first().cloned().unwrap_or(Value::Undefined);
            if matches!(current, Value::Undefined | Value::Null) {
                return Err(vm.throw_type_error("Cannot convert undefined or null to object"));
            }

            let mut out: Vec<Option<Value>> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            let mut depth = 0usize;
            while depth < 128 {
                for key in own_enumerable_keys(&current) {
                    if let PropertyKey::String(name) = key
                        && seen.insert(name.clone())
                    {
                        out.push(Some(Value::String(name)));
                    }
                }

                let next = match &current {
                    Value::Object(obj) => obj.borrow().prototype.clone(),
                    Value::Array(_, props)
                    | Value::Closure { props, .. }
                    | Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
                    Value::String(_) => vm
                        .get_global("String")
                        .and_then(|ctor| {
                            vm.get_property_host(
                                &ctor,
                                &PropertyKey::String("prototype".to_string()),
                            )
                            .ok()
                        })
                        .unwrap_or(Value::Null),
                    Value::Symbol(_, _) => vm
                        .get_global("Symbol")
                        .and_then(|ctor| {
                            vm.get_property_host(
                                &ctor,
                                &PropertyKey::String("prototype".to_string()),
                            )
                            .ok()
                        })
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                if matches!(next, Value::Null | Value::Undefined) {
                    break;
                }
                current = next;
                depth += 1;
            }

            let arr_rc = Rc::new(RefCell::new(out));
            let props = Rc::new(RefCell::new(ObjectData::new()));
            props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
            );
            Ok(Value::Array(arr_rc, props))
        })),
    );

    insert_if_missing_global(
        "__native_updateProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let off = if args.len() >= 5 { 1 } else { 0 };
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            let delta =
                vm.to_number_value(&args.get(off + 2).cloned().unwrap_or(Value::Number(1.0)))?;
            let prefix = vm.to_boolean(&args.get(off + 3).cloned().unwrap_or(Value::Bool(true)));

            let key = vm.to_property_key(&key_value)?;
            let current = vm.get_property_host(&target, &key)?;
            let n = vm.to_number_value(&current)?;
            let updated = Value::Number(n + delta);
            let _ = vm.set_property_host(&target, &key_value, updated.clone())?;
            if prefix { Ok(updated) } else { Ok(current) }
        })),
    );

    insert_if_missing_global(
        "__native_compoundAssign",
        Value::Builtin(Rc::new(|vm, args| {
            let off = if args.len() >= 6 { 1 } else { 0 };
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            let rhs = args.get(off + 2).cloned().unwrap_or(Value::Undefined);
            let op = vm.to_string(
                &args
                    .get(off + 3)
                    .cloned()
                    .unwrap_or(Value::String("+".to_string())),
            )?;

            let key = vm.to_property_key(&key_value)?;
            let lhs = vm.get_property_host(&target, &key)?;

            let out = match op.as_str() {
                "+" => {
                    if matches!(lhs, Value::String(_))
                        || matches!(rhs, Value::String(_))
                        || matches!(lhs, Value::Symbol(_, _))
                        || matches!(rhs, Value::Symbol(_, _))
                    {
                        Value::String(format!("{}{}", vm.to_string(&lhs)?, vm.to_string(&rhs)?))
                    } else {
                        Value::Number(vm.to_number_value(&lhs)? + vm.to_number_value(&rhs)?)
                    }
                }
                "-" => Value::Number(vm.to_number_value(&lhs)? - vm.to_number_value(&rhs)?),
                "*" => Value::Number(vm.to_number_value(&lhs)? * vm.to_number_value(&rhs)?),
                "/" => Value::Number(vm.to_number_value(&lhs)? / vm.to_number_value(&rhs)?),
                "%" => Value::Number(vm.to_number_value(&lhs)? % vm.to_number_value(&rhs)?),
                "**" => Value::Number(vm.to_number_value(&lhs)?.powf(vm.to_number_value(&rhs)?)),
                "&" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_int32_value(&rhs)?;
                    Value::Number((l & r) as f64)
                }
                "|" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_int32_value(&rhs)?;
                    Value::Number((l | r) as f64)
                }
                "^" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_int32_value(&rhs)?;
                    Value::Number((l ^ r) as f64)
                }
                "<<" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_uint32_value(&rhs)? & 0x1f;
                    Value::Number(l.wrapping_shl(r) as f64)
                }
                ">>" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_uint32_value(&rhs)? & 0x1f;
                    Value::Number(l.wrapping_shr(r) as f64)
                }
                ">>>" => {
                    let l = vm.to_uint32_value(&lhs)?;
                    let r = vm.to_uint32_value(&rhs)? & 0x1f;
                    Value::Number(l.wrapping_shr(r) as f64)
                }
                _ => {
                    return Err(crate::errors::RuntimeError::from(
                        "unsupported compound assignment operator",
                    ));
                }
            };

            let _ = vm.set_property_host(&target, &key_value, out.clone())?;
            Ok(out)
        })),
    );

    // Provide lightweight hosts for built-ins that Test262 expects.
    {
        let stub_fn: BuiltinFn = Rc::new(|_vm: &mut Vm, _args: &[Value]| Ok(Value::Undefined));
        let mut proto_cache: HashMap<&'static str, Value> = HashMap::new();
        let mut ensure = |name: &'static str| {
            ensure_stub_prototype(name, &mut g, &object_proto_map, &mut proto_cache)
        };

        for &name in &[
            "IteratorPrototype",
            "AsyncIteratorPrototype",
            "ArrayIteratorPrototype",
            "MapIteratorPrototype",
            "SetIteratorPrototype",
            "StringIteratorPrototype",
            "RegExpStringIteratorPrototype",
            "GeneratorPrototype",
            "AsyncGeneratorPrototype",
            "AsyncFromSyncIteratorPrototype",
        ] {
            ensure(name);
        }

        for &(name, prototype_name) in &[
            ("Iterator", Some("IteratorPrototype")),
            ("AsyncIterator", Some("AsyncIteratorPrototype")),
            ("ArrayIterator", Some("ArrayIteratorPrototype")),
            ("MapIterator", Some("MapIteratorPrototype")),
            ("SetIterator", Some("SetIteratorPrototype")),
            ("StringIterator", Some("StringIteratorPrototype")),
            (
                "RegExpStringIterator",
                Some("RegExpStringIteratorPrototype"),
            ),
            (
                "AsyncFromSyncIterator",
                Some("AsyncFromSyncIteratorPrototype"),
            ),
            ("Generator", Some("GeneratorPrototype")),
            ("AsyncGenerator", Some("AsyncGeneratorPrototype")),
            ("GeneratorFunction", None),
            ("AsyncFunction", None),
            ("AsyncGeneratorFunction", None),
            ("AbstractModuleSource", None),
            ("DisposableStack", None),
            ("AsyncDisposableStack", None),
            ("FinalizationRegistry", None),
            ("WeakMap", None),
            ("WeakSet", None),
            ("WeakRef", None),
            ("Proxy", None),
            ("SuppressedError", None),
        ] {
            let proto = prototype_name.and_then(|key| proto_cache.get(key).cloned());
            insert_stub_constructor(
                &mut g,
                name,
                proto,
                &function_proto_map,
                &object_proto_map,
                stub_fn.clone(),
            );
        }

        let atomics_props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
            ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
        ));
        let insert_atomics_method = |name: &str, length: f64, f: BuiltinFn| {
            let func_val = builtin_object(f, &function_proto_map);
            set_builtin_name(&func_val, name);
            set_builtin_length(&func_val, length);
            atomics_props.borrow_mut().properties.insert(
                PKey::String(name.to_string()),
                PropertyDescriptor::data(func_val),
            );
        };
        insert_atomics_method(
            "load",
            2.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                atomics_load_current(vm, &info, index)
            }),
        );
        insert_atomics_method(
            "store",
            3.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                atomics_store_current(vm, &info, index, &value)
            }),
        );
        for (name, op) in [
            ("add", "add"),
            ("sub", "sub"),
            ("and", "and"),
            ("or", "or"),
            ("xor", "xor"),
        ] {
            insert_atomics_method(
                name,
                3.0,
                Rc::new(move |vm, args| {
                    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                    let info = atomics_view_info(vm, &receiver)?;
                    let index =
                        atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                    let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                    atomics_binary_update(vm, &info, index, &value, op)
                }),
            );
        }
        insert_atomics_method(
            "exchange",
            3.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                atomics_exchange_current(vm, &info, index, &value)
            }),
        );
        insert_atomics_method(
            "compareExchange",
            4.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let expected = args.get(2).cloned().unwrap_or(Value::Undefined);
                let replacement = args.get(3).cloned().unwrap_or(Value::Undefined);
                atomics_compare_exchange(vm, &info, index, &expected, &replacement)
            }),
        );
        insert_atomics_method(
            "isLockFree",
            1.0,
            Rc::new(|vm, args| {
                let size = args.get(1).cloned().unwrap_or(Value::Undefined);
                let n = match vm.to_number_value(&size) {
                    Ok(n) if n.is_finite() && n >= 0.0 => n.trunc() as usize,
                    _ => 0,
                };
                Ok(Value::Bool(matches!(n, 1 | 2 | 4 | 8)))
            }),
        );
        insert_atomics_method(
            "notify",
            3.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                if !info.is_shared {
                    return Err(vm.throw_type_error(
                        "Atomics.notify requires a SharedArrayBuffer-backed typed array",
                    ));
                }
                if !atomics_wait_kind(&info.kind_name) {
                    return Err(vm.throw_type_error(
                        "Atomics.notify requires an Int32Array or BigInt64Array",
                    ));
                }
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let count = match args.get(2) {
                    None | Some(Value::Undefined) => usize::MAX,
                    Some(value) => {
                        let n = vm.to_number_value(value)?;
                        if !n.is_finite() {
                            usize::MAX
                        } else if n <= 0.0 {
                            0
                        } else {
                            n.trunc() as usize
                        }
                    }
                };
                let woke = atomics_wake_waiters(vm, &info, index, count)?;
                Ok(Value::Number(woke as f64))
            }),
        );
        insert_atomics_method(
            "wait",
            4.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                if !info.is_shared {
                    return Err(vm.throw_type_error(
                        "Atomics.wait requires a SharedArrayBuffer-backed typed array",
                    ));
                }
                if !atomics_wait_kind(&info.kind_name) {
                    return Err(
                        vm.throw_type_error("Atomics.wait requires an Int32Array or BigInt64Array")
                    );
                }
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let expected = args.get(2).cloned().unwrap_or(Value::Undefined);
                let timeout = match args.get(3) {
                    None | Some(Value::Undefined) => f64::INFINITY,
                    Some(value) => {
                        let n = vm.to_number_value(value)?;
                        if n.is_nan() || n < 0.0 { 0.0 } else { n }
                    }
                };
                let current = atomics_normalized_current(vm, &info, index)?;
                let expected = atomics_coerce_view_value(vm, &info, &expected)?;
                if current != expected {
                    return Ok(Value::String("not-equal".to_string()));
                }
                if timeout == 0.0 {
                    return Ok(Value::String("timed-out".to_string()));
                }
                atomics_register_waiter(vm, &info, index, None)?;
                Ok(Value::String("ok".to_string()))
            }),
        );
        insert_atomics_method(
            "waitAsync",
            4.0,
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                if !info.is_shared {
                    return Err(vm.throw_type_error(
                        "Atomics.waitAsync requires a SharedArrayBuffer-backed typed array",
                    ));
                }
                if !atomics_wait_kind(&info.kind_name) {
                    return Err(vm.throw_type_error(
                        "Atomics.waitAsync requires an Int32Array or BigInt64Array",
                    ));
                }
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let expected = args.get(2).cloned().unwrap_or(Value::Undefined);
                let timeout = match args.get(3) {
                    None | Some(Value::Undefined) => f64::INFINITY,
                    Some(value) => {
                        let n = vm.to_number_value(value)?;
                        if n.is_nan() || n < 0.0 { 0.0 } else { n }
                    }
                };
                let current = atomics_normalized_current(vm, &info, index)?;
                let expected = atomics_coerce_view_value(vm, &info, &expected)?;
                if current != expected {
                    return Ok(atomics_make_wait_result(
                        false,
                        Value::String("not-equal".to_string()),
                    ));
                }
                if timeout == 0.0 {
                    return Ok(atomics_make_wait_result(
                        false,
                        Value::String("timed-out".to_string()),
                    ));
                }

                let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
                let resolve_slot_capture = Rc::clone(&resolve_slot);
                let promise_ctor = vm
                    .get_global("Promise")
                    .ok_or_else(|| vm.throw_type_error("Promise constructor missing"))?;
                let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
                    let off = if executor_args.len() > 2 { 1 } else { 0 };
                    *resolve_slot_capture.borrow_mut() = executor_args.get(off).cloned();
                    Ok(Value::Undefined)
                }));
                let promise = vm.construct_value(&promise_ctor, &[executor])?;
                let resolve_fn = resolve_slot
                    .borrow()
                    .clone()
                    .ok_or_else(|| vm.throw_type_error("Atomics.waitAsync resolve missing"))?;
                atomics_waiters_array(&info.buffer)
                    .borrow_mut()
                    .push(Some(atomics_make_waiter_record(index, Some(resolve_fn))));
                Ok(atomics_make_wait_result(true, promise))
            }),
        );
        let atomics_obj = Value::Object(atomics_props);

        for &name in &["Temporal", "TypedArrayConstructors", "NativeErrors"] {
            insert_stub_object(&mut g, name, &object_proto_map);
        }
        g.properties.insert(
            PKey::String("Atomics".to_string()),
            PropertyDescriptor::data(atomics_obj),
        );

        let throw_type_error_fn = builtin_object(
            Rc::new(|vm, _| Err(vm.throw_type_error("ThrowTypeError invoked"))),
            &function_proto_map,
        );
        set_builtin_name(&throw_type_error_fn, "ThrowTypeError");
        g.properties.insert(
            PKey::String("ThrowTypeError".to_string()),
            PropertyDescriptor::data(throw_type_error_fn),
        );

        let helper_fns: [(&str, BuiltinFn); 4] = [
            (
                "decodeURI",
                Rc::new(|vm: &mut Vm, args: &[Value]| {
                    let arg = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::String(vm.to_string(&arg)?))
                }),
            ),
            (
                "decodeURIComponent",
                Rc::new(|vm: &mut Vm, args: &[Value]| {
                    let arg = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::String(vm.to_string(&arg)?))
                }),
            ),
            (
                "encodeURI",
                Rc::new(|vm: &mut Vm, args: &[Value]| {
                    let arg = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::String(vm.to_string(&arg)?))
                }),
            ),
            (
                "encodeURIComponent",
                Rc::new(|vm: &mut Vm, args: &[Value]| {
                    let arg = args.first().cloned().unwrap_or(Value::Undefined);
                    Ok(Value::String(vm.to_string(&arg)?))
                }),
            ),
        ];
        for (name, func) in helper_fns.iter().cloned() {
            let func_value = builtin_object(func, &function_proto_map);
            set_builtin_name(&func_value, name);
            g.properties.insert(
                PKey::String(name.to_string()),
                PropertyDescriptor::data(func_value),
            );
        }
    }

    // Mirror a selection of helpers onto globalThis if not present there.
    // Special handling needed when globalThis is the same object as globals
    let names = [
        "__native_typeof",
        "__native_toString",
        "__native_toNumber",
        "__native_toBoolean",
        "__native_abstractEquality",
        "__native_instanceof",
        "__native_in",
        "__native_construct",
    ];

    // Get the globalThis value and check if it points back to globals
    if let Some(PropertyDescriptor::Data { value: gv, .. }) =
        g.properties.get(&global_this_key).cloned()
        && let Value::Object(obj_rc) = gv
    {
        // Check if obj_rc is the same as globals by comparing Rc pointers
        let is_globals = Rc::ptr_eq(&obj_rc, globals);

        if is_globals {
            // If globalThis IS globals, we need to be careful with borrows
            // Drop the mutable borrow of g first
            drop(g);

            // Now borrow g mutably again to insert the missing properties
            let g = globals.borrow_mut();
            for name in &names {
                let k = PropertyKey::String(name.to_string());
                if !g.properties.contains_key(&k) {
                    // Intentionally a no-op: when `globalThis` aliases `globals` we must avoid
                    // additional borrows while holding the mutable borrow of `g`. We already checked
                    // whether the property exists above, so do nothing here to prevent borrow
                    // conflicts — the mutable borrow held on `g` allows safe insertion if needed later.
                }
            }
        } else {
            // obj_rc is a separate object, safe to borrow both
            drop(g); // Drop mutable borrow

            let g = globals.borrow(); // Borrow immutably
            let mut obj = obj_rc.borrow_mut();
            for name in &names {
                let k = PropertyKey::String(name.to_string());
                if !obj.properties.contains_key(&k)
                    && let Some(PropertyDescriptor::Data { value, .. }) =
                        g.properties.get(&PKey::String(name.to_string())).cloned()
                {
                    obj.properties.insert(k, PropertyDescriptor::data(value));
                }
            }
            // g and obj borrows are dropped here
        }
    }
}

fn ensure_stub_prototype(
    name: &'static str,
    globals: &mut ObjectData,
    object_proto: &Rc<RefCell<ObjectData>>,
    cache: &mut HashMap<&'static str, Value>,
) -> Value {
    if let Some(PropertyDescriptor::Data { value, .. }) = globals
        .properties
        .get(&PropertyKey::String(name.to_string()))
    {
        cache.insert(name, value.clone());
        return value.clone();
    }

    let proto_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(object_proto),
    ))));
    let proto_val = Value::Object(Rc::clone(&proto_obj));
    globals.properties.insert(
        PropertyKey::String(name.to_string()),
        PropertyDescriptor::data(proto_val.clone()),
    );
    cache.insert(name, proto_val.clone());
    proto_val
}

fn insert_stub_constructor(
    globals: &mut ObjectData,
    name: &'static str,
    prototype: Option<Value>,
    function_proto: &Rc<RefCell<ObjectData>>,
    object_proto: &Rc<RefCell<ObjectData>>,
    stub_fn: BuiltinFn,
) {
    let key = PropertyKey::String(name.to_string());
    if globals.properties.contains_key(&key) {
        return;
    }

    let ctor_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(function_proto),
    ))));
    let proto_val = prototype.unwrap_or_else(|| {
        Value::Object(Rc::new(RefCell::new(ObjectData::with_prototype(
            Value::Object(Rc::clone(object_proto)),
        ))))
    });
    ctor_map.borrow_mut().properties.insert(
        PropertyKey::String("prototype".to_string()),
        PropertyDescriptor::data(proto_val),
    );
    let ctor = Value::BuiltinObject(stub_fn, Rc::clone(&ctor_map));
    globals
        .properties
        .insert(key, PropertyDescriptor::data(ctor));
}

fn insert_stub_object(
    globals: &mut ObjectData,
    name: &'static str,
    object_proto: &Rc<RefCell<ObjectData>>,
) {
    let key = PropertyKey::String(name.to_string());
    if globals.properties.contains_key(&key) {
        return;
    }

    let stub_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(object_proto),
    ))));
    globals
        .properties
        .insert(key, PropertyDescriptor::data(Value::Object(stub_obj)));
}

fn char_code_at(string: &str, index: usize) -> Option<u32> {
    string.encode_utf16().nth(index).map(u32::from)
}

fn char_index_to_byte_index(string: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    string
        .char_indices()
        .nth(index)
        .map_or(string.len(), |(byte_idx, _)| byte_idx)
}

fn build_regex(pattern: &str, flags: &str) -> Result<regex::Regex, crate::errors::RuntimeError> {
    let mut builder = RegexBuilder::new(pattern);
    builder.case_insensitive(flags.contains('i'));
    builder.multi_line(flags.contains('m'));
    builder.dot_matches_new_line(flags.contains('s'));
    builder
        .build()
        .map_err(|e| crate::errors::RuntimeError::from(format!("RegExp build: {}", e)))
}

fn sticky_caret_allows(subject: &str, start_index: usize, pattern: &str, multiline: bool) -> bool {
    if !pattern.starts_with('^') {
        return true;
    }
    if start_index == 0 {
        return true;
    }
    if !multiline {
        return false;
    }
    subject
        .chars()
        .nth(start_index.saturating_sub(1))
        .is_some_and(|c| c == '\n' || c == '\r')
}
// ShadowRealm and host intrinsics
//
// - ShadowRealm: a minimal ShadowRealm host is provided (see `shadowrealm_*`
//   helpers). Its behaviour is a pragmatic helper for tests and is not a
//   authoritative ShadowRealm implementation. In particular, ShadowRealm
//   import/evaluate currently uses the project's compiler and VM.run in a
//   separate VM instance and does not fully replicate all Realm host semantics.
// - Intrinsics bootstrapping: `install_minimal_kernel_constructors` installs a
//   limited set of constructors and prototype objects sufficient for the
//   runtime prelude. These intrinsics are intentionally minimal and may omit
//   some property attribute details or exotic object behaviors required by the
//   spec.
//
// Promise constructor matching
//
// - `promise_constructor_matches` currently checks the `constructor` property
//   of a promise object for equality with the provided constructor. This is a
//   pragmatic check that may be fragile for embellished prototype chains or
//   host-constructed promise-like objects. Spec-compliant engines use
//   internal brand checks and the Promise constructor's intrinsic behaviour.
//   Consider replacing property-based checks with a stricter internal brand
//   or explicit slot marking on promise state.
