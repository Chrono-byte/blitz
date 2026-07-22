import { createHash } from "node:crypto";
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import * as ts from "typescript";

const ROOT = resolve(import.meta.dirname, "..");
const ASSETS = resolve(ROOT, "../../crates/blitz-runtime/assets/effect/3.16.7");
const EXPECTED_VERSION = "3.16.7";
const SUPPORTED = ["effect", "effect/Effect", "effect/Context", "effect/Layer", "effect/Stream", "effect/Scope", "effect/Schedule", "effect/Match"];

const hash = (file: string) => createHash("sha256").update(readFileSync(file)).digest("hex");
const filesUnder = (root: string, current = root): string[] => readdirSync(current, { withFileTypes: true }).flatMap(entry => {
  const path = join(current, entry.name);
  return entry.isDirectory() ? filesUnder(root, path) : [relative(root, path).split("\\").join("/")];
}).sort();
const fail = (message: string): never => { throw new Error(message); };

const staticImports = (source: string, file: string) => {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, file.endsWith(".d.ts") ? ts.ScriptKind.TS : ts.ScriptKind.JS);
  const values: string[] = [];
  const visit = (node: ts.Node) => {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) values.push(node.moduleSpecifier.text);
    if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) fail(`dynamic import in ${file}`);
    ts.forEachChild(node, visit);
  };
  visit(parsed);
  return values;
};

const verifyManifest = () => {
  const manifest = JSON.parse(readFileSync(join(ASSETS, "manifest.json"), "utf8")) as {
    format: number;
    package: { name: string; version: string; sourceIntegrity: string };
    entries: { specifier: string; source: { path: string; sha256: string }; declaration?: { path: string; sha256: string } }[];
    files: { path: string; sha256: string }[];
  };
  if (manifest.format !== 1 || manifest.package.name !== "effect" || manifest.package.version !== EXPECTED_VERSION) fail("manifest package metadata is not pinned");
  if (!manifest.package.sourceIntegrity.startsWith("sha512-")) fail("manifest has no package integrity");
  const entries = new Map(manifest.entries.map(entry => [entry.specifier, entry]));
  if (entries.size !== manifest.entries.length) fail("manifest contains duplicate specifiers");
  for (const specifier of SUPPORTED) if (!entries.has(specifier)) fail(`missing supported entry ${specifier}`);
  const listed = new Map(manifest.files.map(file => [file.path, file]));
  if (listed.size !== manifest.files.length) fail("manifest contains duplicate files");
  const actual = filesUnder(ASSETS).filter(path => path !== "manifest.json");
  if (actual.join("\n") !== [...listed.keys()].sort().join("\n")) fail("manifest file list is stale");
  for (const file of manifest.files) {
    const path = join(ASSETS, file.path);
    if (hash(path) !== file.sha256) fail(`hash mismatch: ${file.path}`);
  }
  for (const entry of manifest.entries) {
    for (const asset of [entry.source, entry.declaration].filter(asset => asset !== undefined)) {
      const file = listed.get(asset.path);
      if (!file || file.sha256 !== asset.sha256) fail(`entry hash is not backed by files: ${entry.specifier}`);
    }
    if (SUPPORTED.includes(entry.specifier) !== (entry.declaration !== undefined)) fail(`declarations must be curated public entries only: ${entry.specifier}`);
  }
  for (const path of actual.filter(path => path.endsWith(".js") || path.endsWith(".d.ts"))) {
    for (const specifier of staticImports(readFileSync(join(ASSETS, path), "utf8"), path)) {
      const publicImport = SUPPORTED.includes(specifier);
      if (!specifier.startsWith("blitz-internal:effect/") && !publicImport) fail(`import escapes canonical graph: ${path} -> ${specifier}`);
      if (!entries.has(specifier)) fail(`unresolved canonical import: ${path} -> ${specifier}`);
    }
  }
};

const compareTrees = (left: string, right: string) => {
  const leftFiles = filesUnder(left);
  const rightFiles = filesUnder(right);
  if (leftFiles.join("\n") !== rightFiles.join("\n")) fail("generated asset file list differs from committed output");
  for (const file of leftFiles) {
    if (!readFileSync(join(left, file)).equals(readFileSync(join(right, file)))) fail(`generated asset differs: ${file}`);
  }
};

const main = () => {
  verifyManifest();
  const temporary = mkdtempSync(join(tmpdir(), "blitz-effect-assets-"));
  try {
    const result = spawnSync(process.execPath, ["--experimental-strip-types", "scripts/generate.ts"], {
      cwd: ROOT,
      env: { ...process.env, EFFECT_ASSETS_OUTPUT: temporary },
      encoding: "utf8",
    });
    if (result.status !== 0) fail(`deterministic regeneration failed:\n${result.stdout}\n${result.stderr}`);
    compareTrees(ASSETS, temporary);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
  console.log("Effect assets verified: hashes, imports, supported entries, and deterministic regeneration are valid.");
};

main();
