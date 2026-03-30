/*!
Test262 connector script for Blitz.

Usage:
- Optionally set `TEST262_FILTER` to a substring to limit which tests are run
- Optionally set `TEST262_LIMIT` to limit how many tests are executed (default: all)
- Optionally set `TEST262_SEGMENTS` to a comma-separated list of segments
- Optionally set `TEST262_EXCLUDE` to a comma-separated list of segments to skip

CLI (preferred over env):
- --segment seg [,seg...] (repeatable)
- --exclude seg [,seg...] (repeatable)
- --filter substring
- --limit number

Segments may be labels like: annexB, language, built-ins, intl402, staging
or paths relative to test262/test (e.g. built-ins/Array).
APIs.
*/

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser as ClapParser;
use oxc_span::SourceType;
use serde::Deserialize;

use blitz::compiler;
use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey};
use blitz::{parser::Parser, runtime_utils, typecheck, value::Value, vm::Vm};

#[derive(Clone, Copy, Default)]
struct BucketStats {
    pass: usize,
    fail: usize,
    skip: usize,
}

impl BucketStats {
    fn total(self) -> usize {
        self.pass + self.fail + self.skip
    }

    fn attempted(self) -> usize {
        self.pass + self.fail
    }

    fn pass_pct(self) -> f64 {
        let attempted = self.attempted();
        if attempted == 0 {
            0.0
        } else {
            (self.pass as f64 / attempted as f64) * 100.0
        }
    }
}

enum TestResult {
    Pass,
    Fail(String),
    StackOverflow,
    Panic(String),
}

fn run_test_in_thread(file: PathBuf, test262_dir: PathBuf) -> TestResult {
    const STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB stack for tests

    let (tx, rx) = mpsc::channel();

    let _join_handle = thread::Builder::new()
        .name(format!("test-{}", file.display()))
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| {
                // Create fresh globals for this thread (each test gets isolated globals)
                let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));

                // Set up globals like main does
                let global_this = Value::Object(Rc::clone(&globals));
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("globalThis".to_string()),
                    PropertyDescriptor::data(global_this),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("NaN".to_string()),
                    PropertyDescriptor::data(Value::Number(f64::NAN)),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("Infinity".to_string()),
                    PropertyDescriptor::data(Value::Number(f64::INFINITY)),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__native_new_object".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, _args| {
                        Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__native_print".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
                        eprintln!(
                            "{}",
                            args.iter()
                                .map(|v| format!("{}", v))
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                        Ok(Value::Null)
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("print".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
                        eprintln!(
                            "{}",
                            args.iter()
                                .map(|v| format!("{}", v))
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                        Ok(Value::Null)
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__define_property".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                        vm.define_property_builtin(args)
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__native_getOwnPropertyDescriptor".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                        let target = args.first().cloned().unwrap_or(Value::Undefined);
                        let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
                        let key = vm.to_property_key(&key_value).map_err(|e| e.to_string())?;
                        Ok(runtime_utils::own_property_descriptor_value(&target, &key)
                            .unwrap_or(Value::Undefined))
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__native_toString".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                        let v = args.first().cloned().unwrap_or(Value::Undefined);
                        vm.to_string(&v).map(Value::String)
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__native_toNumber".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                        let v = args.first().cloned().unwrap_or(Value::Undefined);
                        vm.to_number_value(&v).map(Value::Number)
                    }))),
                );
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("__native_toBoolean".to_string()),
                    PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                        let v = args.first().cloned().unwrap_or(Value::Undefined);
                        Ok(Value::Bool(vm.to_boolean(&v)))
                    }))),
                );

                // Install kernel constructors (this adds Object, Function, Array, etc.)
                runtime_utils::install_minimal_kernel_constructors(&globals);

                // Wire $262 test harness object
                let test262_obj = make_test262_object(&globals);
                globals.borrow_mut().properties.insert(
                    PropertyKey::String("$262".to_string()),
                    PropertyDescriptor::data(test262_obj),
                );

                // Snapshot for test isolation (though we won't use restore since each test is fresh)
                let baseline_snapshot = snapshot_globals(&globals);

                // Run the test
                run_test_file(&file, &test262_dir, &globals, &baseline_snapshot)
            }));

            match result {
                Ok(Ok(())) => {
                    let _ = tx.send(TestResult::Pass);
                }
                Ok(Err(e)) => {
                    let _ = tx.send(TestResult::Fail(e));
                }
                Err(panic_payload) => {
                    let msg = if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "Unknown panic".to_string()
                    };

                    let is_overflow = msg.to_lowercase().contains("stack overflow")
                        || (msg.contains("thread '") && msg.contains("' has overflowed its stack"));

                    if is_overflow {
                        let _ = tx.send(TestResult::StackOverflow);
                    } else {
                        let _ = tx.send(TestResult::Panic(msg));
                    }
                }
            }
        });

    rx.recv().unwrap_or(TestResult::Panic(
        "Thread failed to send result".to_string(),
    ))
}

fn bucket_keys(test_root: &Path, file: &Path) -> (String, String) {
    let rel = file.strip_prefix(test_root).unwrap_or(file);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let dirs: Vec<String> = rel
        .parent()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();

    let top = dirs
        .first()
        .cloned()
        .or_else(|| parts.first().cloned())
        .unwrap_or_else(|| "<root>".to_string());
    let subsection = if dirs.len() >= 2 {
        format!("{}/{}", dirs[0], dirs[1])
    } else {
        top.clone()
    };
    (top, subsection)
}

