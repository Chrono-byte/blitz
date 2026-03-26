//! Optional runtime tracing for debugging VM execution.
//!
//! Tracing provides diagnostic output to help debug parsing, compilation, and
//! runtime issues. Enable by setting the environment variable:
//! ```bash
//! TSBLITZ_TRACE=1
//! ```
//!
//! # Usage
//!
//! Check if tracing is enabled with [`enabled`], then use the tracing functions
//! to emit diagnostic information:
//!
//! ```
//! use tsblitz::trace;
//!
//! if trace::enabled() {
//!     trace::string("debug label", "value to inspect");
//! }
//! ```
//!
//! # Tracing Functions
//!
//! - [`enabled`] - Check if tracing is active
//! - [`string`] - Log a string value with detailed info (length, bytes, repr)
//! - [`value_string`] - Log a runtime Value when it's a string

use std::env;

/// Returns true if tracing is enabled via TSBLITZ_TRACE environment variable
pub fn enabled() -> bool {
    env::var("TSBLITZ_TRACE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn string_inner(label: &str, s: &str) {
    eprintln!(
        "[trace] {} len={} bytes={:?} repr={:?}",
        label,
        s.len(),
        s.as_bytes(),
        s
    );
}

/// Log string value with label; include length and repr to catch truncation.
pub fn string(label: &str, s: &str) {
    if enabled() {
        string_inner(label, s);
    }
}

/// Log a runtime Value when it's a string.
pub fn value_string(label: &str, v: &crate::value::Value) {
    if !enabled() {
        return;
    }
    use crate::value::Value;
    match v {
        Value::String(s) => string_inner(label, s),
        Value::Number(n) => eprintln!("[trace] {} Number({})", label, n),
        Value::Bool(b) => eprintln!("[trace] {} Bool({})", label, b),
        Value::Null => eprintln!("[trace] {} Null", label),
        Value::Undefined => eprintln!("[trace] {} Undefined", label),
        Value::BigInt(n) => eprintln!("[trace] {} BigInt {}", label, n),
        Value::Symbol(_, desc) => {
            if let Some(d) = desc {
                eprintln!("[trace] {} Symbol({})", label, d);
            } else {
                eprintln!("[trace] {} Symbol()", label);
            }
        }
        Value::Array(arr, _props) => {
            eprintln!("[trace] {} Array(len={})", label, arr.borrow().len())
        }
        Value::Object(map) => eprintln!(
            "[trace] {} Object(keys={})",
            label,
            map.borrow().properties.len()
        ),
        Value::Closure { proto, .. } => eprintln!(
            "[trace] {} Closure(name='{}' arity={})",
            label, proto.name, proto.arity
        ),
        Value::Builtin(_) | Value::BuiltinObject(_, _) => {
            eprintln!("[trace] {} Builtin", label)
        }
        Value::Promise(_) => {
            eprintln!("[trace] {} Promise", label)
        }
    }
}
