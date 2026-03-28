/*!
Test262 connector script for Blitz.

Usage:
- Optionally set `TEST262_FILTER` to a substring to limit which tests are run
- Optionally set `TEST262_LIMIT` to limit how many tests are executed (default: all)
- Optionally set `TEST262_SEGMENTS` to a comma-separated list of segments
- Optionally set `TEST262_EXCLUDE` to a comma-separated list of segments to skip

CLI (preferred over env):
- --segment <seg>[,<seg>...] (repeatable)
- --exclude <seg>[,<seg>...] (repeatable)
- --filter <substring>
- --limit <n>

Segments may be labels like: annexB, language, built-ins, intl402, staging
or paths relative to test262/test (e.g. built-ins/Array).

This connector is intentionally conservative: it runs a subset of
tests that don't require modules, raw-mode harness behavior, or other
features not yet implemented by the engine. The goal is to provide a
small, usable connector that validates end-to-end execution of simple
Test262 tests and demonstrates how to wire up harness files and host
APIs.
*/

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::rc::Rc;

use clap::Parser as ClapParser;
use serde::Deserialize;

use blitz::compiler;
use blitz::trace;
use blitz::value::{ObjectData, PromiseState, PropertyDescriptor, PropertyKey};
use blitz::{module::ModuleLoader, parser::Parser, typecheck, value::Value, vm::Vm};

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

fn has_distinct_subsections(
    top_level: &HashMap<String, BucketStats>,
    subsections: &HashMap<String, BucketStats>,
) -> bool {
    subsections.len() > top_level.len()
        || subsections.keys().any(|key| !top_level.contains_key(key))
}

#[derive(Default)]
struct CloneCtx {
    objects: HashMap<usize, Rc<RefCell<ObjectData>>>,
    arrays: HashMap<usize, Rc<RefCell<Vec<Option<Value>>>>>,
    envs: HashMap<usize, Rc<RefCell<Vec<Rc<RefCell<Value>>>>>>,
}

fn clone_descriptor(desc: &PropertyDescriptor, ctx: &mut CloneCtx) -> PropertyDescriptor {
    match desc {
        PropertyDescriptor::Data {
            value,
            writable,
            enumerable,
            configurable,
        } => PropertyDescriptor::Data {
            value: clone_value(value, ctx),
            writable: *writable,
            enumerable: *enumerable,
            configurable: *configurable,
        },
        PropertyDescriptor::Accessor {
            get,
            set,
            enumerable,
            configurable,
        } => PropertyDescriptor::Accessor {
            get: get.as_ref().map(|v| clone_value(v, ctx)),
            set: set.as_ref().map(|v| clone_value(v, ctx)),
            enumerable: *enumerable,
            configurable: *configurable,
        },
    }
}

fn clone_object_data(obj: &Rc<RefCell<ObjectData>>, ctx: &mut CloneCtx) -> Rc<RefCell<ObjectData>> {
    let key = Rc::as_ptr(obj) as usize;
    if let Some(existing) = ctx.objects.get(&key) {
        return Rc::clone(existing);
    }

    let cloned = Rc::new(RefCell::new(ObjectData::new()));
    ctx.objects.insert(key, Rc::clone(&cloned));

    let src = obj.borrow();
    let prototype = clone_value(&src.prototype, ctx);
    let extensible = src.extensible;
    let properties = src
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), clone_descriptor(v, ctx)))
        .collect();
    drop(src);

    let mut dst = cloned.borrow_mut();
    dst.prototype = prototype;
    dst.extensible = extensible;
    dst.properties = properties;
    drop(dst);

    cloned
}

fn clone_array_elements(
    arr: &Rc<RefCell<Vec<Option<Value>>>>,
    ctx: &mut CloneCtx,
) -> Rc<RefCell<Vec<Option<Value>>>> {
    let key = Rc::as_ptr(arr) as usize;
    if let Some(existing) = ctx.arrays.get(&key) {
        return Rc::clone(existing);
    }

    let cloned = Rc::new(RefCell::new(Vec::new()));
    ctx.arrays.insert(key, Rc::clone(&cloned));

    let values = arr
        .borrow()
        .iter()
        .map(|opt_v| opt_v.as_ref().map(|v| clone_value(v, ctx)))
        .collect();
    *cloned.borrow_mut() = values;
    cloned
}

