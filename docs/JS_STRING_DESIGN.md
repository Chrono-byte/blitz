# JavaScript string representation

Blitz currently stores `Value::String` as Rust `String`. That representation
cannot model every ECMAScript string because Rust requires valid Unicode scalar
values while ECMAScript strings are sequences of arbitrary UTF-16 code units.
In particular, JavaScript preserves isolated leading and trailing surrogates.

The `js-string` crate introduces the semantic string representation used by new
engine components.

## Representation

`JsString` stores immutable UTF-16 code units in `Arc<[u16]>` plus a slice range.
This gives it the properties needed by the VM:

- exact representation of every ECMAScript string;
- constant-time code-unit length and indexing;
- cheap clones and substring views;
- equality, ordering, and hashing by UTF-16 code units;
- explicit, lossy conversion at Rust text boundaries;
- code-point reads that preserve code-unit offsets.

The type intentionally does not expose `Deref<Target = str>`. Code that needs a
Rust string must acknowledge the potentially lossy conversion.

## Engine migration

The migration should be performed in semantic layers rather than with a broad
textual replacement:

1. Use `JsString` in `regex-js` and other new semantic crates.
2. Change string constants and `Value::String` to `JsString`.
3. Change string-valued property keys to `JsString`.
4. Rewrite string abstract operations and built-ins around code-unit indices.
5. Convert to Rust `String` only in parsers, filesystem APIs, diagnostics, and
   other host boundaries.
6. Add Test262 slices for isolated surrogates, astral characters, string
   iteration, replacement, split, and property-key conversion.

During the migration, helpers that return Rust byte offsets should be removed.
They encode the representation mismatch and make it too easy to accidentally
implement ECMAScript indexing in terms of UTF-8.

## Future storage work

The public semantics do not depend on the current `Arc<[u16]>` layout. Once the
engine is correct, the crate can add small-string storage, interning, ropes, or
dependent strings without changing callers.
