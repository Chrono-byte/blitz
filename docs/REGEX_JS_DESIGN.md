# regex-js design

`regex-js` is the ECMAScript regular-expression semantics crate for Blitz. It
does not model JavaScript objects. The engine owns `RegExp` construction,
properties, `lastIndex`, species behavior, and symbol methods; this crate owns
pattern parsing and matching.

## Semantic boundaries

- Pattern and input positions are UTF-16 code-unit offsets.
- Unicode mode reads surrogate pairs as one code point while retaining
  code-unit match spans.
- Matching returns capture spans without allocating capture strings.
- The matcher is immutable. Global and sticky `lastIndex` changes are performed
  by the engine around each match.
- Syntax errors carry a category and UTF-16 pattern offset so the engine can
  create the appropriate JavaScript `SyntaxError`.

## Architecture

1. `flags` validates the JavaScript flag string.
2. `parser` converts pattern UTF-16 into a regular-expression AST and numbers
   captures in source order.
3. `matcher` interprets the AST using ordered backtracking. State consists of a
   UTF-16 input cursor and capture spans.
4. `js-string` owns shared UTF-16 data and implements code-point-aware
   advancement. `regex-js` re-exports `JsString` as `Input`.

Ordered alternatives and greedy/lazy repetition make the result deterministic
according to ECMAScript matching priority. Captures are copied at backtracking
points and captures nested in a quantified atom are reset before every
iteration.

## Integration plan

The first engine integration should replace `RegExp.prototype.exec` and
`RegExp.prototype.test`. The engine should:

1. compile and store a `regex_js::Regex` internal slot during construction;
2. construct `regex_js::Input` from the subject;
3. derive the starting code-unit offset from `lastIndex` for global or sticky
   expressions;
4. call `Regex::find`;
5. update or reset `lastIndex`;
6. materialize the result array from returned capture spans.

Replacement and split should follow after `exec`, using the same match spans
rather than invoking an independent matching implementation.

## Remaining conformance work

- Unicode property escapes and property data;
- lookbehind;
- Unicode set notation and string properties;
- a resource-safe matcher that preserves ECMAScript backtracking order;
- a generated Test262 corpus grouped by RegExp grammar feature.

Features introduced after ES6 should remain gated by their flags and should not
distort the ES6 grammar.

No engine integration should begin until the ES6 Test262 corpus passes. The
post-ES6 features above do not block an ES6 baseline, but unsupported flags and
syntax must continue to fail explicitly rather than silently changing their
meaning.
