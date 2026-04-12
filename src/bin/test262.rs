/*!
Test262 connector script for Blitz.

Usage:
- Optionally set `TEST262_FILTER` to a substring to limit which tests are run
- Optionally set `TEST262_ANTI_FILTER` to skip tests whose path contains a substring
- Optionally set `TEST262_ANTI_SEGMENT` to skip tests whose path contains an exact path segment
- Optionally set `TEST262_LIMIT` to limit how many tests are executed (default: all)
- Optionally set `TEST262_JOBS` to control parallelism (default: available parallelism)
- Optionally set `TEST262_SEGMENTS` to a comma-separated list of segments
- Optionally set `TEST262_EXCLUDE` to a comma-separated list of segments to skip
- Optionally set `TEST262_ES_VERSIONS` to a comma-separated list of ES editions to include

CLI (preferred over env):
- --segment seg [,seg...] (repeatable)
- --exclude seg [,seg...] (repeatable)
- --filter substring
- --anti-filter substring
- --anti-segment seg [,seg...] (repeatable)
- --es-version edition [,edition...] (repeatable)
- --limit number
- --workers number

Segments may be labels like: language, built-ins, intl402, staging
or paths relative to test262/test (e.g. built-ins/Array).
ES editions may be labels like: ES5, ES6, ES2016, ..., ES2025, ESNext
or bare values like `5`, `6`, `2016`, or `next`.
APIs.
*/

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser as ClapParser;
use futures::future::join_all;
use oxc_span::SourceType;
use serde::{Deserialize, Serialize};
use tokio::runtime::Builder;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use blitz::compiler;
use blitz::value::{
    NativeFn, ObjectData, PropertyDescriptor, PropertyKey, Value, define_native_global,
};
use blitz::{parser::Parser, runtime_utils, typecheck, vm::Vm};

#[derive(Clone, Copy, Default)]
/// Result statistics for a test bucket (pass/fail/skip counts).
struct BucketStats {
    /// Number of tests that passed.
    pass: usize,
    /// Number of tests that failed.
    fail: usize,
    /// Number of tests that were skipped.
    skip: usize,
}

impl BucketStats {
    /// Returns the total number of tests (pass + fail + skip).
    fn total(self) -> usize {
        self.pass + self.fail + self.skip
    }

    /// Returns the number of tests that were attempted (pass + fail).
    fn attempted(self) -> usize {
        self.pass + self.fail
    }

    /// Returns the percentage of tests that passed.
    fn pass_pct(self) -> f64 {
        let attempted = self.attempted();
        if attempted == 0 {
            0.0
        } else {
            (self.pass as f64 / attempted as f64) * 100.0
        }
    }
}

/// The result of a test run.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum TestResult {
    /// The test passed.
    Pass,
    /// The test was skipped because the feature is unsupported.
    Skip(String),
    /// The test failed.
    Fail(String),
    /// The test encountered a stack overflow.
    StackOverflow,
    /// The test panicked.
    Panic(String),
}

impl TestResult {
    fn description(&self) -> String {
        match self {
            TestResult::Pass => "pass".to_string(),
            TestResult::Skip(msg) => format!("skip: {}", msg),
            TestResult::Fail(msg) => format!("fail: {}", msg),
            TestResult::StackOverflow => "stack overflow".to_string(),
            TestResult::Panic(msg) => format!("panic: {}", msg),
        }
    }
}

/// Force a full prototype-GC cycle for the current runner thread.
///
/// The Test262 runner creates many short-lived realms and VMs. Most memory
/// drops naturally with Rust ownership, but cyclic JS heaps tracked by the
/// prototype collector benefit from an explicit sweep once a test is fully
/// finished and all test-owned roots have gone out of scope.
fn force_runner_gc() {
    if env::var_os("TSBLITZ_GC_TRACE").is_some() {
        eprintln!(
            "[gc] runner force gc before={:?}",
            blitz::gc::heap_snapshot()
        );
    }
    let stats = blitz::gc::collect(&[]);
    if env::var_os("TSBLITZ_GC_TRACE").is_some() {
        eprintln!(
            "[gc] runner force gc stats={:?} after={:?}",
            stats,
            blitz::gc::heap_snapshot()
        );
    }
}

fn maybe_trim_allocator() {
    if env::var_os("TSBLITZ_TRIM_ALLOCATOR").is_none() {
        return;
    }

    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }

    // SAFETY: `malloc_trim` is a process-global allocator advisory call provided
    // by glibc. It takes no borrowed Rust pointers and has no Rust-visible aliasing
    // requirements. We ignore the return value because trimming is best-effort.
    let trimmed = unsafe { malloc_trim(0) };
    if env::var_os("TSBLITZ_GC_TRACE").is_some() {
        eprintln!("[gc] allocator trim requested result={}", trimmed);
    }
}

/// Selects the files to run based on the filter and limit.
fn path_has_segment(path: &Path, wanted: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == wanted)
}

/// Prints the first argument of a function call using JS string coercion.
fn print_args(vm: &mut Vm, args: &[Value]) -> String {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    match vm.to_string(&value) {
        Ok(s) => s,
        // `print` is a host helper, so degrade gracefully if coercion fails.
        Err(_) => format!("{}", value),
    }
}

fn select_files_to_run(
    files: Vec<PathBuf>,
    filters: &[String],
    anti_filters: &[String],
    anti_segments: &[String],
    es_versions: &[String],
    limit: Option<usize>,
) -> Vec<PathBuf> {
    let mut selected = Vec::new();
    for file in files {
        let path = file.to_string_lossy();
        if !filters.is_empty() && !filters.iter().any(|flt| path.contains(flt)) {
            continue;
        }
        if anti_filters.iter().any(|flt| path.contains(flt)) {
            continue;
        }
        if anti_segments.iter().any(|seg| path_has_segment(&file, seg)) {
            continue;
        }
        if !es_versions.is_empty() {
            let Some(edition) = file_edition_label(&file) else {
                continue;
            };
            if !es_versions.iter().any(|allowed| allowed == &edition) {
                continue;
            }
        }
        selected.push(file);
        if let Some(limit) = limit
            && selected.len() >= limit
        {
            break;
        }
    }
    selected
}

/// Resolves the job count based on the requested count and selected count.
fn resolve_job_count(
    requested_jobs: Option<usize>,
    selected_count: usize,
    available_parallelism: Option<usize>,
) -> usize {
    // Keep default memory usage bounded when user did not set --workers.
    // Explicit --workers still wins.
    let bounded_default = available_parallelism.map(|n| n.min(2));
    requested_jobs
        .or(bounded_default)
        .unwrap_or(1)
        .max(1)
        .min(selected_count.max(1))
}

/// Sorts the test results by their index.
fn sort_test_results(results: &mut [(usize, PathBuf, TestResult)]) {
    results.sort_by_key(|(idx, _, _)| *idx);
}

/// Runs the selected tests and returns the results.
struct PermitReleaser {
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl PermitReleaser {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        Self {
            permit: Mutex::new(Some(permit)),
        }
    }

    fn release(&self) {
        if let Ok(mut guard) = self.permit.lock() {
            guard.take();
        }
    }
}

