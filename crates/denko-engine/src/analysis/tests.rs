// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn temporary_directory(label: &str) -> PathBuf {
  static NEXT: AtomicUsize = AtomicUsize::new(0);
  let directory = std::env::temp_dir().join(format!(
    "blitz-analysis-{label}-{}-{}",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  ));
  std::fs::create_dir_all(&directory).expect("create analysis fixture");
  directory
}

#[test]
fn reports_declarations_and_runtime_globals() {
  let result = analyze(
    crate::js!(
      typescript,
      "interface Answer { value: number }\nconst answer: Answer = { value: 42 };\nfunction read() { return answer; }"
    ),
    Path::new("example.ts"),
    false,
  );

  assert!(result.diagnostics.is_empty());
  assert_eq!(
    result
      .declarations
      .iter()
      .map(|declaration| declaration.name.as_str())
      .collect::<Vec<_>>(),
    ["Answer", "answer", "read"]
  );
  assert!(
    result
      .runtime_globals
      .iter()
      .any(|global| global.name == "Blitz")
  );
}

#[test]
fn reports_nested_declarations() {
  let result = analyze(
    crate::js!(
      typescript,
      "class Service { value: number; read() { const local = 1; return local; } }"
    ),
    Path::new("symbols.ts"),
    false,
  );

  let service = &result.declarations[0];
  assert_eq!(service.name, "Service");
  assert_eq!(service.kind, "class");
  assert_eq!(
    service
      .children
      .iter()
      .map(|declaration| (declaration.name.as_str(), declaration.kind))
      .collect::<Vec<_>>(),
    [("value", "property"), ("read", "method")]
  );
  assert_eq!(service.children[1].children[0].name, "local");
  assert!(service.start < service.selection_start);
  assert!(service.end > service.selection_end);

  let object = analyze(
    crate::js!(
      "const query = define({ handler: (context) => { const user = context.user; return user; } });"
    ),
    Path::new("object-symbols.ts"),
    false,
  );
  let query = &object.declarations[0];
  assert_eq!(query.children[0].name, "handler");
  assert_eq!(query.children[0].kind, "method");
  assert_eq!(query.children[0].children[0].name, "user");
}

#[test]
fn resolves_shadowed_references_by_symbol_identity() {
  let source =
    crate::js!("const value = 1; function read() { const value = 2; return value; } value;");
  let result = analyze(source, Path::new("definitions.ts"), false);
  let outer_declaration = source.find("value").expect("outer declaration");
  let inner_declaration = source[outer_declaration + 1..]
    .find("value")
    .map(|offset| outer_declaration + 1 + offset)
    .expect("inner declaration");
  let inner_reference = source.rfind("value; }").expect("inner reference");
  let outer_reference = source.rfind("value;").expect("outer reference");

  assert!(result.definitions.iter().any(|definition| {
    definition.start == inner_reference && definition.target_start == inner_declaration
  }));
  assert!(result.definitions.iter().any(|definition| {
    definition.start == outer_reference && definition.target_start == outer_declaration
  }));
}

#[test]
fn reports_contextual_callback_parameter_inlay_hints() {
  let declaration = crate::js!(
    typescript,
    r#"
      export interface HandlerContext { readonly userId: string }
      export declare const configure: (options: {
        handler: (context: HandlerContext) => void
      }) => void;
    "#
  );
  let source = crate::js!(
    typescript,
    r#"
      import { configure } from "example/context";
      configure({ handler: (context) => context.userId });
    "#
  );
  let mut project = AnalysisProject::with_module_declarations([(
    "example/context".to_string(),
    declaration.to_string(),
  )]);
  let result = project.analyze_document(source.to_string(), Path::new("editor.ts"), false);

  assert_eq!(
    result
      .inlay_hints
      .iter()
      .map(|hint| (hint.label.as_str(), hint.kind))
      .collect::<Vec<_>>(),
    [("options:", "parameter"), (": { userId: string }", "type")]
  );
}

#[test]
fn serializes_selected_argument_parameter_name_hints() {
  let result = analyze(
    crate::js!(
      typescript,
      "declare function send(message: string): void; declare const input: string; send(input);"
    ),
    Path::new("parameter-hints.ts"),
    false,
  );

  assert_eq!(result.inlay_hints.len(), 1);
  assert_eq!(result.inlay_hints[0].label, "message:");
  assert_eq!(result.inlay_hints[0].kind, "parameter");
}

