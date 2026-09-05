import { resolve } from "node:path";

const manifest = resolve(import.meta.dirname, "../../../Cargo.toml");

// Cargo checks source freshness and respects CARGO_TARGET_DIR. An explicit
// override lets callers test a particular executable without recompilation.
export const blitzCommand = (args: readonly string[]) => {
  const binary = process.env.BLITZ_BIN;
  return binary
    ? { command: binary, args: [...args] }
    : {
        command: "cargo",
        args: [
          "run",
          "--quiet",
          "--locked",
          "--manifest-path",
          manifest,
          "-p",
          "blitz-cli",
          "--",
          ...args,
        ],
      };
};
