/*!
Test262 connector script for tsblitz.

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

This connector is intentionally conservative: it runs a small subset of
tests that don't require modules, async harnesses, includes, or other
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

use serde::Deserialize;

use tsblitz::compiler;
use tsblitz::value::{ObjectData, PropertyDescriptor, PropertyKey};
use tsblitz::{module::ModuleLoader, parser::Parser, typecheck, value::Value, vm::Vm};

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

    let top = parts
        .first()
        .cloned()
        .unwrap_or_else(|| "<root>".to_string());
    let subtree = if parts.len() >= 2 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        top.clone()
    };
    (top, subtree)
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

#[derive(Default)]
struct CloneCtx {
    objects: HashMap<usize, Rc<RefCell<ObjectData>>>,
    arrays: HashMap<usize, Rc<RefCell<Vec<Value>>>>,
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
    arr: &Rc<RefCell<Vec<Value>>>,
    ctx: &mut CloneCtx,
) -> Rc<RefCell<Vec<Value>>> {
    let key = Rc::as_ptr(arr) as usize;
    if let Some(existing) = ctx.arrays.get(&key) {
        return Rc::clone(existing);
    }

    let cloned = Rc::new(RefCell::new(Vec::new()));
    ctx.arrays.insert(key, Rc::clone(&cloned));

    let values = arr.borrow().iter().map(|v| clone_value(v, ctx)).collect();
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
            proto, env, props, ..
        } => Value::Closure {
            proto: Rc::clone(proto),
            env: clone_env(env, ctx),
            lexical_this: None,
            props: clone_object_data(props, ctx),
        },
        Value::Builtin(f) => Value::Builtin(Rc::clone(f)),
        Value::BuiltinObject(f, props) => {
            Value::BuiltinObject(Rc::clone(f), clone_object_data(props, ctx))
        }
    }
}

fn clone_globals(globals: &Rc<RefCell<ObjectData>>) -> Rc<RefCell<ObjectData>> {
    let mut ctx = CloneCtx::default();
    clone_object_data(globals, &mut ctx)
}

fn make_vm_from_snapshot(snapshot: &Rc<RefCell<ObjectData>>) -> Vm {
    Vm::new(clone_globals(snapshot))
}

/// Runner options for the test262 test runner.
struct RunnerOptions {
    /// List of test segments to include.
    includes: Vec<String>,
    /// List of test segments to exclude.
    excludes: Vec<String>,
    /// Substring filter for test names.
    filter: Option<String>,
    /// Optional maximum number of tests to run.
    limit: Option<usize>,
    /// Run only harness files.
    harness_only: bool,
}

/// Prints the usage information for the test262 runner.
fn print_usage() {
    eprintln!("test262 runner options:");
    eprintln!("  --segment <seg>[,<seg>...]   Include segment(s)");
    eprintln!("  --exclude <seg>[,<seg>...]   Exclude segment(s)");
    eprintln!("  --filter <substring>         Substring filter");
    eprintln!("  --limit <n>                  Max tests (default: all)");
    eprintln!("  --harness-only               Run only harness files");
    eprintln!("segments: annexB, language, built-ins, intl402, staging, or path relative to test/");
}

/// Parses the command line arguments
///
/// Returns a `RunnerOptions` struct with the parsed values.
fn parse_args() -> RunnerOptions {
    let mut includes: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut filter = env::var("TEST262_FILTER").ok();
    let mut limit: Option<usize> = env::var("TEST262_LIMIT").ok().and_then(|s| s.parse().ok());
    let mut harness_only = false;

    if let Ok(seg) = env::var("TEST262_SEGMENTS") {
        includes.extend(
            seg.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }
    if let Ok(seg) = env::var("TEST262_EXCLUDE") {
        excludes.extend(
            seg.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }

    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage();
                exit(0);
            }
            "--segment" | "--include" => {
                if let Some(val) = it.next() {
                    includes.extend(
                        val.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from),
                    );
                }
            }
            "--exclude" => {
                if let Some(val) = it.next() {
                    excludes.extend(
                        val.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from),
                    );
                }
            }
            "--filter" => {
                if let Some(val) = it.next() {
                    filter = Some(val);
                }
            }
            "--limit" => {
                if let Some(val) = it.next()
                    && let Ok(n) = val.parse::<usize>()
                {
                    limit = Some(n);
                }
            }
            "--harness-only" | "--harness" => {
                harness_only = true;
            }
            _ => {}
        }
    }

    RunnerOptions {
        includes,
        excludes,
        filter,
        limit,
        harness_only,
    }
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
    // extract content between the full token length and end (skip the starting token)
    let fm = &src[start + "/*---".len()..end];
    Some(fm.trim().to_string())
}

/// Parse Test262 metadata YAML into `Meta`
fn parse_meta(src: &str) -> Option<Meta> {
    let fm = extract_front_matter(src)?;
    // serde_yaml is available via Cargo dependencies for this binary
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
/// source in a fresh realm (new globals). This is a fairly small stub but
/// helps tests that call `$262.evalScript(...)`.
fn builtin_eval_script(_vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    let src = match &args[0] {
        Value::String(s) => s.clone(),
        _ => return Ok(Value::Null),
    };

    // Build a minimal new realm
    let new_globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
    new_globals.borrow_mut().properties.insert(
        PropertyKey::String("print".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            builtin_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }))),
    );

    // Parse / compile / run. Ignore errors and return Null on failure.
    match Parser::parse_source(&src) {
        Ok(session) => {
            let prog = session.program();
            // compile expects the owned local_ast for now; convert via ESTree JSON
            let _estree = prog.to_pretty_estree_ts_json(false);
            // Use the temporary compile_oxc shim so we can avoid an owned local_ast here.
            match compiler::compile_oxc(prog) {
                Ok(chunk) => {
                    let mut vm = Vm::new(new_globals);
                    match vm.run(chunk) {
                        Ok(v) => Ok(v),
                        Err(e) => {
                            eprintln!("[evalScript] runtime error: {}", e);
                            Ok(Value::Null)
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[evalScript] compile error: {}", e);
                    Ok(Value::Null)
                }
            }
        }
        Err(e) => {
            eprintln!("[evalScript] parse error: {}", e);
            Ok(Value::Null)
        }
    }
}

/// Run a source text in the provided globals and return a Result.
fn run_source_in_realm(globals: Rc<RefCell<ObjectData>>, source: &str) -> Result<Value, String> {
    let session = Parser::parse_source(source).map_err(|e| format!("parse: {}", e))?;
    let prog = session.program();
    let chunk = compiler::compile_oxc(prog).map_err(|e| format!("compile: {}", e))?;
    let mut vm = Vm::new(globals);
    vm.run(chunk).map_err(|e| format!("runtime: {}", e))
}

fn install_test262_realm_helpers(
    globals: &Rc<RefCell<ObjectData>>,
    realm_seed: &Rc<RefCell<ObjectData>>,
) {
    let seed = Rc::clone(realm_seed);
    let create_realm = Value::Builtin(Rc::new(move |_vm, _args| {
        let realm_globals = clone_globals(&seed);
        let eval_globals = Rc::clone(&realm_globals);
        realm_globals.borrow_mut().properties.insert(
            PropertyKey::String("eval".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                let src = match args.get(1).or_else(|| args.first()) {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Ok(Value::Undefined),
                };
                run_source_in_realm(Rc::clone(&eval_globals), &src)
                    .map_err(tsblitz::errors::RuntimeError::from)
            }))),
        );

        let realm = Rc::new(RefCell::new(ObjectData::new()));
        realm.borrow_mut().properties.insert(
            PropertyKey::String("global".to_string()),
            PropertyDescriptor::data(Value::Object(realm_globals)),
        );
        Ok(Value::Object(realm))
    }));

    if let Some(PropertyDescriptor::Data {
        value: Value::Object(test262_obj),
        ..
    }) = globals
        .borrow_mut()
        .properties
        .get_mut(&PropertyKey::String("$262".to_string()))
    {
        test262_obj.borrow_mut().properties.insert(
            PropertyKey::String("createRealm".to_string()),
            PropertyDescriptor::data(create_realm),
        );
    }
}

fn relink_error_constructor_properties(globals: &Rc<RefCell<ObjectData>>) {
    let mut vm = Vm::new(Rc::clone(globals));
    for name in [
        "Error",
        "EvalError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "TypeError",
        "URIError",
    ] {
        if let Some(ctor) = vm.get_global(name)
            && let Ok(proto) =
                vm.get_property_host(&ctor, &PropertyKey::String("prototype".to_string()))
        {
            let ctor_desc = PropertyDescriptor::data(ctor.clone());
            match proto {
                Value::Object(proto_obj) => {
                    proto_obj
                        .borrow_mut()
                        .properties
                        .insert(PropertyKey::String("constructor".to_string()), ctor_desc);
                }
                Value::Array(_, props)
                | Value::Closure { props, .. }
                | Value::BuiltinObject(_, props) => {
                    props
                        .borrow_mut()
                        .properties
                        .insert(PropertyKey::String("constructor".to_string()), ctor_desc);
                }
                _ => {}
            }
        }
    }
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
fn unsupported_reason(_src: &str, meta: &Option<Meta>) -> Option<String> {
    if let Some(m) = meta
        && let Some(flags) = &m.flags
        && flags.iter().any(|f| {
            matches!(
                f.as_str(),
                "module" | "raw" | "async" | "noStrict" | "onlyStrict" // onlyStrict/noStrict handled below, but we don't run `module/raw/async`
            )
        })
    {
        // We will still handle onlyStrict/noStrict below in a limited way,
        // but module/raw/async are skipped entirely.
        if flags
            .iter()
            .any(|f| f == "module" || f == "raw" || f == "async")
        {
            let unsupported: Vec<&str> = flags
                .iter()
                .filter_map(|f| match f.as_str() {
                    "module" | "raw" | "async" => Some(f.as_str()),
                    _ => None,
                })
                .collect();
            return Some(format!("UNSUPPORTED: flags {}", unsupported.join(",")));
        }
    }
    None
}

/// Execute a single test file. Returns:
/// - Ok(())        -> test ran and passed
/// - Err(String)   -> test failed or is unsupported
fn run_test_file(
    path: &Path,
    test262_root: &Path,
    baseline_globals: &Rc<RefCell<ObjectData>>,
) -> Result<(), String> {
    let src = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let meta = parse_meta(&src);

    if let Some(reason) = unsupported_reason(&src, &meta) {
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
        // Use the test-friendly override rather than setting process env vars.
        typecheck::set_strict_override(Some(false));
        let _reset_strict = ResetStrict;

        // Restore a fresh realm cloned from the bootstrapped baseline.
        let mut vm = make_vm_from_snapshot(baseline_globals);

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
    baseline_globals: &Rc<RefCell<ObjectData>>,
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
                let rel = format!("harness/{}", name);
                if rel == "harness/sta.js" || rel == "harness/assert.js" {
                    continue;
                }
                extra.push(rel);
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

    let keep_globals: HashSet<String> = baseline_globals
        .borrow()
        .properties
        .keys()
        .filter_map(|k| match k {
            PropertyKey::String(s) => Some(s.clone()),
            PropertyKey::Symbol(_) => None,
        })
        .collect();

    for h in &harness_files {
        let mut vm = make_vm_from_snapshot(baseline_globals);
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
    let mut subtrees: HashMap<String, BucketStats> = HashMap::new();

    // Create a persistent VM and globals, install host builtins and the soft-prelude
    let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            builtin_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("print".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            builtin_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_instanceof".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let ctor = args.get(1).cloned().unwrap_or(Value::Undefined);
            vm.instance_of(&value, &ctor).map(Value::Bool)
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_inProperty".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let prop = args.first().cloned().unwrap_or(Value::Undefined);
            let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
            vm.in_property(&prop, &obj).map(Value::Bool)
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_typeof".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let ty = match value {
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
                Value::Object(_) | Value::Array(..) => "object",
            };
            Ok(Value::String(ty.to_string()))
        }))),
    );
    let mut data = ObjectData::new();
    data.properties.insert(
        PropertyKey::String("evalScript".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            builtin_eval_script(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }))),
    );
    data.properties.insert(
        PropertyKey::String("IsHTMLDDA".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(ObjectData::new())))),
    );
    // Add detachArrayBuffer as a no-op (we don't actually need to detach)
    data.properties.insert(
        PropertyKey::String("detachArrayBuffer".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, _args| {
            // No-op: we don't actually detach ArrayBuffers in this implementation
            Ok(Value::Undefined)
        }))),
    );
    // Add createRealm - creates a new realm with full global object and $262
    data.properties.insert(
        PropertyKey::String("createRealm".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, _args| {
            // Create a new global object for the realm
            let realm_global = Rc::new(RefCell::new(ObjectData::new()));
            
            // Install all the standard built-in constructors using install_minimal_kernel_constructors
            tsblitz::runtime_utils::install_minimal_kernel_constructors(&realm_global);
            
            // Create a new $262 object for this realm with the same structure as the main realm
            let mut new_262_data = ObjectData::new();
            
            // Add evalScript
            new_262_data.properties.insert(
                PropertyKey::String("evalScript".to_string()),
                PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                    builtin_eval_script(vm, args).map_err(tsblitz::errors::RuntimeError::from)
                }))),
            );
            
            // Add IsHTMLDDA
            new_262_data.properties.insert(
                PropertyKey::String("IsHTMLDDA".to_string()),
                PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(ObjectData::new())))),
            );
            
            // Add detachArrayBuffer (no-op)
            new_262_data.properties.insert(
                PropertyKey::String("detachArrayBuffer".to_string()),
                PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, _args| {
                    Ok(Value::Undefined)
                }))),
            );
            
            // Add createRealm recursively so realms can create sub-realms
            new_262_data.properties.insert(
                PropertyKey::String("createRealm".to_string()),
                PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
                    // Call the main createRealm via the main $262
                    if let Some(Value::Object(main_262_obj)) = vm.get_global("$262") {
                        if let Some(PropertyDescriptor::Data { value: Value::Builtin(f), .. }) = 
                            main_262_obj.borrow().properties.get(&PropertyKey::String("createRealm".to_string())) {
                            return f.as_ref()(vm, args);
                        }
                    }
                    Err(tsblitz::errors::RuntimeError::from("createRealm not found"))
                }))),
            );
            
            // Add global property pointing to the realm's global object
            new_262_data.properties.insert(
                PropertyKey::String("global".to_string()),
                PropertyDescriptor::data(Value::Object(Rc::clone(&realm_global))),
            );
            
            Ok(Value::Object(Rc::new(RefCell::new(new_262_data))))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("$262".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(data)))),
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
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_abstractEquality".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Undefined);
            let b = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.abstract_equality(&a, &b) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    // Arithmetic helpers (support BigInt-aware ops similar to main.rs)
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_add".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                    return Ok(Value::BigInt(*x + *y));
                } else {
                    return Err(tsblitz::errors::RuntimeError::from(
                        "Add: cannot mix BigInt and other types",
                    ));
                }
            }
            let a_prim = vm.to_primitive(&a, None)?;
            let b_prim = vm.to_primitive(&b, None)?;
            if matches!(a_prim, Value::String(_)) || matches!(b_prim, Value::String(_)) {
                let sa = vm.to_string(&a_prim)?;
                let sb = vm.to_string(&b_prim)?;
                return Ok(Value::String(format!("{}{}", sa, sb)));
            }
            let na = vm.to_number_value(&a_prim)?;
            let nb = vm.to_number_value(&b_prim)?;
            Ok(Value::Number(na + nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_subtract".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(*x - *y));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na - nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_multiply".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(*x * *y));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na * nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_divide".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                return Err(tsblitz::errors::RuntimeError::from(
                    "Div: bigint not supported",
                ));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na / nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_remainder".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(*x % *y));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na % nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_exponentiate".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                if *y < 0 {
                    return Err(tsblitz::errors::RuntimeError::from(
                        "Exponentiation with negative BigInt not supported",
                    ));
                }
                let mut res: i128 = 1;
                let mut base = *x;
                let mut exp = *y as u128;
                while exp > 0 {
                    if (exp & 1) == 1 {
                        res = res.checked_mul(base).ok_or_else(|| {
                            tsblitz::errors::RuntimeError::from("BigInt overflow in exponentiation")
                        })?;
                    }
                    exp >>= 1;
                    if exp > 0 {
                        base = base.checked_mul(base).ok_or_else(|| {
                            tsblitz::errors::RuntimeError::from("BigInt overflow in exponentiation")
                        })?;
                    }
                }
                return Ok(Value::BigInt(res));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na.powf(nb)))
        }))),
    );

    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_lessThan".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa < sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na < nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_lessThanOrEqual".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa <= sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na <= nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_greaterThan".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa > sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na > nb))
        }))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_greaterThanOrEqual".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa >= sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na >= nb))
        }))),
    );

    // Seed the realm with kernel constructors before running the soft prelude.
    // The prelude may extend/replace these; we avoid re-installing kernel
    // constructors afterward to preserve prelude-level constructor identity.
    tsblitz::runtime_utils::install_minimal_kernel_constructors(&globals);

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
    relink_error_constructor_properties(&globals);
    let realm_seed = clone_globals(&globals);
    install_test262_realm_helpers(&globals, &realm_seed);

    // Snapshot the fully bootstrapped realm once, then restore from it for each test.
    let baseline_globals = clone_globals(&globals);

    if opts.harness_only {
        match run_harness_only(&test262_dir, &baseline_globals) {
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
        let (top_key, subtree_key) = bucket_keys(&test_root, &f);
        match run_test_file(&f, &test262_dir, &baseline_globals) {
            Ok(()) => {
                ran += 1;
                top_level.entry(top_key).or_default().pass += 1;
                subtrees.entry(subtree_key).or_default().pass += 1;
                eprintln!("  PASS");
            }
            Err(e) => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subtrees.entry(subtree_key).or_default().fail += 1;
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

    if !failed.is_empty() {
        eprintln!(
            "Summary: ran {}, passed {}, failed {}, pass rate {:.2}%",
            ran,
            passed,
            failed.len(),
            pass_pct
        );
        print_bucket_summary("Top-level breakdown:", &top_level, 12, false);
        print_bucket_summary("Worst subtrees:", &subtrees, 15, true);

        // Instead of listing every failing file, group failures by subtree
        // and print the failing folders only (top 20). This avoids long lists
        // of individual files and surfaces which folders are problematic.
        let mut failed_by_subtree: HashMap<String, usize> = HashMap::new();
        for (p, _m) in &failed {
            let (_top, subtree) = bucket_keys(&test_root, p);
            *failed_by_subtree.entry(subtree).or_default() += 1;
        }
        let mut rows: Vec<_> = failed_by_subtree.into_iter().collect();
        rows.sort_by(|(an, ac), (bn, bc)| bc.cmp(ac).then_with(|| an.cmp(bn)));
        eprintln!("Failed folders:");
        for (name, cnt) in rows.into_iter() {
            eprintln!("  {:<40} fail {}", name, cnt);
        }
        exit(1);
    } else {
        eprintln!(
            "Test262 connector: ran {} tests, passed {} ({:.2}%)",
            ran, passed, pass_pct
        );
        print_bucket_summary("Top-level breakdown:", &top_level, 12, false);
        print_bucket_summary("Worst subtrees:", &subtrees, 15, true);
        exit(0);
    }
}
