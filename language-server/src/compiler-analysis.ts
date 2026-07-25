import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { TextDocument } from "vscode-languageserver-textdocument";

export interface CompilerDiagnostic {
  readonly message: string;
  readonly start: number;
  readonly end: number;
}

export type CompilerDeclarationKind =
  | "function"
  | "class"
  | "interface"
  | "type"
  | "variable"
  | "method"
  | "property"
  | "constructor"
  | "enum"
  | "enumMember"
  | "namespace";

export interface CompilerDeclaration {
  readonly name: string;
  readonly kind: CompilerDeclarationKind;
  readonly detail: string;
  readonly start: number;
  readonly end: number;
  readonly selection_start: number;
  readonly selection_end: number;
  readonly children: readonly CompilerDeclaration[];
}

export interface CompilerDefinition {
  readonly start: number;
  readonly end: number;
  readonly target_path?: string;
  readonly target_virtual?: string;
  readonly detail?: string;
  readonly target_start: number;
  readonly target_end: number;
  readonly target_selection_start: number;
  readonly target_selection_end: number;
}

export interface RuntimeGlobal {
  readonly name: string;
  readonly detail: string;
}

export interface CompilerVirtualDocument {
  readonly id: string;
  readonly text: string;
}

export interface CompilerInlayHint {
  readonly position: number;
  readonly label: string;
  readonly kind: "type" | "parameter";
}

export interface CompilerAnalysis {
  readonly diagnostics: readonly CompilerDiagnostic[];
  readonly declarations: readonly CompilerDeclaration[];
  readonly definitions: readonly CompilerDefinition[];
  readonly keywords: readonly string[];
  readonly runtime_globals: readonly RuntimeGlobal[];
  readonly virtual_documents: readonly CompilerVirtualDocument[];
  readonly inlay_hints: readonly CompilerInlayHint[];
}

interface CompilerCommand {
  readonly command: string;
  readonly arguments: readonly string[];
}

interface CachedAnalysis {
  readonly version: number;
  readonly source: string;
  readonly projectRevision: number;
  readonly result: Promise<CompilerAnalysis>;
}

const cache = new Map<string, CachedAnalysis>();
const openSources = new Map<string, string>();
let projectRevision = 0;
let compilerProcess: CompilerProcess | undefined;

export function stopCompiler(): void {
  compilerProcess?.stop();
  compilerProcess = undefined;
}

export function forgetDocument(uri: string): void {
  const path = documentPath(uri);
  openSources.delete(path);
  projectRevision += 1;
  cache.clear();
  compilerProcess?.close(path);
}

export function openDocumentSource(path: string): string | undefined {
  return openSources.get(resolve(path));
}

export function analyze(document: TextDocument): Promise<CompilerAnalysis> {
  const source = document.getText();
  const path = documentPath(document.uri);
  if (openSources.get(path) !== source) {
    openSources.set(path, source);
    projectRevision += 1;
    cache.clear();
  }
  const cached = cache.get(document.uri);
  if (
    cached?.version === document.version &&
    cached.source === source &&
    cached.projectRevision === projectRevision
  ) {
    return cached.result;
  }
  const result = runCompiler(source, path);
  cache.set(document.uri, { version: document.version, source, projectRevision, result });
  return result;
}

function documentPath(uri: string): string {
  try {
    return resolve(fileURLToPath(uri));
  } catch {
    return "editor.ts";
  }
}

function runCompiler(source: string, path: string): Promise<CompilerAnalysis> {
  compilerProcess ??= new CompilerProcess();
  return compilerProcess.analyze(source, path);
}

class CompilerProcess {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #pending = new Map<number, (result: CompilerAnalysis) => void>();
  #nextId = 1;
  #stdout = "";
  #stderr = "";
  #stopped = false;

