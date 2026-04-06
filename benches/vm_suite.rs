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
    for fixture in [
        "arithmetic_loop",
        "function_calls",
        "object_properties",
        "closure_calls",
    ] {
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
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(3));

    for fixture in [
        "arithmetic_loop",
        "function_calls",
        "object_properties",
        "closure_calls",
    ] {
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

fn bench_execute_hot(c: &mut Criterion) {
    let mut group = c.benchmark_group("execute_hot");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(3));

    for fixture in [
        "arithmetic_loop",
        "function_calls",
        "object_properties",
        "closure_calls",
    ] {
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
    bench_execute_hot
);
criterion_main!(vm_suite);
