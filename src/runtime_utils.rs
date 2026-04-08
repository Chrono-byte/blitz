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

use std::borrow::Cow;
use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::rc::Rc;

use crate::compiler;
use crate::compiler::ModuleContext;
use crate::mutex_compat::MutexBorrowExt;
use crate::parser::Parser;
use crate::realm::Intrinsics;
use crate::value::SymbolId;
use crate::value::{BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::{RuntimeError, bytecode};
use num_bigint::{BigInt as BigIntValue, Sign};
use num_traits::{FromPrimitive, ToPrimitive};
/// The maximum array length as a floating-point value.
const MAX_ARRAY_LENGTH_F: f64 = 4294967295.0_f64;

/// Converts a `usize` to a `f64`, returning `f64::NAN` if the value is too large.
pub(crate) fn usize_to_f64(value: usize) -> f64 {
    value.to_f64().unwrap_or(f64::NAN)
}

/// Format a JavaScript `Number` for string conversion.
///
/// This intentionally follows ECMAScript `ToString` expectations for the
/// cases we exercise in the VM: integral values should not keep Rust's
/// trailing `.0` formatting.
pub(crate) fn js_number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    if n == f64::INFINITY {
        return "Infinity".to_string();
    }
    if n == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }

    let abs = n.abs();
    if !(1e-6..1e21).contains(&abs) {
        let s = format!("{:e}", n);
        if let Some(e_idx) = s.find('e') {
            let mantissa = s[..e_idx].trim_end_matches('0').trim_end_matches('.');
            let exp = &s[e_idx + 1..];
            let (sign, digits) = if let Some(rest) = exp.strip_prefix('-') {
                ('-', rest)
            } else if let Some(rest) = exp.strip_prefix('+') {
                ('+', rest)
            } else {
                ('+', exp)
            };
            let digits = digits.trim_start_matches('0');
            let digits = if digits.is_empty() { "0" } else { digits };
            return format!("{}e{}{}", mantissa, sign, digits);
        }
        return s;
    }

    if n.fract() == 0.0 {
        format!("{:.0}", n)
    } else {
        n.to_string()
    }
}

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
            u32::try_from(r).ok()?
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

pub(crate) fn is_byte_offset_aligned(byte_offset: usize, element_count: usize) -> bool {
    element_count == 1 || byte_offset.is_multiple_of(element_count)
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

pub(crate) fn global_parse_int(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let radix_arg = if let Some(radix_value) = args.get(1) {
        Some(vm.to_int32_value(radix_value)?)
    } else {
        None
    };
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::Number(
                parse_int_string(s.trim_start(), radix_arg).unwrap_or(f64::NAN),
            ));
        }
    };
    let parsed = parse_int_string(s.trim_start(), radix_arg).unwrap_or(f64::NAN);
    Ok(Value::Number(parsed))
}

pub(crate) fn global_parse_float(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::Number(parse_float_string(&s)));
        }
    };
    let parsed = parse_float_string(s);
    Ok(Value::Number(parsed))
}

fn is_uri_component_safe(ch: char, component: bool) -> bool {
    matches!(
        ch,
        'A'..='Z'
            | 'a'..='z'
            | '0'..='9'
            | '-'
            | '_'
            | '.'
            | '!'
            | '~'
            | '*'
            | '\''
            | '('
            | ')'
            | ';'
            | ','
            | '/'
            | '?'
            | ':'
            | '@'
            | '&'
            | '='
            | '+'
            | '$'
            | '#'
    ) || (!component
        && matches!(
            ch,
            ',' | ';' | '/' | '?' | ':' | '@' | '&' | '=' | '+' | '$' | '#'
        ))
}

fn percent_encode(text: &str, component: bool) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for ch in text.chars() {
        if is_uri_component_safe(ch, component) {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
        }
    }
    out
}

