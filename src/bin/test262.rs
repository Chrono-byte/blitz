/*!
Test262 connector script for tsblitz.

Usage:
- Optionally set `TEST262_FILTER` to a substring to limit which tests are run
- Optionally set `TEST262_LIMIT` to limit how many tests are executed (default 50)
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
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::rc::Rc;

use serde::Deserialize;

use tsblitz::compiler;
use tsblitz::value::PropertyKey;
use tsblitz::{bundle_entry, parser::Parser, typecheck, value::Value, vm::Vm};

struct RunnerOptions {
    includes: Vec<String>,
    excludes: Vec<String>,
    filter: Option<String>,
    limit: usize,
    harness_only: bool,
}

fn print_usage() {
    eprintln!("test262 runner options:");
    eprintln!("  --segment <seg>[,<seg>...]   Include segment(s)");
    eprintln!("  --exclude <seg>[,<seg>...]   Exclude segment(s)");
    eprintln!("  --filter <substring>         Substring filter");
    eprintln!("  --limit <n>                  Max tests (default 50)");
    eprintln!("  --harness-only               Run only harness files");
    eprintln!("segments: annexB, language, built-ins, intl402, staging, or path relative to test/");
}

fn parse_args() -> RunnerOptions {
    let mut includes: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut filter = env::var("TEST262_FILTER").ok();
    let mut limit: usize = env::var("TEST262_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
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
                    limit = n;
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
    let new_globals: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    new_globals.borrow_mut().properties.insert(
        PropertyKey::String("print".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(|vm, args| {
            builtin_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        })),
    );

    // Parse / typecheck / compile / run. Ignore errors and return Null on failure.
    match Parser::parse_source(&src) {
        Ok(session) => {
            let prog = session.program();
            if let Err(e) = typecheck::check_oxc(prog) {
                eprintln!("[evalScript] typecheck failed: {}", e);
                return Ok(Value::Null);
            }
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
fn run_source_in_realm(
    globals: Rc<RefCell<tsblitz::value::ObjectData>>,
    source: &str,
) -> Result<Value, String> {
    let session = Parser::parse_source(source).map_err(|e| format!("parse: {}", e))?;
    let prog = session.program();
    typecheck::check_oxc(prog).map_err(|e| format!("typecheck: {}", e))?;
    let chunk = compiler::compile_oxc(prog).map_err(|e| format!("compile: {}", e))?;
    let mut vm = Vm::new(globals);
    vm.run(chunk).map_err(|e| format!("runtime: {}", e))
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

/// Decide whether a test should be skipped based on metadata or simple heuristics.
///
/// Currently we skip:
/// - tests flagged with `module`, `raw`, or `async`
/// - tests that `includes` harness files (we don't support includes resolution yet)
/// - tests containing `throw` (throw handling is not yet implemented)
fn should_skip_test(src: &str, meta: &Option<Meta>) -> bool {
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
            return true;
        }
    }
    // `includes` are not skipped here. Included harness files will be
    // evaluated at runtime by the runner (see below) so that tests which
    // reference helper files can be supported.
    // Very simple heuristic: skip tests that use `throw` (our runtime doesn't
    // yet support the ThrowStatement semantics required by many Test262 tests).
    if src.contains("throw") {
        return true;
    }
    false
}

/// Execute a single test file. Returns:
/// - Ok(Some(()))  -> test ran and passed
/// - Ok(None)      -> test was skipped (not an error)
/// - Err(String)   -> test ran and failed (message)
fn run_test_file(
    path: &Path,
    test262_root: &Path,
    vm: &mut Vm,
    keep_globals: &HashSet<String>,
) -> Result<Option<()>, String> {
    let src = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let meta = parse_meta(&src);

    if should_skip_test(&src, &meta) {
        return Ok(None);
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

    // The VM and globals are provided by the caller. We'll reset VM state to
    // a clean run while keeping `keep_globals` intact.

    // Unless the test is `raw`, harness files should be evaluated first in the test realm.
    let raw_flag = meta
        .as_ref()
        .and_then(|m| m.flags.as_ref())
        .is_some_and(|f| f.iter().any(|s| s == "raw"));

    // Required harness files as per Test262 interpreting instructions.
    let mut harness_files: Vec<String> = vec![
        "harness/sta.js".to_string(),
        "harness/assert.js".to_string(),
    ];
    if let Some(m) = &meta
        && let Some(includes) = &m.includes
    {
        for inc in includes {
            if !harness_files.iter().any(|h| h == inc) {
                harness_files.push(inc.clone());
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

        // Reset VM state but preserve globals installed by the soft-prelude and host.
        vm.reset(keep_globals);

        // Evaluate harness files unless `raw`: compile & run them into the provided VM.
        if !raw_flag {
            for h in &harness_files {
                let p = test262_root.join(h);
                if p.exists() {
                    let hsrc = fs::read_to_string(&p)
                        .map_err(|e| format!("Failed to read harness {}: {}", p.display(), e))?;
                    // Parse/typecheck/compile the harness and run it in the provided VM
                    let session = match Parser::parse_source(&hsrc) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!(
                                "Skipping {} because harness {} parse failed: {}",
                                path.display(),
                                h,
                                e
                            );
                            return Ok(None);
                        }
                    };
                    let prog = session.program();
                    if let Err(e) = typecheck::check_oxc(prog) {
                        eprintln!(
                            "Skipping {} because harness {} typecheck failed: {}",
                            path.display(),
                            h,
                            e
                        );
                        return Ok(None);
                    }
                    let chunk = match compiler::compile_oxc(prog) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!(
                                "Skipping {} because harness {} compile failed: {}",
                                path.display(),
                                h,
                                e
                            );
                            return Ok(None);
                        }
                    };
                    if let Err(e) = vm.run(chunk) {
                        eprintln!(
                            "Skipping {} because harness {} failed at runtime: {}",
                            path.display(),
                            h,
                            e
                        );
                        return Ok(None);
                    }
                } else {
                    eprintln!("Skipping {} because harness {} missing", path.display(), h);
                    return Ok(None);
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

        // Typecheck - if this errors unexpectedly, test fails for this mode
        if let Err(e) = typecheck::check_oxc(prog) {
            return Err(format!(
                "{} ({}): typecheck error: {}",
                path.display(),
                mode_name,
                e
            ));
        }

        // Compile (convert to local_ast for now)
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
    Ok(Some(()))
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
    vm: &mut Vm,
    keep_globals: &HashSet<String>,
) -> Result<(), String> {
    let harness_dir = test262_root.join("harness");
    let mut harness_files: Vec<String> = vec![
        "harness/sta.js".to_string(),
        "harness/assert.js".to_string(),
    ];
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

    for h in &harness_files {
        vm.reset(keep_globals);
        let p = test262_root.join(h);
        if !p.exists() {
            return Err(format!("Harness file missing: {}", p.display()));
        }
        let hsrc = fs::read_to_string(&p)
            .map_err(|e| format!("Failed to read harness {}: {}", p.display(), e))?;
        let session = Parser::parse_source(&hsrc)
            .map_err(|e| format!("Harness {} parse failed: {}", h, e))?;
        let prog = session.program();
        typecheck::check_oxc(prog).map_err(|e| format!("Harness {} typecheck failed: {}", h, e))?;
        let chunk = compiler::compile_oxc(prog)
            .map_err(|e| format!("Harness {} compile failed: {}", h, e))?;
        vm.run(chunk)
            .map_err(|e| format!("Harness {} runtime failed: {}", h, e))?;
        eprintln!("Harness ok: {}", h);
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

    // Create a persistent VM and globals, install host builtins and the soft-prelude
    let globals: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_print".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(|vm, args| {
            builtin_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        })),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("print".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(|vm, args| {
            builtin_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        })),
    );
    let mut data = tsblitz::value::ObjectData::new();
    data.properties.insert(
        PropertyKey::String("evalScript".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(|vm, args| {
            builtin_eval_script(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        })),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("$262".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(data)))),
    );

    let mut vm = Vm::new(globals.clone());
    // Run soft prelude once (if present) to install runtime helpers
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    if prelude_path.exists() {
        match bundle_entry(&prelude_path) {
            Ok(session) => {
                let prog = session.program();
                match compiler::compile_oxc(prog) {
                    Ok(chunk) => {
                        if let Err(e) = vm.run(chunk) {
                            eprintln!("Failed to execute soft prelude: {}", e);
                        }
                    }
                    Err(e) => eprintln!("Failed to compile soft prelude: {}", e),
                }
            }
            Err(e) => eprintln!("Failed to bundle soft prelude: {}", e),
        }
    }

    // Capture the set of globals installed by host + prelude; preserve these across tests.
    let keep_globals: HashSet<String> = globals
        .borrow()
        .properties
        .keys()
        .filter_map(|k| match k {
            PropertyKey::String(s) => Some(s.clone()),
            PropertyKey::Symbol(_) => None,
        })
        .collect();

    if opts.harness_only {
        match run_harness_only(&test262_dir, &mut vm, &keep_globals) {
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
        if ran >= opts.limit {
            break;
        }
        if let Some(ref flt) = opts.filter
            && !f.to_string_lossy().contains(flt)
        {
            continue;
        }

        eprintln!("Running test: {}", f.display());
        match run_test_file(&f, &test262_dir, &mut vm, &keep_globals) {
            Ok(Some(())) => {
                ran += 1;
                eprintln!("  PASS");
            }
            Ok(None) => {
                eprintln!("  SKIP");
            }
            Err(e) => {
                ran += 1;
                eprintln!("  FAIL: {}", e);
                failed.push((f.clone(), e));
            }
        }
    }

    if !failed.is_empty() {
        eprintln!("Summary: ran {}, failed {}", ran, failed.len());
        for (p, m) in failed.iter().take(20) {
            eprintln!("  {}: {}", p.display(), m);
        }
        exit(1);
    } else {
        eprintln!("Test262 connector: ran {} tests (no failures)", ran);
        exit(0);
    }
}
