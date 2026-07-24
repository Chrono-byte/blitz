#!/usr/bin/env node

import {
  createConnection,
  TextDocuments,
  TextDocumentSyncKind,
  type InitializeResult,
} from "vscode-languageserver/node";
import { TextDocument } from "vscode-languageserver-textdocument";
import {
  forgetDocument,
  getCompletions,
  getDefinition,
  getDiagnostics,
  getDocumentSymbols,
  getHover,
} from "./language-service.js";

const connection = createConnection(process.stdin, process.stdout);
const documents = new TextDocuments(TextDocument);

connection.onInitialize(
  (): InitializeResult => ({
    capabilities: {
      textDocumentSync: TextDocumentSyncKind.Incremental,
      completionProvider: { triggerCharacters: ["."] },
      definitionProvider: true,
      documentSymbolProvider: true,
      hoverProvider: true,
    },
    serverInfo: { name: "blitz-language-server", version: "0.1.0" },
  }),
);

documents.onDidChangeContent(({ document }) => {
  const version = document.version;
  void getDiagnostics(document).then((diagnostics) => {
    if (documents.get(document.uri)?.version !== version) return;
    void connection.sendDiagnostics({ uri: document.uri, version, diagnostics });
  });
});

documents.onDidClose(({ document }) => {
  forgetDocument(document.uri);
  void connection.sendDiagnostics({ uri: document.uri, diagnostics: [] });
});

connection.onCompletion(({ textDocument }) => {
  const document = documents.get(textDocument.uri);
  return document === undefined ? [] : getCompletions(document);
});

connection.onDocumentSymbol(({ textDocument }) => {
  const document = documents.get(textDocument.uri);
  return document === undefined ? [] : getDocumentSymbols(document);
});

connection.onHover(({ textDocument, position }) => {
  const document = documents.get(textDocument.uri);
  return document === undefined ? null : getHover(document, position);
});

connection.onDefinition(({ textDocument, position }) => {
  const document = documents.get(textDocument.uri);
  return document === undefined ? null : getDefinition(document, position);
});

documents.listen(connection);
connection.listen();