/// Prints a summary of the test results for a given label and bucket of tests.
fn print_bucket_summary(
    label: &str,
    buckets: &HashMap<String, BucketStats>,
    limit: usize,
    sort_by_failures: bool,
) {
    let mut rows: Vec<_> = buckets.iter().collect();
    if sort_by_failures {
        rows.sort_by(|(an, av), (bn, bv)| {
            bv.fail
                .cmp(&av.fail)
                .then_with(|| bv.total().cmp(&av.total()))
                .then_with(|| an.cmp(bn))
        });
    } else {
        rows.sort_by(|(an, av), (bn, bv)| {
            bv.total()
                .cmp(&av.total())
                .then_with(|| bv.fail.cmp(&av.fail))
                .then_with(|| an.cmp(bn))
        });
    }

    eprintln!("{}", label);
    for (name, stats) in rows.into_iter().take(limit) {
        eprintln!(
            "  {:<28} pass {:>6} fail {:>6} skip {:>6}  pass rate {:>6.2}%",
            name,
            stats.pass,
            stats.fail,
            stats.skip,
            stats.pass_pct()
        );
    }
}

/// Runner options for the test262 test runner.
#[derive(ClapParser, Debug, Default)]
#[command(
    name = "test262",
    about = "Run the local Test262 connector against the blitz VM"
)]
struct RunnerOptions {
    /// List of test segments to include.
    #[arg(
        long = "segment",
        alias = "include",
        value_name = "SEGMENT",
        value_delimiter = ','
    )]
    includes: Vec<String>,
    /// List of test segments to exclude.
    #[arg(long = "exclude", value_name = "SEGMENT", value_delimiter = ',')]
    excludes: Vec<String>,
    /// Substring filter for test names.
    #[arg(long = "filter", value_name = "SUBSTRING")]
    filter: Option<String>,
    /// Optional maximum number of tests to run.
    #[arg(long = "limit", value_name = "N")]
    limit: Option<usize>,
    /// Run only harness files.
    #[arg(long = "harness-only", alias = "harness", action)]
    harness_only: bool,
}

fn parse_csv_env(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|val| {
            val.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Parses the command line arguments
fn parse_args() -> RunnerOptions {
    let mut opts = RunnerOptions::parse();

    // Keep historical env-var behavior as fallback when CLI flags are omitted.
    if opts.includes.is_empty() {
        opts.includes = parse_csv_env("TEST262_SEGMENTS");
    }
    if opts.excludes.is_empty() {
        opts.excludes = parse_csv_env("TEST262_EXCLUDE");
    }
    if opts.filter.is_none() {
        opts.filter = env::var("TEST262_FILTER").ok();
    }
    if opts.limit.is_none() {
        opts.limit = env::var("TEST262_LIMIT")
            .ok()
            .and_then(|s| s.parse::<usize>().ok());
    }

    opts
}

/// Minimal subset of metadata we care about.
#[derive(Debug, Deserialize)]
struct Negative {
    phase: String,
    #[serde(rename = "type")]
    ty: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    flags: Option<Vec<String>>,
    includes: Option<Vec<String>>,
    negative: Option<Negative>,
    #[allow(dead_code)]
    locale: Option<Vec<String>>,
}

#[derive(Clone)]
struct Test262Error<'a> {
    path: &'a Path,
    mode: Option<&'a str>,
    message: String,
}

impl<'a> Test262Error<'a> {
    fn new(path: &'a Path, mode: &'a str, message: String) -> Self {
        Self {
            path,
            mode: Some(mode),
            message,
        }
    }

    fn for_path(path: &'a Path, mode: Option<&'a str>, message: String) -> Self {
        Self {
            path,
            mode,
            message,
        }
    }

    fn without_mode(path: &'a Path, message: String) -> Self {
        Self::for_path(path, None, message)
    }

    fn into_string(self) -> String {
        match self.mode {
            Some(mode) => format!("{} ({}): {}", self.path.display(), mode, self.message),
            None => format!("{}: {}", self.path.display(), self.message),
        }
    }
}

// Helper to extract the front-matter YAML between /*--- and ---*/
fn extract_front_matter(src: &str) -> Option<String> {
    let start = src.find("/*---")?;
    let end = src.find("---*/")?;
    let fm = &src[start + "/*---".len()..end];
    Some(fm.trim().to_string())
}

/// Parse Test262 metadata YAML into `Meta`
fn parse_meta(src: &str) -> Option<Meta> {
    let fm = extract_front_matter(src)?;
    match serde_yaml::from_str::<Meta>(&fm) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("Failed to parse front-matter YAML: {}", e);
            None
        }
    }
}

/// Simple builtin print that writes to stderr (and returns `null`).
fn builtin_print(_vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    let s = print_args(args);
    eprintln!("[test262 print] {}", s);
    Ok(Value::Null)
}

fn print_args(args: &[Value]) -> String {
    args.iter()
        .map(|v| format!("{}", v))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_source_for_path(
    source: &str,
    path: &Path,
) -> Result<blitz::parser::ParserSession, blitz::errors::ParserError> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let source_type = if file_name.ends_with(".tsx") {
        SourceType::tsx()
    } else if file_name.ends_with(".ts")
        || file_name.ends_with(".mts")
        || file_name.ends_with(".cts")
    {
        SourceType::ts()
    } else {
        SourceType::unambiguous()
    };
    Parser::parse_source_with_type(source, source_type)
}

