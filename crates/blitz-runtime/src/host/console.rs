//! Console output and diagnostic helpers supplied by the Blitz CLI host.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::Instant;

use blitz::Vm;
use blitz::gc;
use blitz::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

#[derive(Default)]
struct ConsoleState {
  counts: HashMap<String, u64>,
  timers: HashMap<String, Instant>,
  group_depth: usize,
}

pub(super) fn install(vm: &mut Vm) {
  let console = Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(vm.realm().object_prototype()),
  ))));
  let state = Rc::new(RefCell::new(ConsoleState::default()));

  for name in ["log", "info", "debug", "dir", "dirxml", "table"] {
    set_method(
      &console,
      name,
      output_method(Rc::clone(&state), false, None),
    );
  }
  set_method(
    &console,
    "warn",
    output_method(Rc::clone(&state), true, None),
  );
  set_method(
    &console,
    "error",
    output_method(Rc::clone(&state), true, None),
  );
  set_method(
    &console,
    "trace",
    output_method(Rc::clone(&state), true, Some("Trace:")),
  );
  set_method(&console, "assert", assert_method(Rc::clone(&state)));
  set_method(&console, "count", count_method(Rc::clone(&state)));
  set_method(
    &console,
    "countReset",
    count_reset_method(Rc::clone(&state)),
  );
  set_method(&console, "time", time_method(Rc::clone(&state)));
  set_method(
    &console,
    "timeLog",
    time_log_method(Rc::clone(&state), false),
  );
  set_method(
    &console,
    "timeEnd",
    time_log_method(Rc::clone(&state), true),
  );
  set_method(&console, "group", group_method(Rc::clone(&state)));
  set_method(&console, "groupCollapsed", group_method(Rc::clone(&state)));
  set_method(&console, "groupEnd", group_end_method(Rc::clone(&state)));
  set_method(&console, "clear", clear_method());

  vm.realm().set_global("console", console);
}

fn set_method(
  console: &Value,
  name: &str,
  function: BuiltinFn,
) {
  let Value::Object(properties) = console else {
    return;
  };
  properties.borrow_mut().properties.insert(
    PropertyKey::String(name.to_string()),
    PropertyDescriptor::data(Value::Builtin(function)),
  );
}

fn output_method(
  state: Rc<RefCell<ConsoleState>>,
  stderr: bool,
  prefix: Option<&'static str>,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let message = format_arguments(vm, actual_args(args));
    emit(&state.borrow(), stderr, prefix, &message);
    Ok(Value::Undefined)
  })
}

fn assert_method(state: Rc<RefCell<ConsoleState>>) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let actual = actual_args(args);
    if actual.first().is_some_and(Value::is_truthy) {
      return Ok(Value::Undefined);
    }
    let detail = format_arguments(vm, actual.get(1..).unwrap_or_default());
    let message = if detail.is_empty() {
      "Assertion failed".to_string()
    } else {
      format!("Assertion failed: {detail}")
    };
    emit(&state.borrow(), true, None, &message);
    Ok(Value::Undefined)
  })
}

fn count_method(state: Rc<RefCell<ConsoleState>>) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let label = label(vm, actual_args(args).first());
    let (depth, count) = {
      let mut state = state.borrow_mut();
      let depth = state.group_depth;
      let count = state.counts.entry(label.clone()).or_insert(0);
      *count += 1;
      (depth, *count)
    };
    emit_at_depth(false, depth, &format!("{label}: {count}"));
    Ok(Value::Undefined)
  })
}

fn count_reset_method(state: Rc<RefCell<ConsoleState>>) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let label = label(vm, actual_args(args).first());
    state.borrow_mut().counts.remove(&label);
    Ok(Value::Undefined)
  })
}

fn time_method(state: Rc<RefCell<ConsoleState>>) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let label = label(vm, actual_args(args).first());
    state
      .borrow_mut()
      .timers
      .insert(label, Instant::now());
    Ok(Value::Undefined)
  })
}