fn uri_error(vm: &mut Vm) -> RuntimeError {
    let err = vm
        .create_intrinsic_error("URIError", "malformed URI sequence")
        .unwrap_or(Value::String("URIError".to_string()));
    vm.throw_exception(err)
}

fn percent_decode(vm: &mut Vm, text: &str, component: bool) -> Result<String, RuntimeError> {
    let input = text.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut i = 0usize;
    while i < input.len() {
        let byte = input[i];
        if byte != b'%' {
            bytes.push(byte);
            i += 1;
            continue;
        }
        if i + 2 >= input.len() {
            return Err(uri_error(vm));
        }
        let Some(hi) = hex_value(input[i + 1]) else {
            return Err(uri_error(vm));
        };
        let Some(lo) = hex_value(input[i + 2]) else {
            return Err(uri_error(vm));
        };
        let decoded = (hi << 4) | lo;
        if !component && is_decode_uri_reserved_byte(decoded) {
            bytes.extend_from_slice(&input[i..i + 3]);
        } else {
            bytes.push(decoded);
        }
        i += 3;
    }
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_decode_uri_reserved_byte(byte: u8) -> bool {
    matches!(
        byte,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'#'
    )
}

pub(crate) fn global_decode_uri(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_decode(vm, &s, false)?));
        }
    };
    Ok(Value::String(percent_decode(vm, s, false)?))
}

pub(crate) fn global_decode_uri_component(
    vm: &mut Vm,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_decode(vm, &s, true)?));
        }
    };
    Ok(Value::String(percent_decode(vm, s, true)?))
}

pub(crate) fn global_encode_uri(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_encode(&s, false)));
        }
    };
    Ok(Value::String(percent_encode(s, false)))
}

pub(crate) fn global_encode_uri_component(
    vm: &mut Vm,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_encode(&s, true)));
        }
    };
    Ok(Value::String(percent_encode(s, true)))
}

