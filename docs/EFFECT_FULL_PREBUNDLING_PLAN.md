# Full Effect and Blitz stdlib graph prebundling plan

## Status

Implemented through runtime cutover. The committed versioned runtime graph
contains the canonical Effect graph plus every manifest-listed public and
private Blitz stdlib JavaScript module. Engine records are parser-free, indices
and live bindings are prelinked, visibility is enforced by public roots, and
runtime installation validates pinned manifests and host ABI before atomic
registration.

The transitional bytecode-only `effect.blitzbc` experiment has been removed.
The final `runtime.blitzbundle` contains graph metadata and verified chunks;
it is not a heap snapshot, so module initializers still execute at startup.
Startup counters distinguish user source work and assert zero bundled parsing,
import extraction, AST/ESTree linking, typechecking, and compilation.

## Objective

Produce one deterministic, committed, network-free runtime artifact containing
everything required to instantiate and execute the supported Effect 3.16.7
graph and stable Blitz stdlib JavaScript modules without parsing or compiling
any bundled JavaScript at runtime.

At runtime Blitz should only:

1. decode and validate the embedded graph bundle;
2. allocate live module-binding slots;
3. install module records and export maps;
4. traverse the bundled dependency graph in specification order; and
5. execute verified bytecode on the single VM thread.

User application modules may continue through the ordinary source parser,
linker, and compiler. Stable `blitz:*` facades and `blitz-internal:*`
JavaScript bridge adapters are part of the prelinked runtime graph. JavaScript
evaluation remains single-threaded.

## Non-goals

- Combining Effect into one JavaScript module.
- Bundling a second copy of Effect for Blitz stdlib modules.
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
- **Prelinked runtime graph bundle**: bytecode plus dependency, module, import,
  export, canonical live-binding, Effect provenance, stdlib manifest, and host
  ABI metadata. It requires no bundled-source parsing.
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
8. The raw engine does not install Effect- or Blitz-specific modules.
9. Every manifest-listed public `blitz:*` facade and private
   `blitz-internal:*` JavaScript adapter is in the same graph as Effect and
   cannot introduce a second Effect identity. “Bundled” means exactly the
   manifest entries; no unmanifested module may appear in the artifact.
10. Rust host primitives are installed before graph evaluation and are matched
    by an explicit host ABI version.
11. Cargo builds and runtime execution require no package-manager or network
    access.
12. Generation from all pinned Effect and stdlib inputs is byte-for-byte
    deterministic.
13. Corrupt, stale, incompatible, or internally inconsistent bundles fail
    before any module initializer executes.

## Final bundle model

The existing versioned bytecode envelope should be replaced by a runtime
wrapper around the host-neutral engine graph. The wrapper owns package
provenance and host compatibility while `PrelinkedModuleGraph` remains generic.
Conceptually:

