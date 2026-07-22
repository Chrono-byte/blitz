# Full Effect module-graph prebundling plan

## Status

In progress. Phases 1–2 are implemented in the engine: validated host-neutral
prelinked graph metadata, parser-free module records, canonical live-binding
allocation, mixed source/prelinked linking, reserved indices, and bundled-edge
evaluation. Phases 3–5 remain.

The foundation for sendable chunks, parallel compilation, deterministic
compiler output, and versioned bytecode serialization is landed. The current
local `effect.blitzbc` experiment is only a bytecode cache: it still requires
Effect source parsing and AST-based linking at startup. It must not be
considered the final artifact.

## Objective

Produce one deterministic, committed, network-free Effect artifact containing
everything required to instantiate and execute the supported Effect 3.16.7
module graph without parsing or compiling Effect JavaScript at runtime.

At runtime Blitz should only:

1. decode and validate the embedded graph bundle;
2. allocate live module-binding slots;
3. install module records and export maps;
4. traverse the bundled dependency graph in specification order; and
5. execute verified bytecode on the single VM thread.

User modules and small Blitz-owned facades may continue through the ordinary
source parser, linker, and compiler. JavaScript evaluation remains
single-threaded.

## Non-goals

- Combining Effect into one JavaScript module.
- Curating a smaller Effect facade or removing supported Effect exports.
- Reimplementing Effect in Rust.
- Evaluating JavaScript on compiler workers.
- Serializing host resources or VM runtime values.
- Snapshotting an already-evaluated JavaScript heap.
- Eliminating the cost of Effect module initializers.

A heap or realm snapshot may be considered separately if module execution is
still a material startup cost after full graph prebundling.

## Terminology

- **Bytecode bundle**: compiled chunks only. It still needs source-derived graph
  construction and linking metadata.
- **Prelinked graph bundle**: bytecode plus dependency, module, import, export,
  and canonical live-binding metadata. It requires no source parsing.
- **Heap snapshot**: serialized state after module evaluation. This is outside
  this plan.

## Required invariants

1. Effect root and deep imports share one module identity graph.
2. Cycles observe preallocated live bindings.
3. Re-exports share the original binding slot rather than copied values.
4. Namespace imports observe the same live export slots.
5. Module evaluation remains deterministic and dependency ordered.
6. Numeric module indices embedded in bytecode remain stable.
7. Source modules loaded before or after Effect cannot collide with reserved
   prelinked indices.
8. The raw engine does not install Effect-specific modules.
9. Cargo builds and runtime execution require no package-manager or network
   access.
10. Generation from the pinned assets is byte-for-byte deterministic.
11. Corrupt, stale, incompatible, or internally inconsistent bundles fail
    before any module initializer executes.

## Final bundle model

The existing versioned bytecode envelope should be extended or wrapped with a
prelinked graph payload. Conceptually:

```rust
struct PrelinkedModuleGraph {
  format_version: u16,
  engine_bytecode_version: BytecodeVersion,
  package_name: String,
  package_version: String,
  source_manifest_hash: [u8; 32],
  modules: Vec<PrelinkedModule>,
  binding_count: u32,
  public_roots: Vec<ModuleIndex>,
}

struct PrelinkedModule {
  specifier: String,
  module_index: ModuleIndex,
  source_sha256: [u8; 32],
  dependencies: Vec<ModuleIndex>,
  exports: Vec<PrelinkedExport>,
  imports: Vec<PrelinkedImport>,
  chunk: Chunk,
}

struct PrelinkedExport {
  exported_name: String,
  binding_id: BindingId,
}

struct PrelinkedImport {
  local_name: String,
  binding_id: BindingId,
}
```

The concrete wire types should use bounded portable integers and deterministic
ordering. Runtime `Rc`, `RefCell`, `Mutex`, `SharedValue`, `ParserSession`, and
raw pointers must never enter the serialized representation.

### Dependency edges

`dependencies` must preserve the source module-request order used by module
evaluation. It must include dependencies introduced by:

- ordinary imports;
- side-effect imports;
- named re-exports;
- namespace re-exports; and
- export-all declarations.

The generated Effect graph is static. Dynamic imports must not silently become
untracked graph edges; generation should reject them unless an explicit future
bundle contract supports them.

### Canonical binding IDs

A binding ID identifies one live ECMAScript module binding. Multiple export
names or modules may reference the same binding ID through re-exports.

During generation:

1. construct and instantiate the complete source graph;
2. sort export candidates by `(module_index, exported_name)`;
3. identify shared source-instantiation slots by pointer identity;
4. assign the next binding ID on first deterministic encounter; and
5. serialize only the resulting integer IDs.

Pointer identity is a generation-time observation only. No address is written
to the artifact.

During runtime installation:

1. allocate exactly `binding_count` shared `Value::Undefined` slots;
2. map every export name to its binding ID's slot;
3. map every import local name to the same slot; and
4. build namespace objects from those export maps.

