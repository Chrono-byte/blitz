// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use blitz_runtime::host::RuntimeOptions;
use blitz_runtime::session::HostedSession;
use denko::value::Value;
use rustyline::DefaultEditor;

enum InputEvent {
  Line(String),
  Closed,
  Error(String),
}

pub(crate) fn run(
  options: RuntimeOptions,
  preloads: &[PathBuf],
) -> Result<i32, denko::errors::Error> {
  let mut session = HostedSession::new(options);
  session.preload(preloads)?;
  let input = spawn_input();
  println!("Blitz REPL. Type exit to quit.");

  loop {
    match input.recv_timeout(Duration::from_millis(20)) {
      Ok(InputEvent::Line(input)) => {
        let line = input.trim();
        if line.is_empty() {
          continue;
        }
        if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
          break;
        }
        match session.evaluate_repl(line) {
          Ok(value) if !matches!(value, Value::Null | Value::Undefined) => {
            println!("{}", denko::inspect::inspect(&value));
          }
          Ok(_) => {}
          Err(error) => eprintln!("Error: {error}"),
        }
      }
      Ok(InputEvent::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
      Ok(InputEvent::Error(error)) => {
        return Err(denko::errors::RuntimeError::from(format!("Readline error: {error}")).into());
      }
      Err(mpsc::RecvTimeoutError::Timeout) => {
        let _processed = session.poll(Duration::from_millis(1))?;
      }
    }
  }

  Ok(session.exit_status().code)
}

fn spawn_input() -> mpsc::Receiver<InputEvent> {
  let (events, receiver) = mpsc::channel();
  drop(thread::spawn(move || {
    let mut editor = match DefaultEditor::new() {
      Ok(editor) => editor,
      Err(error) => {
        drop(events.send(InputEvent::Error(error.to_string())));
        return;
      }
    };
    let history_path = env::temp_dir().join("blitz_repl_history");
    drop(editor.load_history(&history_path));
    loop {
      match editor.readline("> ") {
        Ok(line) => {
          if !line.trim().is_empty() {
            drop(editor.add_history_entry(line.as_str()));
            drop(editor.save_history(&history_path));
          }
          if events.send(InputEvent::Line(line)).is_err() {
            break;
          }
        }
        Err(rustyline::error::ReadlineError::Interrupted) => continue,
        Err(rustyline::error::ReadlineError::Eof) => {
          drop(events.send(InputEvent::Closed));
          break;
        }
        Err(error) => {
          drop(events.send(InputEvent::Error(error.to_string())));
          break;
        }
      }
    }
  }));
  receiver
}
