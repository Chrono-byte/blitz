# js-string

`js-string` provides immutable UTF-16 strings for ECMAScript implementations.
Unlike Rust's `String`, `JsString` can represent every JavaScript string,
including strings containing isolated surrogate code units.

Clones and slices share immutable storage. Equality, ordering, hashing, search,
and indexing operate on UTF-16 code units. Conversion back to a Rust `String`
is explicitly lossy because Rust strings cannot contain isolated surrogates.
