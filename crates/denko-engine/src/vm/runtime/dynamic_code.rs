// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::value::Value;
use crate::vm::Vm;

impl Vm {
  pub fn eval_expression_source(
    &mut self,
    source: &str,
  ) -> Result<Value, RuntimeError> {
    let strict = self.active_strict_mode();
    let wrapped = if strict {
      crate::js!(
        format(script),
        "'use strict'; (function() {{ return ({}); }})()",
        source
      )
    } else {
      crate::js!(format(script), "(function() {{ return ({}); }})()", source)
    };
    let session = crate::parser::Parser::parse_source(&wrapped)
      .map_err(|e| RuntimeError::from(format!("parse: {}", e)))?;
    let code = crate::compiler::compile_repl_session(&session)
      .map_err(|e| RuntimeError::from(format!("compile: {}", e)))?;
    let mut child = self.fork_execution();
    child.adopt_eval_context(self);
    child.set_current_strict_mode(strict);
    match child.run_nested(code, false) {
      Ok(value) => Ok(value),
      Err(failure) => {
        if let Some(thrown) = failure.thrown {
          Err(self.throw_exception(thrown))
        } else {
          Err(failure.error)
        }
      }
    }
  }

  /// Parse and evaluate a single expression source string in the current VM.
  ///
  /// This is the direct-eval equivalent of [`Vm::eval_expression_source`]:
  /// it preserves the caller's execution context and leaves any new `var`
  /// bindings in the current lexical environment chain.
  pub fn eval_expression_source_direct(
    &mut self,
    source: &str,
  ) -> Result<Value, RuntimeError> {
    let strict = self.active_strict_mode();
    let wrapped = if strict {
      crate::js!(
        format(script),
        "'use strict'; (function() {{ return ({}); }})()",
        source
      )
    } else {
      crate::js!(format(script), "(function() {{ return ({}); }})()", source)
    };
    let session = crate::parser::Parser::parse_source(&wrapped)
      .map_err(|e| RuntimeError::from(format!("parse: {}", e)))?;
    let code = crate::compiler::compile_repl_session(&session)
      .map_err(|e| RuntimeError::from(format!("compile: {}", e)))?;
    match self.run_nested(code, true) {
      Ok(value) => Ok(value),
      Err(failure) => {
        if let Some(thrown) = failure.thrown {
          Err(self.throw_exception(thrown))
        } else {
          Err(failure.error)
        }
      }
    }
  }
}
