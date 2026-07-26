import { builtinModules } from "node:module";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { DEFAULT_OUTPUT, readManifest, ROOT } from "./stdlib.ts";

const fail = (message: string): never => { throw new Error(message); };
const matrixPath = resolve(ROOT, "../../docs/node-compat-support.json");
const matrix = JSON.parse(readFileSync(matrixPath, "utf8")) as {
  readonly referenceNodeVersion: string;
  readonly coverageMetric: { readonly inventoryTotal: number; readonly supportedRoots: number; readonly percent: number };
  readonly profiles: Readonly<Record<string, readonly string[]>>;
  readonly modules: readonly { readonly specifier: string; readonly status: string; readonly declaration: string | null }[];
};

if (process.versions.node !== matrix.referenceNodeVersion) fail(`Node compatibility verification requires ${matrix.referenceNodeVersion}, received ${process.versions.node}`);
const inventory = [...new Set(builtinModules
  .filter(specifier => !specifier.startsWith("_"))
  .map(specifier => specifier.startsWith("node:") ? specifier : `node:${specifier}`))].sort();
if (inventory.length !== matrix.coverageMetric.inventoryTotal) fail("Node public-root inventory count is stale");
const matrixInventory = matrix.modules.map(module => module.specifier).sort();
if (matrixInventory.join("\n") !== inventory.join("\n")) fail("Node support matrix inventory differs from module.builtinModules");

const manifest = readManifest(DEFAULT_OUTPUT);
const publicRoots = manifest.entries
  .filter(entry => entry.visibility === "public" && entry.specifier.startsWith("node:"))
  .map(entry => entry.specifier)
  .sort();
const supported = matrix.modules
  .filter(module => module.status === "supported-slice")
  .map(module => module.specifier)
  .sort();
if (publicRoots.join("\n") !== supported.join("\n")) fail("Node support matrix and generated public roots differ");
if (supported.length !== matrix.coverageMetric.supportedRoots) fail("Node supported-root count is stale");
if (Number((supported.length * 100 / inventory.length).toFixed(2)) !== matrix.coverageMetric.percent) fail("Node root coverage percentage is stale");
for (const root of publicRoots) {
  const entry = manifest.entries.find(candidate => candidate.specifier === root);
  if (entry?.declaration === undefined) fail(`Node public root has no declaration: ${root}`);
}
for (const [profile, roots] of Object.entries(matrix.profiles)) {
  if (new Set(roots).size !== roots.length) fail(`Node profile contains duplicate roots: ${profile}`);
  for (const root of roots) if (!supported.includes(root)) fail(`Node profile ${profile} contains unsupported root ${root}`);
}
console.log(`Node compatibility matrix verified: ${supported.length}/${inventory.length} public roots (${matrix.coverageMetric.percent}%).`);