#[test]
fn carries_module_documentation_to_imported_definition_hovers() {
  let declaration = crate::js!(
    typescript,
    r#"
      export declare const succeed: <Value>(value: Value) => Value;
    "#
  );
  let source = crate::js!(
    typescript,
    r#"
      import * as Effect from "effect/Effect";
      Effect.succeed(42);
    "#
  );
  let mut project = AnalysisProject::with_module_declarations([(
    "effect/Effect".to_string(),
    declaration.to_string(),
  )])
  .with_module_documentation([(
    "effect/Effect".to_string(),
    "succeed".to_string(),
    "Creates an Effect that always succeeds.\n@category Creating Effects".to_string(),
  )]);
  let result = project.analyze_document(source.to_string(), Path::new("editor.ts"), false);
  let start = source.find("succeed").expect("succeed reference");
  let definition = result
    .definitions
    .iter()
    .find(|definition| definition.start == start)
    .expect("succeed definition");

  assert_eq!(
    definition.documentation.as_deref(),
    Some("Creates an Effect that always succeeds.\n@category Creating Effects")
  );
}

#[test]
fn resolves_members_from_imported_module_declarations() {
  let declaration = crate::js!(
    typescript,
    r#"
      import type * as Effect from "effect/Effect";
      export interface Connection {
        readonly send: (message: string) => Effect.Effect<void>;
        readonly close: () => Effect.Effect<void>;
      }
      export declare const connect: () => Effect.Effect<Connection>;
    "#
  );
  let effect_declaration = crate::js!(
    typescript,
    r#"
      export interface Effect<Value> {
        readonly pipe: { <Result>(f: (value: Value) => Result): Result };
      }
    "#
  );
  let source = crate::js!(
    typescript,
    r#"
      import * as WebSocket from "blitz:websocket";
      const transmit = (connection: WebSocket.Connection) => connection.send("hello").pipe();
      WebSocket.connect().pipe();
    "#
  );
  let mut project = AnalysisProject::with_module_declarations([
    ("blitz:websocket".to_string(), declaration.to_string()),
    ("effect/Effect".to_string(), effect_declaration.to_string()),
  ]);
  let result = project.analyze_document(source.to_string(), Path::new("editor.ts"), false);
  let start = source.rfind("send").unwrap();
  let definition = result
    .definitions
    .iter()
    .find(|definition| definition.start == start)
    .unwrap();

  assert_eq!(
    definition.target_virtual.as_deref(),
    Some("blitz_websocket.d.ts")
  );
  assert!(
    definition
      .detail
      .as_deref()
      .unwrap()
      .contains("send")
  );
  assert!(
    result
      .virtual_documents
      .iter()
      .any(|document| document.id == "blitz_websocket.d.ts")
  );
  assert_eq!(
    result
      .definitions
      .iter()
      .filter(|definition| {
        definition.target_virtual.as_deref() == Some("effect_Effect.d.ts")
          && definition
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("pipe"))
      })
      .count(),
    2
  );
}

