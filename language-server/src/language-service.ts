import {
  CompletionItemKind,
  DiagnosticSeverity,
  MarkupKind,
  SymbolKind,
  type CompletionItem,
  type Diagnostic,
  type DocumentSymbol,
  type Hover,
  type Location,
  type Position,
  type Range,
} from "vscode-languageserver/node";
import type { TextDocument } from "vscode-languageserver-textdocument";
import { analyze, forgetDocument, type CompilerDeclaration } from "./compiler-analysis.js";

export { forgetDocument };

export async function getDiagnostics(document: TextDocument): Promise<Diagnostic[]> {
  const analysis = await analyze(document);
  return analysis.diagnostics.map((diagnostic) => ({
    severity: DiagnosticSeverity.Error,
    range: byteRange(document, diagnostic.start, diagnostic.end),
    message: diagnostic.message,
    source: "blitz",
  }));
}

export async function getDocumentSymbols(document: TextDocument): Promise<DocumentSymbol[]> {
  const analysis = await analyze(document);
  return analysis.declarations.map((declaration) => {
    const range = declarationRange(document, declaration);
    return {
      name: declaration.name,
      detail: declaration.detail,
      kind: symbolKind(declaration.kind),
      range,
      selectionRange: range,
    };
  });
}

export async function getCompletions(document: TextDocument): Promise<CompletionItem[]> {
  const analysis = await analyze(document);
  return [
    ...analysis.declarations.map((declaration) => ({
      label: declaration.name,
      detail: declaration.detail,
      kind: completionKind(declaration.kind),
    })),
    ...analysis.runtime_globals.map((global) => ({
      label: global.name,
      detail: global.detail,
      kind: CompletionItemKind.Variable,
    })),
    ...analysis.keywords.map((keyword) => ({
      label: keyword,
      kind: CompletionItemKind.Keyword,
    })),
  ];
}

export async function getHover(document: TextDocument, position: Position): Promise<Hover | null> {
  const word = wordAt(document, position);
  if (word === null) return null;
  const analysis = await analyze(document);
  const declaration = analysis.declarations.find((candidate) => candidate.name === word.text);
  const runtimeGlobal = analysis.runtime_globals.find((candidate) => candidate.name === word.text);
  const detail = declaration?.detail ?? runtimeGlobal?.detail;
  return detail === undefined
    ? null
    : {
        contents: { kind: MarkupKind.Markdown, value: `\`\`\`typescript\n${detail}\n\`\`\`` },
        range: word.range,
      };
}

export async function getDefinition(
  document: TextDocument,
  position: Position,
): Promise<Location | null> {
  const word = wordAt(document, position);
  if (word === null) return null;
  const analysis = await analyze(document);
  const declaration = analysis.declarations.find((candidate) => candidate.name === word.text);
  return declaration === undefined
    ? null
    : { uri: document.uri, range: declarationRange(document, declaration) };
}

function declarationRange(document: TextDocument, declaration: CompilerDeclaration): Range {
  return byteRange(document, declaration.start, declaration.end);
}

function byteRange(document: TextDocument, start: number, end: number): Range {
  const source = document.getText();
  return {
    start: document.positionAt(byteOffsetToStringOffset(source, start)),
    end: document.positionAt(byteOffsetToStringOffset(source, end)),
  };
}

function byteOffsetToStringOffset(source: string, byteOffset: number): number {
  const bytes = Buffer.from(source);
  return bytes.subarray(0, Math.min(byteOffset, bytes.length)).toString().length;
}

function wordAt(document: TextDocument, position: Position): { text: string; range: Range } | null {
  const source = document.getText();
  const offset = document.offsetAt(position);
  let start = offset;
  let end = offset;
  while (start > 0 && /[A-Za-z0-9_$]/u.test(source[start - 1]!)) start -= 1;
  while (end < source.length && /[A-Za-z0-9_$]/u.test(source[end]!)) end += 1;
  return start === end
    ? null
    : {
        text: source.slice(start, end),
        range: { start: document.positionAt(start), end: document.positionAt(end) },
      };
}

function symbolKind(kind: CompilerDeclaration["kind"]): SymbolKind {
  switch (kind) {
    case "function":
      return SymbolKind.Function;
    case "class":
      return SymbolKind.Class;
    case "interface":
      return SymbolKind.Interface;
    case "type":
      return SymbolKind.TypeParameter;
    case "variable":
      return SymbolKind.Variable;
  }
}

function completionKind(kind: CompilerDeclaration["kind"]): CompletionItemKind {
  switch (kind) {
    case "function":
      return CompletionItemKind.Function;
    case "class":
      return CompletionItemKind.Class;
    case "interface":
      return CompletionItemKind.Interface;
    case "type":
      return CompletionItemKind.TypeParameter;
    case "variable":
      return CompletionItemKind.Variable;
  }
}
