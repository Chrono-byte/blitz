// SPDX-License-Identifier: MPL-2.0
//! Focused checker coverage for the corrected Effect type foundation.

use std::path::Path;

use denko::parser::Parser;
use denko::typecheck::{VirtualDeclaration, check_with_declarations};

fn check_source(
  source: &str,
  name: &str,
  declarations: &[VirtualDeclaration],
) -> Result<(), denko::errors::TypeCheckerError> {
  let source_type = Parser::source_type_for_path(Path::new(name), false);
  let session = Parser::parse_source_with_type(source, source_type).expect("parse source");
  check_with_declarations(session.program(), declarations)
}

fn check_fixture(name: &str) -> Result<(), denko::errors::TypeCheckerError> {
  let source = include_str!(concat!("fixtures/", "effect-foundation-positive.ts"));
  let source_type = Parser::source_type_for_path(Path::new(name), false);
  let session = Parser::parse_source_with_type(source, source_type).expect("parse fixture");
  let declarations = [
    VirtualDeclaration {
      specifier: "effect/Context".to_string(),
      source: include_str!("fixtures/effect-foundation-context.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "effect/Effect".to_string(),
      source: include_str!("fixtures/effect-foundation-effect.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "effect/Layer".to_string(),
      source: include_str!("fixtures/effect-foundation-layer.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "effect/Fiber".to_string(),
      source: include_str!("fixtures/effect-foundation-fiber.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "blitz:runtime".to_string(),
      source: include_str!("fixtures/effect-foundation-runtime.d.ts").to_string(),
    },
  ];
  check_with_declarations(session.program(), &declarations)
}

#[test]
fn corrected_effect_foundation_fixture_checks() {
  let result = check_fixture("effect-foundation-positive.ts");
  assert!(result.is_ok(), "{result:?}");
}

#[test]
fn corrected_effect_foundation_fixture_rejects_bad_members_and_identity() {
  let source = include_str!("fixtures/effect-foundation-negative.ts");
  let source_type = Parser::source_type_for_path(Path::new("effect-foundation-negative.ts"), false);
  let session = Parser::parse_source_with_type(source, source_type).expect("parse fixture");
  let declarations = [
    VirtualDeclaration {
      specifier: "effect/Context".to_string(),
      source: include_str!("fixtures/effect-foundation-context.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "effect/Layer".to_string(),
      source: include_str!("fixtures/effect-foundation-layer.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "effect/Effect".to_string(),
      source: include_str!("fixtures/effect-foundation-effect.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "blitz:runtime".to_string(),
      source: include_str!("fixtures/effect-foundation-runtime.d.ts").to_string(),
    },
  ];
  assert!(check_with_declarations(session.program(), &declarations).is_err());
}

#[test]
fn run_main_rejects_open_requirements() {
  let source = denko::js!(
    typescript,
    "import type * as Effect from \"effect/Effect\"; import type * as Runtime from \"blitz:runtime\"; const open = null as unknown as Effect.Effect<number, never, \"Clock\">; Runtime.runMain(open);"
  );
  let source_type = Parser::source_type_for_path(Path::new("run-main.ts"), false);
  let session = Parser::parse_source_with_type(source, source_type).expect("parse source");
  let declarations = [
    VirtualDeclaration {
      specifier: "effect/Effect".to_string(),
      source: include_str!("fixtures/effect-foundation-effect.d.ts").to_string(),
    },
    VirtualDeclaration {
      specifier: "blitz:runtime".to_string(),
      source: include_str!("fixtures/effect-foundation-runtime.d.ts").to_string(),
    },
  ];
  assert!(check_with_declarations(session.program(), &declarations).is_err());
}

#[test]
fn missing_virtual_module_is_not_implicit_any() {
  let source = denko::js!(
    typescript,
    "import * as Missing from \"missing/module\"; const value = Missing.answer;"
  );
  let declarations = [VirtualDeclaration {
    specifier: "present/module".to_string(),
    source: denko::js!(typescript, "export declare const answer: number;").to_string(),
  }];
  let result = check_source(source, "missing.ts", &declarations);
  assert!(result.is_err());
}

#[test]
fn aggregate_declaration_can_precede_its_dependency() {
  let declarations = [
    VirtualDeclaration {
      specifier: "root".to_string(),
      source: denko::js!(typescript, "export * as Dependency from \"dependency\";").to_string(),
    },
    VirtualDeclaration {
      specifier: "dependency".to_string(),
      source: denko::js!(typescript, "export declare const answer: number;").to_string(),
    },
  ];
  let result = check_source(
    denko::js!(
      typescript,
      "import * as Root from \"root\"; const answer: number = Root.Dependency.answer;"
    ),
    "aggregate.ts",
    &declarations,
  );
  assert!(result.is_ok(), "{result:?}");
}

#[test]
fn namespace_imports_include_type_only_members() {
  let declarations = [VirtualDeclaration {
    specifier: "api".to_string(),
    source: denko::js!(
      typescript,
      "export interface Service { value: number } export declare const make: () => Service;"
    )
    .to_string(),
  }];
  let result = check_source(
    denko::js!(
      typescript,
      "import type * as Api from \"api\"; const service: Api.Service = Api.make();"
    ),
    "namespace.ts",
    &declarations,
  );
  assert!(result.is_ok(), "{result:?}");
}

#[test]
fn missing_virtual_members_are_rejected() {
  let declarations = [VirtualDeclaration {
    specifier: "api".to_string(),
    source: denko::js!(typescript, "export declare const answer: number;").to_string(),
  }];
  let result = check_source(
    denko::js!(
      typescript,
      "import * as Api from \"api\"; const value = Api.missing;"
    ),
    "missing-member.ts",
    &declarations,
  );
  assert!(result.is_err());
}

#[test]
fn module_local_helpers_resolve_without_becoming_exports() {
  let declarations = [VirtualDeclaration {
    specifier: "effect/Effect".to_string(),
    source: denko::js!(typescript, r#"
      export interface Effect<A> {}
      interface Exit<A> { readonly value: A }
      interface RunCallbackOptions<A> { readonly onExit?: (exit: Exit<A>) => void }
      type Cancel<A> = (options?: RunCallbackOptions<A>) => void;
      export declare const runCallback: <A>(effect: Effect<A>, options?: RunCallbackOptions<A>) => Cancel<A>;
    "#)
    .to_string(),
  }, VirtualDeclaration {
    specifier: "effect/Scope".to_string(),
    source: denko::js!(typescript, r#"
      interface ExecutionStrategy { readonly _tag: "Sequential" }
      export interface Scope { readonly strategy: ExecutionStrategy }
      export declare const make: (executionStrategy?: ExecutionStrategy) => Scope;
    "#)
    .to_string(),
  }];
  let result = check_source(
    denko::js!(
      typescript,
      r#"
      import * as Effect from "effect/Effect";
      import * as Scope from "effect/Scope";
      const cancel = Effect.runCallback(null as unknown as Effect.Effect<number>, {});
      cancel({});
      const tag: "Sequential" = Scope.make({ _tag: "Sequential" }).strategy._tag;
    "#
    ),
    "module-local-helpers.ts",
    &declarations,
  );
  assert!(result.is_ok(), "{result:?}");

  let private_import = check_source(
    denko::js!(
      typescript,
      "import type { RunCallbackOptions } from \"effect/Effect\";"
    ),
    "private-helper-import.ts",
    &declarations,
  );
  assert!(private_import.is_err());
  let private_scope_import = check_source(
    denko::js!(
      typescript,
      "import type { ExecutionStrategy } from \"effect/Scope\";"
    ),
    "private-scope-helper-import.ts",
    &declarations,
  );
  assert!(private_scope_import.is_err());
}

fn bundled_effect_declarations() -> Vec<VirtualDeclaration> {
  vec![
    VirtualDeclaration {
      specifier: "effect/Effect".to_string(),
      source: denko::js!(typescript, r#"
        interface FiberId {}
        interface Exit<A, E> { readonly value: A; readonly error: E }
        interface RunCallbackOptions<A, E = never> {
          readonly onExit?: ((exit: Exit<A, E>) => void) | undefined;
        }
        interface Cancel<A, E = never> {
          (fiberId?: FiberId, options?: RunCallbackOptions<A, E> | undefined): void;
        }
        export interface Effect<A, E = never> {}
        export declare const succeed: <A>(value: A) => Effect<A>;
        export declare const runCallback: <A, E>(effect: Effect<A, E>, options?: RunCallbackOptions<A, E>) => Cancel<A, E>;
      "#)
      .to_string(),
    },
    VirtualDeclaration {
      specifier: "effect/Scope".to_string(),
      source: denko::js!(typescript, r#"
        interface Pipeable<Self> {
          readonly pipe: <A>(ab: (self: Self) => A) => A;
        }
        interface SequentialExecutionStrategy { readonly _tag: "Sequential" }
        interface ParallelExecutionStrategy { readonly _tag: "Parallel" }
        interface ParallelNExecutionStrategy { readonly _tag: "ParallelN"; readonly parallelism: number }
        type ExecutionStrategy = SequentialExecutionStrategy | ParallelExecutionStrategy | ParallelNExecutionStrategy;
        export interface Scope extends Pipeable<Scope> { readonly strategy: ExecutionStrategy }
        export interface CloseableScope extends Scope {}
        export declare const make: (executionStrategy?: ExecutionStrategy) => CloseableScope;
      "#)
      .to_string(),
    },
  ]
}

#[test]
fn bundled_effect_run_callback_and_scope_make_signatures_resolve() {
  let declarations = bundled_effect_declarations();
  let valid = check_source(
    denko::js!(
      typescript,
      r#"
      import * as Effect from "effect/Effect";
      import * as Scope from "effect/Scope";
      const cancel = Effect.runCallback(Effect.succeed(1), {});
      cancel({}, {});
      Scope.make({ _tag: "Sequential" });
    "#
    ),
    "bundled-effect-signatures.ts",
    &declarations,
  );
  assert!(valid.is_ok(), "{valid:?}");
}

#[test]
fn declaration_overloads_and_requirements_are_checked() {
  let declarations = [VirtualDeclaration {
    specifier: "api".to_string(),
    source: denko::js!(
      typescript,
      r#"
      export declare const overloaded: {
        (value: string): number;
        (value: number): string;
      };
      export interface Effect<A, R = never> { readonly _Requirements: (_: R) => void }
      export declare const run: <A>(effect: Effect<A, never>) => void;
    "#
    )
    .to_string(),
  }];
  let valid = check_source(
    denko::js!(
      typescript,
      "import type * as Api from \"api\"; const numberResult: number = Api.overloaded(\"ok\"); const stringResult: string = Api.overloaded(1); const effect = null as unknown as Api.Effect<number>; Api.run(effect);"
    ),
    "overloads.ts",
    &declarations,
  );
  let invalid = check_source(
    denko::js!(
      typescript,
      "import type * as Api from \"api\"; const result: boolean = Api.overloaded(true);"
    ),
    "bad-overload.ts",
    &declarations,
  );
  let open_requirement = check_source(
    denko::js!(
      typescript,
      "import type * as Api from \"api\"; const effect = null as unknown as Api.Effect<number, \"Clock\">; Api.run(effect);"
    ),
    "bad-requirement.ts",
    &declarations,
  );
  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
  assert!(open_requirement.is_err());
}
