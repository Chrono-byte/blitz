#![allow(missing_docs)]
//! Criterion benchmark suite for the Blitz VM pipeline.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use blitz::compiler;
use blitz::parser::Parser;
use blitz::runtime_utils;
use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use blitz::vm::Vm;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

const BASE_FIXTURES: [&str; 4] = [
    "arithmetic_loop",
    "function_calls",
    "object_properties",
    "closure_calls",
];

const HOTPATH_FIXTURES: [&str; 5] = [
    "parse_numbers",
    "parse_numbers_bases",
    "uri_encode",
    "string_methods",
    "array_last_index_of",
];

const HOT_FIXTURES: [&str; 11] = [
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
];

const COLD_FIXTURES: [&str; 10] = [
    "try_catch_no_errors",
    "try_catch_errors",
    "type_coercion_edge_cases",
    "array_for_each",
    "array_map",
    "array_filter",
    "recursive_fib",
    "deep_object_nesting",
    "large_array_creation",
    "json_stringify",
];

const MIXED_FIXTURES: [&str; 3] = ["dom_like_access", "array_iteration", "config_lookup"];

fn benchmark_globals() -> Rc<RefCell<ObjectData>> {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
    );
    runtime_utils::install_minimal_kernel_constructors(&globals);
    globals
}

fn compile_fixture(path: &str) -> blitz::bytecode::Chunk {
    let source =
        std::fs::read_to_string(Path::new(path)).expect("benchmark fixture should be readable");
    let session = Parser::parse_source(&source).expect("benchmark fixture should parse as JS");
    compiler::compile_oxc(session.program(), &source).expect("benchmark fixture should compile")
}

fn fixture_source(path: &str) -> String {
    std::fs::read_to_string(Path::new(path)).expect("benchmark fixture should be readable")
}

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

fn bench_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("compile");
    group.sample_size(100);
    group.measurement_time(Duration::from_secs(5));
    for fixture in BASE_FIXTURES
        .into_iter()
        .chain(HOTPATH_FIXTURES)
        .chain(HOT_FIXTURES)
        .chain(COLD_FIXTURES)
        .chain(MIXED_FIXTURES)
    {
        let path = format!("benches/fixtures/{fixture}.js");
        let source = fixture_source(&path);
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(fixture),
            &source,
            |b, source| {
                b.iter(|| {
                    let session = Parser::parse_source(source).expect("fixture should parse as JS");
                    let _ = compiler::compile_oxc(session.program(), source)
                        .expect("fixture should compile");
                });
            },
        );
    }
    group.finish();
}

fn bench_execute_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_cold");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.warm_up_time(Duration::from_secs(3));

    for fixture in COLD_FIXTURES {
        let path = format!("benches/fixtures/{fixture}.js");
        let chunk = compile_fixture(&path);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
            b.iter(|| {
                let globals = benchmark_globals();
                let mut vm = Vm::new(globals);
                let _ = vm.run(chunk.clone()).expect("fixture should execute");
            });
        });
    }
    group.finish();
}

fn bench_execute_hotpaths(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_hotpaths");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.warm_up_time(Duration::from_secs(3));

    for fixture in HOTPATH_FIXTURES {
        let path = format!("benches/fixtures/{fixture}.js");
        let chunk = compile_fixture(&path);
        let globals = benchmark_globals();
        let keep_globals = baseline_global_names(&globals);
        let mut vm = Vm::new(Rc::clone(&globals));

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
            b.iter_batched(
                || chunk.clone(),
                |chunk| {
                    let _ = vm.run(chunk).expect("fixture should execute");
                    vm.reset(&keep_globals);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_execute_hot(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_hot");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.warm_up_time(Duration::from_secs(3));

    for fixture in BASE_FIXTURES.into_iter().chain(HOT_FIXTURES) {
        let path = format!("benches/fixtures/{fixture}.js");
        let chunk = compile_fixture(&path);
        let globals = benchmark_globals();
        let keep_globals = baseline_global_names(&globals);
        let mut vm = Vm::new(Rc::clone(&globals));

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
            b.iter_batched(
                || chunk.clone(),
                |chunk| {
                    let _ = vm.run(chunk).expect("fixture should execute");
                    vm.reset(&keep_globals);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_execute_mixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_mixed");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.warm_up_time(Duration::from_secs(3));

    for fixture in MIXED_FIXTURES {
        let path = format!("benches/fixtures/{fixture}.js");
        let chunk = compile_fixture(&path);
        let globals = benchmark_globals();
        let keep_globals = baseline_global_names(&globals);
        let mut vm = Vm::new(Rc::clone(&globals));

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(fixture), &chunk, |b, chunk| {
            b.iter_batched(
                || chunk.clone(),
                |chunk| {
                    let _ = vm.run(chunk).expect("fixture should execute");
                    vm.reset(&keep_globals);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    vm_suite,
    bench_compile,
    bench_execute_cold,
    bench_execute_hotpaths,
    bench_execute_hot,
    bench_execute_mixed
);
criterion_main!(vm_suite);
