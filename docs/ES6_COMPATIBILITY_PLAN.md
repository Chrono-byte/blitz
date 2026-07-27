# ES6 Compatibility Plan

The roadmap prioritizes shared semantics first. Current targeted samples suggest
classes are relatively mature, while iterators, generators, construction,
proxies, reflection, and collection built-ins need foundational work.

| Sample | Current result |
| --- | ---: |
| Classes | 39/40 |
| Arrow functions | 14/20 |
| `for...of` | 13/20 |
| Generators | 10/20 |
| `Map` | 4/20 |
| `Array.from` | 0/10 |
| `Proxy` | 4/10 |
| `Reflect` | 1/10 |

These are diagnostic samples, not authoritative overall pass rates.

## Phase 1: Establish the baseline

Create a reproducible ES6 conformance report before further implementation.

- Generate a stable manifest of tests selected by `--es-version ES6`.
- Separate:
  - normative ES6;
  - modules;
  - async tests;
  - negative parse tests;
  - negative runtime tests;
  - Annex B.
- Classify failures by root cause:
  - parser/compiler;
  - stack or control-flow corruption;
  - missing intrinsic;
  - incorrect descriptor;
  - missing abstract operation;
  - exception propagation;
  - realm or job-queue behavior.
- Record crashes, hangs, and unsupported tests independently from assertions.
- Add a CI-friendly ES6 smoke set covering every major feature family.

Exit criterion: repeated runs select the same tests and produce a useful failure
summary without crashes or unexplained skips.

Implementation status: complete as of 2026-07-15. The stable manifest contains
9,294 tests and round-trips byte-for-byte. Schema-v2 JSON reports independent
pass, fail, skip, crash, and hang outcomes plus category and root-cause totals.
Parent-enforced subprocess deadlines and recursive isolation allow the full run
to finish despite 31 tail-call stack overflows and one Promise hang; there are
no unexplained skips. The reviewed counts and test-specific blockers are in
`docs/test262/ES6_BASELINE.md`, and reproducible commands are in
`docs/test262/README.md`.

## Phase 2: Complete shared abstract operations

This is the highest-leverage work. Array, collections, Reflect, Proxy, classes,
and generators all depend on it.

Implement or consolidate:

- `IsCallable`, `IsConstructor`, `Call`, and `Construct`.
- Correct `newTarget` propagation.
- `GetMethod` and consistent callable validation.
- `Get`, `Set`, `HasProperty`, `DeletePropertyOrThrow`.
- `CreateDataProperty` and `CreateDataPropertyOrThrow`.
- `DefinePropertyOrThrow`, `GetOwnProperty`, and `OwnPropertyKeys`.
- `OrdinaryCreateFromConstructor` and prototype selection.
- `ToPrimitive`, `ToPropertyKey`, `ToLength`, and `ToIndex`.
- Completion propagation through getters, setters, coercion hooks, and proxy
  traps.
- Descriptor validation and non-extensible-object invariants.

Avoid feature-specific substitutes such as collecting iterator values without
preserving observable operations or abrupt completions.

Exit criteria:

- `Reflect.apply`, `Reflect.construct`, and basic property methods pass.
- Poisoned getters and conversion hooks propagate their original exceptions.
- Constructor and non-constructor tests distinguish functions correctly.
- Extracted built-ins retain correct behavior and metadata.

## Phase 3: Functions, bindings, and parameters

Finish ES6 execution-context and lexical-binding behavior.

- Arrow functions:
  - lexical `this`;
  - lexical `arguments`;
  - lexical `super` and `new.target`;
  - non-constructability;
  - correct `name`, `length`, and prototype absence.
- Default, rest, and destructured parameters.
- Parameter-environment TDZ and duplicate-name rules.
- Function-name inference.
- Block-scoped function declarations.
- Per-iteration environments for `let` and `const`.
- Destructuring defaults, computed keys, rest elements, and abrupt completion.
- Call and constructor spread using the iterator protocol.

Exit criteria:

- Arrow-function sample reaches 100%.
- Focused parameter and destructuring suites pass.
- Negative syntax tests pass at parse time rather than failing during execution.
- Stack-balance tests cover default parameters, spread calls, and destructuring
  failures.

## Phase 4: Iterators and generators

Build one spec-oriented iterator implementation and route every consumer
through it.

Core iterator operations:

- `GetIterator`
- `IteratorNext`
- `IteratorComplete`
- `IteratorValue`
- `IteratorStep`
- `IteratorClose`

Update consumers:

- `for...of`
- array and call spread
- destructuring
- `Array.from`
- `Map` and `Set` constructors
- `Promise.all` and `Promise.race`
- `yield*`