This preserves cycles and live re-exports without AST-based linking.

### Module indices

Effect module indices must be deterministic and reserved as a range before any
ordinary source module receives an index. The simplest first format requires
Effect modules to use contiguous indices beginning at zero.

Ordinary source modules then begin at `max_reserved_index + 1`, even if only a
subset of Effect modules has been constructed. Late dynamic import of Effect
must not collide with an index previously assigned to a user module.

### Source and debugging data

Runtime parsing must not require source text. `Chunk::source_text` may remain in
the bundle for stack traces and diagnostics, but it is debug data only.

A later size optimization may store source once per module or replace full
source with line tables and a content-addressed optional debug section. That
optimization must not be mixed into the initial correctness milestone.

## Engine changes

### 1. Represent source-backed and prelinked records explicitly

Replace the assumption that every `ModuleRecord` owns a `ParserSession` with an
explicit payload distinction, for example:

```rust
enum ModuleRecordPayload {
  Source(ParserSession),
  Prelinked {
    dependencies: Vec<usize>,
  },
}
```

Do not create an empty or fake parser session for prelinked records. Compiler
and typechecker access must be possible only for the source-backed variant.

### 2. Add a prelinked graph registry

`ModuleLoader` needs a host-neutral registration API for a validated
`PrelinkedModuleGraph`. Registration should:

- reject duplicate specifiers and module indices;
- reserve the complete module-index range;
- retain lookup by canonical specifier and index; and
- avoid constructing runtime slots until the graph is needed by a VM realm.

The engine API must remain Effect-agnostic. Effect-specific package/version
checks belong in `blitz-runtime`.

### 3. Construct records without parsing

When resolution finds a prelinked module, construction should:

- create the module record directly from bundle metadata;
- insert it before walking dependencies so cycles reuse the same record;
- recursively construct bundled dependency indices; and
- never call `Parser::parse_source_with_type` or import extraction.

A prelinked module may depend only on another module in the same validated
prelinked graph for the first format. Mixed dependencies from prelinked code to
runtime source modules should be rejected during generation.

Source modules may import prelinked Effect modules normally.

### 4. Instantiate from binding metadata

`instantiate_all` should separate source and prelinked paths:

- source records retain the existing parser-based behavior;
- prelinked records install export/import maps from binding IDs;
- all prelinked slots are allocated before any prelinked record is marked
  instantiated; and
- mixed source imports resolve against already-created prelinked export slots.

No ESTree JSON generation or parsing should occur for a fully prelinked record.

### 5. Evaluate from bundled dependency edges

Evaluation-order traversal must use bundled dependency indices for prelinked
records and parsed requests for source records. Existing cycle handling and
module states remain authoritative.

All compilation queues must skip prelinked records. `Vm::run` remains on the
calling VM thread.

## Bundle validation

Validation must complete before records are installed or JavaScript executes.
It should reject:

- unsupported container or bytecode versions;
- invalid checksum or payload length;
- duplicate specifiers, indices, or root declarations;
- non-contiguous or out-of-range indices in the initial format;
- out-of-range dependency or binding IDs;
- duplicate export names within one module;
- imports referencing absent bindings;
- unsupported dynamic imports;
- invalid bytecode chunks;
- bytecode module descriptors targeting absent modules or exports;
- source hashes that disagree with the pinned manifest; and
- package/version metadata other than the runtime-supported Effect release.

Validation limits must bound payload size, module count, bindings, dependency
edges, names, source bytes, chunk depth, and total bytecode.

### Bytecode descriptor validation

The validator should inspect module-access instructions and their descriptor
constants. Every encoded `(module_index, export_name)` target must exist in the
prelinked graph. This ensures bytecode and graph metadata cannot drift while
still passing independent structural checks.

## Generator changes

Create or evolve `tools/effect-bytecode` into the sole Rust graph precompiler.
It should:

1. read the pinned Effect manifest and generated ESM files;
2. verify source hashes before compilation;
3. register every canonical module source;
4. construct every public root and its closed static graph;
5. instantiate the full graph with deterministic indices;
6. capture ordered dependency edges;
7. derive canonical binding IDs from instantiated live slots;
8. compile all modules, using bounded parallel workers where beneficial;
9. clear type metadata once during generation;
10. verify every chunk and module descriptor;
11. encode the final prelinked graph; and
12. atomically write the committed artifact.

`--check` must regenerate in memory and byte-compare against the committed
bundle. Generation must be deterministic across repeated processes, not only
within one process.

The TypeScript asset generator remains responsible for producing canonical
source and declarations. The Rust precompiler runs after source generation.
Neither runs from `build.rs`.

## Runtime integration

`blitz-runtime` should embed the final graph with `include_bytes!` and decode it
once with `OnceLock` or an equivalent immutable process cache.

Installation should:

