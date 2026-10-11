import { resolve } from "node:path";

const manifest = resolve(import.meta.dirname, "../../../Cargo.toml");

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