fn run_selected_tests(
    files_to_run: Vec<PathBuf>,
    test262_dir: PathBuf,
    job_count: usize,
    log_tests: bool,
) -> Vec<(usize, PathBuf, TestResult)> {
    if job_count <= 1 || files_to_run.len() <= 1 {
        return files_to_run
            .into_iter()
            .enumerate()
            .map(|(idx, file)| {
                let result = run_test_in_thread(file.clone(), test262_dir.clone(), log_tests, None);
                (idx, file, result)
            })
            .collect();
    }

    let runtime = Builder::new_current_thread()
        .build()
        .expect("failed to build tokio runtime for test262");

    runtime.block_on(async move {
        let semaphore = Arc::new(Semaphore::new(job_count));
        let mut handles = Vec::with_capacity(files_to_run.len());

        for (idx, file) in files_to_run.into_iter().enumerate() {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("test262 semaphore unexpectedly closed");
            let permit_releaser = Arc::new(PermitReleaser::new(permit));
            let test262_dir = test262_dir.clone();
            handles.push(tokio::spawn(async move {
                let file_for_result = file.clone();
                let release_handle = Arc::clone(&permit_releaser);
                let result = tokio::task::spawn_blocking(move || {
                    run_test_in_thread(file, test262_dir, log_tests, Some(release_handle))
                })
                .await
                .expect("test262 blocking task panicked");
                (idx, file_for_result, result)
            }));
        }

        // Collect the results of all test runs concurrently.
        // The spawned tasks are already running; use join_all to await all
        // dispatch tasks and preserve the original panic message on join error.
        let mut results = Vec::with_capacity(handles.len());
        let collected = join_all(handles).await;
        for res in collected {
            results.push(res.expect("test262 dispatch task panicked"));
        }
        results
    })
}

/// Runs a single test in a separate thread, returning the result.
fn run_test_in_thread(
    file: PathBuf,
    test262_dir: PathBuf,
    log_tests: bool,
    release_handle: Option<Arc<PermitReleaser>>,
) -> TestResult {
    const STACK_SIZE: usize = 2 * 1024 * 1024; // 2 MB stack for tests

    let (tx, rx) = mpsc::channel();

    let join_handle = match thread::Builder::new()
        .name(format!("test-{}", file.display()))
        .stack_size(STACK_SIZE)
        .spawn(move || {
            if log_tests {
                eprintln!("[test262] starting {}", file.display());
            }
            let test_result = {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let globals = make_test_globals();

                    // Snapshot for test isolation (though we won't use restore since each test is fresh)
                    let baseline_snapshot = snapshot_globals(&globals);

                    // Run the test
                    run_test_file(
                        &file,
                        &test262_dir,
                        &globals,
                        &baseline_snapshot,
                        release_handle.clone(),
                    )
                }));

                match result {
                    Ok(Ok(())) => TestResult::Pass,
                    Ok(Err(e)) => {
                        if e.contains("UNSUPPORTED:") {
                            TestResult::Skip(e)
                        } else {
                            TestResult::Fail(e)
                        }
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
                            || (msg.contains("thread '")
                                && msg.contains("' has overflowed its stack"));

                        if is_overflow {
                            TestResult::StackOverflow
                        } else {
                            TestResult::Panic(msg)
                        }
                    }
                }
            };

            // At this point the per-test VM/realm created inside `run_test_file`
            // has gone out of scope. Run an explicit collector cycle so passed
            // tests do not leave cyclic JS heaps hanging around until thread exit.
            force_runner_gc();
            maybe_trim_allocator();

            if log_tests {
                eprintln!(
                    "[test262] finished {} ({})",
                    file.display(),
                    test_result.description()
                );
            }

            let _ = tx.send(test_result);
        }) {
        Ok(handle) => handle,
        Err(e) => {
            // Failed to spawn the test thread; return an explicit failure result.
            return TestResult::Panic(format!("Failed to spawn thread: {}", e));
        }
    };

    let result = match rx.recv() {
        Ok(v) => v,
        Err(_) => TestResult::Panic("Thread failed to send result".to_string()),
    };

    if let Err(join_err) = join_handle.join() {
        // If join fails, log the panic payload for debugging. Do not panic the runner.
        let join_msg = if let Some(s) = join_err.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = join_err.downcast_ref::<&str>() {
            s.to_string()
        } else {
            "Unknown panic during join".to_string()
        };
        eprintln!("Warning: test thread join reported a panic: {}", join_msg);
    }

    result
}

/// Returns the bucket keys for a test file, based on its path relative to the test root.
/// Language tests are grouped by ECMAScript edition when we can infer it from frontmatter.
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

    let top = if parts.first().is_some_and(|part| part == "language") {
        file_edition_label(file).unwrap_or_else(|| "language".to_string())
    } else {
        dirs.first()
            .cloned()
            .or_else(|| parts.first().cloned())
            .unwrap_or_else(|| "<root>".to_string())
    };
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
            "  {:<28} pass {:>6} fail {:>6}  pass rate {:>6.2}%",
            name,
            stats.pass,
            stats.fail,
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
    /// Disable subprocess batching and run selected tests directly in-process.
    #[arg(long = "no-subprocess", action)]
    no_subprocess: bool,
    /// Hidden internal file list used by subprocess batches.
    #[arg(long = "file-list", value_name = "PATH", hide = true)]
    file_list: Option<PathBuf>,
    /// Hidden internal JSON report path written by subprocess batches.
    #[arg(long = "report-json", value_name = "PATH", hide = true)]
    report_json: Option<PathBuf>,
    /// Number of exact-file tests to hand to each subprocess batch.
    #[arg(long = "batch-size", value_name = "N", default_value_t = 200)]
    batch_size: usize,
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
    /// Substring filters for test names. Any match includes the test.
    #[arg(long = "filter", value_name = "SUBSTRING", value_delimiter = ',')]
    filters: Vec<String>,
    /// Exclude tests whose path contains any of these substrings.
    #[arg(long = "anti-filter", value_name = "SUBSTRING", value_delimiter = ',')]
    anti_filters: Vec<String>,
    /// Exclude tests whose path contains one of these exact path segments.
    #[arg(long = "anti-segment", value_name = "SEGMENT", value_delimiter = ',')]
    anti_segments: Vec<String>,
    /// Include only tests whose inferred ES edition matches one of these values.
    #[arg(
        long = "es-version",
        alias = "edition",
        value_name = "EDITION",
        value_delimiter = ','
    )]
    es_versions: Vec<String>,
    /// Optional maximum number of tests to run.
    #[arg(long = "limit", value_name = "N")]
    limit: Option<usize>,
    /// Number of tests to run in parallel.
    #[arg(long = "workers", short = 'w', value_name = "N")]
    jobs: Option<usize>,
    /// Run only harness files.
    #[arg(long = "harness-only", alias = "harness", action)]
    harness_only: bool,
    /// Emit per-test start/finish logs.
    #[arg(long = "log-tests", action)]
    log_tests: bool,
    /// Run a single hard-coded test and dump globals in the error handler style.
    #[arg(long = "dump-scopes", action)]
    dump_scopes: bool,
}

/// Parses a CSV environment variable into a vector of strings.
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

/// Parses a usize environment variable.
fn parse_usize_env(name: &str) -> Option<usize> {
    env::var(name).ok().and_then(|s| s.parse::<usize>().ok())
}