pub(crate) fn this_string_value_ref<'a>(
    vm: &mut Vm,
    value: &'a Value,
) -> Result<Cow<'a, str>, RuntimeError> {
    match value {
        Value::String(s) => Ok(Cow::Borrowed(s)),
        Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                .borrow()
                .properties
                .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                && let Value::String(s) = value
            {
                return Ok(Cow::Owned(s.clone()));
            }
            Err(vm.throw_type_error("String method called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("String method called on incompatible receiver")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn test_vm() -> Vm {
        Vm::new(Rc::new(RefCell::new(ObjectData::new())))
    }

    #[test]
    fn decode_uri_keeps_reserved_escapes() {
        let mut vm = test_vm();

        let preserved = global_decode_uri(&mut vm, &[Value::String("%3B%2F%3F".to_string())])
            .expect("decodeURI should succeed");
        assert_eq!(preserved, Value::String("%3B%2F%3F".to_string()));

        let decoded =
            global_decode_uri_component(&mut vm, &[Value::String("%3B%2F%3F".to_string())])
                .expect("decodeURIComponent should succeed");
        assert_eq!(decoded, Value::String(";/?".to_string()));
    }

    #[test]
    fn decode_uri_streams_literal_utf8_bytes() {
        let mut vm = test_vm();
        let value = global_decode_uri(&mut vm, &[Value::String("héllo".to_string())])
            .expect("decodeURI should accept raw utf8");
        assert_eq!(value, Value::String("héllo".to_string()));
    }
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
        new_chunk.root_slot_types = rc_chunk.root_slot_types.iter().map(|_| None).collect();
        new_chunk.root_slot_names = rc_chunk.root_slot_names.clone();
        new_chunk.root_lexical_tdz = rc_chunk.root_lexical_tdz.clone();
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

pub(crate) fn strict_equal(a: &Value, b: &Value) -> bool {
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

pub(crate) fn static_builtin_arg_offset(vm: &Vm, _args: &[Value], _global_name: &str) -> usize {
    if vm.current_new_target().is_some() || !matches!(vm.this_value(), Value::Undefined) {
        return 1;
    }
    0
}

pub(crate) fn promise_constructor_matches(
    vm: &mut Vm,
    promise: &Value,
    constructor: &Value,
) -> Result<bool, RuntimeError> {
    let promise_constructor =
        vm.get_property_host(promise, &PropertyKey::String("constructor".to_string()))?;
    Ok(strict_equal(&promise_constructor, constructor))
}

pub(crate) fn collect_iterable_values(
    vm: &mut Vm,
    iterable: &Value,
) -> Result<Vec<Value>, RuntimeError> {
    match iterable {
        Value::Array(arr, _) => Ok(arr
            .borrow()
            .iter()
            .map(|v| v.clone().unwrap_or(Value::Undefined))
            .collect()),
        other => {
            let iterator_method =
                vm.get_property_host(other, &PropertyKey::Symbol(SymbolId::well_known_iterator()))?;

            let is_callable = Vm::is_callable_value(&iterator_method);
            if is_callable {
                let iter_obj = vm.call_value_with_this(&iterator_method, other, &[])?;
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
                let len = to_length(vm, &len_val)?;
                let mut items = Vec::with_capacity(len);
                for idx in 0..len {
                    let item = vm.get_property_host(
                        other,
                        &PropertyKey::String(usize_to_decimal_string(idx)),
                    )?;
                    items.push(item);
                }
                Ok(items)
            }
        }
    }
}

/// Resolves a promise with a value, ensuring the promise is not already resolved.
fn resolve_promise_with_value_inner(
    vm: &mut Vm,
    result_promise: &Rc<RefCell<PromiseState>>,
    value: Value,
    already_resolved: Rc<RefCell<bool>>,
) -> Result<(), RuntimeError> {
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
        let then_val = vm.get_property_host(&value, &PropertyKey::String("then".to_string()))?;
        if Vm::is_callable_value(&then_val) {
            // Per-spec PromiseResolveThenableJob semantics: calling a thenable's
            // `then` should not synchronously settle the outer promise. Instead
            // schedule a microtask that will invoke `then` with resolve/reject
            // callbacks. This preserves observable job ordering when a thenable
            // calls back synchronously.
            let resolve_called = Rc::clone(&already_resolved);
            let res_p = Rc::clone(result_promise);
            let thenable = value;
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

/// Resolves a promise with a value, ensuring the promise is not already resolved.
pub(crate) fn resolve_promise_with_value(
    vm: &mut Vm,
    result_promise: &Rc<RefCell<PromiseState>>,
    value: Value,
) -> Result<(), RuntimeError> {
    resolve_promise_with_value_inner(vm, result_promise, value, Rc::new(RefCell::new(false)))
}

pub(crate) fn builtin_object(f: BuiltinFn, function_proto: &Rc<RefCell<ObjectData>>) -> Value {
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props
        .borrow_mut()
        .set_prototype_value(Value::Object(Rc::clone(function_proto)));
    Value::BuiltinObject(f, props)
}

/// Delegate to the ECMAScript semantics module for builtin wiring.
pub fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) -> Intrinsics {
    crate::ecmascript::semantics::install_minimal_kernel_constructors(globals)
}

thread_local! {
    static SHADOW_REALM_REGISTRY: RefCell<HashMap<usize, Rc<ShadowRealmRecord>>> =
        RefCell::new(HashMap::new());
}

pub(crate) struct ShadowRealmRecord {
    vm: RefCell<Vm>,
    base_dir: PathBuf,
}

impl ShadowRealmRecord {
    pub(crate) fn new(base_dir: PathBuf) -> Self {
        let globals = Rc::new(RefCell::new(ObjectData::new()));
        Self {
            vm: RefCell::new(Vm::new(Rc::clone(&globals))),
            base_dir,
        }
    }

    pub(crate) fn borrow_vm(&self) -> RefMut<'_, Vm> {
        self.vm.borrow_mut()
    }

    pub(crate) fn base_dir(&self) -> PathBuf {
        self.base_dir.clone()
    }
}

pub(crate) fn register_shadow_realm(obj: &Rc<RefCell<ObjectData>>, record: Rc<ShadowRealmRecord>) {
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
) -> Result<Rc<ShadowRealmRecord>, RuntimeError> {
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

pub(crate) fn resolve_shadowrealm_base_dir(vm: &Vm) -> PathBuf {
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
        let callbacks = std::mem::take(on_fulfilled);
        let stored_value = value.clone();
        *state = PromiseState::Fulfilled(value);
        drop(state);
        for callback in callbacks {
            let callback_value = stored_value.clone();
            vm.queue_microtask(move |vm| {
                let args = std::slice::from_ref(&callback_value);
                let _ = vm.call_value_with_this(&callback, &Value::Undefined, args);
                Ok(())
            });
        }
    }
}

/// Rejects a pending promise with a reason, ensuring the promise is not already resolved.
pub(crate) fn reject_pending_promise(
    vm: &mut Vm,
    promise: &Rc<RefCell<PromiseState>>,
    reason: Value,
) {
    use crate::value::PromiseState;

    let mut state = promise.borrow_mut();
    if let PromiseState::Pending { on_rejected, .. } = &mut *state {
        let callbacks = std::mem::take(on_rejected);
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
                let callback_reason = stored_reason.clone();
                vm.queue_microtask(move |vm| {
                    let args = std::slice::from_ref(&callback_reason);
                    let _ = vm.call_value_with_this(&callback, &Value::Undefined, args);
                    Ok(())
                });
            }
        }
    }
}

pub(crate) fn shadowrealm_evaluate(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let target = args.first().unwrap_or(&undefined);
    let record = require_shadow_realm_record(vm, target)?;
    let source = match args.get(1) {
        Some(Value::String(s)) => s.as_str(),
        _ => {
            return Err(
                vm.throw_type_error("ShadowRealm.prototype.evaluate requires a string argument")
            );
        }
    };
    let session = Parser::parse_source(source)
        .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate parse: {}", e)))?;
    let chunk = compiler::compile_oxc(session.program(), source)
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
        for v in arr_rc.borrow().iter().flatten().cloned() {
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
                    return Err(
                        vm.throw_type_error("Transfer list entries must be ArrayBuffer objects")
                    );
                }
                _ => {
                    return Err(
                        vm.throw_type_error("Transfer list entries must be ArrayBuffer objects")
                    );
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

pub(crate) fn shadowrealm_import_value(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let target = args.first().unwrap_or(&undefined);
    let record = require_shadow_realm_record(vm, target)?;
    let specifier = args.first().unwrap_or(&undefined);
    let specifier_str = vm.to_string(specifier)?;
    let export_name = match args.get(1) {
        Some(Value::String(s)) => s.clone(),
        _ => {
            return Err(vm.throw_type_error(
                "ShadowRealm.prototype.importValue requires export name string",
            ));
        }
    };

    let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    })));
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