#[test]
fn discovers_package_json_declarations_for_contextual_typing() {
  let directory = temporary_directory("package");
  let source_directory = directory.join("src");
  let types_directory = directory
    .join("node_modules")
    .join("fixture-types");
  let legacy_directory = directory
    .join("node_modules")
    .join("fixture-typings");
  let source_package_directory = directory
    .join("node_modules")
    .join("fixture-source");
  std::fs::create_dir_all(&source_directory).expect("create source directory");
  std::fs::create_dir_all(types_directory.join("types")).expect("create types directory");
  std::fs::create_dir_all(&legacy_directory).expect("create legacy package directory");
  std::fs::create_dir_all(&source_package_directory).expect("create source package directory");
  std::fs::write(
    types_directory.join("package.json"),
    r#"{
        "name": "fixture-types",
        "types": "./types/index.d.ts",
        "exports": {
          ".": {
            "import": "./index.js",
            "types": "./types/index.d.ts"
          }
        }
      }"#,
  )
  .expect("write types package manifest");
  std::fs::write(
    types_directory.join("index.js"),
    crate::js!(module, "export function configure() {};"),
  )
  .expect("write runtime entry");
  std::fs::write(
      types_directory.join("types/index.d.ts"),
      crate::js!(
        typescript,
        "/** Configures the fixture handler. */\nexport declare const configure: (options: { handler: (context: { userId: string }) => void }) => void;"
      ),
    )
    .expect("write package declaration");
  std::fs::write(
    legacy_directory.join("package.json"),
    r#"{ "name": "fixture-typings", "typings": "./index.d.ts" }"#,
  )
  .expect("write legacy package manifest");
  std::fs::write(
      legacy_directory.join("index.d.ts"),
      crate::js!(
        typescript,
        "export declare const create: (options: { handler: (context: { name: string }) => void }) => void;"
      ),
    )
    .expect("write legacy declaration");
  std::fs::write(
    source_package_directory.join("package.json"),
    r#"{ "type": "module", "exports": "./index.ts" }"#,
  )
  .expect("write source package manifest");
  std::fs::write(
      source_package_directory.join("index.ts"),
      crate::js!(
        typescript,
        "/** Returns the installed source answer. */\nexport function sourceAnswer(): number { return 42; }"
      ),
    )
    .expect("write source package body");

  let path = source_directory.join("editor.ts");
  let source = crate::js!(
    typescript,
    r#"
      import { configure } from "fixture-types";
      import { create } from "fixture-typings";
      import { sourceAnswer } from "fixture-source";
      configure({ handler: (context) => context.userId });
      create({ handler: (context) => context.name });
      sourceAnswer();
    "#
  );
  let mut project = AnalysisProject::default();
  let result = project.analyze_document(source.to_string(), &path, false);

  assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
  assert_eq!(
    result
      .inlay_hints
      .iter()
      .filter(|hint| hint.kind == "type")
      .map(|hint| hint.label.as_str())
      .collect::<Vec<_>>(),
    [": { userId: string }", ": { name: string }"]
  );
  assert!(
    result
      .virtual_documents
      .iter()
      .any(|document| { document.id == "fixture-types.d.ts" && document.text.contains("userId") })
  );
  assert!(
    result
      .virtual_documents
      .iter()
      .any(|document| { document.id == "fixture-typings.d.ts" && document.text.contains("name") })
  );
  let configure_start = source
    .find("configure")
    .expect("configure import");
  assert!(
    result.definitions.iter().any(|definition| {
      definition.start == configure_start
        && definition.target_path.as_deref()
          == Some(
            normalize_path(&types_directory.join("types/index.d.ts"))
              .to_string_lossy()
              .as_ref(),
          )
        && definition.documentation.as_deref() == Some("Configures the fixture handler.")
    }),
    "{:?}",
    result.definitions
  );
  let source_answer_start = source
    .find("sourceAnswer")
    .expect("source import");
  assert!(result.definitions.iter().any(|definition| {
    definition.start == source_answer_start
      && definition.target_path.as_deref()
        == Some(
          normalize_path(&source_package_directory.join("index.ts"))
            .to_string_lossy()
            .as_ref(),
        )
      && definition.documentation.as_deref() == Some("Returns the installed source answer.")
      && definition.target_end > definition.target_start
  }));
  assert_eq!(project.declaration_cache.len(), 3);

  std::fs::remove_dir_all(directory).expect("remove analysis fixture");
}

#[test]
fn discovers_relative_dts_entries_and_targets_the_source_file() {
  let directory = temporary_directory("relative-dts");
  let source_directory = directory.join("src");
  std::fs::create_dir_all(&source_directory).expect("create source directory");
  let declaration = source_directory.join("nearby.d.ts");
  std::fs::write(
      &declaration,
      crate::js!(
        typescript,
        "export declare const configure: (options: { handler: (context: { value: number }) => void }) => void;"
      ),
    )
    .expect("write nearby declaration");

  let path = source_directory.join("editor.ts");
  let source = crate::js!(
    typescript,
    r#"
      import { configure } from "./nearby";
      configure({ handler: (context) => context.value });
    "#
  );
  let mut project = AnalysisProject::default();
  let result = project.analyze_document(source.to_string(), &path, false);

  assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
  assert!(
    result
      .inlay_hints
      .iter()
      .any(|hint| { hint.kind == "type" && hint.label == ": { value: number }" })
  );
  let configure_start = source
    .find("configure")
    .expect("configure import");
  assert!(
    result.definitions.iter().any(|definition| {
      definition.start == configure_start
        && definition.target_path.as_deref()
          == Some(
            normalize_path(&declaration)
              .to_string_lossy()
              .as_ref(),
          )
    }),
    "{:?}",
    result.definitions
  );
  assert!(
    result
      .virtual_documents
      .iter()
      .any(|document| document.id == "__nearby.d.ts")
  );

  project.update_document(
      crate::js!(
        typescript,
        "export declare const configure: (options: { handler: (context: { value: string }) => void }) => void;"
      )
      .to_string(),
      &declaration,
    );
  let updated = project.analyze_document(source.to_string(), &path, false);
  assert!(
    updated
      .inlay_hints
      .iter()
      .any(|hint| { hint.kind == "type" && hint.label == ": { value: string }" })
  );

  std::fs::remove_dir_all(directory).expect("remove analysis fixture");
}

