import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "..");
const ASSETS = resolve(ROOT, "../../crates/blitz-runtime/assets/effect/3.16.7");
const supported = ["effect", "effect/Effect", "effect/Context", "effect/Layer", "effect/Stream", "effect/Scope", "effect/Schedule", "effect/Match"];
const manifest = JSON.parse(readFileSync(join(ASSETS, "manifest.json"), "utf8")) as {
  entries: { specifier: string; source: { path: string }; declaration?: { path: string } }[];
  package: { version: string };
};
const entries = new Map(manifest.entries.map(entry => [entry.specifier, entry]));
const assert = (condition: unknown, message: string) => { if (!condition) throw new Error(message); };
const canonicalImports = (source: string) => [...source.matchAll(/(?:from|import)\s*["'](blitz-internal:effect\/[^"']+)["']/g)].map(match => match[1]);

assert(manifest.package.version === "3.16.7", "the generated package version must be exact");
for (const specifier of supported) {
  const entry = entries.get(specifier);
  if (!entry) throw new Error(`missing supported entry ${specifier}`);
  const source = readFileSync(join(ASSETS, entry.source.path), "utf8");
  assert(source.includes("blitz-internal:effect/") || (specifier === "effect" && source.includes("effect/Effect")), `${specifier} is not a canonical alias`);
  assert(entry.declaration !== undefined, `${specifier} has no curated declaration`);
}
assert(entries.size > supported.length, "the supported aliases must point at generated internal modules");
for (const entry of manifest.entries.filter(entry => entry.specifier.startsWith("blitz-internal:effect/"))) {
  assert(entry.declaration === undefined, `${entry.specifier} must not embed an upstream declaration`);
}
for (const entry of manifest.entries) {
  const source = readFileSync(join(ASSETS, entry.source.path), "utf8");
  for (const imported of canonicalImports(source)) assert(entries.has(imported), `${entry.specifier} imports missing ${imported}`);
}
console.log(`Effect asset tests passed (${entries.size} manifest entries).`);