fn clone_env(
    env: &Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
    ctx: &mut CloneCtx,
) -> Rc<RefCell<Vec<Rc<RefCell<Value>>>>> {
    let key = Rc::as_ptr(env) as usize;
    if let Some(existing) = ctx.envs.get(&key) {
        return Rc::clone(existing);
    }

    let cloned = Rc::new(RefCell::new(Vec::new()));
    ctx.envs.insert(key, Rc::clone(&cloned));

    let values = env
        .borrow()
        .iter()
        .map(|cell| Rc::new(RefCell::new(clone_value(&cell.borrow(), ctx))))
        .collect();
    *cloned.borrow_mut() = values;
    cloned
}

fn clone_value(value: &Value, ctx: &mut CloneCtx) -> Value {
    match value {
        Value::Number(n) => Value::Number(*n),
        Value::String(s) => Value::String(s.clone()),
        Value::Bool(b) => Value::Bool(*b),
        Value::Undefined => Value::Undefined,
        Value::BigInt(n) => Value::BigInt(*n),
        Value::Symbol(id, desc) => Value::Symbol(id.clone(), desc.clone()),
        Value::Null => Value::Null,
        Value::Object(obj) => Value::Object(clone_object_data(obj, ctx)),
        Value::Array(arr, props) => Value::Array(
            clone_array_elements(arr, ctx),
            clone_object_data(props, ctx),
        ),
        Value::Closure {
            proto,
            env,
            lexical_this,
            props,
        } => Value::Closure {
            proto: Rc::clone(proto),
            env: clone_env(env, ctx),
            lexical_this: lexical_this.as_ref().map(|v| Box::new(clone_value(v, ctx))),
            props: clone_object_data(props, ctx),
        },
        Value::Builtin(f) => Value::Builtin(Rc::clone(f)),
        Value::BuiltinObject(f, props) => {
            Value::BuiltinObject(Rc::clone(f), clone_object_data(props, ctx))
        }
        Value::Promise(state) => {
            let next_state = match &*state.borrow() {
                PromiseState::Pending {
                    on_fulfilled,
                    on_rejected,
                } => PromiseState::Pending {
                    on_fulfilled: on_fulfilled.iter().map(|v| clone_value(v, ctx)).collect(),
                    on_rejected: on_rejected.iter().map(|v| clone_value(v, ctx)).collect(),
                },
                PromiseState::Fulfilled(v) => PromiseState::Fulfilled(clone_value(v, ctx)),
                PromiseState::Rejected(v) => PromiseState::Rejected(clone_value(v, ctx)),
            };
            Value::Promise(Rc::new(RefCell::new(next_state)))
        }
    }
}

fn clone_globals(globals: &Rc<RefCell<ObjectData>>) -> Rc<RefCell<ObjectData>> {
    let mut ctx = CloneCtx::default();
    clone_object_data(globals, &mut ctx)
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
    locale: Option<Vec<String>>,
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
    let s = args
        .iter()
        .map(|v| format!("{}", v))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("[test262 print] {}", s);
    Ok(Value::Null)
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
    let chunk = compiler::compile_oxc(prog).map_err(|e| format!("evalScript compile: {}", e))?;
    let mut eval_vm = Vm::new(Rc::clone(globals));
    eval_vm
        .run(chunk)
        .map_err(|e| format!("evalScript runtime: {}", e))
}