fn time_log_method(
  state: Rc<RefCell<ConsoleState>>,
  remove: bool,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let actual = actual_args(args);
    let label = label(vm, actual.first());
    let (depth, started) = {
      let mut state = state.borrow_mut();
      let started = if remove {
        state.timers.remove(&label)
      } else {
        state.timers.get(&label).copied()
      };
      (state.group_depth, started)
    };
    if let Some(started) = started {
      let suffix = format_arguments(vm, actual.get(1..).unwrap_or_default());
      let suffix = if suffix.is_empty() {
        String::new()
      } else {
        format!(" {suffix}")
      };
      emit_at_depth(
        false,
        depth,
        &format!(
          "{label}: {:.3}ms{suffix}",
          started.elapsed().as_secs_f64() * 1000.0
        ),
      );
    } else {
      emit_at_depth(true, depth, &format!("Timer '{label}' does not exist"));
    }
    Ok(Value::Undefined)
  })
}

fn group_method(state: Rc<RefCell<ConsoleState>>) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let message = format_arguments(vm, actual_args(args));
    let depth = state.borrow().group_depth;
    if !message.is_empty() {
      emit_at_depth(false, depth, &message);
    }
    state.borrow_mut().group_depth += 1;
    Ok(Value::Undefined)
  })
}

fn group_end_method(state: Rc<RefCell<ConsoleState>>) -> BuiltinFn {
  Rc::new(move |_vm, _args| {
    let mut state = state.borrow_mut();
    state.group_depth = state.group_depth.saturating_sub(1);
    Ok(Value::Undefined)
  })
}

fn clear_method() -> BuiltinFn {
  Rc::new(|_vm, _args| {
    print!("\x1b[2J\x1b[H");
    let _result = io::stdout().flush();
    Ok(Value::Undefined)
  })
}

fn actual_args(args: &[Value]) -> &[Value] {
  args.get(1..).unwrap_or_default()
}

fn label(
  vm: &mut Vm,
  value: Option<&Value>,
) -> String {
  value
    .map(|value| stringify(vm, value))
    .unwrap_or_else(|| "default".to_string())
}

fn format_arguments(
  vm: &mut Vm,
  args: &[Value],
) -> String {
  let Some(first) = args.first() else {
    return String::new();
  };
  let Value::String(template) = first else {
    return args
      .iter()
      .map(|value| stringify(vm, value))
      .collect::<Vec<_>>()
      .join(" ");
  };

  let mut output = String::new();
  let mut chars = template.chars().peekable();
  let mut next_argument = 1;
  while let Some(character) = chars.next() {
    if character != '%' {
      output.push(character);
      continue;
    }
    let Some(specifier) = chars.peek().copied() else {
      output.push('%');
      break;
    };
    if specifier == '%' {
      chars.next();
      output.push('%');
      continue;
    }
    if !matches!(specifier, 's' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c')
      || next_argument >= args.len()
    {
      output.push('%');
      continue;
    }
    chars.next();
    let value = &args[next_argument];
    next_argument += 1;
    if specifier != 'c' {
      output.push_str(&stringify(vm, value));
    }
  }

  for value in &args[next_argument..] {
    if !output.is_empty() {
      output.push(' ');
    }
    output.push_str(&stringify(vm, value));
  }
  output
}

fn stringify(
  vm: &mut Vm,
  value: &Value,
) -> String {
  vm.to_string(value)
    .unwrap_or_else(|_| format!("{value}"))
}

fn emit(
  state: &ConsoleState,
  stderr: bool,
  prefix: Option<&str>,
  message: &str,
) {
  let message = match prefix {
    Some(prefix) if message.is_empty() => prefix.to_string(),
    Some(prefix) => format!("{prefix} {message}"),
    None => message.to_string(),
  };
  emit_at_depth(stderr, state.group_depth, &message);
}

fn emit_at_depth(
  stderr: bool,
  depth: usize,
  message: &str,
) {
  let indentation = "  ".repeat(depth);
  if stderr {
    eprintln!("{indentation}{message}");
  } else {
    println!("{indentation}{message}");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn installs_common_console_methods() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    install(&mut vm);

    let console = vm.get_global("console").expect("console global");
    for name in [
      "log",
      "info",
      "debug",
      "warn",
      "error",
      "assert",
      "dir",
      "table",
      "trace",
      "count",
      "countReset",
      "time",
      "timeLog",
      "timeEnd",
      "group",
      "groupCollapsed",
      "groupEnd",
      "clear",
    ] {
      let method = vm
        .get_property_host(&console, &PropertyKey::String(name.to_string()))
        .unwrap_or_else(|_| panic!("read console.{name}"));
      assert!(method.is_callable(), "console.{name} should be callable");
    }
  }
}
