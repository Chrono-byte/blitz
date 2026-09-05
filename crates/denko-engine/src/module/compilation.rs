use super::*;

pub(super) struct ModuleCompileJob {
  pub(super) path: PathBuf,
  pub(super) source: String,
  pub(super) source_type: SourceType,
  pub(super) context: ModuleContext,
}

pub(super) fn compile_modules_parallel(
  jobs: Vec<ModuleCompileJob>,
  pipeline_counters: Arc<ModuleLoaderPipelineCounters>,
) -> Result<HashMap<PathBuf, Chunk>, crate::errors::RuntimeError> {
  if jobs.len() < 2 {
    return Ok(HashMap::new());
  }

  let worker_count = std::thread::available_parallelism()
    .map_or(1, usize::from)
    .min(8)
    .min(jobs.len());
  let jobs = Arc::new(jobs);
  let next_job = AtomicUsize::new(0);
  let (sender, receiver) = mpsc::channel();

  std::thread::scope(|scope| -> Result<(), crate::errors::RuntimeError> {
    for _ in 0..worker_count {
      let jobs = Arc::clone(&jobs);
      let sender = sender.clone();
      let next_job = &next_job;
      let pipeline_counters = Arc::clone(&pipeline_counters);
      std::thread::Builder::new()
        .name("blitz-module-compiler".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn_scoped(scope, move || {
          loop {
            let index = next_job.fetch_add(1, Ordering::Relaxed);
            let Some(job) = jobs.get(index) else {
              break;
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
              pipeline_counters.source_parse();
              crate::parser::Parser::parse_source_with_type(&job.source, job.source_type)
                .map_err(|error| format!("Parse failed: {error}"))
                .and_then(|session| {
                  pipeline_counters.source_compilation();
                  crate::compiler::compile_oxc_with_ctx(
                    session.program(),
                    Some(&job.context),
                    &job.source,
                  )
                  .map(crate::runtime_utils::clear_slot_types_in_chunk)
                  .map_err(|error| format!("Compile failed: {error}"))
                })
            }))
            .unwrap_or_else(|payload| {
              let message = payload
                .downcast_ref::<&str>()
                .map_or_else(
                  || {
                    payload
                      .downcast_ref::<String>()
                      .map(String::as_str)
                  },
                  |message| Some(*message),
                )
                .unwrap_or("unknown compiler panic");
              Err(format!("Compiler worker panicked: {message}"))
            });
            if sender
              .send((index, job.path.clone(), result))
              .is_err()
            {
              break;
            }
          }
        })
        .map_err(|error| {
          crate::errors::RuntimeError::from(format!("Failed to spawn compiler worker: {error}"))
        })?;
    }
    Ok(())
  })?;
  drop(sender);

  let mut results: Vec<_> = receiver.into_iter().collect();
  results.sort_unstable_by_key(|(index, _, _)| *index);
  let mut compiled = HashMap::with_capacity(jobs.len());
  for (_, path, result) in results {
    let code = result.map_err(crate::errors::RuntimeError::from)?;
    compiled.insert(path, code);
  }
  Ok(compiled)
}

/// Evaluate a module graph using a shared loader handle.
///
/// Compilation is performed concurrently, while evaluation remains ordered
/// and single-threaded on `vm`. This helper borrows the loader only for short
/// phases so nested dynamic imports can re-enter the same cache while module
/// code is running.
///
/// # Errors
///
/// Returns a [`crate::errors::RuntimeError`] if compilation or evaluation fails.
///
/// # Panics
///
/// Panics if the entry cannot be resolved to a canonical path.
pub fn evaluate_entry_shared(
  loader: &Rc<RefCell<ModuleLoader>>,
  entry: &Path,
  vm: &mut Vm,
  enable_typecheck: bool,
) -> Result<(), crate::errors::RuntimeError> {
  let promise = evaluate_entry_promise_shared(loader, entry, vm, enable_typecheck)?;
  async_evaluation::finish_host_evaluation(vm, &promise)
}

/// Start evaluating a linked module graph and return its cached evaluation promise.
/// Pending top-level awaits resume through the VM job queue; this function does
/// not drain jobs or require the promise to settle synchronously.
///
/// # Errors
/// Returns an error if graph preparation or compilation fails.
pub fn evaluate_entry_promise_shared(
  loader: &Rc<RefCell<ModuleLoader>>,
  entry: &Path,
  vm: &mut Vm,
  enable_typecheck: bool,
) -> Result<Value, crate::errors::RuntimeError> {
  let entry = loader
    .borrow_mut()
    .prepare_evaluation(entry, vm, enable_typecheck)?;
  async_evaluation::evaluate(vm, &entry)
}

