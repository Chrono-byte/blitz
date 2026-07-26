# regex-js

`regex-js` is a host-neutral ECMAScript regular-expression parser and matcher.
It operates on UTF-16 code-unit offsets so matches can be exposed directly as
ECMAScript string indices.

The crate owns pattern syntax and matching. A JavaScript engine remains
responsible for `RegExp` objects, `lastIndex`, species construction, and the
symbol methods.

The initial implementation covers the ES6 pattern core:

- literals, dot, alternatives, and groups;
- greedy and lazy quantifiers;
- character classes and the standard character-class escapes;
- start, end, word-boundary, and lookahead assertions;
- numeric and named backreferences;
- ECMAScript ignore-case canonicalization in Unicode and legacy modes;
- ES6 Annex B pattern grammar in non-Unicode mode;
- `d`, `g`, `i`, `m`, `s`, `u`, and `y` flags.

Unicode property escapes, lookbehind, and Unicode set notation remain later
conformance slices. The crate is not ready for engine integration until its
ES6 behavior has been exercised by a generated Test262 corpus and the recursive
matcher has been replaced with a resource-safe execution strategy.
