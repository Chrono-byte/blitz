// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::future::Future;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::rc::Rc;
use std::thread;

use bytes::Bytes;
use denko::compiler;
use denko::parser::Parser as JsParser;
use denko::value::{ObjectData, Value};
use fastwebsockets::{Frame, OpCode, Payload};
use http_body_util::Empty;
use hyper::Request;
use hyper::header::{CONNECTION, UPGRADE};
use hyper::rt::Executor;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;

use super::*;

struct SpawnExecutor;

impl<Fut> Executor<Fut> for SpawnExecutor
where
  Fut: Future + Send + 'static,
  Fut::Output: Send + 'static,
{
  fn execute(
    &self,
    future: Fut,
  ) {
    tokio::spawn(future);
  }
}

#[test]
fn deno_surface_manifest_is_well_formed_and_tracks_server_lifecycle() {
  let manifest: serde_json::Value =
    serde_json::from_str(include_str!("../../compat/deno-surface.json"))
      .expect("parse Deno surface manifest");
  assert_eq!(manifest["schemaVersion"], 1);
  assert_eq!(manifest["target"], "Deno 2.9.3");
  let members = manifest["members"]
    .as_array()
    .expect("surface members are an array");
  let mut paths = std::collections::BTreeSet::new();
  for member in members {
    let path = member["path"].as_str().expect("surface path");
    assert!(paths.insert(path), "duplicate surface path: {path}");
    assert!(matches!(
      member["status"].as_str(),
      Some("compatible" | "partial" | "extension" | "planned")
    ));
  }
  for required in [
    "Blitz.serve().addr",
    "Blitz.serve().finished",
    "Blitz.serve().shutdown",
    "Blitz.serve().ref",
    "Blitz.serve().unref",
  ] {
    assert!(
      paths.contains(required),
      "missing surface member: {required}"
    );
  }

  let declarations = include_str!("../../types/blitz.d.ts");
  for declaration in [
    "readonly addr: BlitzNetAddr",
    "readonly finished: Promise<void>",
    "shutdown(): Promise<void>",
    "ref(): void",
    "unref(): void",
  ] {
    assert!(
      declarations.contains(declaration),
      "missing host declaration: {declaration}"
    );
  }
}

#[test]
fn compatibility_globals_are_disabled_by_default() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let options = RuntimeOptions::from_host(vec!["blitz-test".to_string()]);
  let _runtime = install_runtime(&mut vm, options);
  assert_eq!(vm.get_global("Blitz"), None);
}

#[test]
fn effect_root_is_single_owned_interruptible_and_finalized() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let runtime = install_runtime(
    &mut vm,
    RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
  );
  let directory = env::temp_dir().join(format!("blitz-effect-root-{}", std::process::id()));
  std::fs::create_dir_all(&directory).expect("create Effect root directory");
  let entry = directory.join("entry.mjs");
  std::fs::write(
    &entry,
    denko::js!(
      r#"
        import * as Effect from "effect/Effect";
        import * as Runtime from "blitz:runtime";

        globalThis.rootFinalized = false;
        globalThis.secondRootStarted = false;
        globalThis.secondRootRejected = false;
        Runtime.runMain(
          Effect.ensuring(
            Effect.never,
            Effect.sync(() => { rootFinalized = true; })
          ),
          { disableSignalHandlers: true }
        );
        try {
          Runtime.runMain(Effect.sync(() => { secondRootStarted = true; }));
        } catch {
          secondRootRejected = true;
        }
      "#
    ),
  )
  .expect("write Effect root entry");
  let loader = vm.module_loader_handle();
  {
    let mut loader = loader.borrow_mut();
    loader
      .construct(&directory, "./entry.mjs")
      .expect("construct Effect root graph");
    loader
      .instantiate_all()
      .expect("instantiate Effect root graph");
  }
  denko::module::evaluate_entry_shared(&loader, &entry, &mut vm, false)
    .expect("evaluate Effect root entry");

  assert!(runtime.is_live());
  assert_eq!(vm.get_global("secondRootRejected"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("secondRootStarted"), Some(Value::Bool(false)));
  assert!(
    runtime
      .interrupt_root(&mut vm, 130)
      .expect("interrupt Effect root")
  );
  vm.flush_microtasks().expect("settle Effect root");
  assert_eq!(vm.get_global("rootFinalized"), Some(Value::Bool(true)));
  assert_eq!(runtime.exit_status(), Some(130));
  assert!(!runtime.is_live());
  std::fs::remove_dir_all(directory).expect("remove Effect root directory");
}