fn is_async_done_marker(s: &str) -> Option<Option<String>> {
    if s == "Test262:AsyncTestComplete" {
        Some(None)
    } else {
        s.strip_prefix("Test262:AsyncTestFailure:")
            .map(|rest| Some(rest.to_string()))
    }
}

/// A simple `$262.evalScript` implementation that evaluates the provided
/// source in the current realm (not a fresh realm). Per Test262 spec:
/// ParseScript → ScriptEvaluation in current realm.
///
/// The key fix: uses `Rc::clone` (shared handle) instead of deep cloning
/// globals, so mutations in evalScript are visible to the caller.
fn builtin_eval_script(globals: &Rc<RefCell<ObjectData>>, args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    let src = match &args[0] {
        Value::String(s) => s.clone(),
        _ => return Ok(Value::Null),
    };

    // Parse / compile / run in the current realm's globals.
    // Rc::clone gives us a shared handle — the new VM sees mutations from
    // the calling VM and vice versa. This matches spec: evalScript runs
    // in the SAME realm.
    let session = Parser::parse_source(&src).map_err(|e| format!("evalScript parse: {}", e))?;
    let prog = session.program();
    let chunk =
        compiler::compile_oxc(prog, &src).map_err(|e| format!("evalScript compile: {}", e))?;
    let mut eval_vm = Vm::new(Rc::clone(globals));
    eval_vm
        .run(chunk)
        .map_err(|e| format!("evalScript runtime: {}", e))
}

/// Create a `$262` object for a realm, with proper createRealm that builds
/// fresh intrinsics (not a heap clone) and evalScript that shares globals.
fn make_test262_object(globals: &Rc<RefCell<ObjectData>>) -> Value {
    let mut data = ObjectData::new();
    let function_proto = globals
        .borrow()
        .properties
        .get(&PropertyKey::String("Function".to_string()))
        .and_then(|desc| match desc {
            PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            _ => None,
        })
        .and_then(|function_ctor| match function_ctor {
            Value::Object(obj) => obj
                .borrow()
                .properties
                .get(&PropertyKey::String("prototype".to_string()))
                .and_then(|desc| match desc {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                }),
            Value::BuiltinObject(_, props) => props
                .borrow()
                .properties
                .get(&PropertyKey::String("prototype".to_string()))
                .and_then(|desc| match desc {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                }),
            _ => None,
        })
        .unwrap_or(Value::Null);

    let host_builtin = |f: blitz::value::BuiltinFn| {
        let props = Rc::new(RefCell::new(ObjectData::with_prototype(
            function_proto.clone(),
        )));
        Value::BuiltinObject(f, props)
    };

    // evalScript: shares globals via Rc::clone (not deep clone)
    let eval_globals = Rc::clone(globals);
    data.properties.insert(
        PropertyKey::String("evalScript".to_string()),
        PropertyDescriptor::data(host_builtin(Rc::new(move |_vm, args| {
            builtin_eval_script(&eval_globals, args).map_err(blitz::errors::RuntimeError::from)
        }))),
    );

    // IsHTMLDDA: plain object placeholder (full implementation requires exotic object semantics)
    data.properties.insert(
        PropertyKey::String("IsHTMLDDA".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(ObjectData::new())))),
    );

    // detachArrayBuffer: null out [[ArrayBufferData]] and set byteLength to 0
    data.properties.insert(
        PropertyKey::String("detachArrayBuffer".to_string()),
        PropertyDescriptor::data(host_builtin(Rc::new(|_vm, args| {
            if let Some(Value::Object(obj)) = args.first() {
                let mut obj_data = obj.borrow_mut();
                // Per spec: [[ArrayBufferData]] = null
                obj_data.properties.insert(
                    PropertyKey::String("[[ArrayBufferData]]".to_string()),
                    PropertyDescriptor::data(Value::Null),
                );
                obj_data.properties.insert(
                    PropertyKey::String("byteLength".to_string()),
                    PropertyDescriptor::data(Value::Number(0.0)),
                );
            }
            Ok(Value::Undefined)
        }))),
    );
    data.properties.insert(
        PropertyKey::String("gc".to_string()),
        PropertyDescriptor::data(host_builtin(Rc::new(|_vm, _args| {
            Err("gc not supported".into())
        }))),
    );
    data.properties.insert(
        PropertyKey::String("AbstractModuleSource".to_string()),
        PropertyDescriptor::data(Value::Undefined),
    );

    data.properties.insert(
        PropertyKey::String("agent".to_string()),
        PropertyDescriptor::data(host_builtin(Rc::new(|_vm, _args| {
            Err("agent not supported".into())
        }))),
    );

    // createRealm: builds a FRESH realm with fresh intrinsics, NOT a heap clone.
    // Per spec: each realm gets distinct intrinsic identities.
    data.properties.insert(
        PropertyKey::String("createRealm".to_string()),
        PropertyDescriptor::data(host_builtin(Rc::new(|_vm, _args| {
            let realm_global = Rc::new(RefCell::new(ObjectData::new()));

            runtime_utils::install_minimal_kernel_constructors(&realm_global);

            let new_262 = make_test262_object(&realm_global);

            realm_global.borrow_mut().properties.insert(
                PropertyKey::String("$262".to_string()),
                PropertyDescriptor::data(new_262.clone()),
            );

            if let Value::Object(obj) = &new_262 {
                obj.borrow_mut().properties.insert(
                    PropertyKey::String("global".to_string()),
                    PropertyDescriptor::data(Value::Object(Rc::clone(&realm_global))),
                );
            }

            Ok(new_262)
        }))),
    );

    // global: the realm's global object
    data.properties.insert(
        PropertyKey::String("global".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(globals))),
    );

    Value::Object(Rc::new(RefCell::new(data)))
}