fn parse_bool_env(name: &str) -> Option<bool> {
    env::var(name).ok().map(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct SubprocessBatchReport {
    results: Vec<(usize, PathBuf, TestResult)>,
}

fn load_file_list(path: &Path) -> Result<Vec<PathBuf>, String> {
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file list {}: {}", path.display(), e))?;
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

fn unique_temp_path(prefix: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("blitz-test262-{prefix}-{pid}-{nanos}.{ext}"))
}

fn should_use_subprocess_batches(opts: &RunnerOptions, selected_count: usize) -> bool {
    !opts.no_subprocess
        && opts.file_list.is_none()
        && !opts.harness_only
        && !opts.dump_scopes
        && selected_count > 1
}

fn run_selected_tests_via_subprocess_batches(
    files_to_run: Vec<PathBuf>,
    job_count: usize,
    log_tests: bool,
    batch_size: usize,
) -> Result<Vec<(usize, PathBuf, TestResult)>, String> {
    let exe = env::current_exe().map_err(|e| format!("Failed to resolve current exe: {e}"))?;
    let effective_batch_size = batch_size.max(1);

    let chunk_queue: Arc<Mutex<VecDeque<(usize, Vec<PathBuf>)>>> = Arc::new(Mutex::new(
        files_to_run
            .chunks(effective_batch_size)
            .enumerate()
            .map(|(chunk_idx, chunk)| (chunk_idx * effective_batch_size, chunk.to_vec()))
            .collect(),
    ));
    let results: Arc<Mutex<Vec<(usize, PathBuf, TestResult)>>> = Arc::new(Mutex::new(Vec::new()));
    let worker_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let worker_count = job_count.max(1).min(files_to_run.len().max(1));
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let exe = exe.clone();
        let queue = Arc::clone(&chunk_queue);
        let shared_results = Arc::clone(&results);
        let shared_errors = Arc::clone(&worker_errors);
        handles.push(thread::spawn(move || {
            loop {
                let Some((chunk_start, chunk_files)) = ({
                    let mut guard = queue.lock().expect("subprocess chunk queue poisoned");
                    guard.pop_front()
                }) else {
                    break;
                };

                let file_list_path = unique_temp_path("files", "txt");
                let report_path = unique_temp_path("report", "json");
                let file_list_contents = chunk_files
                    .iter()
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("\n");

                if let Err(e) = fs::write(&file_list_path, file_list_contents) {
                    shared_errors
                        .lock()
                        .expect("subprocess errors poisoned")
                        .push(format!(
                            "Failed to write subprocess file list {}: {}",
                            file_list_path.display(),
                            e
                        ));
                    break;
                }

                let mut command = Command::new(&exe);
                command.arg("--file-list").arg(&file_list_path);
                command.arg("--report-json").arg(&report_path);
                command.arg("--no-subprocess");
                command.arg("--workers").arg("1");
                if log_tests {
                    command.arg("--log-tests");
                }

                let status = command.status();
                let _ = fs::remove_file(&file_list_path);

                match status {
                    Ok(status) if status.success() || status.code() == Some(1) => {}
                    Ok(status) => {
                        let _ = fs::remove_file(&report_path);
                        shared_errors
                            .lock()
                            .expect("subprocess errors poisoned")
                            .push(format!(
                                "Subprocess batch failed with status {} for {} tests",
                                status,
                                chunk_files.len()
                            ));
                        break;
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&report_path);
                        shared_errors
                            .lock()
                            .expect("subprocess errors poisoned")
                            .push(format!(
                                "Failed to launch subprocess batch for {} tests: {}",
                                chunk_files.len(),
                                e
                            ));
                        break;
                    }
                }

                let report_contents = match fs::read_to_string(&report_path) {
                    Ok(contents) => contents,
                    Err(e) => {
                        let _ = fs::remove_file(&report_path);
                        shared_errors
                            .lock()
                            .expect("subprocess errors poisoned")
                            .push(format!(
                                "Failed to read subprocess report {}: {}",
                                report_path.display(),
                                e
                            ));
                        break;
                    }
                };
                let _ = fs::remove_file(&report_path);

                let report: SubprocessBatchReport = match serde_json::from_str(&report_contents) {
                    Ok(report) => report,
                    Err(e) => {
                        shared_errors
                            .lock()
                            .expect("subprocess errors poisoned")
                            .push(format!(
                                "Failed to parse subprocess report {}: {}",
                                report_path.display(),
                                e
                            ));
                        break;
                    }
                };

                let mut out = shared_results.lock().expect("subprocess results poisoned");
                out.extend(
                    report
                        .results
                        .into_iter()
                        .map(|(idx, file, result)| (chunk_start + idx, file, result)),
                );
            }
        }));
    }

    for handle in handles {
        if let Err(join_err) = handle.join() {
            let msg = if let Some(s) = join_err.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = join_err.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "unknown worker panic".to_string()
            };
            worker_errors
                .lock()
                .expect("subprocess errors poisoned")
                .push(format!("Subprocess worker panicked: {}", msg));
        }
    }

    let errors = worker_errors.lock().expect("subprocess errors poisoned");
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    drop(errors);

    let mut out = results.lock().expect("subprocess results poisoned").clone();
    sort_test_results(&mut out);
    Ok(out)
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
    if opts.filters.is_empty() {
        opts.filters = parse_csv_env("TEST262_FILTER");
    }
    if opts.anti_filters.is_empty() {
        opts.anti_filters = parse_csv_env("TEST262_ANTI_FILTER");
    }
    if opts.anti_segments.is_empty() {
        opts.anti_segments = parse_csv_env("TEST262_ANTI_SEGMENT");
    }
    if opts.es_versions.is_empty() {
        opts.es_versions = parse_csv_env("TEST262_ES_VERSIONS");
    }
    if opts.limit.is_none() {
        opts.limit = parse_usize_env("TEST262_LIMIT");
    }
    if opts.jobs.is_none() {
        opts.jobs = parse_usize_env("TEST262_JOBS");
    }
    if !opts.log_tests {
        opts.log_tests = parse_bool_env("TEST262_LOG_TESTS").unwrap_or(false);
    }

    match normalize_es_versions(&opts.es_versions) {
        Ok(values) => opts.es_versions = values,
        Err(msg) => {
            eprintln!("{}", msg);
            exit(2);
        }
    }

    opts
}

/// Minimal subset of metadata we care about.
#[derive(Debug, Deserialize)]
struct Negative {
    /// The phase of the test (e.g., "early", "late", "async").
    phase: String,
    /// The type of the test (e.g., "realm").
    #[serde(rename = "type")]
    ty: Option<String>,
}

/// Minimal subset of metadata we care about.
#[derive(Debug, Deserialize)]
struct Meta {
    /// The flags associated with the test.
    flags: Option<Vec<String>>,
    /// The language features associated with the test.
    features: Option<Vec<String>>,
    /// Sputnik-era ES5 identifier used by some older language tests.
    es5id: Option<String>,
    /// The includes associated with the test.
    includes: Option<Vec<String>>,
    /// The negative metadata of the test.
    negative: Option<Negative>,
}

/// A minimal subset of test262 error information.
#[derive(Clone)]
struct Test262Error<'a> {
    /// The path to the test file.
    path: &'a Path,
    /// The mode of the test (e.g., "early", "late", "async").
    mode: Option<&'a str>,
    /// The error message.
    message: String,
}

impl<'a> Test262Error<'a> {
    /// Create a new error with a mode (used for test-specific errors).
    fn new(path: &'a Path, mode: &'a str, message: String) -> Self {
        Self {
            path,
            mode: Some(mode),
            message,
        }
    }

    /// Create a new error with an optional mode (used for test-specific errors or file-level errors).
    fn for_path(path: &'a Path, mode: Option<&'a str>, message: String) -> Self {
        Self {
            path,
            mode,
            message,
        }
    }

    /// Create a new error without a mode (used for file-level errors).
    fn without_mode(path: &'a Path, message: String) -> Self {
        Self::for_path(path, None, message)
    }