#[test]
fn second_runtime_installation_is_rejected_before_side_effects() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let options = RuntimeOptions::from_host(vec!["blitz-test".to_string()]);
  let _runtime = install_runtime(&mut vm, options.clone());
  let result = try_install_runtime(&mut vm, options);
  assert!(matches!(result, Err(RuntimeInstallError::AlreadyInstalled)));
}

#[test]
fn private_host_bridge_is_not_exposed_on_the_global_object() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(Rc::clone(&globals));
  let _runtime = install_runtime(
    &mut vm,
    RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
  );
  assert_eq!(vm.get_global("__blitz_host_v1"), None);
  assert!(
    !globals
      .borrow()
      .properties
      .contains_key(&PropertyKey::String("__blitz_host_v1".to_string().into()))
  );
}

#[test]
fn public_host_callables_have_function_metadata() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let _runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz-test".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz-test".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: true,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = denko::js!(
    r#"
      const functions = [
        [Response, "Response", 0],
        [Headers, "Headers", 0],
        [WebSocket, "WebSocket", 1],
        [URL, "URL", 1],
        [AbortSignal, "AbortSignal", 0],
        [AbortSignal.abort, "abort", 0],
        [AbortController, "AbortController", 0],
        [AbortController.prototype.abort, "abort", 0],
        [setTimeout, "setTimeout", 1],
        [setInterval, "setInterval", 1],
        [clearTimeout, "clearTimeout", 1],
        [clearInterval, "clearInterval", 1],
        [performance.now, "now", 0],
        [process.cwd, "cwd", 0],
        [Blitz.serve, "serve", 1],
        [Blitz.upgradeWebSocket, "upgradeWebSocket", 1],
        [Blitz.stdin.onLine, "onLine", 1],
        [Blitz.stdin.onClose, "onClose", 1],
        [Blitz.stdin.setPrompt, "setPrompt", 1],
        [Blitz.stdin.close, "close", 0],
        [Blitz.stdout.write, "write", 1],
        [Blitz.stdout.writeLine, "writeLine", 1]
      ];
      const consoleNames = [
        "log", "info", "debug", "dir", "dirxml", "table", "warn", "error",
        "trace", "assert", "count", "countReset", "time", "timeLog",
        "timeEnd", "group", "groupCollapsed", "groupEnd", "clear"
      ];
      globalThis.hostFunctionMetadataOk =
        functions.every(([fn, name, length]) =>
          fn.name === name &&
          fn.length === length &&
          Object.getPrototypeOf(fn) === Function.prototype
        ) &&
        consoleNames.every((name) =>
          console[name].name === name &&
          console[name].length === 0 &&
          Object.getPrototypeOf(console[name]) === Function.prototype
        );
    "#
  );
  let session = JsParser::parse_source(source).expect("parse host metadata script");
  let chunk = compiler::compile_session(&session).expect("compile metadata script");
  vm.run(chunk)
    .expect("evaluate host metadata script");

  assert_eq!(
    vm.get_global("hostFunctionMetadataOk"),
    Some(Value::Bool(true))
  );
  let global = vm.get_global("global").expect("global alias");
  assert!(!denko::inspect::inspect(&global).contains("[Native Function]"));
}