/// Recursively gather `.js` test files under the given directory while skipping
/// files with `_FIXTURE` in their name.
fn collect_tests(dir: &Path, out: &mut Vec<PathBuf>, excludes: &[PathBuf]) {
    if !dir.exists() {
        return;
    }
    for ex in excludes {
        if dir.starts_with(ex) {
            return;
        }
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for ent in entries.flatten() {
        let p = ent.path();
        if excludes.iter().any(|ex| p.starts_with(ex)) {
            continue;
        }
        if let Ok(ft) = ent.file_type() {
            if ft.is_dir() {
                collect_tests(&p, out, excludes);
            } else if ft.is_file()
                && let Some(ext) = p.extension().and_then(|s| s.to_str())
                && ext == "js"
                && let Some(name) = p.file_name().and_then(|s| s.to_str())
            {
                if name.contains("_FIXTURE") {
                    continue;
                }
                out.push(p.clone());
            }
        }
    }
}

/// Decide whether a test is unsupported before execution begins.
fn unsupported_reason(_path: &Path, _src: &str, meta: &Option<Meta>) -> Option<String> {
    if let Some(m) = meta
        && let Some(flags) = &m.flags
        && flags
            .iter()
            .any(|f| matches!(f.as_str(), "module" | "noStrict" | "onlyStrict"))
    {
        // We will still handle onlyStrict/noStrict below in a limited way.
        let allow_module = env::var("TSBLITZ_TEST262_ENABLE_MODULE")
            .ok()
            .is_none_or(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        if flags.iter().any(|f| f == "module" && !allow_module) {
            let unsupported: Vec<&str> = flags
                .iter()
                .filter_map(|f| match f.as_str() {
                    "module" if !allow_module => Some(f.as_str()),
                    _ => None,
                })
                .collect();
            return Some(format!("UNSUPPORTED: flags {}", unsupported.join(",")));
        }
    }
    None
}

/// Restore globals to the state captured by a snapshot.
///
/// This removes properties that were ADDED during the test, and restores
/// properties that were MODIFIED. This is lighter than a full deep clone:
/// we only track keys, and we rely on the fact that builtins/closures share
/// state via `Rc`.
fn restore_globals(
    globals: &Rc<RefCell<ObjectData>>,
    baseline: &HashMap<PropertyKey, PropertyDescriptor>,
) {
    let mut g = globals.borrow_mut();
    // Remove keys that were added during the test
    g.properties.retain(|k, _| baseline.contains_key(k));
    // Restore keys that were modified
    for (k, baseline_desc) in baseline {
        g.properties.insert(k.clone(), baseline_desc.clone());
    }
}

/// Snapshot global property descriptors (deeper than just keys).
fn snapshot_globals(globals: &Rc<RefCell<ObjectData>>) -> HashMap<PropertyKey, PropertyDescriptor> {
    globals
        .borrow()
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Execute a single test file. Returns:
/// - Ok(())        -> test ran and passed
/// - Err(String)   -> test failed or is unsupported
///
/// Uses snapshot/restore for test isolation instead of deep heap cloning.
/// The `globals` Rc is shared across tests; we restore from `baseline_snapshot`
/// before each test mode to undo mutations from the previous test.
fn run_test_file(
    path: &Path,
    test262_root: &Path,
    globals: &Rc<RefCell<ObjectData>>,
    baseline_snapshot: &HashMap<PropertyKey, PropertyDescriptor>,
) -> Result<(), String> {
    let src = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let meta = parse_meta(&src);

    if let Some(reason) = unsupported_reason(path, &src, &meta) {
        return Err(Test262Error::without_mode(path, reason).into_string());
    }

    // Unless the test is `raw`, harness files should be evaluated first in the test realm.
    let raw_flag = meta
        .as_ref()
        .and_then(|m| m.flags.as_ref())
        .is_some_and(|f| f.iter().any(|s| s == "raw"));
    let has_async_flag = meta
        .as_ref()
        .and_then(|m| m.flags.as_ref())
        .is_some_and(|f| f.iter().any(|s| s == "async"));

    // Determine the run modes (non-strict / strict) according to flags:
    // - onlyStrict -> run strict only
    // - noStrict -> run non-strict only
    // - default -> run both non-strict and strict
    let mut run_non_strict = true;
    let mut run_strict = true;
    if let Some(m) = &meta
        && let Some(flags) = &m.flags
    {
        if flags.iter().any(|f| f == "onlyStrict") {
            run_non_strict = false;
            run_strict = true;
        } else if flags.iter().any(|f| f == "noStrict") {
            run_non_strict = true;
            run_strict = false;
        }
    }
    if raw_flag {
        run_non_strict = true;
        run_strict = false;
    }

    // Required harness files as per Test262 interpreting instructions.
    let mut harness_files: Vec<String> = vec!["assert.js".to_string(), "sta.js".to_string()];
    if has_async_flag && !raw_flag {
        harness_files.push("doneprintHandle.js".to_string());
    }

    // Include metadata-specified files after the harness.
    let mut include_files: Vec<String> = Vec::new();
    if let Some(m) = &meta
        && let Some(includes) = &m.includes
    {
        for inc in includes {
            // Normalize harness include paths: tests sometimes list includes
            // as "harness/assert.js" but we store and later load harness
            // files from test262/harness/<name>. Ensure we only keep the
            // basename when encountering a harness/ prefix to avoid
            // constructing paths like test262/harness/harness/assert.js.
            let normalized = if let Some(rest) = inc.strip_prefix("harness/") {
                rest.to_string()
            } else {
                inc.clone()
            };

            if normalized == "sta.js"
                || normalized == "assert.js"
                || normalized == "doneprintHandle.js"
                || include_files.iter().any(|h| h == &normalized)
            {
                continue;
            }
            include_files.push(normalized);
        }
    }

    let include_sources = if raw_flag {
        Vec::new()
    } else {
        let mut sources = Vec::new();
        for inc in &include_files {
            let path = test262_root.join("harness").join(inc);
            if !path.exists() {
                eprintln!(
                    "[test262] optional include missing (continuing): {}",
                    path.display()
                );
                continue;
            }
            let inc_src = fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read include {}: {}", path.display(), e))?;
            sources.push(inc_src);
        }
        sources
    };

    // For both modes, run and validate according to `negative` metadata.
    struct ResetStrict;
    impl Drop for ResetStrict {
        fn drop(&mut self) {
            typecheck::set_strict_override(None);
        }
    }
    for (mode_name, do_run) in &[("non-strict", run_non_strict), ("strict", run_strict)] {
        if !do_run {
            continue;
        }

        // Configure strictness for the engine (runtime-configured via TSBLITZ_STRICT).
        let is_strict = *mode_name == "strict";
        let is_async = has_async_flag;
        // Use the test-friendly override rather than setting process env vars.
        typecheck::set_strict_override(Some(false));
        let _reset_strict = ResetStrict;

        // Restore globals from the baseline snapshot (test isolation).
        // This undoes any mutations from the previous test without deep cloning.
        restore_globals(globals, baseline_snapshot);
        globals.borrow_mut().properties.insert(
            PropertyKey::String("__TSBLITZ_TEST_PATH".to_string()),
            PropertyDescriptor::data(Value::String(path.to_string_lossy().to_string())),
        );
        let mut vm = Vm::new(Rc::clone(globals));

        // Async tests must signal completion through $DONE(error?).
        let done_state: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
        if is_async {
            let done_state_for_print = Rc::clone(&done_state);
            vm.globals_rc().borrow_mut().properties.insert(
                PropertyKey::String("print".to_string()),
                PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                    let s = print_args(args);
                    if done_state_for_print.borrow().is_none()
                        && let Some(marker) = is_async_done_marker(&s)
                    {
                        *done_state_for_print.borrow_mut() = Some(marker);
                        return Ok(Value::Null);
                    }
                    eprintln!("[test262 print] {}", s);
                    Ok(Value::Null)
                }))),
            );
        }

        // Evaluate harness files unless `raw`: compile & run them into the provided VM.
        if !raw_flag {
            for h in &harness_files {
                let p = test262_root.join("harness").join(h);
                if p.exists() {
                    let hsrc = fs::read_to_string(&p)
                        .map_err(|e| format!("Failed to read harness {}: {}", p.display(), e))?;
                    // Parse/typecheck/compile the harness and run it in the provided VM
                    let session = match parse_source_for_path(&hsrc, &p) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!(
                                "Unsupported {} because harness {} parse failed: {}",
                                path.display(),
                                h,
                                e
                            );
                            return Err(format!(
                                "{} (UNSUPPORTED: harness {} parse failed: {})",
                                path.display(),
                                h,
                                e
                            ));
                        }
                    };
                    let prog = session.program();
                    let chunk = match compiler::compile_oxc(prog, &hsrc) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!(
                                "Unsupported {} because harness {} compile failed: {}",
                                path.display(),
                                h,
                                e
                            );
                            return Err(format!(
                                "{} (UNSUPPORTED: harness {} compile failed: {})",
                                path.display(),
                                h,
                                e
                            ));
                        }
                    };
                    if let Err(e) = vm.run(chunk) {
                        eprintln!(
                            "Unsupported {} because harness {} failed at runtime: {}",
                            path.display(),
                            h,
                            e
                        );
                        return Err(format!(
                            "{} (UNSUPPORTED: harness {} runtime failed: {})",
                            path.display(),
                            h,
                            e
                        ));
                    }
                } else {
                    eprintln!(
                        "Unsupported {} because harness {} missing",
                        path.display(),
                        h
                    );
                    return Err(format!(
                        "{} (UNSUPPORTED: harness {} missing)",
                        path.display(),
                        h
                    ));
                }
            }
        }
        // Apply strict override for the test source after harness setup.
        typecheck::set_strict_override(Some(is_strict));

        // Prepare source for this run (includes + test, with optional strict directive)
        let mut run_src = String::new();
        if *mode_name == "strict" && !raw_flag {
            // Per INTERPRETING.md: strict-mode run prefixes the entire source with "use strict".
            run_src.push_str("\"use strict\";\n");
        }
        for inc_src in &include_sources {
            run_src.push_str(inc_src);
            run_src.push('\n');
        }
        run_src.push_str(&src);

        // Handle parse-phase negative expectations first
        if let Some(m) = &meta
            && let Some(neg) = &m.negative
            && neg.phase == "parse"
        {
            // Expect parse to fail
            match Parser::parse_source(&run_src) {
                Ok(session) => {
                    // Parser succeeded, but check if compiler would catch this as a SyntaxError
                    let prog = session.program();
                    if let Err(e) = compiler::compile_oxc(prog, &run_src) {
                        let err_str = e.to_string();
                        // If the compiler caught a SyntaxError, treat this as the parser catching it
                        if err_str.contains("SyntaxError") || err_str.contains("redeclaration") {
                            continue;
                        }
                    }
                    return Err(Test262Error::new(
                        path,
                        mode_name,
                        "expected parse error (phase=parse) but parse succeeded".to_string(),
                    )
                    .into_string());
                }
                Err(_) => {
                    // got parse error as expected for this mode; continue to next mode
                    continue;
                }
            }
        }

        if let Some(m) = &meta
            && let Some(neg) = &m.negative
            && neg.phase == "resolution"
        {
            return Err(Test262Error::new(
                path,
                mode_name,
                "UNSUPPORTED resolution phase".to_string(),
            )
            .into_string());
        }

        // Otherwise, parse must succeed
        let session = match Parser::parse_source(&run_src) {
            Ok(s) => s,
            Err(e) => {
                return Err(Test262Error::new(
                    path,
                    mode_name,
                    format!("unexpected parse error: {e}"),
                )
                .into_string());
            }
        };
        let prog = session.program();

        // Compile the JS source directly; Test262 should not be gated on TS-only checks.
        let chunk = match compiler::compile_oxc(prog, &run_src) {
            Ok(c) => c,
            Err(e) => {
                return Err(
                    Test262Error::new(path, mode_name, format!("compile error: {e}")).into_string(),
                );
            }
        };

        // Execute using the provided persistent VM
        let run_result = vm.run(chunk);

        // For async tests, drain the microtask queue to ensure promise callbacks execute
        let run_result = if is_async {
            // First check if synchronous execution failed
            match run_result {
                Err(e) => Err(e),
                Ok(v) => {
                    // Drain microtasks to allow promise callbacks to run
                    if let Err(e) = vm.flush_microtasks() {
                        Err(e)
                    } else {
                        // Now check if $DONE was called
                        let start = Instant::now();
                        let timeout = Duration::from_secs(2);

                        let done_result = loop {
                            if let Some(state) = done_state.borrow().as_ref() {
                                let async_result = match state {
                                    None => Ok(v),
                                    Some(err) => Err(format!("async test failed: {}", err).into()),
                                };
                                break async_result;
                            }

                            if start.elapsed() > timeout {
                                break Err("async test timeout".into());
                            }

                            thread::sleep(Duration::from_millis(10));
                        };

                        done_result
                    }
                }
            }
        } else {
            run_result
        };

        // If the test expects a runtime error, ensure one occurred.
        if let Some(m) = &meta
            && let Some(neg) = &m.negative
            && neg.phase == "runtime"
        {
            match &run_result {
                Err(e) => {
                    if let Some(expected) = &neg.ty {
                        let err_str = e.to_string();
                        if !err_str.contains(expected) {
                            return Err(Test262Error::new(
                                path,
                                mode_name,
                                format!("expected error type {expected}, got {err_str}"),
                            )
                            .into_string());
                        }
                    }
                    continue;
                }
                Ok(_) => {
                    return Err(Test262Error::new(
                        path,
                        mode_name,
                        "expected runtime error but execution succeeded".to_string(),
                    )
                    .into_string());
                }
            }
        }

        // For non-negative tests, execution must succeed.
        if run_result.is_err() {
            return Err(Test262Error::new(
                path,
                mode_name,
                format!("unexpected runtime error: {}", run_result.err().unwrap()),
            )
            .into_string());
        }
    }

    // If we reached here for all required modes, the test passed.
    Ok(())
}