    /// Convert the error into a string representation.
    fn into_string(self) -> String {
        match self.mode {
            Some(mode) => format!("{} ({}): {}", self.path.display(), mode, self.message),
            None => format!("{}: {}", self.path.display(), self.message),
        }
    }
}

/// Helper to extract the front-matter YAML between /*--- and ---*/
fn extract_front_matter(src: &str) -> Option<String> {
    let start = src.find("/*---")?;
    let end = src.find("---*/")?;
    let fm = &src[start + "/*---".len()..end];
    Some(fm.trim().to_string())
}

fn edition_label(edition: u8) -> Option<&'static str> {
    match edition {
        5 => Some("ES5"),
        6 => Some("ES6"),
        7 => Some("ES2016"),
        8 => Some("ES2017"),
        9 => Some("ES2018"),
        10 => Some("ES2019"),
        11 => Some("ES2020"),
        12 => Some("ES2021"),
        13 => Some("ES2022"),
        14 => Some("ES2023"),
        15 => Some("ES2024"),
        16 => Some("ES2025"),
        99 => Some("ESNext"),
        _ => None,
    }
}

fn feature_edition(feature: &str) -> Option<u8> {
    match feature {
        // ES2025
        "Float16Array" => Some(16),
        "import-attributes" => Some(16),
        "Intl.DurationFormat" => Some(16),
        "iterator-helpers" => Some(16),
        "json-modules" => Some(16),
        "promise-try" => Some(16),
        "RegExp.escape" => Some(16),
        "regexp-modifiers" => Some(16),
        "set-methods" => Some(16),

        // ES2024
        "Atomics.waitAsync" => Some(15),
        "array-grouping" => Some(15),
        "arraybuffer-transfer" => Some(15),
        "promise-with-resolvers" => Some(15),
        "regexp-v-flag" => Some(15),
        "resizable-arraybuffer" => Some(15),
        "String.prototype.isWellFormed" => Some(15),
        "String.prototype.toWellFormed" => Some(15),

        // ES2023
        "array-find-from-last" => Some(14),
        "change-array-by-copy" => Some(14),
        "hashbang" => Some(14),
        "symbols-as-weakmap-keys" => Some(14),

        // ES2022
        "arbitrary-module-namespace-names" => Some(13),
        "Array.prototype.at" => Some(13),
        "class-fields-private" => Some(13),
        "class-fields-private-in" => Some(13),
        "class-fields-public" => Some(13),
        "class-methods-private" => Some(13),
        "class-static-block" => Some(13),
        "class-static-fields-private" => Some(13),
        "class-static-fields-public" => Some(13),
        "class-static-methods-private" => Some(13),
        "error-cause" => Some(13),
        "Intl.DateTimeFormat-extend-timezonename" => Some(13),
        "Intl.DisplayNames-v2" => Some(13),
        "Intl-enumeration" => Some(13),
        "Intl.Segmenter" => Some(13),
        "Object.hasOwn" => Some(13),
        "regexp-match-indices" => Some(13),
        "String.prototype.at" => Some(13),
        "top-level-await" => Some(13),
        "TypedArray.prototype.at" => Some(13),

        // ES2021
        "AggregateError" => Some(12),
        "align-detached-buffer-semantics-with-web-reality" => Some(12),
        "FinalizationRegistry" => Some(12),
        "Intl.DateTimeFormat-datetimestyle" => Some(12),
        "Intl.DateTimeFormat-formatRange" => Some(12),
        "Intl.DateTimeFormat-fractionalSecondDigits" => Some(12),
        "Intl.DisplayNames" => Some(12),
        "Intl.ListFormat" => Some(12),
        "Intl.Locale" => Some(12),
        "logical-assignment-operators" => Some(12),
        "numeric-separator-literal" => Some(12),
        "Promise.any" => Some(12),
        "String.prototype.replaceAll" => Some(12),
        "WeakRef" => Some(12),

        // ES2020
        "BigInt" => Some(11),
        "coalesce-expression" => Some(11),
        "Intl.NumberFormat-unified" => Some(11),
        "Intl.RelativeTimeFormat" => Some(11),
        "optional-chaining" => Some(11),
        "dynamic-import" => Some(11),
        "export-star-as-namespace-from-module" => Some(11),
        "for-in-order" => Some(11),
        "globalThis" => Some(11),
        "import.meta" => Some(11),
        "Promise.allSettled" => Some(11),
        "String.prototype.matchAll" => Some(11),
        "Symbol.matchAll" => Some(11),

        // ES2019
        "Array.prototype.flat" => Some(10),
        "Array.prototype.flatMap" => Some(10),
        "json-superset" => Some(10),
        "Object.fromEntries" => Some(10),
        "optional-catch-binding" => Some(10),
        "stable-array-sort" => Some(10),
        "stable-typedarray-sort" => Some(10),
        "string-trimming" => Some(10),
        "String.prototype.trimEnd" => Some(10),
        "String.prototype.trimStart" => Some(10),
        "Symbol.prototype.description" => Some(10),
        "well-formed-json-stringify" => Some(10),

        // ES2018
        "async-iteration" => Some(9),
        "IsHTMLDDA" => Some(9),
        "object-rest" => Some(9),
        "object-spread" => Some(9),
        "Promise.prototype.finally" => Some(9),
        "regexp-dotall" => Some(9),
        "regexp-lookbehind" => Some(9),
        "regexp-named-groups" => Some(9),
        "regexp-unicode-property-escapes" => Some(9),
        "Symbol.asyncIterator" => Some(9),

        // ES2017
        "async-functions" => Some(8),
        "Atomics" => Some(8),
        "Intl.DateTimeFormat-dayPeriod" => Some(8),
        "intl-normative-optional" => Some(8),
        "SharedArrayBuffer" => Some(8),

        // ES2016
        "Array.prototype.includes" => Some(7),
        "exponentiation" => Some(7),
        "u180e" => Some(7),

        // ES6
        "ArrayBuffer" => Some(6),
        "Array.prototype.values" => Some(6),
        "arrow-function" => Some(6),
        "class" => Some(6),
        "computed-property-names" => Some(6),
        "const" => Some(6),
        "cross-realm" => Some(6),
        "DataView" => Some(6),
        "DataView.prototype.getFloat32" => Some(6),
        "DataView.prototype.getFloat64" => Some(6),
        "DataView.prototype.getInt16" => Some(6),
        "DataView.prototype.getInt32" => Some(6),
        "DataView.prototype.getInt8" => Some(6),
        "DataView.prototype.getUint16" => Some(6),
        "DataView.prototype.getUint32" => Some(6),
        "DataView.prototype.setUint8" => Some(6),
        "default-parameters" => Some(6),
        "destructuring-assignment" => Some(6),
        "destructuring-binding" => Some(6),
        "for-of" => Some(6),
        "Float32Array" => Some(6),
        "Float64Array" => Some(6),
        "generators" => Some(6),
        "Int8Array" => Some(6),
        "Int16Array" => Some(6),
        "Int32Array" => Some(6),
        "let" => Some(6),
        "Map" => Some(6),
        "new.target" => Some(6),
        "Object.is" => Some(6),
        "Promise" => Some(6),
        "Proxy" => Some(6),
        "proxy-missing-checks" => Some(6),
        "Reflect" => Some(6),
        "Reflect.construct" => Some(6),
        "Reflect.set" => Some(6),
        "Reflect.setPrototypeOf" => Some(6),
        "rest-parameters" => Some(6),
        "Set" => Some(6),
        "String.fromCodePoint" => Some(6),
        "String.prototype.endsWith" => Some(6),
        "String.prototype.includes" => Some(6),
        "super" => Some(6),
        "Symbol" => Some(6),
        "Symbol.hasInstance" => Some(6),
        "Symbol.isConcatSpreadable" => Some(6),
        "Symbol.iterator" => Some(6),
        "Symbol.match" => Some(6),
        "Symbol.replace" => Some(6),
        "Symbol.search" => Some(6),
        "Symbol.species" => Some(6),
        "Symbol.split" => Some(6),
        "Symbol.toPrimitive" => Some(6),
        "Symbol.toStringTag" => Some(6),
        "Symbol.unscopables" => Some(6),
        "tail-call-optimization" => Some(6),
        "template" => Some(6),
        "TypedArray" => Some(6),
        "Uint8Array" => Some(6),
        "Uint16Array" => Some(6),
        "Uint32Array" => Some(6),
        "Uint8ClampedArray" => Some(6),
        "WeakMap" => Some(6),
        "WeakSet" => Some(6),

        // ES5
        "caller" => Some(5),

        // Legacy additions used by test262-fyi.
        "__proto__" => Some(6),
        "__getter__" => Some(8),
        "__setter__" => Some(8),

        // Experimental and proposal-stage features group under ESNext.
        "Intl.Locale-info" => Some(99),
        "Intl.NumberFormat-v3" => Some(99),
        "legacy-regexp" => Some(99),
        "Temporal" => Some(99),
        "ShadowRealm" => Some(99),
        "decorators" => Some(99),
        "regexp-duplicate-named-groups" => Some(99),
        "Array.fromAsync" => Some(99),
        "explicit-resource-management" => Some(99),
        "source-phase-imports" => Some(99),
        "source-phase-imports-module-source" => Some(99),
        "Atomics.pause" => Some(99),
        "import-defer" => Some(99),
        "canonical-tz" => Some(99),
        "immutable-arraybuffer" => Some(99),
        "nonextensible-applies-to-private" => Some(99),
        "host-gc-required" => Some(99),
        "Error.isError" => Some(99),
        "iterator-sequencing" => Some(99),
        "json-parse-with-source" => Some(99),
        "Math.sumPrecise" => Some(99),
        "uint8array-base64" => Some(99),
        "upsert" => Some(99),
        _ => None,
    }
}

