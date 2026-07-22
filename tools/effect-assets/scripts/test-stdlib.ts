import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

import { DEFAULT_OUTPUT, filesUnder, readManifest, ROOT, verifyManifest, type HostPrimitive, type StdlibManifest } from "./stdlib.ts";

const assert = (condition: unknown, message: string) => { if (!condition) throw new Error(message); };
type MutableManifest = {
  readonly format: number;
  readonly package: StdlibManifest["package"];
  host_primitive_catalog: HostPrimitive[];
  readonly entries: Array<{
    specifier: string;
    visibility: "public" | "private";
    source: { path: string; sha256: string };
    declaration?: { path: string; sha256: string };
    dependencies: readonly string[];
    host_primitives: readonly HostPrimitive[];
  }>;
  readonly files: StdlibManifest["files"];
};
const rejects = (operation: () => void, message: string) => {
  try {
    operation();
  } catch {
    return;
  }
  throw new Error(message);
};
const cloneManifest = (): MutableManifest => JSON.parse(JSON.stringify(readManifest(DEFAULT_OUTPUT))) as MutableManifest;

const testManifestValidation = () => {
  const duplicate = cloneManifest();
  duplicate.entries.push(duplicate.entries[0]);
  rejects(() => verifyManifest(DEFAULT_OUTPUT, duplicate), "duplicate stdlib specifiers must be rejected");

  const invalidVisibility = cloneManifest();
  const app = invalidVisibility.entries.find(entry => entry.specifier === "blitz:app");
  if (app === undefined) throw new Error("blitz:app is present for visibility testing");
  app.visibility = "private";
  rejects(() => verifyManifest(DEFAULT_OUTPUT, invalidVisibility), "invalid stdlib visibility must be rejected");

  const staleHash = cloneManifest();
  const match = staleHash.entries.find(entry => entry.specifier === "blitz:match");
  if (match === undefined) throw new Error("blitz:match is present for hash testing");
  match.source.sha256 = "0".repeat(64);
  rejects(() => verifyManifest(DEFAULT_OUTPUT, staleHash), "stale stdlib hashes must be rejected");

  const missingHostPrimitive = cloneManifest();
  missingHostPrimitive.host_primitive_catalog.splice(0, 1);
  rejects(() => verifyManifest(DEFAULT_OUTPUT, missingHostPrimitive), "missing host primitives must be rejected");
};

const compareTrees = (left: string, right: string) => {
  const leftFiles = filesUnder(left);
  const rightFiles = filesUnder(right);
  assert(leftFiles.join("\n") === rightFiles.join("\n"), "stdlib generation is not deterministic");
  for (const file of leftFiles) assert(
    readFileSync(join(left, file)).equals(readFileSync(join(right, file))),
    `stdlib generation differs for ${file}`,
  );
};

const testDeterminism = () => {
  const first = mkdtempSync(join(tmpdir(), "blitz-stdlib-test-first-"));
  const second = mkdtempSync(join(tmpdir(), "blitz-stdlib-test-second-"));
  try {
    for (const output of [first, second]) {
      const result = spawnSync(process.execPath, ["--experimental-strip-types", "scripts/generate-stdlib.ts"], {
        cwd: ROOT,
        env: { ...process.env, BLITZ_STDLIB_OUTPUT: output },
        encoding: "utf8",
      });
      assert(result.status === 0, `stdlib generation failed:\n${result.stdout}\n${result.stderr}`);
    }
    compareTrees(first, second);
  } finally {
    rmSync(first, { recursive: true, force: true });
    rmSync(second, { recursive: true, force: true });
  }
};

testManifestValidation();
testDeterminism();
console.log("Blitz stdlib asset tests passed: duplicates, visibility, stale hashes, and deterministic output are covered.");