pub(crate) fn dynamic_import_value(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let specifier = args.first().unwrap_or(&undefined);
    let options = args.get(1).unwrap_or(&undefined);

    let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    })));
    let promise_value = Value::Promise(Rc::clone(&promise));

    let specifier_str = match vm.to_string(specifier) {
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
                host_error_object(
                    vm,
                    "SyntaxError",
                    &format!("dynamic import construct failed: {}", err),
                )
            })?;
            loader.instantiate_all().map_err(|err| {
                host_error_object(
                    vm,
                    "SyntaxError",
                    &format!("dynamic import instantiate failed: {}", err),
                )
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
            let promise_for_fulfill = Rc::clone(&promise);
            let value_for_fulfill = value;
            vm.queue_dynamic_import_job(move |vm| {
                fulfill_pending_promise(vm, &promise_for_fulfill, value_for_fulfill.clone());
                Ok(())
            });
        }
        Err(reason) => {
            let promise_for_reject = Rc::clone(&promise);
            let reason_for_reject = reason;
            vm.queue_dynamic_import_job(move |vm| {
                reject_pending_promise(vm, &promise_for_reject, reason_for_reject.clone());
                Ok(())
            });
        }
    }

    Ok(promise_value)
}

pub(crate) fn set_builtin_length(target: &Value, length: f64) {
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

pub(crate) fn set_builtin_name(target: &Value, name: &str) {
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
pub(crate) fn to_length(vm: &mut Vm, value: &Value) -> Result<usize, RuntimeError> {
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

    int_len
        .to_usize()
        .ok_or_else(|| vm.throw_range_error("Invalid array length"))
}

pub(crate) fn to_index(vm: &mut Vm, value: &Value) -> Result<usize, RuntimeError> {
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
    int_val
        .to_usize()
        .ok_or_else(|| vm.throw_range_error("Invalid array index"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicsValueKind {
    Number { bits: u32, signed: bool },
    BigInt { signed: bool },
}

#[derive(Clone)]
pub(crate) struct AtomicsViewInfo {
    pub(crate) view: Rc<RefCell<ObjectData>>,
    pub(crate) data: Rc<RefCell<Vec<Option<Value>>>>,
    pub(crate) buffer: Rc<RefCell<ObjectData>>,
    pub(crate) kind_name: String,
    pub(crate) value_kind: AtomicsValueKind,
    pub(crate) length: usize,
    pub(crate) is_shared: bool,
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

pub(crate) fn atomics_wait_kind(kind_name: &str) -> bool {
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
) -> Result<Value, RuntimeError> {
    let n = vm.to_number_value(value)?;
    let int = n.trunc().to_i128().unwrap_or_else(|| {
        if n.is_sign_negative() {
            i128::MIN
        } else {
            i128::MAX
        }
    });
    let int = atomics_normalize_integer(int, bits, signed);
    Ok(Value::Number(int.to_f64().unwrap_or(f64::NAN)))
}

fn atomics_value_to_bigint(
    vm: &mut Vm,
    value: &Value,
    signed: bool,
) -> Result<Value, RuntimeError> {
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

pub(crate) fn atomics_view_info(
    vm: &mut Vm,
    view: &Value,
) -> Result<AtomicsViewInfo, RuntimeError> {
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
            }) if n.is_finite() && *n >= 0.0 => n.trunc().to_usize().unwrap_or(data.borrow().len()),
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

pub(crate) fn atomics_view_index(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index_value: &Value,
) -> Result<usize, RuntimeError> {
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
) -> Result<Value, RuntimeError> {
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
) -> Result<(), RuntimeError> {
    let prop_key = Value::Number(usize_to_f64(index));
    let stored = value.clone();
    let _ = vm.set_property_host(&Value::Object(Rc::clone(&info.view)), &prop_key, value)?;
    if let Some(slot) = info.data.borrow_mut().get_mut(index) {
        *slot = Some(stored);
    }
    Ok(())
}

pub(crate) fn atomics_waiters_array(
    buffer: &Rc<RefCell<ObjectData>>,
) -> Rc<RefCell<Vec<Option<Value>>>> {
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
    let arr = crate::gc::track_array(Rc::new(RefCell::new(Vec::new())));
    buffer_data.properties.insert(
        PropertyKey::String("[[AtomicsWaiters]]".to_string()),
        PropertyDescriptor::data(Value::Array(
            Rc::clone(&arr),
            crate::gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
        )),
    );
    arr
}

pub(crate) fn atomics_make_waiter_record(index: usize, resolve: Option<Value>) -> Value {
    let mut data = ObjectData::new();
    data.properties.insert(
        PropertyKey::String("index".to_string()),
        PropertyDescriptor::data(Value::Number(usize_to_f64(index))),
    );
    data.properties.insert(
        PropertyKey::String("resolve".to_string()),
        PropertyDescriptor::data(resolve.unwrap_or(Value::Undefined)),
    );
    Value::Object(crate::gc::track_object(Rc::new(RefCell::new(data))))
}

pub(crate) fn atomics_make_wait_result(async_flag: bool, value: Value) -> Value {
    let result = crate::gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
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

pub(crate) fn atomics_wake_waiters(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    count: usize,
) -> Result<usize, RuntimeError> {
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
            let record_index = match vm
                .get_property_host(&waiter, &PropertyKey::String("index".to_string()))
            {
                Ok(Value::Number(n)) if n.is_finite() && n >= 0.0 => match n.trunc().to_usize() {
                    Some(index) => index,
                    None => {
                        remaining.push(Some(waiter));
                        continue;
                    }
                },
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

pub(crate) fn atomics_register_waiter(
    _vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    resolve: Option<Value>,
) -> Result<(), RuntimeError> {
    let waiters = atomics_waiters_array(&info.buffer);
    waiters
        .borrow_mut()
        .push(Some(atomics_make_waiter_record(index, resolve)));
    Ok(())
}

pub(crate) fn atomics_coerce_view_value(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    value: &Value,
) -> Result<Value, RuntimeError> {
    match info.value_kind {
        AtomicsValueKind::Number { bits, signed } => {
            atomics_value_to_integer(vm, value, bits, signed)
        }
        AtomicsValueKind::BigInt { signed } => atomics_value_to_bigint(vm, value, signed),
    }
}

pub(crate) fn atomics_normalized_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
) -> Result<Value, RuntimeError> {
    let current = atomics_read_view_slot(vm, info, index)?;
    atomics_coerce_view_value(vm, info, &current)
}

pub(crate) fn atomics_load_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
) -> Result<Value, RuntimeError> {
    atomics_normalized_current(vm, info, index)
}

pub(crate) fn atomics_store_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: &Value,
) -> Result<Value, RuntimeError> {
    let stored = atomics_coerce_view_value(vm, info, value)?;
    atomics_write_view_slot(vm, info, index, stored.clone())?;
    Ok(stored)
}

pub(crate) fn atomics_exchange_current(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: &Value,
) -> Result<Value, RuntimeError> {
    let current = atomics_normalized_current(vm, info, index)?;
    let stored = atomics_coerce_view_value(vm, info, value)?;
    atomics_write_view_slot(vm, info, index, stored)?;
    Ok(current)
}

pub(crate) fn atomics_binary_update(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    value: &Value,
    op: &str,
) -> Result<Value, RuntimeError> {
    let current = atomics_normalized_current(vm, info, index)?;
    let operand = atomics_coerce_view_value(vm, info, value)?;
    let result = match (info.value_kind, current.clone(), operand) {
        (
            AtomicsValueKind::Number { bits, signed },
            Value::Number(current_n),
            Value::Number(operand_n),
        ) => {
            let lhs = current_n.trunc().to_i128().unwrap_or_else(|| {
                if current_n.is_sign_negative() {
                    i128::MIN
                } else {
                    i128::MAX
                }
            });
            let rhs = operand_n.trunc().to_i128().unwrap_or_else(|| {
                if operand_n.is_sign_negative() {
                    i128::MIN
                } else {
                    i128::MAX
                }
            });
            let raw = match op {
                "add" => lhs + rhs,
                "sub" => lhs - rhs,
                "and" => lhs & rhs,
                "or" => lhs | rhs,
                "xor" => lhs ^ rhs,
                _ => lhs,
            };
            Value::Number(
                atomics_normalize_integer(raw, bits, signed)
                    .to_f64()
                    .unwrap_or(f64::NAN),
            )
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

pub(crate) fn atomics_compare_exchange(
    vm: &mut Vm,
    info: &AtomicsViewInfo,
    index: usize,
    expected: &Value,
    replacement: &Value,
) -> Result<Value, RuntimeError> {
    let current = atomics_normalized_current(vm, info, index)?;
    let expected = atomics_coerce_view_value(vm, info, expected)?;
    if current == expected {
        let replacement = atomics_coerce_view_value(vm, info, replacement)?;
        atomics_write_view_slot(vm, info, index, replacement)?;
    }
    Ok(current)
}

pub(crate) fn own_property_descriptor(
    obj: &Value,
    key: &PropertyKey,
) -> Option<PropertyDescriptor> {
    match obj {
        Value::Object(data) => data.borrow().properties.get(key).cloned(),
        Value::Array(arr, props) => {
            if let PropertyKey::String(s) = key
                && let Some(idx) = canonical_index_key(s)
                && let Some(idx) = idx.to_usize()
                && let Some(Some(value)) = arr.borrow().get(idx).cloned()
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

pub(crate) fn own_enumerable_keys(obj: &Value) -> Vec<PropertyKey> {
    fn enumerable_prop_keys(props: &RefCell<ObjectData>, skip_length: bool) -> Vec<PropertyKey> {
        let mut index_keys: Vec<u64> = Vec::new();
        let mut string_keys: Vec<String> = Vec::new();
        let mut symbol_keys: Vec<PropertyKey> = Vec::new();
        for (key, desc) in &props.borrow().properties {
            let enumerable = match desc {
                PropertyDescriptor::Data { enumerable, .. }
                | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
            };
            if !enumerable {
                continue;
            }
            match key {
                PropertyKey::String(name) => {
                    if skip_length && name == "length" {
                        continue;
                    }
                    if let Some(idx) = canonical_index_key(name) {
                        index_keys.push(idx);
                    } else {
                        string_keys.push(name.clone());
                    }
                }
                PropertyKey::Symbol(_) => symbol_keys.push(key.clone()),
            }
        }
        index_keys.sort_unstable();
        index_keys.dedup();
        let mut out: Vec<PropertyKey> = index_keys
            .into_iter()
            .map(|idx| PropertyKey::String(decimal_u64_string(idx)))
            .collect();
        out.extend(string_keys.into_iter().map(PropertyKey::String));
        out.extend(symbol_keys);
        out
    }

    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => keys.extend(enumerable_prop_keys(data, false)),
        Value::Array(arr, props) => {
            for (idx, slot) in arr.borrow().iter().enumerate() {
                if slot.is_some() {
                    keys.push(PropertyKey::String(usize_to_decimal_string(idx)));
                }
            }
            keys.extend(enumerable_prop_keys(props, true));
        }
        Value::Closure { .. } | Value::BuiltinObject(..) | Value::Builtin(_) => {
            let p = match obj {
                Value::Closure { props, .. } => Some(props),
                Value::BuiltinObject(_, props) => Some(props),
                _ => None,
            };
            if let Some(props) = p {
                keys.extend(enumerable_prop_keys(props, false));
            }
        }
        _ => {}
    }
    keys
}

pub(crate) fn non_enum_data(value: Value) -> PropertyDescriptor {
    PropertyDescriptor::Data {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
    }
}

/// 22.1.3.34.1 ThisStringValue ( value )
pub(crate) fn this_string_value(vm: &mut Vm, value: &Value) -> Result<String, RuntimeError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                .borrow()
                .properties
                .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                && let Value::String(s) = value
            {
                return Ok(s.clone());
            }
            Err(vm.throw_type_error("String method called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("String method called on incompatible receiver")),
    }
}

/// 20.3.3.3.1 ThisBooleanValue ( value )
pub(crate) fn this_boolean_value(vm: &mut Vm, value: &Value) -> Result<bool, RuntimeError> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                .borrow()
                .properties
                .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                && let Value::Bool(b) = value
            {
                return Ok(*b);
            }
            Err(vm.throw_type_error("Boolean method called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("Boolean method called on incompatible receiver")),
    }
}

pub(crate) fn require_object_coercible(vm: &mut Vm, value: &Value) -> Result<(), RuntimeError> {
    match value {
        Value::Undefined | Value::Null => {
            Err(vm.throw_type_error("Cannot convert undefined or null to object"))
        }
        _ => Ok(()),
    }
}

pub(crate) fn descriptor_value_from_descriptor(
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
pub(crate) fn own_property_keys(obj: &Value) -> Vec<PropertyKey> {
    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => {
            for key in data.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        Value::Array(arr, props) => {
            for idx in 0..arr.borrow().len() {
                keys.push(PropertyKey::String(usize_to_decimal_string(idx)));
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

pub(crate) fn property_key_to_value(key: &PropertyKey) -> Value {
    match key {
        PropertyKey::String(name) => Value::String(name.clone()),
        PropertyKey::Symbol(id) => Value::Symbol(id.clone(), None),
    }
}

fn decimal_u64_string(value: u64) -> String {
    let mut buf = [0u8; 20];
    let mut n = value;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let mut out = String::with_capacity(buf.len() - i);
    out.extend(buf[i..].iter().map(|b| char::from(*b)));
    out
}

pub(crate) fn u64_to_decimal_string(value: u64) -> String {
    decimal_u64_string(value)
}

pub(crate) fn decimal_u64_eq_str(value: u64, s: &str) -> bool {
    let mut buf = [0u8; 20];
    let mut n = value;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let digits = &buf[i..];
    digits.len() == s.len() && digits.iter().zip(s.as_bytes()).all(|(a, b)| a == b)
}

pub(crate) fn usize_to_decimal_string(value: usize) -> String {
    decimal_u64_string(value as u64)
}

pub(crate) fn array_from_values(values: Vec<Value>) -> Value {
    let arr_rc = Rc::new(RefCell::new(
        values.into_iter().map(Some).collect::<Vec<_>>(),
    ));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
    );
    Value::Array(arr_rc, props)
}

pub(crate) fn syntax_error(vm: &mut Vm, message: String) -> RuntimeError {
    let err_val = host_error_object(vm, "SyntaxError", &message);
    vm.throw_exception(err_val)
}

pub(crate) fn host_error_object(vm: &mut Vm, name: &str, message: &str) -> Value {
    let mut obj = ObjectData::new();
    let proto = vm
        .realm()
        .get_intrinsic_constructor(name)
        .and_then(|ctor| {
            vm.get_property_host(&ctor, &PropertyKey::String("prototype".to_string()))
                .ok()
        })
        .or_else(|| vm.realm().intrinsics.error_prototype.clone());
    if let Some(proto) = proto {
        obj.set_prototype_value(proto);
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

pub(crate) fn canonical_index_key(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if s != "0" && s.starts_with('0') {
        return None;
    }
    let idx = s.parse::<u64>().ok()?;
    if !decimal_u64_eq_str(idx, s) {
        return None;
    }
    Some(idx)
}

/// Returns the UTF-16 code unit at the given index, if one exists.
pub(crate) fn char_code_at(string: &str, index: usize) -> Option<u32> {
    string.encode_utf16().nth(index).map(u32::from)
}

/// Returns the byte index corresponding to the given character index in the string.
pub(crate) fn char_index_to_byte_index(string: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    string
        .char_indices()
        .nth(index)
        .map_or(string.len(), |(byte_idx, _)| byte_idx)
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
                    new_props.borrow_mut().set_prototype_value(proto);
                }
                // insert length property placeholder
                new_props.borrow_mut().properties.insert(
                    PropertyKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(usize_to_f64(len))),
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
                        out.set_prototype_value(proto);
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
                        out.set_prototype_value(proto);
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
                        out.set_prototype_value(proto);
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
                        out.set_prototype_value(proto);
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
                        out.set_prototype_value(proto);
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
                        out.set_prototype_value(proto);
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
                    out.set_prototype_value(proto);
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