Complete generators:

- suspended-start, suspended-yield, executing, and completed states;
- `.next(value)`, `.throw(value)`, and `.return(value)`;
- re-entry protection;
- `yield*` forwarding;
- missing `throw`/`return` handling;
- iterator closing during abrupt completion;
- generator prototype and method descriptors.

Exit criteria:

- `Array.from` covers iterator lookup, mapping, custom constructors, exception
  propagation, and iterator closing.
- `for...of` closes iterators on `break`, `return`, and exceptions.
- Generator and `yield*` Test262 subsets pass without stack imbalance.
- Map, Set, Promise, spread, and destructuring use the same iterator helpers.

## Phase 5: Close class semantics

The basic class implementation is already strong, so this should follow the
foundations rather than drive them.

Complete:

- Computed method-name evaluation, including `yield` inside generator contexts.
- Class declaration and expression TDZ.
- Named class-expression inner binding.
- Default base and derived constructors.
- Derived `this` initialization and `super()` restrictions.
- `extends null` and invalid-superclass handling.
- Constructor/prototype property descriptors.
- Static and instance methods, getters, setters, and computed names.
- Method `[[HomeObject]]` for `super` reads, writes, and calls.
- Class strictness and prohibition on calling constructors without `new`.
- Function `name` and `length` inference for class members.

ES6 scope should exclude fields, private names, static blocks, and decorators.

Exit criterion: complete normative ES6 class directories pass, including
negative syntax and runtime tests.

## Phase 6: Standard-library completion

Implement families in dependency order.

### 1. Symbols

- Global registry.
- Well-known symbols.
- Symbol property keys.
- Wrapper behavior.
- Correct descriptors and `@@toStringTag`.

### 2. Object and Reflect

- Complete Reflect's ES6 methods.
- Align Object methods with the same internal property operations.
- Property ordering and descriptor correctness.

### 3. Proxy

- Callable and constructable proxies.
- All ES6 traps.
- Revocation.
- Trap fallback.
- Required invariants for non-configurable and non-extensible properties.

### 4. Array

- `Array.from` and `Array.of`.
- Iterator integration.
- `@@species`.
- `@@isConcatSpreadable`.
- ES6 prototype additions and unscopables.

### 5. Collections

- Map, Set, WeakMap, and WeakSet.
- SameValueZero keys.
- Iterator constructors and iterator closing.
- Mutation behavior during iteration.
- Constructor and method branding.

### 6. Promise

- Constructor and resolving functions.
- Thenable assimilation.
- Reaction jobs and deterministic checkpoints.
- `all`, `race`, `resolve`, and `reject`.
- Species and constructor behavior.

### 7. Binary data

- ArrayBuffer and DataView.
- Typed-array construction paths.
- Numeric conversion and bounds behavior.
- Iterators and species.

### 8. Remaining ES6 additions

- String and String iterator methods.
- Number and Math additions.
- RegExp flags, symbol methods, and constructor behavior.
- Date and function metadata changes.

Each family should land in focused PRs with descriptor tests as well as
behavioral tests.

## Phase 7: Modules, realms, and jobs

- Complete module parsing and early errors.
- Instantiate module graphs before evaluation.
- Support live bindings, re-exports, default exports, and namespace objects.
- Handle cyclic graphs using strongly connected components.
- Preserve realm-specific intrinsic prototypes during construction.
- Formalize microtask checkpoints:
  - after script or module evaluation;
  - after host callbacks;
  - before returning control to the host.
- Ensure module and Promise tests cannot hang the runner.

Exit criteria:

- ES6 module graph, cycle, namespace, and live-binding tests pass.
- Cross-realm constructor tests select the correct intrinsic prototype.
- Promise ordering is deterministic across repeated runs.

## Phase 8: Hardening and release gate

For every implementation slice:

1. Add a focused Rust unit test.
2. Run the exact failing Test262 case.
3. Run its containing feature directory.
4. Run `cargo test -p denko-engine --lib`.
5. Run `cargo test --workspace`.
6. Run the ES6 smoke manifest.

Final gates:

- No VM crashes, hangs, or stack-balance failures.
- No unsupported normative ES6 features.
- All negative parse tests fail in the correct phase and error category.
- Full normative ES6 pass rate is tracked by test count, not sampled percentage.
- Annex B is reported separately and completed after normative ES6.
- Any remaining failures have documented, test-specific blockers rather than
  broad feature exclusions.

A practical implementation sequence is: abstract operations ->
functions/bindings -> iterators -> generators -> classes ->
Symbol/Object/Reflect -> Proxy -> Array/collections -> Promise -> typed arrays ->
modules -> Annex B.
