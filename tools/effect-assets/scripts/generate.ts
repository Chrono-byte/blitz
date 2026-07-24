import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve, sep } from "node:path";
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { transform } from "esbuild";
import { rollup } from "rollup";
import * as ts from "typescript";

const VERSION = "3.16.7";
const SUPPORTED = ["effect", "effect/Effect", "effect/Cause", "effect/Exit", "effect/Fiber", "effect/FiberId", "effect/HashSet", "effect/Chunk", "effect/Option", "effect/Either", "effect/Duration", "effect/Function", "effect/Context", "effect/Layer", "effect/Ref", "effect/Stream", "effect/Scope", "effect/Schedule", "effect/Match"] as const;
const EFFECT_MODULES = SUPPORTED.slice(1);
const ROOT = resolve(import.meta.dirname, "..");
const OUTPUT = resolve(process.env.EFFECT_ASSETS_OUTPUT ?? resolve(ROOT, "../../crates/blitz-runtime/assets/effect", VERSION));
const PACKAGE_NAME = "effect";
const PACKAGE_PREFIX = "blitz-internal:effect/";
const RUNTIME_EXPORT_ALIASES: Readonly<Record<string, string>> = {
  "Fiber.await": "_await",
};

type PackageMetadata = {
  name: string;
  version: string;
  exports?: unknown;
};

type ManifestFile = {
  path: string;
  sha256: string;
  sourceType: "module" | "declaration" | "metadata" | "license" | "rust";
};

type OutputEntry = {
  specifier: string;
  source: { path: string; sha256: string; sourceType: "module" };
  declaration?: { path: string; sha256: string; sourceType: "declaration" };
};

type Manifest = {
  format: 1;
  package: {
    name: string;
    version: string;
    sourceIntegrity: string;
    metadata: string;
    license: string;
  };
  entries: OutputEntry[];
  files: ManifestFile[];
};

const normalize = (value: string) => value.replace(/\r\n?/g, "\n");
const sha256 = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");
const packageJson = (root: string) => JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as PackageMetadata;
const slash = (value: string) => value.split(sep).join("/");
const filesUnder = (root: string, current = root): string[] => readdirSync(current, { withFileTypes: true }).flatMap(entry => {
  const path = join(current, entry.name);
  return entry.isDirectory() ? filesUnder(root, path) : [slash(relative(root, path))];
}).sort();

const readIntegrity = () => {
  const lockfile = readFileSync(join(ROOT, "pnpm-lock.yaml"), "utf8");
  const match = lockfile.match(/effect@3\.16\.7:\n\s+resolution: \{integrity: ([^}]+)\}/);
  if (!match) throw new Error("pnpm-lock.yaml has no exact effect@3.16.7 integrity");
  return match[1];
};

const pickExport = (value: unknown): string | undefined => {
  if (typeof value === "string") return value;
  if (!value || typeof value !== "object") return undefined;
  const record = value as Record<string, unknown>;
  for (const key of ["import", "default"]) {
    const target = pickExport(record[key]);
    if (target) return target;
  }
  return undefined;
};

const packageRootFromResolution = (file: string) => {
  let current = resolve(file);
  while (current !== dirname(current)) {
    if (existsSync(join(current, "package.json"))) return current;
    current = dirname(current);
  }
  throw new Error(`Cannot find package root for ${file}`);
};

const exportedRuntimeNames = (file: string) => {
  const source = readFileSync(file, "utf8");
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const names: string[] = [];
  const add = (name: ts.Node | undefined) => {
    if (name && ts.isIdentifier(name)) names.push(name.text);
  };
  const isExported = (node: ts.Node) => ts.canHaveModifiers(node) && (ts.getModifiers(node)?.some(modifier => modifier.kind === ts.SyntaxKind.ExportKeyword) ?? false);

  for (const statement of parsed.statements) {
    if (ts.isVariableStatement(statement) && isExported(statement)) {
      for (const declaration of statement.declarationList.declarations) add(declaration.name);
    } else if (ts.isFunctionDeclaration(statement) && isExported(statement)) {
      add(statement.name);
    } else if (ts.isClassDeclaration(statement) && isExported(statement)) {
      add(statement.name);
    } else if (ts.isEnumDeclaration(statement) && isExported(statement)) {
      add(statement.name);
    } else if (ts.isExportDeclaration(statement) && !statement.isTypeOnly && statement.exportClause && ts.isNamedExports(statement.exportClause)) {
      for (const element of statement.exportClause.elements) if (!element.isTypeOnly) add(element.name);
    }
  }
  return [...new Set(names)].sort();
};