  constructor() {
    const compiler = compilerCommand();
    this.#child = spawn(compiler.command, [...compiler.arguments, "analyze", "--server"]);
    this.#child.stdout.on("data", (chunk: Buffer) => this.#onStdout(chunk.toString()));
    this.#child.stderr.on("data", (chunk: Buffer) => {
      this.#stderr = `${this.#stderr}${chunk.toString()}`.slice(-16_384);
    });
    this.#child.stdin.on("error", () => undefined);
    this.#child.on("error", (error: Error) =>
      this.#fail(`Could not start Blitz compiler: ${error.message}`),
    );
    this.#child.on("close", (code) => {
      if (this.#stopped) return;
      this.#fail(this.#stderr.trim() || `Blitz compiler server exited with code ${String(code)}`);
    });
    for (const [path, source] of openSources) this.update(path, source);
  }

  analyze(source: string, path: string): Promise<CompilerAnalysis> {
    const id = this.#nextId++;
    return new Promise((resolveResult) => {
      this.#pending.set(id, resolveResult);
      this.#child.stdin.write(
        `${JSON.stringify({ method: "analyze", id, path, source })}\n`,
        (error) => {
          if (error === null || error === undefined) return;
          const resolvePending = this.#pending.get(id);
          this.#pending.delete(id);
          resolvePending?.(failedAnalysis(`Could not write to Blitz compiler: ${error.message}`));
        },
      );
    });
  }

  update(path: string, source: string): void {
    this.#child.stdin.write(`${JSON.stringify({ method: "update", path, source })}\n`);
  }

  close(path: string): void {
    this.#child.stdin.write(`${JSON.stringify({ method: "close", path })}\n`);
  }

  stop(): void {
    this.#stopped = true;
    this.#child.stdin.end();
    this.#child.kill();
    this.#fail("Blitz compiler stopped");
  }

  #onStdout(chunk: string): void {
    this.#stdout += chunk;
    for (;;) {
      const newline = this.#stdout.indexOf("\n");
      if (newline === -1) return;
      const line = this.#stdout.slice(0, newline);
      this.#stdout = this.#stdout.slice(newline + 1);
      if (line.trim() === "") continue;
      this.#handleResponse(line);
    }
  }

  #handleResponse(line: string): void {
    let value: unknown;
    try {
      value = JSON.parse(line);
    } catch (error: unknown) {
      this.#fail(
        `Could not decode Blitz analysis response: ${error instanceof Error ? error.message : String(error)}`,
      );
      return;
    }
    if (!isRecord(value) || typeof value.id !== "number") {
      this.#fail("Invalid Blitz analysis server response");
      return;
    }
    const resolveResult = this.#pending.get(value.id);
    if (resolveResult === undefined) return;
    this.#pending.delete(value.id);
    resolveResult(
      isCompilerAnalysis(value.result)
        ? value.result
        : failedAnalysis(
            typeof value.error === "string" ? value.error : "Invalid Blitz analysis output",
          ),
    );
  }

  #fail(message: string): void {
    for (const resolveResult of this.#pending.values()) resolveResult(failedAnalysis(message));
    this.#pending.clear();
    if (compilerProcess === this) {
      compilerProcess = undefined;
      projectRevision += 1;
      cache.clear();
    }
  }
}

function compilerCommand(): CompilerCommand {
  const configured = process.env.BLITZ_COMPILER;
  if (configured !== undefined) return { command: configured, arguments: [] };
  for (const root of [process.cwd(), resolve(process.cwd(), "..")]) {
    const local = resolve(
      root,
      "target",
      "debug",
      process.platform === "win32" ? "blitz.exe" : "blitz",
    );
    if (existsSync(local)) return { command: local, arguments: [] };
    const manifest = resolve(root, "Cargo.toml");
    if (existsSync(manifest)) {
      return {
        command: "cargo",
        arguments: ["run", "--quiet", "-p", "blitz-cli", "--manifest-path", manifest, "--"],
      };
    }
  }
  return { command: "blitz", arguments: [] };
}

function failedAnalysis(message: string): CompilerAnalysis {
  return {
    diagnostics: [{ message, start: 0, end: 1 }],
    declarations: [],
    definitions: [],
    keywords: [],
    runtime_globals: [],
    virtual_documents: [],
    inlay_hints: [],
  };
}

function isCompilerAnalysis(value: unknown): value is CompilerAnalysis {
  return (
    isRecord(value) &&
    Array.isArray(value.diagnostics) &&
    value.diagnostics.every(
      (item: unknown) =>
        isRecord(item) &&
        typeof item.message === "string" &&
        typeof item.start === "number" &&
        typeof item.end === "number",
    ) &&
    Array.isArray(value.declarations) &&
    value.declarations.every(isDeclaration) &&
    Array.isArray(value.definitions) &&
    value.definitions.every(isDefinition) &&
    Array.isArray(value.keywords) &&
    value.keywords.every((item: unknown) => typeof item === "string") &&
    Array.isArray(value.runtime_globals) &&
    value.runtime_globals.every(
      (item: unknown) =>
        isRecord(item) && typeof item.name === "string" && typeof item.detail === "string",
    ) &&
    Array.isArray(value.virtual_documents) &&
    value.virtual_documents.every(
      (item: unknown) =>
        isRecord(item) && typeof item.id === "string" && typeof item.text === "string",
    ) &&
    Array.isArray(value.inlay_hints) &&
    value.inlay_hints.every(
      (item: unknown) =>
        isRecord(item) &&
        typeof item.position === "number" &&
        typeof item.label === "string" &&
        (item.kind === "type" || item.kind === "parameter"),
    )
  );
}

function isDeclaration(value: unknown): value is CompilerDeclaration {
  return (
    isRecord(value) &&
    typeof value.name === "string" &&
    [
      "function",
      "class",
      "interface",
      "type",
      "variable",
      "method",
      "property",
      "constructor",
      "enum",
      "enumMember",
      "namespace",
    ].includes(String(value.kind)) &&
    typeof value.detail === "string" &&
    typeof value.start === "number" &&
    typeof value.end === "number" &&
    typeof value.selection_start === "number" &&
    typeof value.selection_end === "number" &&
    Array.isArray(value.children) &&
    value.children.every(isDeclaration)
  );
}

function isDefinition(value: unknown): value is CompilerDefinition {
  return (
    isRecord(value) &&
    typeof value.start === "number" &&
    typeof value.end === "number" &&
    (value.target_path === undefined || typeof value.target_path === "string") &&
    (value.target_virtual === undefined || typeof value.target_virtual === "string") &&
    (value.detail === undefined || typeof value.detail === "string") &&
    typeof value.target_start === "number" &&
    typeof value.target_end === "number" &&
    typeof value.target_selection_start === "number" &&
    typeof value.target_selection_end === "number"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
