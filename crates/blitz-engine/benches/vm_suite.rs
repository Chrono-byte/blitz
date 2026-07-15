#![allow(missing_docs)]
//! Criterion benchmark suite for the Blitz VM pipeline.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use blitz::compiler;
use blitz::parser::Parser;
use blitz::runtime_utils;
use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use blitz::vm::Vm;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// Fixtures for cold-start benchmarking (executed once per benchmark).
const COLD_START_FIXTURES: [&str; 4] = [
  "arithmetic_loop",
  "function_calls",
  "object_properties",
  "closure_calls",
];

/// Fixtures for fast execution benchmarking (executed once per benchmark).
const FAST_EXEC_FIXTURES: [&str; 18] = [
  "parse_numbers",
  "parse_numbers_bases",
  "uri_encode",
  "string_methods",
  "array_last_index_of",
  "arithmetic_multi_ops",
  "property_access_chain",
  "array_element_access",
  "nested_scope",
  "method_call_chain",
  "string_concat_loop",
  "array_push_loop",
  "object_property_write_loop",
  "conditional_branching",
  "ternary_operator",
  "dense_array_last_index_of",
  "json_stringify",
  "deep_object_nesting",
];

/// Fixtures for slow execution benchmarking (executed once per benchmark).
const SLOW_EXEC_FIXTURES: [&str; 6] = [
  "try_catch_no_errors",
  "try_catch_errors",
  "type_coercion_edge_cases",
  "array_for_each",
  "array_map",
  "array_filter",
];

/// Fixtures for stress execution benchmarking (executed once per benchmark).
const STRESS_EXEC_FIXTURES: [&str; 2] = ["recursive_fib", "large_array_creation"];

/// Extra fixtures for benchmarking (executed once per benchmark).
const EXTRA_FIXTURES: [&str; 3] = ["dom_like_access", "array_iteration", "config_lookup"];

/// Returns a new `ObjectData` instance with `globalThis` property installed.
fn benchmark_globals() -> Rc<RefCell<ObjectData>> {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string()),
    PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
  );
  runtime_utils::install_minimal_kernel_constructors(&globals);
  globals
}

/// Returns a source string for the unicode comment hotpath benchmark.
fn unicode_comment_hotpath_source() -> String {
  r#"
        var hex = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F"];
        for (var i = 0; i < 4096; i++) {
            var uu = hex[(i >> 12) & 15] + hex[(i >> 8) & 15] + hex[(i >> 4) & 15] + hex[i & 15];
            var xx = String.fromCharCode("0x" + uu);
            if (xx.length !== 1) {
                throw new Error("unexpected char length");
            }
        }
    "#
  .to_string()
}

/// Returns a source string for the comment-only eval benchmark.
fn comment_only_eval_source() -> String {
  r#"
        for (var i = 0; i < 4096; i++) {
            eval("/* comment */");
        }
    "#
  .to_string()
}

/// Returns a `Value` for the hotpath number value.
fn hotpath_number_value() -> Value {
  Value::String("0x1234".to_string())
}

/// Compiles a benchmark fixture from the given path.
fn compile_fixture(path: &Path) -> blitz::bytecode::Chunk {
  let source = std::fs::read_to_string(path).expect("benchmark fixture should be readable");
  let session = Parser::parse_source(&source).expect("benchmark fixture should parse as JS");
  compiler::compile_oxc(session.program(), &source).expect("benchmark fixture should compile")
}

/// Returns the source text for the given benchmark fixture path.
fn fixture_source(path: &Path) -> String {
  std::fs::read_to_string(path).expect("benchmark fixture should be readable")
}

/// Returns a benchmark fixture path relative to this crate.
fn fixture_path(fixture: &str) -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("benches")
    .join("fixtures")
    .join(format!("{fixture}.js"))
}

/// Returns the baseline set of global names from the given globals object.
fn baseline_global_names(globals: &Rc<RefCell<ObjectData>>) -> HashSet<String> {
  globals
    .borrow()
    .properties
    .keys()
    .filter_map(|key| match key {
      PropertyKey::String(name) => Some(name.clone()),
      _ => None,
    })
    .collect()
}

/// Benchmarks the compilation of benchmark fixtures.
fn bench_compile(c: &mut Criterion) {
  let mut group = c.benchmark_group("compile");
  group.sample_size(30);
  group.measurement_time(Duration::from_secs(3));
  group.warm_up_time(Duration::from_secs(1));
  for fixture in COLD_START_FIXTURES
    .into_iter()
    .chain(FAST_EXEC_FIXTURES)
    .chain(SLOW_EXEC_FIXTURES)
    .chain(STRESS_EXEC_FIXTURES)
    .chain(EXTRA_FIXTURES)
  {
    let path = fixture_path(fixture);
    let source = fixture_source(&path);
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_with_input(
      BenchmarkId::from_parameter(fixture),
      &source,
      |b, source| {
        b.iter(|| {
          let session = Parser::parse_source(source).expect("fixture should parse as JS");
          drop(compiler::compile_oxc(session.program(), source).expect("fixture should compile"));
        });
      },
    );
  }
  group.finish();
}

/// Benchmarks the execution of benchmark fixtures in cold start scenarios.
fn bench_execute_cold(c: &mut Criterion) {
  let mut group = c.benchmark_group("execute_cold");
  group.sample_size(10);
  group.measurement_time(Duration::from_secs(5));
  group.warm_up_time(Duration::from_secs(1));

  for fixture in COLD_START_FIXTURES {
    let path = fixture_path(fixture);
    let chunk = compile_fixture(&path);
    group.throughput(Throughput::Elements(1));
    group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
      b.iter(|| {
        let globals = benchmark_globals();
        let mut vm = Vm::new(globals);
        drop(
          vm.run(chunk.clone())
            .expect("fixture should execute"),
        );
      });
    });
  }
  group.finish();
}