#[test]
fn runtime_handle_tracks_additional_resource_liveness() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: false,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  assert!(!runtime.is_live());
  let resource = runtime.register_resource("test-resource", true);
  assert!(runtime.is_live());
  resource.unreference();
  assert!(!runtime.is_live());
  assert!(resource.close());
}

#[test]
fn timer_globals_dispatch_callbacks_and_track_liveness() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let mut runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: false,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = denko::js!(
    r#"
      globalThis.timerResult = 0;
      globalThis.clockIsMonotonic = performance.now() >= 0;
      const cancelled = setInterval(() => { timerResult = -1; }, 1000);
      clearInterval(cancelled);
      clearInterval(cancelled);
      setTimeout((left, right) => { timerResult = left + right; }, 1, 2, 3);
    "#
  );
  let session = JsParser::parse_source(source).expect("parse timer script");
  let chunk = compiler::compile_session(&session).expect("compile timer script");
  vm.run(chunk).expect("evaluate timer script");

  assert!(runtime.is_live());
  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch timer callback")
  );
  assert_eq!(vm.get_global("timerResult"), Some(Value::Number(5.0)));
  assert_eq!(vm.get_global("clockIsMonotonic"), Some(Value::Bool(true)));
  assert!(!runtime.is_live());
}

#[test]
fn blitz_serve_returns_a_lifecycle_server_handle() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: true,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = denko::js!(
    r#"
      globalThis.serverFinished = false;
      globalThis.server = Blitz.serve(
        { hostname: "127.0.0.1", port: 0 },
        () => new Response("ok")
      );
      server.finished.then(() => { serverFinished = true; });
      globalThis.serverShape =
        server.addr.transport === "tcp" &&
        server.addr.hostname === server.hostname &&
        server.addr.port === server.port &&
        typeof server.shutdown === "function" &&
        typeof server.ref === "function" &&
        typeof server.unref === "function";
    "#
  );
  let session = JsParser::parse_source(source).expect("parse server handle script");
  let chunk = compiler::compile_session(&session).expect("compile server handle script");
  vm.run(chunk)
    .expect("evaluate server handle script");
  let address = runtime
    .local_addr()
    .expect("bound server address");
  assert_eq!(vm.get_global("serverShape"), Some(Value::Bool(true)));
  assert!(runtime.is_live());

  let source = denko::js!(
    r#"
      server.unref();
      globalThis.unrefCalled = true;
    "#
  );
  let session = JsParser::parse_source(source).expect("parse server unref script");
  let chunk = compiler::compile_session(&session).expect("compile unref script");
  vm.run(chunk).expect("unref server");
  assert_eq!(vm.get_global("unrefCalled"), Some(Value::Bool(true)));
  assert!(!runtime.is_live());

  let source = denko::js!(
    r#"
      server.ref();
      globalThis.shutdownIdentity = server.shutdown() === server.finished;
      server.shutdown();
    "#
  );
  let session = JsParser::parse_source(source).expect("parse server shutdown script");
  let chunk = compiler::compile_session(&session).expect("compile shutdown script");
  vm.run(chunk).expect("shut down server");
  vm.flush_microtasks()
    .expect("settle server completion");
  assert_eq!(vm.get_global("shutdownIdentity"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("serverFinished"), Some(Value::Bool(true)));
  assert!(!runtime.is_live());

  let rebound = std::net::TcpListener::bind(address).expect("released server port can be rebound");
  drop(rebound);
}

