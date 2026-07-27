# Blitz CLI surface

This document maps Bun 1.3's command-line surface to Blitz. It is a product
map, not a promise that unsupported commands are accepted. `blitz --help`
should only advertise commands that work.

## Command map

| Bun command | Blitz command | Status | Owning layer |
|---|---|---:|---|
| `bun run <file>` | `blitz run <file>` or `blitz <file>` | Available | CLI orchestration, Blitz runtime execution |
| `bun run <script>` | `blitz run <script>` | Planned | CLI and future package-script service |
| `bun repl` | `blitz repl` or `blitz` | Available | Hosted REPL in `blitz-runtime`, launched by CLI |
| `bun test` | `blitz test` | Planned | Future user test runner; not the Test262 harness |
| `bun x` | `blitz x` | Planned | Future package manager and process launcher |
| `bun exec` | `blitz exec` | Deferred | Future shell/process tooling |
| `bun install` / `i` | `blitz install` / `i` | Planned | Future package manager |
| `bun add` / `a` | `blitz add` / `a` | Planned | Future package manager |
| `bun remove` / `rm` | `blitz remove` / `rm` | Planned | Future package manager |
| `bun update` | `blitz update` | Planned | Future package manager |
| `bun audit` | `blitz audit` | Planned | Future lockfile and advisory service |
| `bun outdated` | `blitz outdated` | Planned | Future package manager |
| `bun link` / `unlink` | Same names | Planned | Future package manager |
| `bun publish` | `blitz publish` | Planned | Future registry client |
| `bun patch` | `blitz patch` | Deferred | Future package manager |
| `bun pm ...` | `blitz pm ...` | Planned | Lockfile/cache/bin/pack utilities |
| `bun info` | `blitz info` | Planned | Future registry client |
| `bun why` | `blitz why` | Planned | Future dependency graph tooling |
| `bun build` | `blitz build` | Planned | Future bundler/compiler frontend |
| `bun init` | `blitz init` | Planned | Project scaffolding |
| `bun create` | `blitz create` | Deferred | Template acquisition requires package tooling |
| `bun upgrade` | `blitz upgrade` | Distribution-dependent | Release installer, not runtime semantics |
| `bun feedback` | No direct mapping | Not planned | Use the project issue tracker |

Blitz-specific commands remain first-class:

- `blitz check` — in-process TypeScript checking.
- `blitz analyze` — machine-readable editor analysis.

## Runtime flag map

| Bun flag | Blitz direction | Priority |
|---|---|---:|
| `-e, --eval` | Evaluate hosted script source | High |
| `-p, --print` | Evaluate and inspect the completion value | High |
| `-r, --preload`; `--require`; `--import` | Preload modules before the entry point | High |
| `--cwd` | Select resolution and process working directory | High |
| `--env-file`; `--no-env-file` | Explicit environment loading policy | High |
| `--watch`; `--hot`; `--no-clear-screen` | Restart/reload supervisor | Medium |
| `--conditions` | Additional package export conditions | Medium |
| `--if-present` | Package-script convenience | Medium |
| `--install`; `--no-install`; `-i` | Package-manager integration | After install support |
| `--inspect*` | Denko debugger integration | After debugger protocol support |
| `--cpu-prof*`; `--heap-prof*` | Denko profiling integration | After profiler support |
| `--port` | Map to the runtime's default listen address | Medium |
| `--console-depth` | Runtime inspection policy | Medium |
| `--unhandled-rejections` | Runtime rejection policy | Medium |
| `--filter`; `--workspaces`; parallel modes | Workspace script execution | After package scripts |
| TLS, DNS, fetch, and user-agent flags | Host networking configuration | As corresponding APIs mature |
| V8/Bun-specific flags such as `--bun` | No compatibility requirement | None |

`--node-compat=off|core|tooling|full` is a Blitz-specific hosted-runtime flag
and should remain global.

## Implementation order

1. Finish the execution shell: explicit `repl`, `--eval`, `--print`, preload,
   working-directory, environment-file, and package arguments.
2. Add package.json script discovery so `blitz run <name>` is useful without
   conflating missing source files with scripts silently.
3. Build a user test runner independently from Test262 infrastructure.
4. Add package installation and lockfile primitives, then expose the package
   command family and `x`.
5. Add bundling/build commands only after output format and module-resolution
   contracts are stable.

Command parsing and presentation belong to `blitz-cli`. Hosted execution and
REPL capabilities belong to `blitz-runtime`. ECMAScript behavior and debugger
or profiler primitives belong to Denko.