impl ModuleLoader {
  pub(super) fn prepare_evaluation(
    &mut self,
    entry: &Path,
    vm: &mut Vm,
    enable_typecheck: bool,
  ) -> Result<Rc<RefCell<ModuleRecord>>, crate::errors::RuntimeError> {
    use crate::errors::RuntimeError;
    for record in self.map.values() {
      let record = record.borrow();
      if let Some(index) = record.module_index {
        vm.ensure_module_exports(index, record.exports.clone());
      }
    }
    self.initialize_namespace_reexports(vm);
    let order = self
      .evaluation_order_from(entry)
      .map_err(|error| RuntimeError::from(format!("Module resolution failed: {error}")))?;
    let entry_path = order
      .last()
      .ok_or_else(|| RuntimeError::from("empty module graph"))?;
    let entry = Rc::clone(&self.map[entry_path]);
    let declarations = self.virtual_declarations_for_typecheck();
    let jobs = order
      .iter()
      .filter_map(|path| {
        let record = self.map.get(path)?.borrow();
        if record.state != ModuleState::Instantiated || record.bytecode.is_some() {
          return None;
        }
        let ModuleRecordPayload::Source {
          session,
          source_type,
          ..
        } = &record.payload
        else {
          return None;
        };
        Some(ModuleCompileJob {
          path: path.clone(),
          source: session.source().to_string(),
          source_type: *source_type,
          context: ModuleContext {
            module_index: record
              .module_index
              .expect("instantiated module index"),
            imports: record.import_sources.clone(),
            exports: record.exports.keys().cloned().collect(),
            namespace_exports: record.namespace_exports.clone(),
          },
        })
      })
      .collect();
    let mut compiled = compile_modules_parallel(jobs, Arc::clone(&self.pipeline_counters))?;
    for path in order {
      let record = Rc::clone(&self.map[&path]);
      if record.borrow().state != ModuleState::Instantiated {
        continue;
      }
      let dependencies = self
        .evaluation_dependencies(&record)
        .map_err(|error| RuntimeError::from(format!("Module resolution failed: {error}")))?;
      let mut record = record.borrow_mut();
      record.evaluation.dependencies = dependencies.iter().map(Rc::downgrade).collect();
      if record.host_initialized {
        continue;
      }
      if let ModuleRecordPayload::Source {
        session,
        dependencies,
        ..
      } = &record.payload
        && enable_typecheck
      {
        self.pipeline_counters.source_typecheck();
        crate::typecheck::check_with_declarations_and_source_imports(
          session.program(),
          &declarations,
          dependencies,
        )
        .map_err(|error| RuntimeError::from(format!("Typecheck failed: {error}")))?;
      }
      if record.bytecode.is_none() {
        let code = if let Some(code) = compiled.remove(&path) {
          code
        } else {
          let ModuleRecordPayload::Source { session, .. } = &record.payload else {
            return Err(RuntimeError::from("prelinked module missing bytecode"));
          };
          self.pipeline_counters.source_compilation();
          crate::compiler::compile_oxc_with_ctx(
            session.program(),
            Some(&ModuleContext {
              module_index: record
                .module_index
                .expect("instantiated module index"),
              imports: record.import_sources.clone(),
              exports: record.exports.keys().cloned().collect(),
              namespace_exports: record.namespace_exports.clone(),
            }),
            session.source(),
          )
          .map_err(|error| RuntimeError::from(format!("Compile failed: {error}")))?
        };
        record.bytecode = Some(crate::runtime_utils::clear_slot_types_in_chunk(code));
      }
      record.evaluation.has_tla = record
        .bytecode
        .as_ref()
        .is_some_and(|code| code.has_top_level_await);
    }
    Ok(entry)
  }

  fn evaluation_dependencies(
    &mut self,
    record: &Rc<RefCell<ModuleRecord>>,
  ) -> Result<Vec<Rc<RefCell<ModuleRecord>>>, BundlerError> {
    enum Requests {
      Source(Vec<String>, PathBuf, Option<String>),
      Prelinked(Vec<usize>),
    }
    let requests = {
      let record = record.borrow();
      match &record.payload {
        ModuleRecordPayload::Source { dependencies, .. } => Requests::Source(
          dependencies.clone(),
          record
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
          record
            .path
            .to_string_lossy()
            .starts_with(VIRTUAL_PATH_PREFIX)
            .then(|| record.specifier.clone()),
        ),
        ModuleRecordPayload::Prelinked { dependencies } => {
          Requests::Prelinked(dependencies.clone())
        }
      }
    };
    match requests {
      Requests::Source(requests, base, importer) => requests
        .into_iter()
        .map(|specifier| self.construct_from_importer(&base, importer.as_deref(), &specifier))
        .collect(),
      Requests::Prelinked(indices) => indices
        .into_iter()
        .map(|index| {
          let location = self
            .prelinked_by_index
            .get(&index)
            .copied()
            .ok_or_else(|| BundlerError::from(format!("missing prelinked dependency {index}")))?;
          self.construct_prelinked_at(location)
        })
        .collect(),
    }
  }
}