#[test]
fn deno_server_lifecycle_fixture_matches_blitz() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: true,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = format!(
    "{}\nobserveServerLifecycle(Blitz).then((result) => {{\n\
       globalThis.compatTransport = result.transport;\n\
       globalThis.compatPort = result.portIsAssigned;\n\
       globalThis.compatShutdown = result.shutdownReturnsPromise;\n\
       globalThis.compatFinished = result.finishedSettled;\n\
       }});",
    include_str!("../../compat/fixtures/server-lifecycle.js")
  );
  let session = JsParser::parse_source(&source).expect("parse Deno compatibility fixture");
  let chunk = compiler::compile_session(&session).expect("compile compatibility fixture");
  vm.run(chunk)
    .expect("evaluate compatibility fixture");
  vm.flush_microtasks()
    .expect("settle compatibility fixture");

  assert_eq!(
    vm.get_global("compatTransport"),
    Some(Value::String("tcp".to_string().into()))
  );
  assert_eq!(vm.get_global("compatPort"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("compatShutdown"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("compatFinished"), Some(Value::Bool(true)));
  assert!(!runtime.is_live());
}

#[test]
fn effect_http_serves_an_effect_handler_without_blitz_globals() {
  thread::Builder::new()
    .name("effect-http-host".to_string())
    .stack_size(16 * 1024 * 1024)
    .spawn(|| {
      let globals = Rc::new(RefCell::new(ObjectData::new()));
      let mut vm = Vm::new(globals);
      let mut runtime = install_runtime(
        &mut vm,
        RuntimeOptions {
          argv: vec!["blitz".to_string()],
          env: BTreeMap::new(),
          exec_path: "blitz".to_string(),
          listen_addr: "127.0.0.1:0"
            .parse()
            .expect("test listen address"),
          install_compatibility_globals: false,
          node_compat: NodeCompatProfile::Core,
          conditions: Vec::new(),
        },
      );
      let directory = env::temp_dir().join(format!("blitz-effect-http-{}", std::process::id()));
      std::fs::create_dir_all(&directory).expect("create Effect HTTP directory");
      let entry = directory.join("entry.mjs");
      std::fs::write(
        &entry,
        denko::js!(
          r#"
        import * as Effect from "effect/Effect";
        import * as Cause from "effect/Cause";
        import * as Http from "blitz:http";
        import * as Runtime from "blitz:runtime";

        globalThis.effectHttpHandlerCancelled = false;
        Runtime.runMain(Runtime.provideLive(Http.serve({
          hostname: "127.0.0.1",
          port: 0,
          handler: (request) => request.url === "http://localhost/slow"
            ? Effect.ensuring(
                Effect.never,
                Effect.sync(() => { effectHttpHandlerCancelled = true; })
              )
            : Effect.sleep("1 millis").pipe(
                Effect.as(new Response("effect-http"))
              )
        })), {
          reportFailure: (cause) => {
            const failure = Cause.failureOption(cause);
            if (failure._tag === "Some") {
              globalThis.effectHttpFailureTag = failure.value?._tag;
              globalThis.effectHttpFailureReason = failure.value?.reason;
              globalThis.effectHttpFailureMessage = failure.value?.message;
            }
          }
        });
      "#
        ),
      )
      .expect("write Effect HTTP entry");
      let loader = vm.module_loader_handle();
      {
        let mut loader = loader.borrow_mut();
        loader
          .construct(&directory, "./entry.mjs")
          .expect("construct Effect HTTP graph");
        loader
          .instantiate_all()
          .expect("instantiate Effect HTTP graph");
      }
      denko::module::evaluate_entry_shared(&loader, &entry, &mut vm, false)
        .expect("evaluate Effect HTTP entry");
      assert_eq!(vm.get_global("Blitz"), None);

      let address = runtime.local_addr().unwrap_or_else(|| {
        panic!(
          "Effect HTTP listener address: tag={:?} reason={:?} message={:?}",
          vm.get_global("effectHttpFailureTag"),
          vm.get_global("effectHttpFailureReason"),
          vm.get_global("effectHttpFailureMessage"),
        )
      });
      let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).expect("connect Effect HTTP client");
        stream
          .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
          .expect("write Effect HTTP request");
        let mut response = String::new();
        stream
          .read_to_string(&mut response)
          .expect("read Effect HTTP response");
        response
      });
      assert!(
        runtime
          .run_once(&mut vm)
          .expect("dispatch Effect HTTP request")
      );
      assert!(
        runtime
          .run_once(&mut vm)
          .expect("dispatch asynchronous Effect HTTP handler")
      );
      let response = client.join().expect("Effect HTTP client thread");
      assert!(response.ends_with("effect-http"), "{response}");

      let slow_client = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).expect("connect slow Effect HTTP client");
        stream
          .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
          .expect("write slow Effect HTTP request");
        let mut response = String::new();
        stream
          .read_to_string(&mut response)
          .expect("read cancelled Effect HTTP response");
        response
      });
      assert!(
        runtime
          .run_once(&mut vm)
          .expect("dispatch pending Effect HTTP handler")
      );
      runtime
        .shutdown(&mut vm)
        .expect("shutdown Effect HTTP server");
      vm.flush_microtasks()
        .expect("settle cancelled Effect HTTP handler");
      let slow_response = slow_client
        .join()
        .expect("slow Effect HTTP client thread");
      assert!(slow_response.starts_with("HTTP/1.1 503"), "{slow_response}");
      assert_eq!(
        vm.get_global("effectHttpHandlerCancelled"),
        Some(Value::Bool(true))
      );
      assert_eq!(runtime.exit_status(), Some(1));
      std::fs::remove_dir_all(directory).expect("remove Effect HTTP directory");
    })
    .expect("spawn Effect HTTP host thread")
    .join()
    .expect("Effect HTTP host thread succeeds");
}

