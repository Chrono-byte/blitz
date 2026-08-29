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
  {
    let loader_ref = loader.borrow();
    for rec_rc in loader_ref.map.values() {
      let rec = rec_rc.borrow();
      if let Some(idx) = rec.module_index {
        let exports_clone = rec.exports.clone();
        vm.install_module_exports(idx, exports_clone);
      }
    }
    loader_ref.initialize_namespace_reexports(vm);
  }

  let order = {
    let mut loader_ref = loader.borrow_mut();
    loader_ref
      .evaluation_order_from(entry)
      .map_err(|e| crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e)))?
  };

  let declarations = {
    let loader_ref = loader.borrow();
    loader_ref.virtual_declarations_for_typecheck()
  };

  let compile_jobs = {
    let loader_ref = loader.borrow();
    order
      .iter()
      .filter_map(|path| {
        let record = loader_ref.map.get(path)?.borrow();
        if !matches!(record.state, ModuleState::Instantiated) || record.bytecode.is_some() {
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
              .expect("module_index should be set"),
            imports: record.import_sources.clone(),
            exports: record.exports.keys().cloned().collect(),
            namespace_exports: record.namespace_exports.clone(),
          },
        })
      })
      .collect()
  };
  let pipeline_counters = {
    let loader_ref = loader.borrow();
    Arc::clone(&loader_ref.pipeline_counters)
  };
  let mut compiled = compile_modules_parallel(compile_jobs, pipeline_counters)?;

  for path in order {
    let maybe_chunk = 'prepare: {
      let loader_ref = loader.borrow_mut();
      let Some(rec_rc) = loader_ref.map.get(&path).cloned() else {
        break 'prepare None;
      };
      let rec_rc_return = Rc::clone(&rec_rc);
      let mut rec = rec_rc.borrow_mut();
      if matches!(rec.state, ModuleState::Evaluating) {
        None
      } else if matches!(rec.state, ModuleState::Instantiated) {
        if rec.host_initialized {
          rec.state = ModuleState::Evaluated;
          break 'prepare None;
        }
        rec.state = ModuleState::Evaluating;
        let code = match &rec.payload {
          ModuleRecordPayload::Prelinked { .. } => rec.bytecode.clone().ok_or_else(|| {
            crate::errors::RuntimeError::from(
              "Prelinked module record is missing its validated chunk",
            )
          })?,
          ModuleRecordPayload::Source {
            session,
            dependencies,
            ..
          } => {
            let program = session.program();
            if enable_typecheck {
              loader_ref.pipeline_counters.source_typecheck();
              crate::typecheck::check_with_declarations_and_source_imports(
                program,
                &declarations,
                dependencies,
              )
              .map_err(|error| {
                crate::errors::RuntimeError::from(format!("Typecheck failed: {error}"))
              })?;
            }
            let module_index = rec
              .module_index
              .expect("module_index should be set");
            let context = ModuleContext {
              module_index,
              imports: rec.import_sources.clone(),
              exports: rec.exports.keys().cloned().collect(),
              namespace_exports: rec.namespace_exports.clone(),
            };
            let code = if let Some(code) = rec.bytecode.clone() {
              crate::runtime_utils::clear_slot_types_in_chunk(code)
            } else if let Some(code) = compiled.remove(&path) {
              code
            } else {
              loader_ref.pipeline_counters.source_compilation();
              let code =
                crate::compiler::compile_oxc_with_ctx(program, Some(&context), session.source())
                  .map_err(|error| {
                    crate::errors::RuntimeError::from(format!("Compile failed: {error}"))
                  })?;
              crate::runtime_utils::clear_slot_types_in_chunk(code)
            };
            rec.bytecode = Some(code.clone());
            code
          }
        };
        Some((rec_rc_return, code))
      } else {
        None
      }
    };

    if let Some((rec_rc, code)) = maybe_chunk {
      let specifier = rec_rc.borrow().specifier.clone();
      vm.set_current_module_specifier(Some(specifier));
      let result = vm.run(code);
      vm.set_current_module_specifier(None);
      result?;
      rec_rc.borrow_mut().state = ModuleState::Evaluated;
    }
  }

  Ok(())
}
