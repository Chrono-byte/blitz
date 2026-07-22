import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import * as ts from "typescript";

import {
  EFFECT_VERSION,
  HOST_ABI_VERSION,
  RUNTIME_ABI_VERSION,
  STDLIB_FORMAT_VERSION,
  STDLIB_VERSION,
} from "../src/stdlib-manifest.ts";

export type AssetReference = {
  readonly path: string;
  readonly sha256: string;
};

export type HostPrimitive = {
  readonly name: string;
  readonly abi_version: number;
  readonly methods: readonly string[];
};

export type StdlibEntry = {
  readonly specifier: string;
  readonly visibility: "public" | "private";
  readonly source: AssetReference;
  readonly declaration?: AssetReference;
  readonly dependencies: readonly string[];
  readonly host_primitives: readonly HostPrimitive[];
};

export type StdlibManifest = {
  readonly format: number;
  readonly package: {
    readonly name: string;
    readonly version: string;
    readonly effect_version: string;
    readonly runtime_abi_version: number;
    readonly host_abi_version: number;
  };
  readonly host_primitive_catalog: readonly HostPrimitive[];
  readonly entries: readonly StdlibEntry[];
  readonly files: readonly AssetReference[];
};

export const ROOT = resolve(import.meta.dirname, "..");
export const EFFECT_ASSETS = resolve(ROOT, "../../crates/blitz-runtime/assets/effect", EFFECT_VERSION);
export const DEFAULT_OUTPUT = resolve(ROOT, "../../crates/blitz-runtime/assets/stdlib");
export const MANIFEST_NAME = "manifest.json";

export const normalize = (value: string) => value.replace(/\r\n?/g, "\n");
export const sha256 = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");

export const filesUnder = (root: string, current = root): string[] => readdirSync(current, { withFileTypes: true })
  .flatMap(entry => {
    const path = join(current, entry.name);
    return entry.isDirectory() ? filesUnder(root, path) : [relative(root, path).split(sep).join("/")];
  })
  .sort();

export const staticImports = (source: string, file: string): string[] => {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, file.endsWith(".d.ts") ? ts.ScriptKind.TS : ts.ScriptKind.JS);
  const values: string[] = [];
  const visit = (node: ts.Node) => {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      values.push(node.moduleSpecifier.text);
    }
    if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
      throw new Error(`dynamic import in ${file}`);
    }
    ts.forEachChild(node, visit);
  };
  visit(parsed);
  return values;
};

const fail = (message: string): never => { throw new Error(message); };
const unique = (values: readonly string[]) => [...new Set(values)];
const isSha256 = (value: string) => /^[0-9a-f]{64}$/.test(value);
const isSafeRelativePath = (value: string) => value.length > 0
  && !value.startsWith("/")
  && !value.split("/").includes("..")
  && !value.split("/").includes("");

const readJson = <T>(path: string) => JSON.parse(readFileSync(path, "utf8")) as T;

export const readManifest = (root: string): StdlibManifest => readJson<StdlibManifest>(join(root, MANIFEST_NAME));

const effectSpecifiers = (effectRoot: string) => {
  const manifest = readJson<{ readonly package: { readonly name: string; readonly version: string }; readonly entries: readonly { readonly specifier: string }[] }>(join(effectRoot, MANIFEST_NAME));
  if (manifest.package.name !== "effect" || manifest.package.version !== EFFECT_VERSION) {
    fail("the pinned Effect manifest is not compatible with the stdlib manifest");
  }
  return new Set(manifest.entries.map(entry => entry.specifier));
};

const verifyAssetReference = (root: string, listed: Map<string, AssetReference>, asset: AssetReference, label: string) => {
  if (!isSafeRelativePath(asset.path) || !isSha256(asset.sha256)) fail(`${label} has an invalid path or SHA-256`);
  const file = listed.get(asset.path);
  if (!file || file.sha256 !== asset.sha256) fail(`${label} is not backed by files`);
  const path = resolve(root, asset.path);
  if (!path.startsWith(`${resolve(root)}${sep}`)) fail(`${label} escapes the asset root`);
  if (sha256(readFileSync(path)) !== asset.sha256) fail(`hash mismatch: ${asset.path}`);
};