#[test]
fn blitz_serve_dispatches_deno_style_callback_over_http() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let mut runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: true,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = denko::js!(
    r#"
      async function callbackFunc(request, info) {
        const body = await request.text();
        const parsed = await new Response('{"value":"json"}').json();
        const headers = new Headers({ "x-blitz-response": "present" });
        headers.append("x-blitz-response", "appended");
        return new Response(
          request.method + " " + request.url + " " +
            request.headers.get("x-blitz-test") + " " + info.remoteAddr.transport + " " +
            body + " " + parsed.value,
          { status: 201, headers: headers }
        );
      }
      Blitz.serve({ hostname: "127.0.0.1", port: 0 }, callbackFunc);
    "#
  );
  let session = JsParser::parse_source(source).expect("parse server script");
  let chunk = compiler::compile_session(&session).expect("compile server script");
  vm.run(chunk).expect("evaluate server script");

  let address = runtime.local_addr().expect("bound HTTP address");
  let client = thread::spawn(move || {
    let mut stream = TcpStream::connect(address).expect("connect HTTP client");
    stream
        .write_all(
          b"POST /hello?from=test HTTP/1.1\r\nHost: localhost\r\nX-Blitz-Test: present\r\nContent-Length: 7\r\nConnection: close\r\n\r\npayload",
        )
        .expect("write HTTP request");
    let mut response = String::new();
    stream
      .read_to_string(&mut response)
      .expect("read HTTP response");
    response
  });

  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch HTTP request")
  );
  let response = client.join().expect("HTTP client thread");
  assert!(response.starts_with("HTTP/1.1 201 Created"), "{response}");
  assert!(
    response
      .to_ascii_lowercase()
      .contains("x-blitz-response: present, appended"),
    "{response}"
  );
  assert!(
    response.ends_with("POST http://localhost/hello?from=test present tcp payload json"),
    "{response}"
  );
  runtime
    .shutdown(&mut vm)
    .expect("stop HTTP server");
}

