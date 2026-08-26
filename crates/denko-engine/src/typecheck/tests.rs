// SPDX-License-Identifier: MPL-2.0

use super::*;

fn check_typescript(source: &str) -> Result<(), TypeCheckerError> {
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  with_strict_override(Some(false), || check(session.program()))
}

#[test]
fn logical_ops_accept_non_boolean_operands() {
  let src = crate::js!(
    typescript,
    r#"
            var a = 1;
            var b = 'b';
            var x = a || b;
            var y = a && b;
            var z = 1 && 2;
        "#
  );
  let session = crate::parser::Parser::parse_source(src).expect("parse ok");
  let program = session.program();
  assert!(check(program).is_ok());
}

#[test]
fn rejects_annotated_variable_mismatch() {
  let result = check_typescript(crate::js!(typescript, "const count: number = 'wrong';"));

  assert!(result.is_err());
}

#[test]
fn rejects_assignment_mismatch() {
  let result = check_typescript(crate::js!(
    typescript,
    "let count: number = 1; count = 'wrong';"
  ));

  assert!(result.is_err());
}

#[test]
fn rejects_function_return_mismatch() {
  let result = check_typescript(crate::js!(
    typescript,
    "function answer(): number { return 'wrong'; }"
  ));

  assert!(result.is_err());
}

#[test]
fn rejects_call_argument_mismatch() {
  let result = check_typescript(crate::js!(
    typescript,
    "function square(value: number): number { return value * value; } square('wrong');"
  ));

  assert!(result.is_err());
}

