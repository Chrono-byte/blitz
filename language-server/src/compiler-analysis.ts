import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { TextDocument } from "vscode-languageserver-textdocument";

export interface CompilerDiagnostic {
  readonly message: string;
  readonly start: number;
  readonly end: number;
}

export interface CompilerDeclaration {
  readonly name: string;
  readonly kind: "function" | "class" | "interface" | "type" | "variable";
  readonly detail: string;
  readonly start: number;
  readonly end: number;
}

export interface RuntimeGlobal {
  readonly name: string;
  readonly detail: string;
}

export interface CompilerAnalysis {
  readonly diagnostics: readonly CompilerDiagnostic[];
  readonly declarations: readonly CompilerDeclaration[];
  readonly keywords: readonly string[];
  readonly runtime_globals: readonly RuntimeGlobal[];
}

interface CompilerCommand {
  readonly command: string;
  readonly arguments: readonly string[];
}

interface CachedAnalysis {
  readonly version: number;
  readonly source: string;
  readonly result: Promise<CompilerAnalysis>;
}

const cache = new Map<string, CachedAnalysis>();

export function forgetDocument(uri: string): void {
  cache.delete(uri);
}

export function analyze(document: TextDocument): Promise<CompilerAnalysis> {
  const source = document.getText();
  const cached = cache.get(document.uri);
  if (cached?.version === document.version && cached.source === source) return cached.result;
  const result = runCompiler(source, documentPath(document.uri));
  cache.set(document.uri, { version: document.version, source, result });
  return result;
}

function documentPath(uri: string): string {
  try {
    return fileURLToPath(uri);
  } catch {
    return "editor.ts";
  }
}

function runCompiler(source: string, path: string): Promise<CompilerAnalysis> {
  const compiler = compilerCommand();
  return new Promise((resolveResult) => {
    const child = spawn(compiler.command, [...compiler.arguments, "analyze", "--path", path, "-"]);
    let stdout = "";
    let stderr = "";
    let settled = false;
    const finish = (result: CompilerAnalysis): void => {
      if (settled) return;
      settled = true;
      resolveResult(result);
    };
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    child.stdin.on("error", () => undefined);
    child.on("error", (error: Error) =>
      finish(failedAnalysis(`Could not start Blitz compiler: ${error.message}`)),
    );
    child.on("close", (code) => {
      if (code !== 0) {
        finish(failedAnalysis(stderr.trim() || `Blitz compiler exited with code ${String(code)}`));
        return;
      }
      try {
        const value: unknown = JSON.parse(stdout);
        finish(isCompilerAnalysis(value) ? value : failedAnalysis("Invalid Blitz analysis output"));
      } catch (error: unknown) {
        finish(
          failedAnalysis(
            `Could not decode Blitz analysis: ${error instanceof Error ? error.message : String(error)}`,
          ),
        );
      }
    });
    child.stdin.end(source);
  });
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
    keywords: [],
    runtime_globals: [],
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
    Array.isArray(value.keywords) &&
    value.keywords.every((item: unknown) => typeof item === "string") &&
    Array.isArray(value.runtime_globals) &&
    value.runtime_globals.every(
      (item: unknown) =>
        isRecord(item) && typeof item.name === "string" && typeof item.detail === "string",
    )
  );
}

function isDeclaration(value: unknown): value is CompilerDeclaration {
  return (
    isRecord(value) &&
    typeof value.name === "string" &&
    ["function", "class", "interface", "type", "variable"].includes(String(value.kind)) &&
    typeof value.detail === "string" &&
    typeof value.start === "number" &&
    typeof value.end === "number"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