#[test]
fn blitz_upgrade_websocket_exchanges_events_and_messages() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let mut runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: true,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = denko::js!(
    r#"
      function handler(request) {
        if (request.headers.get("upgrade") !== "websocket") {
          return new Response("upgrade required", { status: 426 });
        }
        const upgraded = Blitz.upgradeWebSocket(request);
        globalThis.serverSocketHasWebSocketPrototype =
          Object.getPrototypeOf(upgraded.socket) === WebSocket.prototype &&
          upgraded.socket instanceof WebSocket;
        upgraded.socket.addEventListener("open", function() {
          upgraded.socket.send("ready");
        });
        upgraded.socket.onmessage = function(event) {
          upgraded.socket.send("echo:" + event.data);
        };
        return upgraded.response;
      }
      Blitz.serve(handler);
    "#
  );
  let session = JsParser::parse_source(source).expect("parse WebSocket server script");
  let chunk = compiler::compile_session(&session).expect("compile WebSocket server script");
  vm.run(chunk)
    .expect("evaluate WebSocket server script");

  let address = runtime.local_addr().expect("bound HTTP address");
  let client = thread::spawn(move || {
    tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("client runtime")
      .block_on(async move {
        let stream = tokio::net::TcpStream::connect(address)
          .await
          .expect("connect WebSocket client");
        let request = Request::builder()
          .method("GET")
          .uri(format!("http://{address}/socket"))
          .header("Host", address.to_string())
          .header(UPGRADE, "websocket")
          .header(CONNECTION, "upgrade")
          .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
          )
          .header("Sec-WebSocket-Version", "13")
          .body(Empty::<Bytes>::new())
          .expect("WebSocket request");
        let (mut socket, response): (fastwebsockets::WebSocket<TokioIo<Upgraded>>, _) =
          fastwebsockets::handshake::client(&SpawnExecutor, request, stream)
            .await
            .expect("WebSocket handshake");
        assert_eq!(response.status(), hyper::StatusCode::SWITCHING_PROTOCOLS);
        let ready = socket
          .read_frame()
          .await
          .expect("read ready frame");
        socket
          .write_frame(Frame::text(Payload::Owned(b"hello".to_vec())))
          .await
          .expect("write client message");
        let echo = socket
          .read_frame()
          .await
          .expect("read echo frame");
        socket
          .write_frame(Frame::close(1000, b"done"))
          .await
          .expect("write close frame");
        (ready, echo)
      })
  });

  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch upgrade request")
  );
  assert_eq!(
    vm.get_global("serverSocketHasWebSocketPrototype"),
    Some(Value::Bool(true))
  );
  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch open event")
  );
  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch ready drain")
  );
  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch message event")
  );
  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch echo drain")
  );
  assert!(
    runtime
      .run_once(&mut vm)
      .expect("dispatch close event")
  );
  let (ready, echo) = client.join().expect("WebSocket client thread");
  assert_eq!(ready.opcode, OpCode::Text);
  assert_eq!(ready.payload.as_ref(), b"ready");
  assert_eq!(echo.opcode, OpCode::Text);
  assert_eq!(echo.payload.as_ref(), b"echo:hello");
  runtime
    .shutdown(&mut vm)
    .expect("stop HTTP server");
}

#[test]
fn websocket_constructor_connects_to_blitz_server() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let mut runtime = install_runtime(
    &mut vm,
    RuntimeOptions {
      argv: vec!["blitz".to_string()],
      env: BTreeMap::new(),
      exec_path: "blitz".to_string(),
      listen_addr: "127.0.0.1:0"
        .parse()
        .expect("test listen address"),
      install_compatibility_globals: true,
      node_compat: NodeCompatProfile::Core,
      conditions: Vec::new(),
    },
  );
  let source = denko::js!(
    r#"
      const server = Blitz.serve((request) => {
        const upgraded = Blitz.upgradeWebSocket(request);
        upgraded.socket.onmessage = (event) => upgraded.socket.send("echo:" + event.data);
        return upgraded.response;
      });
      const client = new WebSocket("ws://127.0.0.1:" + server.port + "/chat");
      globalThis.clientSocketHasWebSocketPrototype =
        Object.getPrototypeOf(client) === WebSocket.prototype &&
        client instanceof WebSocket;
      client.onopen = () => client.send("hello");
      client.onmessage = (event) => {
        globalThis.clientResult = event.data;
        client.close(1000, "done");
      };
    "#
  );
  let session = JsParser::parse_source(source).expect("parse WebSocket client script");
  let chunk = compiler::compile_session(&session).expect("compile WebSocket client script");
  vm.run(chunk)
    .expect("evaluate WebSocket client script");
  assert_eq!(
    vm.get_global("clientSocketHasWebSocketPrototype"),
    Some(Value::Bool(true))
  );

  for _step in 0..16 {
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("drive client event")
    );
    if vm.get_global("clientResult") == Some(Value::String("echo:hello".to_string().into())) {
      break;
    }
  }
  assert_eq!(
    vm.get_global("clientResult"),
    Some(Value::String("echo:hello".to_string().into()))
  );
  runtime
    .shutdown(&mut vm)
    .expect("stop client and server");
}