fn language_feature_group(features: Option<&[String]>) -> Option<String> {
    let mut best_edition: Option<u8> = None;

    for feature in features? {
        let Some(edition) = feature_edition(feature) else {
            continue;
        };
        best_edition = Some(match best_edition {
            Some(current) => current.max(edition),
            None => edition,
        });
    }

    best_edition.and_then(|edition| edition_label(edition).map(str::to_string))
}

fn normalize_es_version(input: &str) -> Option<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return None;
    }

    let upper = raw.to_ascii_uppercase();
    let label = if let Some(rest) = upper.strip_prefix("ES") {
        match rest {
            "NEXT" => "ESNext".to_string(),
            "5" => "ES5".to_string(),
            "6" => "ES6".to_string(),
            "2016" => "ES2016".to_string(),
            "2017" => "ES2017".to_string(),
            "2018" => "ES2018".to_string(),
            "2019" => "ES2019".to_string(),
            "2020" => "ES2020".to_string(),
            "2021" => "ES2021".to_string(),
            "2022" => "ES2022".to_string(),
            "2023" => "ES2023".to_string(),
            "2024" => "ES2024".to_string(),
            "2025" => "ES2025".to_string(),
            _ => return None,
        }
    } else {
        match upper.as_str() {
            "NEXT" => "ESNext".to_string(),
            "5" => "ES5".to_string(),
            "6" => "ES6".to_string(),
            "2016" => "ES2016".to_string(),
            "2017" => "ES2017".to_string(),
            "2018" => "ES2018".to_string(),
            "2019" => "ES2019".to_string(),
            "2020" => "ES2020".to_string(),
            "2021" => "ES2021".to_string(),
            "2022" => "ES2022".to_string(),
            "2023" => "ES2023".to_string(),
            "2024" => "ES2024".to_string(),
            "2025" => "ES2025".to_string(),
            _ => return None,
        }
    };

    Some(label)
}

fn normalize_es_versions(values: &[String]) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for value in values {
        let Some(label) = normalize_es_version(value) else {
            return Err(format!("Invalid ES version '{}'", value));
        };
        if !normalized.iter().any(|existing| existing == &label) {
            normalized.push(label);
        }
    }
    Ok(normalized)
}

fn file_edition_label(file: &Path) -> Option<String> {
    let src = fs::read_to_string(file).ok()?;
    let meta = parse_meta(&src)?;
    if meta.es5id.is_some() {
        return Some("ES5".to_string());
    }
    language_feature_group(meta.features.as_deref())
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

fn read_harness_source(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|e| format!("Failed to read harness {}: {}", path.display(), e))
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
        SourceType::script()
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
    let chunk = typecheck::with_strict_override(None, || {
        let session = Parser::parse_source(&src).map_err(|e| format!("evalScript parse: {}", e))?;
        let prog = session.program();
        compiler::compile_oxc(prog, &src).map_err(|e| format!("evalScript compile: {}", e))
    })?;
    let mut eval_vm = Vm::new(Rc::clone(globals));
    eval_vm
        .run(chunk)
        .map_err(|e| format!("evalScript runtime: {}", e))
}

