# Blitz CLI

`blitz-cli` owns command-line orchestration and interactive presentation.

- Make the CLI and REPL useful for developers and end users through history, diagnostics, value inspection, and clearly out-of-band REPL commands.
- REPL formatting may be richer than ECMAScript `ToString`, but it must not mutate the value, execute user code, or change evaluation semantics.
- Keep REPL commands distinct from JavaScript evaluation. A name evaluated as JavaScript should follow normal identifier resolution unless the CLI recognized the input as an explicit REPL command first.
- Preserve the difference between raw-engine and hosted-runtime execution modes.
- Report engine errors and completion values accurately. Do not rewrite source or results to hide engine correctness problems.
- Put semantic fixes in `denko-engine` and host API behavior in `blitz-runtime`; keep this crate focused on orchestration and presentation.