/// Benchmarks the execution of benchmark fixtures in fast execution scenarios.
fn bench_execute_fast(c: &mut Criterion) {
  let mut group = c.benchmark_group("execute_fast");
  group.sample_size(10);
  group.measurement_time(Duration::from_secs(5));
  group.warm_up_time(Duration::from_secs(2));

  for fixture in FAST_EXEC_FIXTURES {
    let path = fixture_path(fixture);
    let chunk = compile_fixture(&path);
    let globals = benchmark_globals();
    let keep_globals = baseline_global_names(&globals);
    let mut vm = Vm::new(Rc::clone(&globals));

    group.throughput(Throughput::Elements(1));
    group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
      b.iter_batched(
        || chunk.clone(),
        |chunk| {
          drop(vm.run(chunk).expect("fixture should execute"));
          vm.reset(&keep_globals);
        },
        BatchSize::SmallInput,
      );
    });
  }
  group.finish();
}

/// Benchmarks the execution of benchmark fixtures in slow execution scenarios.
fn bench_execute_slow(c: &mut Criterion) {
  let mut group = c.benchmark_group("execute_slow");
  group.sample_size(10);
  group.measurement_time(Duration::from_secs(5));
  group.warm_up_time(Duration::from_secs(1));

  for fixture in SLOW_EXEC_FIXTURES {
    let path = fixture_path(fixture);
    let chunk = compile_fixture(&path);
    let globals = benchmark_globals();
    let keep_globals = baseline_global_names(&globals);
    let mut vm = Vm::new(Rc::clone(&globals));

    group.throughput(Throughput::Elements(1));
    group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
      b.iter_batched(
        || chunk.clone(),
        |chunk| {
          drop(vm.run(chunk).expect("fixture should execute"));
          vm.reset(&keep_globals);
        },
        BatchSize::SmallInput,
      );
    });
  }
  group.finish();
}

/// Benchmarks the execution of benchmark fixtures in stress scenarios.
fn bench_execute_stress(c: &mut Criterion) {
  let mut group = c.benchmark_group("execute_stress");
  group.sample_size(10);
  group.measurement_time(Duration::from_secs(3));
  group.warm_up_time(Duration::from_secs(1));

  for fixture in STRESS_EXEC_FIXTURES {
    let path = fixture_path(fixture);
    let chunk = compile_fixture(&path);
    let globals = benchmark_globals();
    let keep_globals = baseline_global_names(&globals);
    let mut vm = Vm::new(Rc::clone(&globals));

    group.throughput(Throughput::Elements(1));
    group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
      b.iter_batched(
        || chunk.clone(),
        |chunk| {
          drop(vm.run(chunk).expect("fixture should execute"));
          vm.reset(&keep_globals);
        },
        BatchSize::SmallInput,
      );
    });
  }
  group.finish();
}

/// Benchmarks the execution of unicode hotpath fixtures.
fn bench_execute_unicode_hotpaths(c: &mut Criterion) {
  let mut group = c.benchmark_group("execute_unicode_hotpaths");
  group.sample_size(10);
  group.measurement_time(Duration::from_secs(3));
  group.warm_up_time(Duration::from_secs(1));

  for (fixture, source) in [
    ("a5_like_from_char_code", unicode_comment_hotpath_source()),
    ("a6_like_comment_only_eval", comment_only_eval_source()),
  ] {
    let session = Parser::parse_source(&source).expect("benchmark source should parse as JS");
    let chunk =
      compiler::compile_oxc(session.program(), &source).expect("benchmark source should compile");
    let globals = benchmark_globals();
    let keep_globals = baseline_global_names(&globals);
    let mut vm = Vm::new(Rc::clone(&globals));

    group.throughput(Throughput::Elements(1));
    group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
      b.iter_batched(
        || chunk.clone(),
        |chunk| {
          drop(
            vm.run(chunk)
              .expect("benchmark source should execute"),
          );
          vm.reset(&keep_globals);
        },
        BatchSize::SmallInput,
      );
    });
  }
  group.finish();
}

/// Benchmarks micro hotpaths, such as string concatenation and number-to-string conversions.
fn bench_micro_hotpaths(c: &mut Criterion) {
  let mut group = c.benchmark_group("micro_hotpaths");
  group.sample_size(20);
  group.measurement_time(Duration::from_secs(3));
  group.warm_up_time(Duration::from_secs(1));

  let globals = benchmark_globals();
  let mut vm = Vm::new(Rc::clone(&globals));
  let string_ctor = vm
    .get_global("String")
    .expect("String constructor available");
  let from_char_code = vm
    .get_property_host(
      &string_ctor,
      &PropertyKey::String("fromCharCode".to_string()),
    )
    .expect("String.fromCharCode available");

  group.bench_function("to_number_hex_string", |b| {
    b.iter(|| {
      let value = hotpath_number_value();
      let _ = vm
        .to_number_value(&value)
        .expect("number coercion");
    });
  });

  group.bench_function("from_char_code_dispatch", |b| {
    b.iter(|| {
      let value = hotpath_number_value();
      drop(
        vm.call_value_with_this(
          &from_char_code,
          &string_ctor,
          &[value, Value::String("1".to_string())],
        )
        .expect("builtin dispatch"),
      );
    });
  });

  group.finish();
}

criterion_group!(
  vm_suite,
  bench_compile,
  bench_execute_cold,
  bench_execute_fast,
  bench_execute_slow,
  bench_execute_stress,
  bench_execute_unicode_hotpaths,
  bench_micro_hotpaths
);
criterion_main!(vm_suite);
