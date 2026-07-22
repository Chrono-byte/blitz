import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

import { DEFAULT_OUTPUT, filesUnder, readManifest, ROOT, verifyManifest, type StdlibManifest } from "./stdlib.ts";

const fail = (message: string): never => { throw new Error(message); };
const compareTrees = (left: string, right: string) => {
  const leftFiles = filesUnder(left);
  const rightFiles = filesUnder(right);
  if (leftFiles.join("\n") !== rightFiles.join("\n")) fail("generated stdlib file list differs from committed output");
  for (const file of leftFiles) {
    if (!readFileSync(join(left, file)).equals(readFileSync(join(right, file)))) fail(`generated stdlib asset differs: ${file}`);
  }
};

const main = () => {
  verifyManifest(DEFAULT_OUTPUT);
  const temporary = mkdtempSync(join(tmpdir(), "blitz-stdlib-assets-"));
  try {
    const result = spawnSync(process.execPath, ["--experimental-strip-types", "scripts/generate-stdlib.ts"], {
      cwd: ROOT,
      env: { ...process.env, BLITZ_STDLIB_OUTPUT: temporary },
      encoding: "utf8",
    });
    if (result.status !== 0) fail(`deterministic stdlib regeneration failed:\n${result.stdout}\n${result.stderr}`);
    compareTrees(DEFAULT_OUTPUT, temporary);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
  console.log("Blitz stdlib assets verified: hashes, visibility, dependencies, ABI metadata, and deterministic regeneration are valid.");
};

main();