fn resolve_segment(test_root: &Path, segment: &str) -> PathBuf {
    let mut seg = segment.trim().trim_start_matches("./").to_string();
    if let Some(rest) = seg.strip_prefix("test/") {
        seg = rest.to_string();
    }
    match seg.as_str() {
        "" | "all" | "test" => test_root.to_path_buf(),
        "annexB" => test_root.join("annexB"),
        "language" => test_root.join("language"),
        "built-ins" => test_root.join("built-ins"),
        "intl402" => test_root.join("intl402"),
        "staging" => test_root.join("staging"),
        _ => test_root.join(seg),
    }
}

fn run_harness_only(
    test262_root: &Path,
    globals: &Rc<RefCell<ObjectData>>,
    baseline_snapshot: &HashMap<PropertyKey, PropertyDescriptor>,
) -> Result<(), String> {
    let harness_dir = test262_root.join("harness");
    let mut harness_files: Vec<String> = vec!["assert.js".to_string(), "sta.js".to_string()];
    if harness_dir.exists() {
        let mut extra: Vec<String> = Vec::new();
        for ent in fs::read_dir(&harness_dir)
            .map_err(|e| format!("Failed to read harness dir: {}", e))?
            .flatten()
        {
            let p = ent.path();
            if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if ext != "js" {
                    continue;
                }
            } else {
                continue;
            }
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                // Just store the filename, not the full path - it will be joined with harness dir later.
                if name == "sta.js" || name == "assert.js" {
                    continue;
                }
                extra.push(name.to_string());
            }
        }
        extra.sort();
        harness_files.extend(extra);
    }

    // Helper: print richer VM diagnostics when a harness file causes a runtime
    // failure. This includes preserved globals and important builtin shapes so
    // we can reason about missing functions or prototype properties.
    fn print_vm_diag(vm: &Vm, keep_globals: &HashSet<String>) {
        eprintln!("--- VM DIAGNOSTICS ---");
        eprintln!("Preserved globals: {:?}", keep_globals);
        let inspect_globals = [
            "$262",
            "Atomics",
            "Function",
            "Promise",
            "SharedArrayBuffer",
            "Int32Array",
        ];
        for name in &inspect_globals {
            match vm.get_global(name) {
                Some(v) => {
                    eprintln!("Global {}: {}", name, v);
                    match vm.list_global_own_keys(name) {
                        Some(keys) => eprintln!("  own keys: {:?}", keys),
                        None => eprintln!("  own keys: <none or non-object>"),
                    }
                    match vm.list_global_prototype_keys(name) {
                        Some(pkeys) => eprintln!("  prototype keys: {:?}", pkeys),
                        None => eprintln!("  prototype keys: <none or non-object>"),
                    }
                }
                None => eprintln!("Global {}: <missing>", name),
            }
        }
        eprintln!("--- END DIAGNOSTICS ---");
    }

    let keep_globals: HashSet<String> = globals
        .borrow()
        .properties
        .keys()
        .filter_map(|k| match k {
            PropertyKey::String(s) => Some(s.clone()),
            PropertyKey::Symbol(_) => None,
        })
        .collect();

    restore_globals(globals, baseline_snapshot);
    let mut vm = Vm::new(Rc::clone(globals));
    for h in &harness_files {
        let p = test262_root.join("harness").join(h);
        if !p.exists() {
            return Err(format!("Harness file missing: {}", p.display()));
        }
        let hsrc = fs::read_to_string(&p)
            .map_err(|e| format!("Failed to read harness {}: {}", p.display(), e))?;
        let session = parse_source_for_path(&hsrc, &p)
            .map_err(|e| format!("Harness {} parse failed: {}", h, e))?;
        let prog = session.program();
        let chunk = compiler::compile_oxc(prog, &hsrc)
            .map_err(|e| format!("Harness {} compile failed: {}", h, e))?;
        match vm.run(chunk) {
            Ok(_) => {
                eprintln!("Harness ok: {}", h);
            }
            Err(e) => {
                eprintln!("Harness {} runtime failed: {}", h, e);
                print_vm_diag(&vm, &keep_globals);
                return Err(format!("Harness {} runtime failed: {}", h, e));
            }
        }
    }
    Ok(())
}