/// Create a `$262` object for a realm, with proper createRealm that builds
/// fresh intrinsics (not a heap clone) and evalScript that shares globals.
fn make_test262_object(globals: &Rc<RefCell<ObjectData>>) -> Value {
    let mut data = ObjectData::new();

    // evalScript: shares globals via Rc::clone (not deep clone)
    let eval_globals = Rc::clone(globals);
    data.properties.insert(
        PropertyKey::String("evalScript".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
            builtin_eval_script(&eval_globals, args)
                .map_err(blitz::errors::RuntimeError::from)
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
        PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
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

    // createRealm: builds a FRESH realm with fresh intrinsics, NOT a heap clone.
    // Per spec: each realm gets distinct intrinsic identities.
    data.properties.insert(
        PropertyKey::String("createRealm".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, _args| {
            // Create a fresh global object for the new realm.
            let realm_global = Rc::new(RefCell::new(ObjectData::new()));

            // Install builtins from scratch — gives each realm distinct
            // constructor/prototype identities. This is correct per spec:
            // Realm = fresh intrinsics, not a clone of the parent.
            blitz::runtime_utils::install_minimal_kernel_constructors(&realm_global);

            // Copy the calling realm's $262 object structure into the new realm.
            let new_262 = make_test262_object(&realm_global);

            // Wire the new realm's global to also have a $262 property.
            realm_global.borrow_mut().properties.insert(
                PropertyKey::String("$262".to_string()),
                PropertyDescriptor::data(new_262.clone()),
            );

            // The $262 object also exposes `global` pointing to the realm's global.
            let mut realm_wrapper = ObjectData::new();
            realm_wrapper.properties.insert(
                PropertyKey::String("global".to_string()),
                PropertyDescriptor::data(Value::Object(Rc::clone(&realm_global))),
            );
            // Copy all properties from the $262 object
            if let Value::Object(ref obj) = new_262 {
                for (k, v) in &obj.borrow().properties {
                    realm_wrapper.properties.insert(k.clone(), v.clone());
                }
            }

            Ok(Value::Object(Rc::new(RefCell::new(realm_wrapper))))
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
        && flags.iter().any(|f| {
            matches!(
                f.as_str(),
                "module" | "raw" | "noStrict" | "onlyStrict" // onlyStrict/noStrict handled below, but we don't run `module/raw`
            )
        })
    {
        // We will still handle onlyStrict/noStrict below in a limited way,
        // but module/raw are skipped entirely.
        if flags.iter().any(|f| f == "module" || f == "raw") {
            let unsupported: Vec<&str> = flags
                .iter()
                .filter_map(|f| match f.as_str() {
                    "module" | "raw" => Some(f.as_str()),
                    _ => None,
                })
                .collect();
            return Some(format!("UNSUPPORTED: flags {}", unsupported.join(",")));
        }
    }
    None
}

/// Snapshot the set of global property keys (for test isolation).
fn snapshot_global_keys(globals: &Rc<RefCell<ObjectData>>) -> HashSet<PropertyKey> {
    globals.borrow().properties.keys().cloned().collect()
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
        return Err(format!("{} ({})", path.display(), reason));
    }

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

    // Unless the test is `raw`, harness files should be evaluated first in the test realm.
    let raw_flag = meta
        .as_ref()
        .and_then(|m| m.flags.as_ref())
        .is_some_and(|f| f.iter().any(|s| s == "raw"));

    // Required harness files as per Test262 interpreting instructions.
    let mut harness_files: Vec<String> = vec!["sta.js".to_string(), "assert.js".to_string()];
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

            if normalized != "sta.js"
                && normalized != "assert.js"
                && !harness_files.iter().any(|h| h == &normalized)
            {
                harness_files.push(normalized);
            }
        }
    }

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
        let is_async = meta
            .as_ref()
            .and_then(|m| m.flags.as_ref())
            .is_some_and(|flags| flags.iter().any(|f| f == "async"));
        // Use the test-friendly override rather than setting process env vars.
        typecheck::set_strict_override(Some(false));
        let _reset_strict = ResetStrict;

        // Restore globals from the baseline snapshot (test isolation).
        // This undoes any mutations from the previous test without deep cloning.
        restore_globals(globals, baseline_snapshot);
        let mut vm = Vm::new(Rc::clone(globals));

        // Async tests must signal completion through $DONE(error?).
        let done_state: Rc<RefCell<Option<Option<Value>>>> = Rc::new(RefCell::new(None));
        if is_async {
            let done_state_for_cb = Rc::clone(&done_state);
            vm.globals_rc().borrow_mut().properties.insert(
                PropertyKey::String("$DONE".to_string()),
                PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                    if trace::enabled() {
                        eprintln!("[DONE host] called args={:?}", args);
                    }
                    // Record only the first completion signal.
                    if done_state_for_cb.borrow().is_none() {
                        let first = args.first().cloned().unwrap_or(Value::Undefined);
                        let payload = if matches!(first, Value::Undefined) {
                            None
                        } else {
                            Some(first)
                        };
                        *done_state_for_cb.borrow_mut() = Some(payload);
                    }
                    Ok(Value::Undefined)
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
                    let session = match Parser::parse_source(&hsrc) {
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
                    let chunk = match compiler::compile_oxc(prog) {
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

        // Prepare source for this run (strict-mode transformation if needed)
        let run_src = if *mode_name == "strict" && !raw_flag {
            // Per INTERPRETING.md: insert "use strict"; at the start
            format!("\"use strict\";\n{}", src)
        } else {
            src.clone()
        };

        // Handle parse-phase negative expectations first
        if let Some(m) = &meta
            && let Some(neg) = &m.negative
            && neg.phase == "parse"
        {
            // Expect parse to fail
            match Parser::parse_source(&run_src) {
                Ok(_) => {
                    return Err(format!(
                        "{} ({}): expected parse error (phase=parse) but parse succeeded",
                        path.display(),
                        mode_name
                    ));
                }
                Err(_) => {
                    // got parse error as expected for this mode; continue to next mode
                    continue;
                }
            }
        }

        // Otherwise, parse must succeed
        let session = match Parser::parse_source(&run_src) {
            Ok(s) => s,
            Err(e) => {
                return Err(format!(
                    "{} ({}): unexpected parse error: {}",
                    path.display(),
                    mode_name,
                    e
                ));
            }
        };
        let prog = session.program();

        // Compile the JS source directly; Test262 should not be gated on TS-only checks.
        let chunk = match compiler::compile_oxc(prog) {
            Ok(c) => c,
            Err(e) => {
                return Err(format!(
                    "{} ({}): compile error: {}",
                    path.display(),
                    mode_name,
                    e
                ));
            }
        };

        // Execute using the provided persistent VM
        let run_result = vm.run(chunk);

        // Async tests report result via $DONE.
        let run_result = if is_async {
            match run_result {
                Err(e) => Err(e),
                Ok(v) => match done_state.borrow().as_ref() {
                    Some(None) => Ok(v),
                    Some(Some(done_err)) => {
                        let msg = match vm.to_string(done_err) {
                            Ok(s) => s,
                            Err(_) => "async test failed".to_string(),
                        };
                        Err(format!("async $DONE called with error: {}", msg).into())
                    }
                    None => Err("async test did not call $DONE".into()),
                },
            }
        } else {
            run_result
        };

        // If the test expects a runtime error, ensure one occurred.
        if let Some(m) = &meta
            && let Some(neg) = &m.negative
            && neg.phase == "runtime"
        {
            if run_result.is_err() {
                // ok - expected runtime error
                continue;
            } else {
                return Err(format!(
                    "{} ({}): expected runtime error but execution succeeded",
                    path.display(),
                    mode_name
                ));
            }
        }

        // For non-negative tests, execution must succeed.
        if run_result.is_err() {
            return Err(format!(
                "{} ({}): unexpected runtime error: {}",
                path.display(),
                mode_name,
                run_result.err().unwrap()
            ));
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
    let mut harness_files: Vec<String> = Vec::new();
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
                // Just store the filename, not the full path - it will be joined with harness dir later
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

    for h in &harness_files {
        restore_globals(globals, baseline_snapshot);
        let mut vm = Vm::new(Rc::clone(globals));
        let p = test262_root.join("harness").join(h);
        if !p.exists() {
            return Err(format!("Harness file missing: {}", p.display()));
        }
        let hsrc = fs::read_to_string(&p)
            .map_err(|e| format!("Failed to read harness {}: {}", p.display(), e))?;
        let session = Parser::parse_source(&hsrc)
            .map_err(|e| format!("Harness {} parse failed: {}", h, e))?;
        let prog = session.program();
        let chunk = compiler::compile_oxc(prog)
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
    // Use the local `./test262` checkout only. This runner is intended to be
    // hermetic and operate against a repository checked out next to the project.
    // If `./test262` is missing we skip the connector rather than using a
    // remote or environment-provided path.
    let test262_dir = PathBuf::from("test262");
    if !test262_dir.exists() {
        eprintln!("Local './test262' not found; skipping Test262 connector");
        exit(0);
    }

    let opts = parse_args();

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

    // Seed the realm with kernel constructors before running the soft prelude.
    // The prelude may extend/replace these; we avoid re-installing kernel
    // constructors afterward to preserve prelude-level constructor identity.
    blitz::runtime_utils::install_minimal_kernel_constructors(&globals);

    // Wire $262 test harness object with proper createRealm (fresh intrinsics,
    // not heap clone) and evalScript (shares globals via Rc::clone).
    let test262_obj = make_test262_object(&globals);
    globals.borrow_mut().properties.insert(
        PropertyKey::String("$262".to_string()),
        PropertyDescriptor::data(test262_obj),
    );

    let mut vm = Vm::new(globals.clone());
    // Run soft prelude once (if present) to install runtime helpers
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    if prelude_path.exists() {
        let mut loader = ModuleLoader::new();
        if let Err(e) = (|| -> Result<(), String> {
            loader
                .construct(
                    prelude_path.parent().unwrap_or(Path::new(".")),
                    &format!("./{}", prelude_path.file_name().unwrap().to_string_lossy()),
                )
                .map_err(|e| format!("construct prelude failed: {}", e))?;
            loader
                .instantiate_all()
                .map_err(|e| format!("instantiate prelude failed: {}", e))?;
            loader
                .evaluate_entry(&prelude_path, &mut vm)
                .map_err(|e| format!("evaluate prelude failed: {}", e))?;
            Ok(())
        })() {
            eprintln!("Failed to execute soft prelude: {}", e);
        }
    }

    // Snapshot the fully bootstrapped globals for test isolation.
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

        eprintln!("Running test: {}", f.display());
        let (top_key, subsection_key) = bucket_keys(&test_root, &f);
        match run_test_file(&f, &test262_dir, &globals, &baseline_snapshot) {
            Ok(()) => {
                ran += 1;
                top_level.entry(top_key).or_default().pass += 1;
                subsections.entry(subsection_key).or_default().pass += 1;
                eprintln!("  PASS");
            }
            Err(e) => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                eprintln!("  FAIL: {}", e);
                failed.push((f.clone(), e));
            }
        }
    }

    let passed = ran.saturating_sub(failed.len());
    let pass_pct = if ran == 0 {
        0.0
    } else {
        (passed as f64 / ran as f64) * 100.0
    };
    let show_subsections = has_distinct_subsections(&top_level, &subsections);

    if !failed.is_empty() {
        eprintln!(
            "Summary: ran {}, passed {}, failed {}, pass rate {:.2}%",
            ran,
            passed,
            failed.len(),
            pass_pct
        );
        print_bucket_summary("Sections:", &top_level, 12, false);
        if show_subsections {
            print_bucket_summary("Worst subsections:", &subsections, 15, true);
        }

        // Instead of listing every failing file, group failures by subsection
        // and print the failing subsection folders only.
        if show_subsections {
            let mut failed_by_subsection: HashMap<String, usize> = HashMap::new();
            for (p, _m) in &failed {
                let (_top, subsection) = bucket_keys(&test_root, p);
                *failed_by_subsection.entry(subsection).or_default() += 1;
            }
            let mut rows: Vec<_> = failed_by_subsection.into_iter().collect();
            rows.sort_by(|(an, ac), (bn, bc)| bc.cmp(ac).then_with(|| an.cmp(bn)));
            eprintln!("Failed subsections:");
            for (name, cnt) in rows.into_iter() {
                eprintln!("  {:<40} fail {}", name, cnt);
            }
        }
        eprintln!("Failed tests:");
        for (path, msg) in &failed {
            let rel = path.strip_prefix(&test262_dir).unwrap_or(path);
            eprintln!("  {}: {}", rel.display(), msg);
        }
        exit(1);
    } else {
        eprintln!(
            "Test262 connector: ran {} tests, passed {} ({:.2}%)",
            ran, passed, pass_pct
        );
        print_bucket_summary("Sections:", &top_level, 12, false);
        if show_subsections {
            print_bucket_summary("Worst subsections:", &subsections, 15, true);
        }
        exit(0);
    }
}