#[test]
fn effect_irc_chat_v2_scripts_parse_and_compile() {
  let server = include_str!("../../../../examples/irc-chat-v2/server.ts");
  let session =
    JsParser::parse_source_with_type(server, oxc_span::SourceType::ts().with_module(true))
      .expect("parse Effect IRC server module");
  compiler::compile_session(&session).expect("compile Effect IRC server module");

  for (name, source) in [
    (
      "alice",
      include_str!("../../../../examples/irc-chat-v2/alice.ts"),
    ),
    (
      "bob",
      include_str!("../../../../examples/irc-chat-v2/bob.ts"),
    ),
  ] {
    let session =
      JsParser::parse_source_with_type(source, oxc_span::SourceType::ts().with_module(true))
        .unwrap_or_else(|error| panic!("parse Effect IRC {name} client: {error}"));
    compiler::compile_session(&session)
      .unwrap_or_else(|error| panic!("compile Effect IRC {name} client: {error}"));
  }
}

#[test]
fn effect_irc_chat_v2_server_runs_join_and_message_protocol() {
  thread::Builder::new()
    .name("effect-irc-v2".to_string())
    .stack_size(16 * 1024 * 1024)
    .spawn(|| {
      let globals = Rc::new(RefCell::new(ObjectData::new()));
      let mut vm = Vm::new(globals);
      let mut runtime = install_runtime(
        &mut vm,
        RuntimeOptions {
          argv: vec!["blitz".to_string()],
          env: BTreeMap::new(),
          exec_path: "blitz".to_string(),
          listen_addr: "127.0.0.1:0"
            .parse()
            .expect("test listen address"),
          install_compatibility_globals: false,
          node_compat: NodeCompatProfile::Core,
          conditions: Vec::new(),
        },
      );
      let source =
        include_str!("../../../../examples/irc-chat-v2/server.ts").replace("port: 8000", "port: 0");
      let directory = env::temp_dir().join(format!("blitz-effect-irc-v2-{}", std::process::id()));
      std::fs::create_dir_all(&directory).expect("create Effect IRC directory");
      let entry = directory.join("server.ts");
      std::fs::write(&entry, source).expect("write Effect IRC server");
      let loader = vm.module_loader_handle();
      {
        let mut loader = loader.borrow_mut();
        loader
          .construct(&directory, "./server.ts")
          .expect("construct Effect IRC graph");
        loader
          .instantiate_all()
          .expect("instantiate Effect IRC graph");
      }
      denko::module::evaluate_entry_shared(&loader, &entry, &mut vm, false)
        .expect("evaluate Effect IRC server");
      assert_eq!(vm.get_global("Blitz"), None);

      let address = runtime.local_addr().expect("IRC server address");
      let wrong_route = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).expect("connect to IRC HTTP server");
        write!(
          stream,
          "GET /wrong HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )
        .expect("write wrong-route request");
        let mut response = String::new();
        stream
          .read_to_string(&mut response)
          .expect("read wrong-route response");
        response
      });
      assert!(
        runtime
          .run_once(&mut vm)
          .expect("dispatch wrong-route request")
      );
      let wrong_route = wrong_route
        .join()
        .expect("wrong-route client thread");
      assert!(
        wrong_route.starts_with("HTTP/1.1 404"),
        "unexpected wrong-route response: {wrong_route}"
      );

      let client = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
          .enable_all()
          .build()
          .expect("IRC client runtime")
          .block_on(async move {
            async fn connect(address: SocketAddr) -> fastwebsockets::WebSocket<TokioIo<Upgraded>> {
              let stream = tokio::net::TcpStream::connect(address)
                .await
                .expect("connect IRC client");
              let request = Request::builder()
                .method("GET")
                .uri(format!("http://{address}/chat"))
                .header("Host", address.to_string())
                .header(UPGRADE, "websocket")
                .header(CONNECTION, "upgrade")
                .header(
                  "Sec-WebSocket-Key",
                  fastwebsockets::handshake::generate_key(),
                )
                .header("Sec-WebSocket-Version", "13")
                .body(Empty::<Bytes>::new())
                .expect("IRC upgrade request");
              fastwebsockets::handshake::client(&SpawnExecutor, request, stream)
                .await
                .expect("IRC handshake")
                .0
            }

            let mut alice = connect(address).await;
            alice
              .write_frame(Frame::text(Payload::Owned(b"JOIN Alice".to_vec())))
              .await
              .expect("send Alice join");
            let alice_welcome = alice
              .read_frame()
              .await
              .expect("read Alice welcome");
            let alice_join = alice.read_frame().await.expect("read Alice join");

            let mut bob = connect(address).await;
            bob
              .write_frame(Frame::text(Payload::Owned(b"JOIN Bob".to_vec())))
              .await
              .expect("send Bob join");
            let bob_welcome = bob.read_frame().await.expect("read Bob welcome");
            let alice_sees_bob = alice
              .read_frame()
              .await
              .expect("read Bob join on Alice");
            let bob_join = bob
              .read_frame()
              .await
              .expect("read Bob join on Bob");

            alice
              .write_frame(Frame::text(Payload::Owned(b"MSG hello".to_vec())))
              .await
              .expect("send Alice message");
            let alice_message = alice
              .read_frame()
              .await
              .expect("read Alice message on Alice");
            let bob_message = bob
              .read_frame()
              .await
              .expect("read Alice message on Bob");
            alice
              .write_frame(Frame::close(1000, b"done"))
              .await
              .expect("close Alice");
            bob
              .write_frame(Frame::close(1000, b"done"))
              .await
              .expect("close Bob");
            (
              alice_welcome,
              alice_join,
              bob_welcome,
              alice_sees_bob,
              bob_join,
              alice_message,
              bob_message,
            )
          })
      });

      let mut event_count = 0;
      while !client.is_finished() {
        assert!(
          runtime
            .run_once(&mut vm)
            .expect("drive IRC event")
        );
        event_count += 1;
        assert!(event_count < 100, "IRC protocol did not quiesce");
      }
      let (
        alice_welcome,
        alice_join,
        bob_welcome,
        alice_sees_bob,
        bob_join,
        alice_message,
        bob_message,
      ) = client.join().expect("IRC client thread");
      assert_eq!(
        alice_welcome.payload.as_ref(),
        b"* Welcome, Alice. Type /quit to leave."
      );
      assert_eq!(
        bob_welcome.payload.as_ref(),
        b"* Welcome, Bob. Type /quit to leave."
      );
      assert_eq!(alice_join.payload.as_ref(), b"* Alice joined the room");
      assert_eq!(alice_sees_bob.payload.as_ref(), b"* Bob joined the room");
      assert_eq!(bob_join.payload.as_ref(), b"* Bob joined the room");
      assert_eq!(alice_message.payload.as_ref(), b"<Alice> hello");
      assert_eq!(bob_message.payload.as_ref(), b"<Alice> hello");
      runtime
        .shutdown(&mut vm)
        .expect("stop IRC server");
      std::fs::remove_dir_all(directory).expect("remove Effect IRC directory");
    })
    .expect("spawn Effect IRC thread")
    .join()
    .expect("Effect IRC thread succeeds");
}
