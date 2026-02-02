//! Optional tracing for debugging. Set env TSBLITZ_TRACE=1 to enable.

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
        Value::Array(arr, _props) => {
            eprintln!("[trace] {} Array(len={})", label, arr.borrow().len())
        }
        Value::Object(map) => eprintln!("[trace] {} Object(keys={})", label, map.borrow().len()),
        Value::Closure { proto, .. } => eprintln!(
            "[trace] {} Closure(name='{}' arity={})",
            label, proto.name, proto.arity
        ),
        Value::Builtin(_) => eprintln!("[trace] {} Builtin", label),
    }
}