#[test]
fn project_uses_open_document_overlays_and_invalidates_dependents() {
  let directory = temporary_directory("overlays");
  let dependency = directory.join("dependency.ts");
  let importer = directory.join("importer.ts");
  let importer_source = crate::js!(
    module,
    "import { value } from \"./dependency.ts\";\nconsole.log(value);"
  )
  .to_string();
  let mut project = AnalysisProject::default();

  project.analyze_document(
    crate::js!(module, "export const value = 1;").to_string(),
    &dependency,
    false,
  );
  let first = project.analyze_document(importer_source.clone(), &importer, false);
  let first_target = first
    .definitions
    .iter()
    .find(|definition| definition.target_path.as_deref() == dependency.to_str())
    .unwrap()
    .target_selection_start;

  project.analyze_document(
    "// unsaved change\nexport const value = 2;".to_string(),
    &dependency,
    false,
  );
  let second = project.analyze_document(importer_source, &importer, false);
  let second_target = second
    .definitions
    .iter()
    .find(|definition| definition.target_path.as_deref() == dependency.to_str())
    .unwrap()
    .target_selection_start;

  assert!(second_target > first_target);

  std::fs::remove_dir_all(directory).expect("remove analysis fixture");
}

#[test]
fn resolves_ecmascript_builtin_globals_to_virtual_declarations() {
  let source = crate::js!(typescript, "const values = new Map<string, number>();");
  let result = analyze(source, Path::new("editor.ts"), false);
  assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
  let start = source.find("Map").unwrap();
  let definition = result
    .definitions
    .iter()
    .find(|definition| definition.start == start)
    .unwrap();

  assert_eq!(definition.target_virtual.as_deref(), Some(BUILTIN_LIB_ID));
  assert!(
    definition
      .detail
      .as_deref()
      .unwrap()
      .contains("MapConstructor")
  );
  assert!(
    result
      .virtual_documents
      .iter()
      .any(|document| document.id == BUILTIN_LIB_ID)
  );
}

#[test]
fn resolves_es2023_intrinsic_surface_declarations() {
  let source = crate::js!(
    typescript,
    "Promise.allSettled([]); Reflect.ownKeys({}); BigInt(1);"
  );
  let result = analyze(source, Path::new("editor.ts"), false);
  assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
  for name in ["Promise", "Reflect", "BigInt"] {
    let start = source
      .find(name)
      .expect("builtin name appears in source");
    assert!(
      result
        .definitions
        .iter()
        .any(|definition| definition.start == start
          && definition.target_virtual.as_deref() == Some(BUILTIN_LIB_ID)),
      "{name} did not resolve to the built-in declaration"
    );
  }
}

#[test]
fn treats_unloaded_editor_imports_as_external_bindings() {
  let result = analyze(
    crate::js!(
      typescript,
      "import * as Ref from \"effect/Ref\"; type ValueRef = Ref.Ref<number>; Ref.make(1);"
    ),
    Path::new("editor.ts"),
    false,
  );

  assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn reports_type_errors() {
  let result = analyze(
    crate::js!(typescript, "const answer: number = 'wrong';"),
    Path::new("example.ts"),
    false,
  );

  assert_eq!(result.diagnostics.len(), 1);
  assert!(
    result.diagnostics[0]
      .message
      .contains("not assignable")
  );
}
