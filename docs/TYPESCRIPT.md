# TypeScript model

Blitz checks TypeScript directly from OXC's AST. Runtime execution erases type-only syntax;
the checker owns a separate normalized type representation and evaluator.

## Implemented type language

- primitive, literal, `any`, `unknown`, `never`, `null`, `undefined`, and `void` types
- arrays, tuples, functions, structural objects, unions, and intersections
- generic aliases and interfaces with constraints and default arguments
- recursive and distributive conditional types
- `infer` bindings and variadic tuple-tail matching
- `keyof` and indexed access types
- mapped object types
- template-literal type products

Named definitions are evaluated with substitution environments. Conditional types can recurse
and deconstruct tuples, providing the recursive computation substrate used by TypeScript's
type-level programs. Evaluation has a 128-instantiation budget, and template products are capped
at 4096 results so invalid or explosive programs cannot hang the runtime.

## Next compatibility layers

- optional and readonly property metadata, index signatures, and exact excess-property checks
- `infer` matching for functions, objects, and suffix/middle tuple rests
- mapped-type modifiers and key remapping
- intrinsic string transformations and complete template-literal matching
- overload sets, generic function inference, variance, and contextual typing
- per-file symbol tables, imports/exports, declaration merging, and project graphs
- control-flow narrowing, definite assignment, and accumulated source-located diagnostics
- complete `tsconfig.json` option, include, exclude, and project-reference semantics

Unsupported constructs currently degrade to a permissive type. They should move to explicit
diagnostics as each compatibility layer is implemented.
