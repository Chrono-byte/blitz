import { mkdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { pathToFileURL } from "node:url";
import {
  CompletionItemKind,
  DiagnosticSeverity,
  InlayHintKind,
  MarkupKind,
  SymbolKind,
  type CompletionItem,
  type Diagnostic,
  type DocumentSymbol,
  type Hover,
  type InlayHint,
  type LocationLink,
  type Position,
  type Range,
} from "vscode-languageserver/node";
import { TextDocument } from "vscode-languageserver-textdocument";
import {
  analyze,
  forgetDocument,
  openDocumentSource,
  type CompilerDeclaration,
  type CompilerDefinition,
  type CompilerVirtualDocument,
} from "./compiler-analysis.js";

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

export async function getInlayHints(document: TextDocument, range: Range): Promise<InlayHint[]> {
  const analysis = await analyze(document);
  return analysis.inlay_hints.flatMap((hint) => {
    const position = bytePosition(document, hint.position);
    return positionBefore(position, range.start) || positionBefore(range.end, position)
      ? []
      : [
          {
            position,
            label: hint.label,
            kind: hint.kind === "parameter" ? InlayHintKind.Parameter : InlayHintKind.Type,
            paddingLeft: hint.kind === "type",
            paddingRight: hint.kind === "parameter",
          },
        ];
  });
}

export async function getDocumentSymbols(document: TextDocument): Promise<DocumentSymbol[]> {
  const analysis = await analyze(document);
  return analysis.declarations.map((declaration) => documentSymbol(document, declaration));
}

export async function getCompletions(document: TextDocument): Promise<CompletionItem[]> {
  const analysis = await analyze(document);
  return [
    ...flattenDeclarations(analysis.declarations).map((declaration) => ({
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
  const definition = definitionsAt(document, position, analysis.definitions)[0];
  const declarations = flattenDeclarations(analysis.declarations);
  const declaration =
    definition === undefined
      ? declarations.find((candidate) => candidate.name === word.text)
      : declarations.find(
          (candidate) =>
            definition.target_path === undefined &&
            candidate.selection_start === definition.target_selection_start &&
            candidate.selection_end === definition.target_selection_end,
        );
  const runtimeGlobal = analysis.runtime_globals.find((candidate) => candidate.name === word.text);
  const detail = definition?.detail ?? declaration?.detail ?? runtimeGlobal?.detail;
  const documentation = definition?.documentation ?? declaration?.documentation;
  if (detail === undefined && documentation === undefined) return null;
  const sections = [
    detail === undefined ? undefined : `\`\`\`typescript\n${detail}\n\`\`\``,
    documentation,
  ].filter((section): section is string => section !== undefined);
  return {
    contents: { kind: MarkupKind.Markdown, value: sections.join("\n\n---\n\n") },
    range: word.range,
  };
}

export async function getDefinition(
  document: TextDocument,
  position: Position,
): Promise<LocationLink[] | null> {
  const word = wordAt(document, position);
  if (word === null) return null;
  const analysis = await analyze(document);
  const definitions = definitionsAt(document, position, analysis.definitions);
  if (definitions.length === 0) return null;
  const links = await Promise.all(
    definitions.map(async (definition): Promise<LocationLink | null> => {
      const targetDocument = await definitionTargetDocument(
        document,
        definition,
        analysis.virtual_documents,
      );
      if (targetDocument === null) return null;
      return {
        originSelectionRange: word.range,
        targetUri: targetDocument.uri,
        targetRange: byteRange(targetDocument, definition.target_start, definition.target_end),
        targetSelectionRange: byteRange(
          targetDocument,
          definition.target_selection_start,
          definition.target_selection_end,
        ),
      } satisfies LocationLink;
    }),
  );
  const resolved = links.flatMap((link) => (link === null ? [] : [link]));
  return resolved.length === 0 ? null : resolved;
}

function documentSymbol(document: TextDocument, declaration: CompilerDeclaration): DocumentSymbol {
  return {
    name: declaration.name,
    detail: declaration.detail,
    kind: symbolKind(declaration.kind),
    range: byteRange(document, declaration.start, declaration.end),
    selectionRange: byteRange(document, declaration.selection_start, declaration.selection_end),
    ...(declaration.children.length === 0
      ? {}
      : { children: declaration.children.map((child) => documentSymbol(document, child)) }),
  };
}

function flattenDeclarations(declarations: readonly CompilerDeclaration[]): CompilerDeclaration[] {
  return declarations.flatMap((declaration) => [
    declaration,
    ...flattenDeclarations(declaration.children),
  ]);
}

function definitionsAt(
  document: TextDocument,
  position: Position,
  definitions: readonly CompilerDefinition[],
) {
  const stringOffset = document.offsetAt(position);
  const byteOffset = Buffer.byteLength(document.getText().slice(0, stringOffset));
  return definitions.filter(
    (definition) => definition.start <= byteOffset && byteOffset <= definition.end,
  );
}

async function definitionTargetDocument(
  document: TextDocument,
  definition: CompilerDefinition,
  virtualDocuments: readonly CompilerVirtualDocument[],
): Promise<TextDocument | null> {
  if (definition.target_virtual !== undefined) {
    const virtualDocument = virtualDocuments.find(
      (candidate) => candidate.id === definition.target_virtual,
    );
    return virtualDocument === undefined ? null : materializeVirtualDocument(virtualDocument);
  }
  if (definition.target_path === undefined) return document;
  const targetPath = definition.target_path;
  const openSource = openDocumentSource(targetPath);
  if (openSource !== undefined) {
    return TextDocument.create(pathToFileURL(targetPath).href, "typescript", 0, openSource);
  }
  return readFile(targetPath, "utf8").then(
    (source) => TextDocument.create(pathToFileURL(targetPath).href, "typescript", 0, source),
    () => null,
  );
}

async function materializeVirtualDocument(
  virtualDocument: CompilerVirtualDocument,
): Promise<TextDocument | null> {
  const directory = join(tmpdir(), "blitz-language-server");
  const targetPath = join(directory, basename(virtualDocument.id));
  try {
    await mkdir(directory, { recursive: true });
    await writeFile(targetPath, virtualDocument.text, "utf8");
    return TextDocument.create(
      pathToFileURL(targetPath).href,
      "typescript",
      0,
      virtualDocument.text,
    );
  } catch {
    return null;
  }
}

function byteRange(document: TextDocument, start: number, end: number): Range {
  return {
    start: bytePosition(document, start),
    end: bytePosition(document, end),
  };
}

function bytePosition(document: TextDocument, offset: number): Position {
  return document.positionAt(byteOffsetToStringOffset(document.getText(), offset));
}

function positionBefore(left: Position, right: Position): boolean {
  return left.line < right.line || (left.line === right.line && left.character < right.character);
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
      return SymbolKind.Class;
    case "variable":
      return SymbolKind.Variable;
    case "method":
      return SymbolKind.Method;
    case "property":
      return SymbolKind.Field;
    case "constructor":
      return SymbolKind.Constructor;
    case "enum":
      return SymbolKind.Enum;
    case "enumMember":
      return SymbolKind.EnumMember;
    case "namespace":
      return SymbolKind.Namespace;
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
      return CompletionItemKind.Class;
    case "variable":
      return CompletionItemKind.Variable;
    case "method":
      return CompletionItemKind.Method;
    case "property":
      return CompletionItemKind.Field;
    case "constructor":
      return CompletionItemKind.Constructor;
    case "enum":
      return CompletionItemKind.Enum;
    case "enumMember":
      return CompletionItemKind.EnumMember;
    case "namespace":
      return CompletionItemKind.Module;
  }
}