const parseSpecifiers = (source: string, file: string) => {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const results: { start: number; end: number; value: string }[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) {
      if (node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
        results.push({ start: node.moduleSpecifier.getStart(parsed) + 1, end: node.moduleSpecifier.getEnd() - 1, value: node.moduleSpecifier.text });
      }
    }
    if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
      throw new Error(`Dynamic import is not supported in ${file}`);
    }
    ts.forEachChild(node, visit);
  };
  visit(parsed);
  return results;
};

const rewriteImports = (source: string, file: string, visit: (specifier: string) => string) => {
  const imports = parseSpecifiers(source, file);
  let output = "";
  let cursor = 0;
  for (const item of imports) {
    output += source.slice(cursor, item.start);
    output += visit(item.value);
    cursor = item.end;
  }
  return normalize(output + source.slice(cursor));
};

const writeText = (root: string, path: string, content: string) => {
  const destination = join(root, path);
  mkdirSync(dirname(destination), { recursive: true });
  writeFileSync(destination, normalize(content), "utf8");
};

const main = async () => {
  const requireFromEffect = createRequire(join(ROOT, "node_modules/effect/package.json"));
  const effectRoot = packageRootFromResolution(requireFromEffect.resolve("effect/package.json"));
  const effectMetadata = packageJson(effectRoot);
  if (effectMetadata.version !== VERSION) throw new Error(`Expected effect@${VERSION}, got ${effectMetadata.version}`);
  if (!effectMetadata.exports || typeof effectMetadata.exports !== "object") throw new Error("effect has no exports map");
  const exportsMap = effectMetadata.exports as Record<string, unknown>;
  const moduleSources = new Map<string, string>();
  const moduleNames = new Map<string, string[]>();
  const declarations = new Map<string, string>();

  for (const specifier of EFFECT_MODULES) {
    const declarationName = specifier.slice(`${PACKAGE_NAME}/`.length);
    const declarationPath = join(ROOT, "declarations/effect", `${declarationName}.d.ts`);
    const names = exportedRuntimeNames(declarationPath);
    if (names.length === 0) throw new Error(`No runtime names declared in ${declarationPath}`);
    const exportValue = exportsMap[`./${declarationName}`];
    const target = pickExport(exportValue);
    if (!target) throw new Error(`No import export for ${specifier}`);
    const sourcePath = resolve(effectRoot, target);
    if (!sourcePath.startsWith(`${effectRoot}${sep}`) || !existsSync(sourcePath)) throw new Error(`Invalid source for ${specifier}: ${target}`);
    moduleSources.set(declarationName, sourcePath);
    moduleNames.set(declarationName, names);
    declarations.set(declarationName, declarationPath);
  }

  const temp = `${OUTPUT}.tmp-${process.pid}`;
  const bundleRoot = join(temp, ".bundle");
  rmSync(temp, { recursive: true, force: true });
  mkdirSync(join(temp, "entries"), { recursive: true });
  const entryPaths: Record<string, string> = {};
  for (const [moduleName, sourcePath] of moduleSources) {
    for (const exportName of moduleNames.get(moduleName) ?? []) {
      const entryName = `${moduleName}__${exportName}`;
      const entryPath = join(temp, "entries", `${entryName}.ts`);
      const upstreamName = RUNTIME_EXPORT_ALIASES[`${moduleName}.${exportName}`] ?? exportName;
      // Effect's declaration exposes Fiber.await through a private local name,
      // while the runtime module exports the public alias only.
      const entrySource = moduleName === "Fiber" && exportName === "await"
        ? `import { await as ${upstreamName} } from ${JSON.stringify(sourcePath)};\nexport { ${upstreamName} as ${exportName} };\n`
        : `export { ${upstreamName} as ${exportName} } from ${JSON.stringify(sourcePath)};\n`;
      writeText(temp, `entries/${entryName}.ts`, entrySource);
      entryPaths[entryName] = entryPath;
    }
  }

  const bundle = await rollup({
    input: entryPaths,
    treeshake: { moduleSideEffects: true },
    onwarn(warning) {
      throw new Error(`Rollup warning: ${warning.message}`);
    },
  });
  await bundle.write({
    dir: bundleRoot,
    format: "es",
    preserveModules: true,
    preserveModulesRoot: effectRoot,
    entryFileNames: "[name].js",
    chunkFileNames: "chunks/[name]-[hash].js",
  });
  await bundle.close();

  const bundleFiles = filesUnder(bundleRoot).filter(path => path.endsWith(".js"));
  if (bundleFiles.length === 0) throw new Error("Rollup emitted no JavaScript modules");
  const canonicalByBundlePath = new Map(bundleFiles.map(path => [resolve(bundleRoot, path), `${PACKAGE_PREFIX}${path}`]));
  const files: ManifestFile[] = [];
  const moduleEntries: OutputEntry[] = [];
  let tracingDetectorReplacements = 0;
  for (const bundlePath of bundleFiles) {
    const absolutePath = resolve(bundleRoot, bundlePath);
    let rollupSource = readFileSync(absolutePath, "utf8");
    const detector = "const isNotOptimizedAway = /*#__PURE__*/standard.effect_internal_function(() => new Error().stack)?.includes(\"effect_internal_function\") === true;";
    if (rollupSource.includes(detector)) {
      rollupSource = rollupSource.replace(detector, "const isNotOptimizedAway = false;");
      tracingDetectorReplacements += 1;
    }
    rollupSource = rollupSource.replace(
      "self instanceof URL",
      'typeof URL !== "undefined" && self instanceof URL',
    );
    // Blitz does not yet implement WeakMap methods. Effect uses these caches
    // only as identity maps, so retain behavior at the cost of weak retention.
    rollupSource = rollupSource.replaceAll("new WeakMap()", "new Map()");
    const minified = await transform(rollupSource, {
      format: "esm",
      target: "es2022",
      minifyWhitespace: true,
      minifyIdentifiers: true,
      minifySyntax: false,
      legalComments: "none",
    });
    let source = rewriteImports(minified.code, absolutePath, specifier => {
      if (!specifier.startsWith(".")) throw new Error(`Rollup left a non-canonical import in ${bundlePath}: ${specifier}`);
      const target = resolve(dirname(absolutePath), specifier);
      const canonical = canonicalByBundlePath.get(target);
      if (!canonical) throw new Error(`Unresolved Rollup module import in ${bundlePath}: ${specifier}`);
      return canonical;
    });
    const path = `modules/${bundlePath}`;
    writeText(temp, path, source);
    const canonical = `${PACKAGE_PREFIX}${bundlePath}`;
    moduleEntries.push({ specifier: canonical, source: { path, sha256: sha256(source), sourceType: "module" } });
    files.push({ path, sha256: sha256(source), sourceType: "module" });
  }
  if (tracingDetectorReplacements === 0) {
    throw new Error("Effect tracing detector was not found; review the host compatibility transform");
  }
  rmSync(bundleRoot, { recursive: true, force: true });
  rmSync(join(temp, "entries"), { recursive: true, force: true });

  const publicEntries: OutputEntry[] = [];
  const addPublic = (specifier: string, source: string, declarationName: string) => {
    const sourcePath = `modules/public/${specifier === PACKAGE_NAME ? "index" : declarationName}.js`;
    const declarationPath = `declarations/effect/${declarationName}.d.ts`;
    const declaration = normalize(readFileSync(join(ROOT, declarationPath), "utf8"));
    writeText(temp, sourcePath, source);
    writeText(temp, declarationPath, declaration);
    const sourceHash = sha256(source);
    const declarationHash = sha256(declaration);
    files.push({ path: sourcePath, sha256: sourceHash, sourceType: "module" });
    files.push({ path: declarationPath, sha256: declarationHash, sourceType: "declaration" });
    publicEntries.push({
      specifier,
      source: { path: sourcePath, sha256: sourceHash, sourceType: "module" },
      declaration: { path: declarationPath, sha256: declarationHash, sourceType: "declaration" },
    });
  };

  addPublic("effect", EFFECT_MODULES.map(name => `export * as ${name.slice(`${PACKAGE_NAME}/`.length)} from '${name}';`).join("\n") + "\n", "index");
  for (const specifier of EFFECT_MODULES) {
    const moduleName = specifier.slice(`${PACKAGE_NAME}/`.length);
    const source = (moduleNames.get(moduleName) ?? [])
      .map(exportName => `export { ${exportName} } from '${PACKAGE_PREFIX}${moduleName}__${exportName}.js';`)
      .join("\n") + "\n";
    addPublic(specifier, source, moduleName);
  }

  const metadata = normalize(JSON.stringify(JSON.parse(readFileSync(join(effectRoot, "package.json"), "utf8")), null, 2) + "\n");
  const license = normalize(readFileSync(join(effectRoot, "LICENSE"), "utf8"));
  writeText(temp, "package.json", metadata);
  writeText(temp, "LICENSE", license);
  files.push({ path: "package.json", sha256: sha256(metadata), sourceType: "metadata" });
  files.push({ path: "LICENSE", sha256: sha256(license), sourceType: "license" });

  const entries = [...moduleEntries, ...publicEntries].sort((a, b) => a.specifier.localeCompare(b.specifier));
  files.sort((a, b) => a.path.localeCompare(b.path));
  const embedded = [
    "// @generated by tools/effect-assets/scripts/generate.ts; do not edit.",
    "/// Curated, split Effect runtime modules embedded by blitz-runtime.",
    "pub const EMBEDDED_EFFECT_MODULES: &[EmbeddedEffectModule] = &[",
    ...entries.map(entry => [
      "  EmbeddedEffectModule {",
      `    specifier: ${JSON.stringify(entry.specifier)},`,
      `    source: include_str!(${JSON.stringify(entry.source.path)}),`,
      `    declaration: ${entry.declaration ? `Some(include_str!(${JSON.stringify(entry.declaration.path)}))` : "None"},`,
      `    source_sha256: ${JSON.stringify(entry.source.sha256)},`,
      `    declaration_sha256: ${entry.declaration ? `Some(${JSON.stringify(entry.declaration.sha256)})` : "None"},`,
      "  },",
    ].join("\n")),
    "];",
    "",
  ].join("\n");
  writeText(temp, "embedded.rs", embedded);
  files.push({ path: "embedded.rs", sha256: sha256(embedded), sourceType: "rust" });
  files.sort((a, b) => a.path.localeCompare(b.path));

  const manifest: Manifest = {
    format: 1,
    package: { name: effectMetadata.name, version: effectMetadata.version, sourceIntegrity: readIntegrity(), metadata: "package.json", license: "LICENSE" },
    entries,
    files,
  };
  writeText(temp, "manifest.json", JSON.stringify(manifest, null, 2) + "\n");

  const old = `${OUTPUT}.old-${process.pid}`;
  rmSync(old, { recursive: true, force: true });
  if (existsSync(OUTPUT)) renameSync(OUTPUT, old);
  try {
    renameSync(temp, OUTPUT);
    rmSync(old, { recursive: true, force: true });
  } catch (error) {
    if (existsSync(OUTPUT)) rmSync(OUTPUT, { recursive: true, force: true });
    if (existsSync(old)) renameSync(old, OUTPUT);
    throw error;
  }
  const bytes = files.reduce((sum, file) => sum + statSync(join(OUTPUT, file.path)).size, 0);
  console.log(`Generated ${entries.length} module entries (${files.length} hashed files, ${bytes} bytes) at ${relative(ROOT, OUTPUT)}`);
};

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