1. validate the generic bundle in `blitz-engine`;
2. validate Effect package/version and manifest hashes in `blitz-runtime`;
3. register the prelinked graph into the VM's module loader; and
4. register Blitz-owned source facades separately.

The finalized runtime path must not register Effect modules through
`VirtualModule::javascript` or the transitional source-backed
`VirtualModule::precompiled` path.

## Testing strategy

### Codec tests

- deterministic round trip preserving all fields;
- unsupported version rejection;
- checksum and truncation rejection;
- defensive limit enforcement;
- invalid dependency, binding, module, and descriptor rejection;
- nested chunk and source sharing preservation where applicable.

### Engine graph tests

Use small synthetic bundles covering:

- a direct import;
- a side-effect-only dependency;
- a cycle with reads after initialization;
- named re-export aliases sharing one slot;
- export-all resolution;
- namespace import identity;
- source module importing a prelinked module;
- prelinked range reservation before and after source instantiation;
- late dynamic import without module-index collision; and
- deterministic evaluation order.

At least one test must make the bundled bytecode observably differ from any
retained debug source, proving that execution does not compile or run source.

### No-parser/no-compiler regression

Add test-only parser and compiler counters or explicit hooks. The Effect smoke
test must assert that constructing, instantiating, and evaluating the bundled
Effect graph performs:

- zero Effect parser invocations;
- zero Effect import-extraction parses;
- zero Effect compiler invocations; and
- zero Effect typechecker invocations.

Do not infer this only from elapsed time.

### Runtime tests

- bundle entries and manifest entries match exactly;
- synchronous `Effect.runFork` still observes `42`;
- root and deep imports share runtime identities;
- corruption fails before module side effects;
- raw `blitz-engine` still lacks Effect modules;
- Blitz source facades can import prelinked Effect exports;
- IRC chat v2 starts and serves `/health` with the final bundle.

## Performance measurement

Add phase timing behind an opt-in diagnostic flag, covering:

- bundle decode and validation;
- record construction;
- slot allocation and linking;
- module evaluation; and
- event-loop/server readiness.

Record a baseline before replacing the transitional bundle. Acceptance for the
full-prelink milestone is based on behavior, not a fixed machine-specific time:

- Effect parse time is zero;
- Effect compile time is zero;
- Effect AST/ESTree linking time is zero; and
- server readiness improves measurably in repeated warm and cold runs.

If evaluation dominates afterward, report it separately rather than claiming
that prebundling can eliminate it.

## Migration phases

### Phase 1: Graph metadata model — complete

- [x] Add portable prelinked graph and binding metadata types.
- [x] Add deterministic dependency capture to source module construction.
- [x] Add validation independent of Effect.
- [x] Add synthetic graph tests.

### Phase 2: Parser-free module records — complete

- [x] Introduce explicit source/prelinked record payloads.
- [x] Construct prelinked dependency graphs without parser sessions.
- [x] Reserve module indices globally.
- [x] Instantiate live slots from canonical binding IDs.
- [x] Evaluate through bundled dependency edges.

### Phase 3: Full Effect generator

- Emit dependency, binding, root, hash, and bytecode metadata.
- Add descriptor validation.
- Prove cross-process deterministic output.
- Generate the final artifact under
  `crates/blitz-runtime/assets/effect/3.16.7/`.

### Phase 4: Runtime cutover

- Embed and cache the final graph bundle.
- Remove source-backed Effect registration.
- Keep Blitz facades source-backed.
- Add no-parser/no-compiler and identity regressions.

### Phase 5: Remove transitional work

- Delete the bytecode-only Effect artifact and transitional generator behavior.
- Remove documentation claiming that bytecode-only caching is full
  precompilation.
- Ensure asset verification understands only the final artifact.
- Commit the final bundle together with generator, runtime loader, tests, and
  documentation.

## Commit strategy

Keep the large generated artifact out of Git until the format and parser-free
runtime path are complete.

Recommended commits:

1. engine prelinked graph model and validation;
2. parser-free construction, instantiation, and evaluation;
3. deterministic Effect graph generator and verifier;
4. runtime cutover plus final committed bundle; and
5. documentation and measured startup results.

Every commit should pass formatting, workspace checks, relevant tests, Clippy,
`git diff --check`, and raw-engine isolation tests.

## Completion criteria

The work is complete when:

- the committed Effect artifact is reproducible byte-for-byte;
- no Effect source is parsed, import-scanned, linked through ESTree, typechecked,
  or compiled at runtime;
- live bindings, cycles, re-exports, namespaces, and shared identities pass;
- bytecode and graph metadata are validated before side effects;
- source and late dynamic modules cannot collide with Effect indices;
- synchronous Effect execution and IRC chat v2 still work;
- the raw engine remains Effect-agnostic; and
- documentation accurately distinguishes prelinked bytecode from a heap
  snapshot.