#[test]
fn resolves_interface_shapes() {
  let valid = check_typescript(crate::js!(
    typescript,
    "interface User { name: string; age: number } const user: User = { name: 'Ada', age: 36 };"
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "interface User { name: string; age: number } const user: User = { name: 'Ada' };"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}

#[test]
fn checks_exported_declarations() {
  let result = check_typescript(crate::js!(
    typescript,
    "export const count: number = 'wrong';"
  ));

  assert!(result.is_err());
}

#[test]
fn infers_common_expression_types() {
  let result = check_typescript(crate::js!(
    typescript,
    r#"
        const compared: boolean = 1 < 2;
        const equal: boolean = "a" === "b";
        const shifted: number = 2 << 1;
        const message: string = `answer: ${shifted}`;
        const missing: undefined = undefined;
      "#
  ));

  assert!(result.is_ok());
}

#[test]
fn contextually_types_callback_parameters_for_object_arguments() {
  let source = crate::js!(
    typescript,
    r#"
        declare function configure(options: {
          handler: (context: { userId: string; active: boolean }) => void
        }): void;
        configure({ handler: (context) => context.userId });
      "#
  );
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  let (result, hints) = check_and_inlay_hints_with_declarations(session.program(), &[], &[]);

  assert!(result.is_ok(), "{result:?}");
  let type_hints = hints
    .iter()
    .filter(|hint| hint.kind == TypeInlayHintKind::Type)
    .collect::<Vec<_>>();
  assert_eq!(type_hints.len(), 1);
  assert_eq!(
    type_hints[0].position,
    source.rfind("(context)").unwrap() + "(context".len()
  );
  assert_eq!(type_hints[0].label, ": { active: boolean; userId: string }");
}

#[test]
fn instantiates_generic_object_context_from_a_sibling_property() {
  let source = crate::js!(
    typescript,
    r#"
        type Options<T> = {
          value: T;
          handler: (context: T) => void
        };
        declare function configure<T>(options: Options<T>): void;
        configure({ value: "ready" as string, handler: (context) => context });
      "#
  );
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  let (result, hints) = check_and_inlay_hints_with_declarations(session.program(), &[], &[]);

  assert!(result.is_ok(), "{result:?}");
  assert_eq!(
    hints
      .iter()
      .map(|hint| (&hint.kind, hint.label.as_str()))
      .collect::<Vec<_>>(),
    [
      (&TypeInlayHintKind::Parameter, "options:"),
      (&TypeInlayHintKind::Type, ": string"),
    ]
  );
}

#[test]
fn ranks_specific_overloads_and_discards_rejected_candidate_hints() {
  let source = crate::js!(
    typescript,
    r#"
        declare function choose(options: {
          handler: (value: string | number) => void
        }): void;
        declare function choose(options: {
          handler: (value: string) => void
        }): void;
        declare function choose(options: {
          handler: (value: boolean) => void;
          required: number
        }): void;
        choose({ handler: (value) => value });
      "#
  );
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  let (result, hints) = check_and_inlay_hints_with_declarations(session.program(), &[], &[]);

  assert!(result.is_ok(), "{result:?}");
  assert_eq!(
    hints
      .iter()
      .map(|hint| (&hint.kind, hint.label.as_str()))
      .collect::<Vec<_>>(),
    [
      (&TypeInlayHintKind::Parameter, "options:"),
      (&TypeInlayHintKind::Type, ": string"),
    ]
  );
}

#[test]
fn emits_selected_parameter_name_hints_with_suppression_rules() {
  let source = crate::js!(
    typescript,
    r#"
        declare function choose(text: string): void;
        declare function choose(count: number, radix: number): void;
        declare function collect(first: string, ...rest: number[]): void;
        declare const input: string;
        declare const countValue: number;
        choose(input);
        choose(countValue, 10);
        collect(input, countValue, 10);
      "#
  );
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  let (result, hints) = check_and_inlay_hints_with_declarations(session.program(), &[], &[]);

  assert!(result.is_ok(), "{result:?}");
  assert_eq!(
    hints
      .iter()
      .map(|hint| (&hint.kind, hint.label.as_str()))
      .collect::<Vec<_>>(),
    [
      (&TypeInlayHintKind::Parameter, "text:"),
      (&TypeInlayHintKind::Parameter, "count:"),
      (&TypeInlayHintKind::Parameter, "first:"),
    ]
  );
  assert_eq!(
    hints[0].position,
    source.find("choose(input)").expect("first call") + "choose(".len()
  );
  assert_eq!(
    hints[1].position,
    source
      .find("choose(countValue, 10)")
      .expect("second call")
      + "choose(".len()
  );
  assert_eq!(
    hints[2].position,
    source.find("collect(input").expect("rest call") + "collect(".len()
  );
}

#[test]
fn emits_safe_local_and_simple_function_return_hints() {
  let source = crate::js!(
    typescript,
    r#"
      let count = 1;
      const text = "ready";
      const shape = { id: 1 };
      const dynamic = eval("value");
      const annotated: number = 1;

      function identity(value: number) { return value; }
      function empty() {}
      async function asyncValue() { return 1; }
      function recursive() { return recursive(); }
      function conditional(flag: boolean) { if (flag) return 1; }
    "#
  );
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  let (result, hints) = check_and_inlay_hints_with_declarations(session.program(), &[], &[]);

  assert!(result.is_ok(), "{result:?}");
  assert_eq!(
    hints
      .iter()
      .map(|hint| hint.label.as_str())
      .collect::<Vec<_>>(),
    [
      ": number",
      ": \"ready\"",
      ": { id: 1 }",
      ": number",
      ": void",
    ]
  );
  assert!(
    hints
      .windows(2)
      .all(|pair| pair[0].position <= pair[1].position)
  );
  assert_eq!(
    hints
      .iter()
      .filter(|hint| hint.label == ": number")
      .count(),
    2
  );
  assert_eq!(
    hints[0].position,
    source.find("count").expect("count binding") + "count".len()
  );
  assert_eq!(
    hints[3].position,
    source
      .find("(value: number)")
      .expect("identity parameters")
      + "(value: number)".len()
  );
  assert_eq!(
    hints[4].position,
    source.find("empty()").expect("empty parameters") + "empty()".len()
  );
  assert!(!hints.iter().any(|hint| hint.label == ": any"));
}

#[test]
fn emits_return_hint_for_expression_arrow_but_not_async_arrow() {
  let source = crate::js!(
    typescript,
    r#"
      const identity = (value: number) => value;
      const asyncValue = async (value: number) => value;
    "#
  );
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session =
    crate::parser::Parser::parse_source_with_type(source, source_type).expect("parse TypeScript");
  let (result, hints) = check_and_inlay_hints_with_declarations(session.program(), &[], &[]);

  assert!(result.is_ok(), "{result:?}");
  assert_eq!(
    hints
      .iter()
      .map(|hint| hint.label.as_str())
      .collect::<Vec<_>>(),
    [": (arg0: number) => number", ": number"]
  );
  assert_eq!(
    hints[0].position,
    source.find("identity").expect("identity binding") + "identity".len()
  );
  assert_eq!(
    hints[1].position,
    source
      .find("(value: number)")
      .expect("identity parameters")
      + "(value: number)".len()
  );
}

#[test]
fn strict_mode_rejects_implicit_any_parameters() {
  let source_type =
    crate::parser::Parser::source_type_for_path(std::path::Path::new("test.ts"), false);
  let session = crate::parser::Parser::parse_source_with_type(
    crate::js!(
      typescript,
      "function identity(value): number { return value; }"
    ),
    source_type,
  )
  .expect("parse TypeScript");

  let result = with_strict_override(Some(true), || check(session.program()));

  assert!(result.is_err());
}

#[test]
fn evaluates_generic_aliases_and_defaults() {
  let valid = check_typescript(crate::js!(
    typescript,
    r#"
        type Box<T extends string = "default"> = { value: T };
        const explicit: Box<"value"> = { value: "value" };
        const fallback: Box = { value: "default" };
      "#
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Box<T extends string> = { value: T }; const value: Box<number> = { value: 1 };"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}

#[test]
fn evaluates_conditional_types() {
  let valid = check_typescript(crate::js!(
    typescript,
    r#"
        type IsString<T> = T extends string ? true : false;
        const yes: IsString<"value"> = true;
        const no: IsString<number> = false;
      "#
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type IsString<T> = T extends string ? true : false; const value: IsString<number> = true;"
  ));

  assert!(valid.is_ok());
  assert!(invalid.is_err());
}

#[test]
fn distributes_conditional_types_over_unions() {
  let valid = check_typescript(crate::js!(
    typescript,
    "type Strings<T> = T extends string ? T : never; const value: Strings<\"a\" | 1> = \"a\";"
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Strings<T> = T extends string ? T : never; const value: Strings<\"a\" | 1> = 1;"
  ));

  assert!(valid.is_ok());
  assert!(invalid.is_err());
}

#[test]
fn evaluates_recursive_aliases_with_a_budget() {
  let result = check_typescript(crate::js!(
    typescript,
    "type EventuallyTrue<T> = T extends true ? true : EventuallyTrue<true>; const value: EventuallyTrue<false> = true;"
  ));

  assert!(result.is_ok());
}

#[test]
fn merges_object_intersections() {
  let valid = check_typescript(crate::js!(
    typescript,
    "type Both = { name: string } & { age: number }; const value: Both = { name: 'Ada', age: 36 };"
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Both = { name: string } & { age: number }; const value: Both = { name: 'Ada' };"
  ));

  assert!(valid.is_ok());
  assert!(invalid.is_err());
}

#[test]
fn evaluates_keyof_and_indexed_access_types() {
  let valid = check_typescript(crate::js!(
    typescript,
    r#"
        type Person = { name: string; age: number };
        type PersonKey = keyof Person;
        type Age = Person["age"];
        const key: PersonKey = "name";
        const age: Age = 36;
      "#
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Person = { name: string }; const key: keyof Person = 'missing';"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}

#[test]
fn evaluates_generic_tuple_indexing() {
  let valid = check_typescript(crate::js!(
    typescript,
    "type Head<T extends unknown[]> = T[0]; const value: Head<[string, number]> = 'first';"
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Head<T extends unknown[]> = T[0]; const value: Head<[string, number]> = 1;"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}

#[test]
fn infers_types_from_tuple_patterns() {
  let valid = check_typescript(crate::js!(
    typescript,
    "type Head<T> = T extends [infer H, ...unknown[]] ? H : never; const value: Head<[string, number]> = 'first';"
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Head<T> = T extends [infer H, ...unknown[]] ? H : never; const value: Head<[string, number]> = 1;"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}

#[test]
fn recursively_computes_over_inferred_tuple_tails() {
  let source = crate::js!(
    typescript,
    r#"
      type FirstString<T extends unknown[]> =
        T extends [infer Head, ...infer Tail]
          ? Head extends string
            ? Head
            : Tail extends unknown[]
              ? FirstString<Tail>
              : never
          : never;
      const value: FirstString<[number, boolean, "found"]> = "found";
    "#
  );

  let result = check_typescript(source);

  assert!(result.is_ok(), "{result:?}");
}

#[test]
fn evaluates_mapped_object_types() {
  let valid = check_typescript(crate::js!(
    typescript,
    r#"
        type Copy<T> = { [K in keyof T]: T[K] };
        type Source = { name: string; age: number };
        const value: Copy<Source> = { name: "Ada", age: 36 };
      "#
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Flags<T> = { [K in keyof T]: boolean }; const value: Flags<{ ready: string }> = { ready: 'yes' };"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}

#[test]
fn evaluates_template_literal_type_products() {
  let valid = check_typescript(crate::js!(
    typescript,
    "type Event = `on:${\"open\" | \"close\"}`; const value: Event = 'on:open';"
  ));
  let invalid = check_typescript(crate::js!(
    typescript,
    "type Event = `on:${\"open\" | \"close\"}`; const value: Event = 'on:missing';"
  ));

  assert!(valid.is_ok(), "{valid:?}");
  assert!(invalid.is_err());
}