fn main() {
    let opts = parse_args();

    // Use the local `./test262` checkout only. This runner is intended to be
    // hermetic and operate against a repository checked out next to the project.
    // If `./test262` is missing we skip the connector rather than using a
    // remote or environment-provided path.
    let test262_dir = PathBuf::from("test262");
    if !test262_dir.exists() {
        eprintln!("Local './test262' not found; skipping Test262 connector");
        exit(0);
    }

    let test_root = test262_dir.join("test");
    let mut files = Vec::new();
    let include_roots = if opts.includes.is_empty() {
        vec![test_root.clone()]
    } else {
        opts.includes
            .iter()
            .map(|s| resolve_segment(&test_root, s))
            .collect()
    };
    let exclude_roots: Vec<PathBuf> = opts
        .excludes
        .iter()
        .map(|s| resolve_segment(&test_root, s))
        .collect();

    for root in &include_roots {
        collect_tests(root, &mut files, &exclude_roots);
    }

    files.sort();

    let mut ran = 0usize;
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    let mut top_level: HashMap<String, BucketStats> = HashMap::new();
    let mut subsections: HashMap<String, BucketStats> = HashMap::new();

    // Create a persistent VM and globals, install host builtins and the soft-prelude
    let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));

    // Provide globalThis reference
    let global_this = Value::Object(Rc::clone(&globals));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string()),
        PropertyDescriptor::data(global_this),
    );

    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            builtin_print(vm, args).map_err(blitz::errors::RuntimeError::from)
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("print".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            builtin_print(vm, args).map_err(blitz::errors::RuntimeError::from)
        }))),
    );

    // ECMAScript global constants
    globals.borrow_mut().properties.insert(
        PropertyKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("Infinity".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );

    // Provide a few more kernel-native helpers the soft prelude expects.
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_new_object".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, _args| {
            use std::cell::RefCell;
            use std::rc::Rc;
            Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__define_property".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            vm.define_property_builtin(args)
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_getOwnPropertyDescriptor".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let target = args.first().cloned().unwrap_or(Value::Undefined);
            let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
            let key = vm.to_property_key(&key_value)?;
            Ok(runtime_utils::own_property_descriptor_value(&target, &key)
                .unwrap_or(Value::Undefined))
        }))),
    );

    // Seed the realm with kernel constructors before running tests.
    runtime_utils::install_minimal_kernel_constructors(&globals);

    // Wire $262 test harness object with proper createRealm (fresh intrinsics,
    // not heap clone) and evalScript (shares globals via Rc::clone).
    let test262_obj = make_test262_object(&globals);
    globals.borrow_mut().properties.insert(
        PropertyKey::String("$262".to_string()),
        PropertyDescriptor::data(test262_obj),
    );

    let _vm = Vm::new(globals.clone());

    // Snapshot the bootstrapped globals for test isolation.
    // Each test restores from this snapshot instead of deep-cloning.
    let baseline_snapshot = snapshot_globals(&globals);

    if opts.harness_only {
        match run_harness_only(&test262_dir, &globals, &baseline_snapshot) {
            Ok(()) => {
                eprintln!("Harness-only run: ok");
                exit(0);
            }
            Err(e) => {
                eprintln!("Harness-only run: fail: {}", e);
                exit(1);
            }
        }
    }

    for f in files {
        if let Some(limit) = opts.limit
            && ran >= limit
        {
            break;
        }
        if let Some(ref flt) = opts.filter
            && !f.to_string_lossy().contains(flt)
        {
            continue;
        }

        let (top_key, subsection_key) = bucket_keys(&test_root, &f);

        let result = run_test_in_thread(f.clone(), test262_dir.clone());

        match result {
            TestResult::Pass => {
                ran += 1;
                top_level.entry(top_key).or_default().pass += 1;
                subsections.entry(subsection_key).or_default().pass += 1;
            }
            TestResult::Fail(e) => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                eprintln!("FAIL: {}", e);
                failed.push((f.clone(), e));
            }
            TestResult::StackOverflow => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                if let Ok(rel) = f.strip_prefix(&test262_dir) {
                    eprintln!("STACKOVERFLOW: {}", rel.display());
                } else {
                    eprintln!("STACKOVERFLOW: {}", f.display());
                }
                failed.push((f.clone(), "Stack overflow".into()));
            }
            TestResult::Panic(msg) => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                if let Ok(rel) = f.strip_prefix(&test262_dir) {
                    eprintln!("PANIC: {}: {}", rel.display(), msg);
                } else {
                    eprintln!("PANIC: {}: {}", f.display(), msg);
                }
                failed.push((f.clone(), format!("Panic: {}", msg)));
            }
        }
    }

    let passed = ran.saturating_sub(failed.len());
    let pass_pct = if ran == 0 {
        0.0
    } else {
        (passed as f64 / ran as f64) * 100.0
    };

    let exit_code = if failed.is_empty() { 0 } else { 1 };

    eprintln!(
        "Test262: ran {}, passed {}, failed {} ({:.2}% pass rate)",
        ran,
        passed,
        failed.len(),
        pass_pct
    );
    print_bucket_summary("Sections:", &top_level, 12, false);

    exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock moved backwards")
            .as_nanos();
        let dir = env::temp_dir().join(format!("blitz-{prefix}-{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    struct TempDirGuard(PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn bucket_keys_groups_top_level_and_subsection() {
        let root = PathBuf::from("/repo/test262/test");
        let file = root.join("built-ins/Array/prototype/push.js");

        let (top, subsection) = bucket_keys(&root, &file);

        assert_eq!(top, "built-ins");
        assert_eq!(subsection, "built-ins/Array");
    }

    #[test]
    fn parse_csv_env_trims_and_discards_empty_values() {
        let _guard = env_lock().lock().expect("env lock");
        let name = "BLITZ_TEST_PARSE_CSV_ENV";

        unsafe {
            env::set_var(name, "  alpha, ,beta,, gamma  ");
        }

        let parsed = parse_csv_env(name);

        unsafe {
            env::remove_var(name);
        }

        assert_eq!(parsed, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn extract_front_matter_and_parse_meta_read_test262_metadata() {
        let src = r#"/*---
flags: [module, async]
includes:
  - harness/assert.js
  - extra.js
negative:
  phase: runtime
---*/
const x = 1;
"#;

        let fm = extract_front_matter(src).expect("front matter");
        assert!(fm.contains("flags: [module, async]"));
        assert!(fm.contains("includes:"));

        let meta = parse_meta(src).expect("metadata");
        let flags = meta.flags.expect("flags");
        assert_eq!(flags, vec!["module", "async"]);
        let includes = meta.includes.expect("includes");
        assert_eq!(includes, vec!["harness/assert.js", "extra.js"]);
        let negative = meta.negative.expect("negative");
        assert_eq!(negative.phase, "runtime");
    }

    #[test]
    fn unsupported_reason_checks_module_flag_toggle() {
        let _guard = env_lock().lock().expect("env lock");
        let src = "/*---\nflags: [module]\n---*/\n";
        let meta = parse_meta(src);
        let path = Path::new("/tmp/test.js");

        unsafe {
            env::set_var("TSBLITZ_TEST262_ENABLE_MODULE", "0");
        }
        assert_eq!(
            unsupported_reason(path, src, &meta),
            Some("UNSUPPORTED: flags module".to_string())
        );

        unsafe {
            env::set_var("TSBLITZ_TEST262_ENABLE_MODULE", "1");
        }
        assert_eq!(unsupported_reason(path, src, &meta), None);

        unsafe {
            env::remove_var("TSBLITZ_TEST262_ENABLE_MODULE");
        }
    }

    #[test]
    fn collect_tests_skips_fixture_files_and_excluded_roots() {
        let root = temp_dir("collect-tests");
        let _guard = TempDirGuard(root.clone());
        let include_dir = root.join("language");
        let excluded_dir = root.join("built-ins");

        write_file(&include_dir.join("good.js"), "const ok = 1;\n");
        write_file(&include_dir.join("skip_FIXTURE.js"), "const skip = 1;\n");
        write_file(&excluded_dir.join("ignored.js"), "const nope = 1;\n");

        let mut out = Vec::new();
        collect_tests(&root, &mut out, &[excluded_dir.clone()]);
        out.sort();

        assert_eq!(out, vec![include_dir.join("good.js")]);
    }
}