```rust
struct RuntimePrecompiledBundle {
  format_version: u16,
  runtime_abi_version: u16,
  host_bridge_abi_version: u16,
  effect_version: String,
  effect_manifest_hash: [u8; 32],
  stdlib_manifest_hash: [u8; 32],
  // Public specifiers correspond exactly to graph.public_roots. All other
  // manifest entries are internal-only and reachable only by bundled edges.
  graph: PrelinkedModuleGraph,
}

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

The generated runtime graph is static. Dynamic imports must not silently become
untracked graph edges; generation should reject them unless an explicit future
bundle contract supports them.

Every bundled JavaScript dependency must resolve inside this graph. Private
bridge adapters access Rust-installed host primitives through the versioned host
ABI; they must not create untracked source-module dependencies. Support for
external prelinked graphs or native-module imports is a future format change,
not an implicit fallback.

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

Bundled runtime module indices must be deterministic and reserved as a range
before any ordinary source module receives an index. The first format requires
all Effect and Blitz stdlib graph modules to use contiguous indices beginning
at zero.

Ordinary source modules then begin at `max_reserved_index + 1`, even if only a
subset of runtime modules has been constructed. Late imports of Effect or Blitz
stdlib modules must not collide with an index previously assigned to a user
module.

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
- retain every module by index for bundled dependency traversal;
- expose canonical-specifier lookup to source importers only for
  `public_roots`; private modules remain constructible only through bundled
  dependency indices; and
- avoid constructing runtime slots until the graph is needed by a VM realm.

The engine API must remain Effect-agnostic and may support multiple generic
graphs for other embedders. `blitz-runtime` enforces its stronger policy: one
runtime graph registration per VM, with a second runtime-graph installation
rejected before mutation. Every dependency edge remains within its owning
graph.

### 3. Construct records without parsing

When resolution finds a prelinked module, construction should:

- create the module record directly from bundle metadata;
- insert it before walking dependencies so cycles reuse the same record;
- recursively construct bundled dependency indices; and
- never call `Parser::parse_source_with_type` or import extraction.

A prelinked module may depend only on another module in the same validated
prelinked graph for the first format. Mixed dependencies from prelinked code to
runtime source modules should be rejected during generation.

User source modules may import only prelinked modules listed in
`public_roots`. Bundled modules traverse private dependencies by module index;
direct source imports of non-root specifiers fail even when the module exists
in the graph. This public-root capability is the private-resolution enforcement
mechanism and does not depend on a forgeable importer string.

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
constants. Cross-module `(module_index, export_name)` targets must name a
bundled dependency export. Owner-module descriptors may name compiler-generated
module-private top-level bindings; those remain outside the public export map
and namespace object. Writes may never target another module or the synthetic
`*` namespace. This keeps bytecode and graph metadata aligned without exposing
module-private closure state.

## Generator changes

Create or evolve `crates/blitz-precompiler` into the sole Rust runtime-graph
precompiler. It should:

1. read the pinned Effect manifest, generated ESM files, and Blitz stdlib
   manifest;
2. verify every Effect, public facade, private adapter, declaration, and license
   hash before compilation;
3. reject duplicate specifiers and invalid public/private visibility metadata;
4. register every canonical Effect and Blitz JavaScript module source;
5. construct every public and private runtime root and its closed static graph;
6. reject dependencies outside that graph and unsupported dynamic imports;
7. instantiate the full graph with deterministic indices;
8. capture ordered dependency edges;
9. derive canonical binding IDs from instantiated live slots;
10. compile all modules, using bounded parallel workers where beneficial;
11. clear type metadata once during generation;
12. verify every chunk and module descriptor;
13. encode Effect provenance, stdlib manifest hash, and host ABI compatibility
    with the final prelinked graph; and
14. atomically write the committed artifact.

`--check` must regenerate in memory and byte-compare against the committed
bundle. Generation must be deterministic across repeated processes, not only
within one process. The precompiler is a checked workspace tool with an
explicit output path consumed by `include_bytes!`; its crate, CLI contract, and
artifact location land before runtime cutover.

The TypeScript asset generator remains responsible for producing canonical
Effect source and declarations. A data-driven stdlib asset step owns Blitz
facade/adapter source, declarations, visibility, hashes, and host ABI metadata.
The Rust precompiler runs only after both input manifests verify. None runs from
`build.rs`.

## Runtime integration

`blitz-runtime` should embed the final graph with `include_bytes!` and decode it
once with `OnceLock` or an equivalent immutable process cache.

Installation should:

1. decode and validate the complete wrapper without installing records;
2. validate the generic graph in `denko-engine`;
3. validate Effect version/hash, stdlib manifest hash, runtime ABI, and host
   bridge ABI in `blitz-runtime`;
4. install all required Rust host primitives;
5. atomically register the one prelinked runtime graph together with generated
   declarations for public roots; and
6. expose only manifest-public specifiers to user resolution.

These steps are atomic: any duplicate, missing bridge, visibility, ABI, hash,
or graph failure occurs before records are visible or initializers execute.
The finalized runtime path must not register bundled Effect or Blitz stdlib
modules through `VirtualModule::javascript` or the transitional source-backed
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

Add test-only parser, import-extraction, typechecker, and compiler counters or
explicit hooks. Runtime smoke tests must distinguish user-module work from the
reserved runtime graph and assert that constructing, instantiating, and
evaluating all bundled Effect and Blitz stdlib modules performs:

- zero bundled-module parser invocations;
- zero bundled-module import-extraction parses;
- zero bundled-module compiler invocations; and
- zero bundled-module typechecker invocations.

Do not infer this only from elapsed time.

### Runtime tests

- Effect and stdlib bundle entries match their manifests exactly;
- public `blitz:*` entries resolve and private `blitz-internal:*` entries are
  rejected by user resolution;
- duplicate specifiers, a second runtime-graph installation, missing host
  primitives, and Effect/runtime/host ABI mismatches fail atomically before side
  effects;
- synchronous `Effect.runFork` still observes `42`;
- root and deep imports share runtime identities;
- corruption fails before module side effects;
- raw `denko-engine` still lacks Effect and Blitz stdlib modules;
- prelinked Blitz facades import the canonical prelinked Effect exports;
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

- bundled Effect and stdlib parse time is zero;
- bundled Effect and stdlib compile time is zero;
- bundled Effect and stdlib AST/ESTree linking time is zero; and
- server readiness improves measurably in repeated warm and cold runs.

If evaluation dominates afterward, report it separately rather than claiming
that prebundling can eliminate it.

### Current result

The first committed runtime graph is 37,246,595 bytes including retained debug
source. On the implementation machine, three warm CLI runs of the synchronous
Effect `42` smoke took 6.40–7.00 seconds end to end. With
`BLITZ_RUNTIME_TIMINGS=1`, one representative run reported 3.42 seconds for
decode plus generic validation, 158 milliseconds for pinned runtime/host policy
validation, and 47 milliseconds for graph/declaration registration; the
remaining time is primarily module initialization. Pipeline counters confirm
that bundled parsing, import extraction, AST/ESTree linking, typechecking, and
compilation are zero. This is a correctness milestone, not a claim that
initializer or artifact-size optimization is complete.

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

### Phase 3: Full runtime-graph generator — complete

- Establish the data-driven Blitz stdlib manifest, visibility model, and host
  ABI before adding new stdlib modules.
- Emit dependency, binding, root, hash, visibility, provenance, ABI, and
  bytecode metadata for Effect plus Blitz stdlib JavaScript.
- Add descriptor and closed-graph validation.
- Prove cross-process deterministic output.
- Generate the final artifact under a versioned `blitz-runtime` asset path.

### Phase 4: Runtime cutover — complete

- Embed and cache the final runtime graph bundle.
- Install host primitives before atomic graph registration.
- Remove source-backed Effect and Blitz stdlib registration.
- Enforce public/private resolution visibility.
- Add no-parser/no-compiler, ABI, atomic-failure, and identity regressions.

### Phase 5: Remove transitional work — complete

- Delete the bytecode-only Effect artifact and transitional generator behavior.
- Delete duplicated Rust facade source/constants and source-backed stdlib
  registration.
- Remove documentation claiming that bytecode-only caching is full
  precompilation.
- Ensure asset verification understands only the final runtime artifact.
- Commit the final bundle together with generator, runtime loader, tests, and
  documentation.

## Commit strategy

Keep the large generated artifact out of Git until the format and parser-free
runtime path are complete.

Recommended commits:

1. engine prelinked graph model and validation;
2. parser-free construction, instantiation, and evaluation;
3. deterministic runtime graph and stdlib-manifest generator and verifier;
4. runtime cutover plus final committed bundle; and
5. documentation and measured startup results.

Every commit should pass formatting, workspace checks, relevant tests, Clippy,
`git diff --check`, and raw-engine isolation tests.

## Completion criteria

The work is complete when:

- the committed runtime graph artifact is reproducible byte-for-byte;
- no bundled Effect or Blitz stdlib source is parsed, import-scanned, linked
  through ESTree, typechecked, or compiled at runtime;
- live bindings, cycles, re-exports, namespaces, and shared identities pass;
- bytecode and graph metadata are validated before side effects;
- source and late dynamic modules cannot collide with Effect indices;
- synchronous Effect execution and IRC chat v2 still work;
- public/private visibility, singleton runtime-graph installation, and
  Effect/runtime/host ABI checks fail atomically;
- the raw engine remains Effect- and Blitz-agnostic; and
- documentation accurately distinguishes prelinked bytecode from a heap
  snapshot.