export const verifyManifest = (root: string, manifest = readManifest(root), effectRoot = EFFECT_ASSETS) => {
  if (manifest.format !== STDLIB_FORMAT_VERSION) fail("stdlib manifest format is not supported");
  if (manifest.package.name !== "blitz-stdlib" || manifest.package.version !== STDLIB_VERSION) fail("stdlib package metadata is not pinned");
  if (manifest.package.effect_version !== EFFECT_VERSION) fail("stdlib Effect version is not pinned");
  if (manifest.package.runtime_abi_version !== RUNTIME_ABI_VERSION) fail("stdlib runtime ABI is not supported");
  if (manifest.package.host_abi_version !== HOST_ABI_VERSION) fail("stdlib host ABI is not supported");

  const catalog = [...manifest.host_primitive_catalog];
  const catalogNames = catalog.map(primitive => primitive.name);
  if (unique(catalogNames).length !== catalogNames.length || catalog.some(primitive => primitive.name.length === 0 || primitive.abi_version <= 0 || primitive.methods.length === 0 || unique(primitive.methods).length !== primitive.methods.length)) fail("stdlib host primitive catalog is invalid");
  if (catalogNames.join("\n") !== [...catalogNames].sort().join("\n")) fail("stdlib host primitive catalog is not sorted");

  const entries = [...manifest.entries];
  const entryMap = new Map(entries.map(entry => [entry.specifier, entry]));
  if (entryMap.size !== entries.length) fail("stdlib manifest contains duplicate specifiers");
  if (entries.map(entry => entry.specifier).join("\n") !== [...entries].sort((left, right) => left.specifier.localeCompare(right.specifier)).map(entry => entry.specifier).join("\n")) fail("stdlib entries are not sorted");

  const files = [...manifest.files];
  const fileMap = new Map(files.map(file => [file.path, file]));
  if (fileMap.size !== files.length) fail("stdlib manifest contains duplicate files");
  if (files.map(file => file.path).join("\n") !== [...files].sort((left, right) => left.path.localeCompare(right.path)).map(file => file.path).join("\n")) fail("stdlib files are not sorted");
  const actual = filesUnder(root).filter(path => path !== MANIFEST_NAME);
  if (actual.join("\n") !== [...fileMap.keys()].sort().join("\n")) fail("stdlib manifest file list is stale");
  for (const file of files) verifyAssetReference(root, fileMap, file, `file ${file.path}`);

  const known = new Set([...effectSpecifiers(effectRoot), ...entries.map(entry => entry.specifier)]);
  const declaredHostPrimitives = new Map(catalog.map(primitive => [primitive.name, primitive]));
  for (const entry of entries) {
    const isPublic = entry.visibility === "public";
    if (!isPublic && entry.visibility !== "private") fail(`invalid visibility: ${entry.specifier}`);
    if (isPublic !== entry.specifier.startsWith("blitz:")) fail(`visibility does not match specifier: ${entry.specifier}`);
    if (!isPublic && entry.declaration !== undefined) fail(`private entry has a declaration: ${entry.specifier}`);
    if (unique(entry.dependencies).length !== entry.dependencies.length) fail(`duplicate dependency: ${entry.specifier}`);
    const primitiveNames = entry.host_primitives.map(primitive => primitive.name);
    if (unique(primitiveNames).length !== primitiveNames.length) fail(`duplicate host primitive: ${entry.specifier}`);
    for (const dependency of entry.dependencies) if (!known.has(dependency)) fail(`unresolved dependency: ${entry.specifier} -> ${dependency}`);
    for (const primitive of entry.host_primitives) {
      const declared = declaredHostPrimitives.get(primitive.name);
      if (declared === undefined || JSON.stringify(declared) !== JSON.stringify(primitive)) fail(`missing or inconsistent host primitive: ${entry.specifier} -> ${primitive.name}`);
    }

    verifyAssetReference(root, fileMap, entry.source, `source for ${entry.specifier}`);
    if (entry.declaration !== undefined) verifyAssetReference(root, fileMap, entry.declaration, `declaration for ${entry.specifier}`);

    const source = readFileSync(resolve(root, entry.source.path), "utf8");
    const sourceDependencies = unique(staticImports(source, entry.source.path));
    if (sourceDependencies.join("\n") !== entry.dependencies.join("\n")) fail(`dependency order is stale: ${entry.specifier}`);
    if (entry.declaration !== undefined) {
      for (const dependency of staticImports(readFileSync(resolve(root, entry.declaration.path), "utf8"), entry.declaration.path)) {
        if (!known.has(dependency)) fail(`unresolved declaration dependency: ${entry.specifier} -> ${dependency}`);
      }
    }
  }
};

export const manifestConstants = {
  format: STDLIB_FORMAT_VERSION,
  version: STDLIB_VERSION,
  effectVersion: EFFECT_VERSION,
  runtimeAbiVersion: RUNTIME_ABI_VERSION,
  hostAbiVersion: HOST_ABI_VERSION,
} as const;