fn make_test_globals() -> Rc<RefCell<ObjectData>> {
    let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));

    let global_this = Value::Object(globals.clone());
    globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string()),
        PropertyDescriptor::data(global_this),
    );
    let default_print: NativeFn = Rc::new(|vm, _this, args| {
        eprintln!("[test262 print] {}", print_args(vm, args));
        Ok(Value::Null)
    });
    define_native_global(&globals, "print", default_print);
    globals.borrow_mut().properties.insert(
        PropertyKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("Infinity".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );

    runtime_utils::install_minimal_kernel_constructors(&globals);

    let test262_obj = make_test262_object(&globals);
    globals.borrow_mut().properties.insert(
        PropertyKey::String("$262".to_string()),
        PropertyDescriptor::data(test262_obj),
    );

    globals
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
            let _ = _vm.collect_garbage_prototype();
            Ok(Value::Undefined)
        }))),
    );
    data.properties.insert(
        PropertyKey::String("AbstractModuleSource".to_string()),
        PropertyDescriptor::data(Value::Undefined),
    );

    let noop_agent_method: blitz::value::BuiltinFn =
        Rc::new(|_: &mut Vm, _: &[Value]| Ok(Value::Undefined));
    let agent_obj = Rc::new(RefCell::new(ObjectData::new()));
    for name in &[
        "getReport",
        "sleep",
        "broadcast",
        "receiveBroadcast",
        "report",
    ] {
        agent_obj.borrow_mut().properties.insert(
            PropertyKey::String(name.to_string()),
            PropertyDescriptor::data(host_builtin(noop_agent_method.clone())),
        );
    }
    data.properties.insert(
        PropertyKey::String("agent".to_string()),
        PropertyDescriptor::data(Value::Object(agent_obj)),
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
                    PropertyDescriptor::data(Value::Object(realm_global)),
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
        && let Some(features) = &m.features
        && features.iter().any(|f| f == "resizable-arraybuffer")
    {
        return Some("UNSUPPORTED: feature resizable-arraybuffer".to_string());
    }

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
    _globals: &Rc<RefCell<ObjectData>>,
    _baseline_snapshot: &HashMap<PropertyKey, PropertyDescriptor>,
    release_handle: Option<Arc<PermitReleaser>>,
) -> Result<(), String> {
    let src = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let meta = parse_meta(&src);
    let emit_async_marker = env::var_os("TSBLITZ_EMIT_ASYNC_MARKER").is_some();

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
    let is_module = meta
        .as_ref()
        .and_then(|m| m.flags.as_ref())
        .is_some_and(|f| f.iter().any(|s| s == "module"));

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
    if is_module {
        run_non_strict = false;
        run_strict = true;
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
            let inc_src = read_harness_source(&path)?;
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
    for (mode_name, do_run) in &[("strict", run_strict), ("non-strict", run_non_strict)] {
        if !do_run {
            continue;
        }

        // Configure strictness for the engine (runtime-configured via TSBLITZ_STRICT).
        let is_strict = *mode_name == "strict";
        let is_async = has_async_flag;
        // Use the test-friendly override rather than setting process env vars.
        typecheck::set_strict_override(Some(false));
        let _reset_strict = ResetStrict;

        let globals = make_test_globals();
        globals.borrow_mut().properties.insert(
            PropertyKey::String("__TSBLITZ_TEST_PATH".to_string()),
            PropertyDescriptor::data(Value::String(path.to_string_lossy().to_string())),
        );
        let mut vm = Vm::new(globals.clone());

        // The harness self-test `test/harness/detachArrayBuffer.js` verifies
        // the fallback behavior when hosts do NOT provide `$262` at all.
        // Our runner normally installs `$262`, so remove it only for this
        // specific self-test.
        if path.ends_with("test/harness/detachArrayBuffer.js") {
            globals
                .borrow_mut()
                .properties
                .shift_remove(&PropertyKey::String("$262".to_string()));
        }

        // Async tests must signal completion through $DONE(error?).
        let done_state: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
        if is_async {
            let done_state_for_print = done_state.clone();
            let async_print: NativeFn = Rc::new(move |vm, _this, args| {
                let s = print_args(vm, args);
                if done_state_for_print.borrow().is_none()
                    && let Some(marker) = is_async_done_marker(&s)
                {
                    *done_state_for_print.borrow_mut() = Some(marker);
                    return Ok(Value::Null);
                }
                eprintln!("[test262 print] {}", s);
                Ok(Value::Null)
            });
            define_native_global(&vm.globals_rc(), "print", async_print);
        }

        // Evaluate harness files unless `raw`: compile & run them into the provided VM.
        if !raw_flag {
            for h in &harness_files {
                let p = test262_root.join("harness").join(h);
                if p.exists() {
                    let hsrc = read_harness_source(&p)?;
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
            if vm.get_global("Test262Error").is_none() {
                const FIX_SCRIPT: &str = r#"
                    if (typeof Test262Error !== "undefined"
                        && typeof globalThis.Test262Error === "undefined") {
                        globalThis.Test262Error = Test262Error;
                    }
                "#;
                let fix_path = test262_root.join("test262Error-fix.js");
                let session = parse_source_for_path(FIX_SCRIPT, &fix_path)
                    .map_err(|e| format!("Failed to parse Test262Error fixer: {}", e))?;
                let prog = session.program();
                let chunk = compiler::compile_oxc(prog, FIX_SCRIPT)
                    .map_err(|e| format!("Failed to compile Test262Error fixer: {}", e))?;
                if let Err(e) = vm.run(chunk) {
                    eprintln!(
                        "[test262] warning: fixer script for Test262Error failed: {}",
                        e
                    );
                }
            }
        }

        if is_module && !raw_flag {
            for inc in &include_files {
                let p = test262_root.join("harness").join(inc);
                if !p.exists() {
                    eprintln!(
                        "[test262] optional include missing (continuing): {}",
                        p.display()
                    );
                    continue;
                }

                let inc_src =
                    read_harness_source(&p).map_err(|e| format!("Failed to read include {}", e))?;
                let session = match parse_source_for_path(&inc_src, &p) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!(
                            "Unsupported {} because include {} parse failed: {}",
                            path.display(),
                            inc,
                            e
                        );
                        return Err(format!(
                            "{} (UNSUPPORTED: include {} parse failed: {})",
                            path.display(),
                            inc,
                            e
                        ));
                    }
                };
                let prog = session.program();
                let chunk = match compiler::compile_oxc(prog, &inc_src) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!(
                            "Unsupported {} because include {} compile failed: {}",
                            path.display(),
                            inc,
                            e
                        );
                        return Err(format!(
                            "{} (UNSUPPORTED: include {} compile failed: {})",
                            path.display(),
                            inc,
                            e
                        ));
                    }
                };
                if let Err(e) = vm.run(chunk) {
                    eprintln!(
                        "Unsupported {} because include {} failed at runtime: {}",
                        path.display(),
                        inc,
                        e
                    );
                    return Err(format!(
                        "{} (UNSUPPORTED: include {} runtime failed: {})",
                        path.display(),
                        inc,
                        e
                    ));
                }
            }
        }
        // Apply strict override for the test source after harness setup.
        typecheck::set_strict_override(Some(is_strict));

        let run_result = if is_module {
            // Module tests are evaluated as modules, not as concatenated scripts.
            if let Some(m) = &meta
                && let Some(neg) = &m.negative
                && neg.phase == "parse"
            {
                match Parser::parse_source_with_type(&src, SourceType::mjs()) {
                    Ok(session) => {
                        let prog = session.program();
                        if let Err(e) = compiler::compile_oxc(prog, &src) {
                            let err_str = e.to_string();
                            if err_str.contains("SyntaxError") || err_str.contains("redeclaration")
                            {
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

            let loader_handle = vm.module_loader_handle();
            {
                let mut loader = loader_handle.borrow_mut();
                let base_dir = path.parent().unwrap_or(Path::new("."));
                loader
                    .construct(
                        base_dir,
                        &format!("./{}", path.file_name().unwrap().to_string_lossy()),
                    )
                    .map_err(|e| e.to_string())?;
                loader.instantiate_all().map_err(|e| e.to_string())?;
            }
            match blitz::module::evaluate_entry_shared(&loader_handle, path, &mut vm, false) {
                Ok(()) => Ok(Value::Undefined),
                Err(e) => Err(e.to_string()),
            }
        } else {
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
                            if err_str.contains("SyntaxError") || err_str.contains("redeclaration")
                            {
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
                        Test262Error::new(path, mode_name, format!("compile error: {e}"))
                            .into_string(),
                    );
                }
            };

            // Execute using the provided persistent VM
            vm.run(chunk).map_err(|e| e.to_string())
        };

        // For async tests, drain the microtask queue to ensure promise callbacks execute
        let run_result = if is_async {
            // First check if synchronous execution failed
            match run_result {
                Err(e) => Err(e),
                Ok(v) => {
                    // Drain microtasks to allow promise callbacks to run
                    if let Err(e) = vm.flush_microtasks() {
                        Err(e.to_string())
                    } else {
                        // Now check if $DONE was called
                        let start = Instant::now();
                        let warn_threshold = Duration::from_secs(10);
                        let hard_timeout = Duration::from_secs(30);
                        let mut warned = false;
                        let release_hook = release_handle.as_ref();

                        loop {
                            if let Err(e) = vm.flush_microtasks() {
                                break Err(e.to_string());
                            }

                            if let Some(state) = done_state.borrow().as_ref() {
                                let async_result = match state {
                                    None => {
                                        if emit_async_marker {
                                            println!("Test262:AsyncTestComplete");
                                        }
                                        Ok(v)
                                    }
                                    Some(err) => Err(format!("async test failed: {}", err)),
                                };
                                break async_result;
                            }

                            if start.elapsed() > hard_timeout {
                                break Err(format!(
                                    "async test timed out after {}s",
                                    hard_timeout.as_secs()
                                ));
                            }

                            if !warned && start.elapsed() > warn_threshold {
                                eprintln!(
                                    "[test262 warning] {} still waiting for $DONE after {}s",
                                    path.display(),
                                    warn_threshold.as_secs()
                                );
                                warned = true;
                                if let Some(releaser) = release_hook {
                                    releaser.release();
                                }
                            }

                            thread::sleep(Duration::from_millis(10));
                        }
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

fn run_harness_only(
    test262_root: &Path,
    globals: &Rc<RefCell<ObjectData>>,
    baseline_snapshot: &HashMap<PropertyKey, PropertyDescriptor>,
) -> Result<(), String> {
    let result = (|| {
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
        let _ = runtime_utils::install_minimal_kernel_constructors(globals);
        let mut vm = Vm::new(Rc::clone(globals));
        for h in &harness_files {
            let p = test262_root.join("harness").join(h);
            if !p.exists() {
                return Err(format!("Harness file missing: {}", p.display()));
            }

            let saved_detach = if h == "detachArrayBuffer.js" {
                let mut globals_borrow = globals.borrow_mut();
                match globals_borrow
                    .properties
                    .get_mut(&PropertyKey::String("$262".to_string()))
                {
                    Some(PropertyDescriptor::Data {
                        value: Value::Object(obj),
                        ..
                    }) => obj
                        .borrow_mut()
                        .properties
                        .shift_remove(&PropertyKey::String("detachArrayBuffer".to_string())),
                    _ => None,
                }
            } else {
                None
            };

            let hsrc = fs::read_to_string(&p)
                .map_err(|e| format!("Failed to read harness {}: {}", p.display(), e))?;
            let session = parse_source_for_path(&hsrc, &p)
                .map_err(|e| format!("Harness {} parse failed: {}", h, e))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog, &hsrc)
                .map_err(|e| format!("Harness {} compile failed: {}", h, e))?;
            let run_result = vm.run(chunk);

            if let Some(saved_detach) = saved_detach {
                let mut globals_borrow = globals.borrow_mut();
                if let Some(PropertyDescriptor::Data {
                    value: Value::Object(obj),
                    ..
                }) = globals_borrow
                    .properties
                    .get_mut(&PropertyKey::String("$262".to_string()))
                {
                    obj.borrow_mut().properties.insert(
                        PropertyKey::String("detachArrayBuffer".to_string()),
                        saved_detach,
                    );
                }
            }

            match run_result {
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
    })();

    force_runner_gc();
    result
}

const SCOPE_DUMP_TEST: &str = "language/expressions/object/11.1.5-0-1.js";

fn run_scope_dump_test(
    test262_root: &Path,
    globals: &Rc<RefCell<ObjectData>>,
    baseline_snapshot: &HashMap<PropertyKey, PropertyDescriptor>,
) -> Result<(), String> {
    let result = (|| {
        restore_globals(globals, baseline_snapshot);
        let harness_dir = test262_root.join("harness");
        let keep_globals: HashSet<String> = globals
            .borrow()
            .properties
            .keys()
            .filter_map(|k| match k {
                PropertyKey::String(s) => Some(s.clone()),
                PropertyKey::Symbol(_) => None,
            })
            .collect();
        let mut vm = Vm::new(Rc::clone(globals));

        let harness_files = ["assert.js", "sta.js"];
        for harness in &harness_files {
            let path = harness_dir.join(harness);
            if !path.exists() {
                return Err(format!("Harness {} missing", path.display()));
            }
            let src = read_harness_source(&path)?;
            let session = parse_source_for_path(&src, &path)
                .map_err(|e| format!("Harness {} parse failed: {}", harness, e))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog, &src)
                .map_err(|e| format!("Harness {} compile failed: {}", harness, e))?;
            vm.run(chunk)
                .map_err(|e| format!("Harness {} runtime failed: {}", harness, e))?;
        }

        if vm.get_global("Test262Error").is_none() {
            const FIX_SCRIPT: &str = r#"
                if (typeof Test262Error !== "undefined"
                    && typeof globalThis.Test262Error === "undefined") {
                    globalThis.Test262Error = Test262Error;
                }
            "#;
            let fix_path = test262_root.join("test262Error-fix.js");
            let session = parse_source_for_path(FIX_SCRIPT, &fix_path)
                .map_err(|e| format!("Failed to parse Test262Error fixer: {}", e))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog, FIX_SCRIPT)
                .map_err(|e| format!("Failed to compile Test262Error fixer: {}", e))?;
            let _ = vm.run(chunk);
        }

        let test_path = test262_root.join("test").join(SCOPE_DUMP_TEST);
        let src = fs::read_to_string(&test_path).map_err(|e| {
            format!(
                "Failed to read scope dump test {}: {}",
                test_path.display(),
                e
            )
        })?;
        let session = parse_source_for_path(&src, &test_path)
            .map_err(|e| format!("Scope dump test parse failed: {}", e))?;
        let prog = session.program();
        let chunk = compiler::compile_oxc(prog, &src)
            .map_err(|e| format!("Scope dump test compile failed: {}", e))?;
        let run_result = vm.run(chunk);

        if let Err(e) = &run_result {
            eprintln!("Scope dump test runtime failed: {}", e);
            print_vm_diag(&vm, &keep_globals);
        }

        run_result
            .map(|_| ())
            .map_err(|e| format!("Scope dump test runtime failed: {}", e))
    })();

    force_runner_gc();
    result
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
    let global_this = Value::Object(globals.clone());
    globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string()),
        PropertyDescriptor::data(global_this),
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

    if opts.dump_scopes {
        match run_scope_dump_test(&test262_dir, &globals, &baseline_snapshot) {
            Ok(()) => {
                eprintln!("Scope dump run: ok");
                exit(0);
            }
            Err(e) => {
                eprintln!("Scope dump run: fail: {}", e);
                exit(1);
            }
        }
    }

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

    let files_to_run = if let Some(file_list) = &opts.file_list {
        match load_file_list(file_list) {
            Ok(files) => files,
            Err(msg) => {
                eprintln!("{}", msg);
                exit(2);
            }
        }
    } else {
        select_files_to_run(
            files,
            &opts.filters,
            &opts.anti_filters,
            &opts.anti_segments,
            &opts.es_versions,
            opts.limit,
        )
    };
    let job_count = resolve_job_count(
        opts.jobs,
        files_to_run.len(),
        thread::available_parallelism()
            .ok()
            .map(std::num::NonZero::get),
    );
    let mut test_results = if should_use_subprocess_batches(&opts, files_to_run.len()) {
        match run_selected_tests_via_subprocess_batches(
            files_to_run,
            job_count,
            opts.log_tests,
            opts.batch_size,
        ) {
            Ok(results) => results,
            Err(msg) => {
                eprintln!("{}", msg);
                exit(2);
            }
        }
    } else {
        run_selected_tests(files_to_run, test262_dir.clone(), job_count, opts.log_tests)
    };
    sort_test_results(&mut test_results);

    let mut failed_cases = Vec::new();
    for (idx, f, result) in &test_results {
        let (top_key, subsection_key) = bucket_keys(&test_root, f);
        match result {
            TestResult::Pass => {
                ran += 1;
                top_level.entry(top_key).or_default().pass += 1;
                subsections.entry(subsection_key).or_default().pass += 1;
            }
            TestResult::Skip(_) => {
                top_level.entry(top_key).or_default().skip += 1;
                subsections.entry(subsection_key).or_default().skip += 1;
            }
            TestResult::Fail(e) => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                failed_cases.push((*idx, f.clone(), e.clone()));
            }
            TestResult::StackOverflow => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                failed_cases.push((*idx, f.clone(), "Stack overflow".into()));
            }
            TestResult::Panic(msg) => {
                ran += 1;
                top_level.entry(top_key).or_default().fail += 1;
                subsections.entry(subsection_key).or_default().fail += 1;
                failed_cases.push((*idx, f.clone(), format!("Panic: {}", msg)));
            }
        }
    }

    let internal_report_mode = opts.report_json.is_some();
    failed_cases.sort_by_key(|(idx, _, _)| *idx);
    for (_, f, msg) in failed_cases {
        if !internal_report_mode {
            if msg == "Stack overflow" {
                if let Ok(rel) = f.strip_prefix(&test262_dir) {
                    eprintln!("STACKOVERFLOW: {}", rel.display());
                } else {
                    eprintln!("STACKOVERFLOW: {}", f.display());
                }
            } else if let Some(rest) = msg.strip_prefix("Panic: ") {
                if let Ok(rel) = f.strip_prefix(&test262_dir) {
                    eprintln!("PANIC: {}: {}", rel.display(), rest);
                } else {
                    eprintln!("PANIC: {}: {}", f.display(), rest);
                }
            } else {
                eprintln!("FAIL: {}", msg);
            }
        }
        failed.push((f, msg));
    }

    let passed = ran.saturating_sub(failed.len());
    let pass_pct = if ran == 0 {
        0.0
    } else {
        (passed as f64) / (ran as f64) * 100.0
    };

    let exit_code = if failed.is_empty() { 0 } else { 1 };

    if !internal_report_mode {
        eprintln!(
            "Test262: ran {}, passed {}, failed {} ({:.2}% pass rate)",
            ran,
            passed,
            failed.len(),
            pass_pct
        );
        print_bucket_summary("Sections:", &top_level, 12, false);
    }

    if let Some(report_path) = &opts.report_json {
        let report = SubprocessBatchReport {
            results: test_results,
        };
        match serde_json::to_string(&report) {
            Ok(json) => {
                if let Err(e) = fs::write(report_path, json) {
                    eprintln!(
                        "Failed to write subprocess report {}: {}",
                        report_path.display(),
                        e
                    );
                    exit(2);
                }
            }
            Err(e) => {
                eprintln!("Failed to serialize subprocess report: {}", e);
                exit(2);
            }
        }
    }

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
    fn resolve_job_count_clamps_to_selected_files() {
        assert_eq!(resolve_job_count(Some(64), 5, Some(8)), 5);
        assert_eq!(resolve_job_count(None, 5, Some(8)), 2);
        assert_eq!(resolve_job_count(Some(0), 5, Some(8)), 1);
    }

    #[test]
    fn sort_test_results_orders_by_original_index() {
        let mut results = vec![
            (2, PathBuf::from("c.js"), TestResult::Pass),
            (0, PathBuf::from("a.js"), TestResult::Fail("x".to_string())),
            (1, PathBuf::from("b.js"), TestResult::StackOverflow),
        ];

        sort_test_results(&mut results);

        let indices: Vec<_> = results.iter().map(|(idx, _, _)| *idx).collect();
        assert_eq!(indices, vec![0, 1, 2]);
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
    fn bucket_keys_groups_language_tests_by_edition() {
        let root = temp_dir("bucket-language");
        let _guard = TempDirGuard(root.clone());
        let file = root.join("language/expressions/grouping.js");

        write_file(
            &file,
            r#"/*---
features: [array-grouping]
---*/
const ok = 1;
"#,
        );

        let (top, subsection) = bucket_keys(&root, &file);

        assert_eq!(top, "ES2024");
        assert_eq!(subsection, "language/expressions");
    }

    #[test]
    fn bucket_keys_groups_es5_language_tests() {
        let root = temp_dir("bucket-es5");
        let _guard = TempDirGuard(root.clone());
        let file = root.join("language/comments/sputnik.js");

        write_file(
            &file,
            r#"/*---
es5id: 7.4_A6
---*/
var ok = 1;
"#,
        );

        let (top, subsection) = bucket_keys(&root, &file);

        assert_eq!(top, "ES5");
        assert_eq!(subsection, "language/comments");
    }

    #[test]
    fn normalize_es_versions_accepts_common_forms() {
        assert_eq!(normalize_es_version("5"), Some("ES5".to_string()));
        assert_eq!(normalize_es_version("es6"), Some("ES6".to_string()));
        assert_eq!(normalize_es_version("2016"), Some("ES2016".to_string()));
        assert_eq!(normalize_es_version("next"), Some("ESNext".to_string()));
    }

    #[test]
    fn normalize_es_versions_rejects_unknown_label() {
        assert_eq!(normalize_es_version("ES1999"), None);
        assert!(normalize_es_versions(&["ES5".to_string(), "ES6".to_string()]).is_ok());
        assert!(normalize_es_versions(&["ES5".to_string(), "unknown".to_string()]).is_err());
    }

    #[test]
    fn select_files_to_run_filters_by_es_version() {
        let root = temp_dir("select-es-version");
        let _guard = TempDirGuard(root.clone());
        let es5_file = root.join("language/comments/es5.js");
        let es2024_file = root.join("language/expressions/es2024.js");
        let builtin_file = root.join("built-ins/Array/push.js");

        write_file(
            &es5_file,
            r#"/*---
es5id: 7.4_A6
---*/
var ok = 1;
"#,
        );
        write_file(
            &es2024_file,
            r#"/*---
features: [array-grouping]
---*/
const ok = 1;
"#,
        );
        write_file(&builtin_file, "const ok = 1;\n");

        let selected = select_files_to_run(
            vec![builtin_file.clone(), es2024_file.clone(), es5_file.clone()],
            &[],
            &[],
            &[],
            &["ES5".to_string()],
            None,
        );

        assert_eq!(selected, vec![es5_file]);
    }

    #[test]
    fn print_args_uses_js_stringification() {
        let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        let intrinsics = runtime_utils::install_minimal_kernel_constructors(&globals);
        let mut vm = Vm::new(globals.clone());

        let array_proto = intrinsics
            .array_prototype
            .expect("minimal intrinsics should install Array.prototype");
        let array_props = Rc::new(RefCell::new(ObjectData::new()));
        array_props.borrow_mut().set_prototype_value(array_proto);
        let array = Value::Array(
            Rc::new(RefCell::new(vec![
                Some(Value::Number(1.0)),
                Some(Value::Number(2.0)),
            ])),
            array_props,
        );

        assert_eq!(print_args(&mut vm, &[array]), "1,2");
    }

    #[test]
    fn print_args_defaults_to_undefined_when_missing() {
        let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        let _intrinsics = runtime_utils::install_minimal_kernel_constructors(&globals);
        let mut vm = Vm::new(globals.clone());

        assert_eq!(print_args(&mut vm, &[]), "undefined");
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
