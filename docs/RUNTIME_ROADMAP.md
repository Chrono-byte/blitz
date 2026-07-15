# Blitz Runtime Roadmap

Blitz is both an ECMAScript engine and, through its host layer, a standalone
JavaScript runtime. The engine implements language semantics. The runtime owns
operating-system capabilities, module loading policy, the event loop, developer
tooling, and compatibility APIs.

## Architecture

```text
`blitz` CLI (`crates/blitz-cli`)
  -> toolchain and package management
  -> host capability layer (`crates/blitz-runtime/src/host`)
    -> builtin modules and Web APIs
    -> async event loop and I/O drivers
    -> package and module resolution
  -> ECMAScript engine (`crates/blitz-engine`)
    -> parser and compiler
    -> VM, values, realms, and garbage collector
```

`blitz-engine` is the separate host-neutral executable for engine diagnostics
and conformance work. It does not install the host capability layer. Embedders
can continue to consume the `blitz` library without inheriting runtime policy.

Host capabilities must not be implemented as ECMAScript semantics. Embedders
should eventually be able to select the capabilities they install and apply a
permission policy without modifying the VM.

## Compatibility Direction

The runtime uses standards-based Web APIs where they are portable and supports
the most useful Node.js APIs for existing applications. Node compatibility is
measured behavior rather than branding: `node:` builtin modules, `process`,
CommonJS, package exports, and event-loop ordering each need focused fixtures.

## Milestones

### 0. Host foundation

- [x] Dedicated host-runtime boundary
- [x] `console` output, diagnostics, counters, timers, and grouping
- [x] `process.argv`, `process.env`, `process.cwd()`, platform, architecture,
  executable path, and Blitz version metadata
- [x] CLI script arguments
- [ ] Host capability and permission configuration

### 1. Local applications

- Builtin-module registry for `node:` specifiers
- `node:path`, `node:fs`, `node:fs/promises`, `node:os`, and `node:util`
- File URLs and `import.meta.url`
- Correct `.js`, `.mjs`, `.cjs`, and `.json` source classification
- `package.json` `type`, `main`, `imports`, and `exports`
- CommonJS wrapper, `require()`, module cache, and ESM interoperation

The milestone is complete when a multi-file application can read configuration,
load a local dependency through both ESM and CommonJS, and receive command-line
arguments without source changes.

### 2. Async runtime

- Runtime-owned event loop with explicit liveness
- `setTimeout`, `setInterval`, `setImmediate`, and cancellation
- Microtask checkpoints compatible with ECMAScript jobs
- Async filesystem operations backed by Tokio
- Signals and graceful shutdown

### 3. Packages

- Node-compatible `node_modules` traversal and conditional exports
- Workspace and symlink behavior
- Lockfile-independent execution of already installed packages
- Compatibility suite built from small package fixtures
- Package installation only after execution and resolution semantics stabilize

### 4. Network and Web platform

- `URL`, `URLSearchParams`, encoding, events, and abort signals
- `fetch`, `Headers`, `Request`, `Response`, and streams
- TCP, TLS, DNS, and an HTTP server API
- Web Crypto primitives

### 5. TypeScript and tooling

- Zero-configuration TypeScript stripping through Oxc
- `blitz check`, `fmt`, `lint`, `test`, and `bundle`
- Source maps and useful async stack traces
- Watch mode and inspector protocol

### 6. Security and distribution

- Filesystem, environment, network, subprocess, and FFI permissions
- Stable embedding API
- Cross-platform release binaries and installer
- Compatibility and performance dashboards

## Next Slice

Implement a builtin-module registry that resolves `node:path` without touching
the filesystem. Use it to ship `node:path` first, then reuse the registry for
`node:fs` and other native modules. This creates the correct seam before adding
more module-resolution branches.
