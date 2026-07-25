import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { TextDocument } from "vscode-languageserver-textdocument";
import { stopCompiler } from "../src/compiler-analysis.js";
import {
  getCompletions,
  forgetDocument,
  getDefinition,
  getDiagnostics,
  getDocumentSymbols,
  getHover,
  getInlayHints,
} from "../src/language-service.js";

await test("provides Blitz diagnostics and navigation", async () => {
  const source = await readFile(
    new URL("../../test/fixtures/language-service-positive.txt", import.meta.url),
    "utf8",
  );
  const document = TextDocument.create("file:///editor.ts", "typescript", 1, source);

  assert.deepEqual(await getDiagnostics(document), []);
  assert.deepEqual(
    (await getDocumentSymbols(document)).map(({ name }) => name),
    ["Answer", "answer", "read"],
  );
  assert.ok((await getCompletions(document)).some(({ label }) => label === "Blitz"));
  assert.match(JSON.stringify(await getHover(document, { line: 2, character: 35 })), /answer/u);
  assert.deepEqual(
    (await getDefinition(document, { line: 2, character: 35 }))?.[0]?.targetSelectionRange.start,
    {
      line: 1,
      character: 6,
    },
  );
});

await test("returns nested symbols and scope-correct definitions", async () => {
  const source = [
    "const value = 1;",
    "class Service {",
    "  field: number = value;",
    "  read() { const value = 2; return value; }",
    "}",
    "value;",
  ].join("\n");
  const document = TextDocument.create("file:///nested.ts", "typescript", 1, source);
  const symbols = await getDocumentSymbols(document);
  const service = symbols.find(({ name }) => name === "Service");

  assert.deepEqual(
    service?.children?.map(({ name }) => name),
    ["field", "read"],
  );
  assert.deepEqual(
    service?.children?.[1]?.children?.map(({ name }) => name),
    ["value"],
  );
  assert.deepEqual(
    (await getDefinition(document, { line: 3, character: 40 }))?.[0]?.targetSelectionRange.start,
    { line: 3, character: 17 },
  );
  assert.deepEqual(
    (await getDefinition(document, { line: 5, character: 2 }))?.[0]?.targetSelectionRange.start,
    { line: 0, character: 6 },
  );
});

await test("resolves imports, namespace members, and re-exports across files", async () => {
  const source = [
    'import primary, * as Source from "./fixtures/definitions-source.js";',
    'import { renamed } from "./fixtures/definitions-barrel.js";',
    "",
    "primary();",
    "Source.value;",
    "renamed();",
  ].join("\n");
  const uri = new URL("../../test/cross-file-importer.ts", import.meta.url).href;
  const document = TextDocument.create(uri, "typescript", 1, source);
  const primary = (await getDefinition(document, { line: 3, character: 2 }))?.[0];
  const value = (await getDefinition(document, { line: 4, character: 9 }))?.[0];
  const renamed = (await getDefinition(document, { line: 5, character: 2 }))?.[0];

  assert.match(primary?.targetUri ?? "", /definitions-source\.js$/u);
  assert.equal(primary?.targetSelectionRange.start.line, 3);
  assert.match(value?.targetUri ?? "", /definitions-source\.js$/u);
  assert.equal(value?.targetSelectionRange.start.line, 6);
  assert.match(renamed?.targetUri ?? "", /definitions-source\.js$/u);
  assert.equal(renamed?.targetSelectionRange.start.line, 0);
});

await test("returns contextual callback parameter inlay hints", async () => {
  const source = [
    "declare function configure(options: { handler: (context: { userId: string }) => void }): void;",
    "configure({ handler: (context) => context.userId });",
  ].join("\n");
  const document = TextDocument.create("file:///contextual.ts", "typescript", 1, source);
  const hints = await getInlayHints(document, {
    start: { line: 0, character: 0 },
    end: { line: 1, character: 60 },
  });

  assert.deepEqual(hints, [
    {
      position: { line: 1, character: 10 },
      label: "options:",
      kind: 2,
      paddingLeft: false,
      paddingRight: true,
    },
    {
      position: { line: 1, character: 29 },
      label: ": { userId: string }",
      kind: 1,
      paddingLeft: true,
      paddingRight: false,
    },
  ]);
});

await test("maps argument parameter-name hints with right padding", async () => {
  const source = [
    "declare function send(message: string): void;",
    "declare const input: string;",
    "send(input);",
  ].join("\n");
  const document = TextDocument.create("file:///parameter-hints.ts", "typescript", 1, source);
  const hints = await getInlayHints(document, {
    start: { line: 0, character: 0 },
    end: { line: 2, character: 12 },
  });

  assert.deepEqual(hints, [
    {
      position: { line: 2, character: 5 },
      label: "message:",
      kind: 2,
      paddingLeft: false,
      paddingRight: true,
    },
  ]);
});

await test("hovers and navigates to typed members from runtime declarations", async () => {
  const source = [
    'import * as WebSocket from "blitz:websocket";',
    'const transmit = (connection: WebSocket.Connection) => connection.send("hello").pipe();',
    'WebSocket.connect({ url: "ws://localhost" }).pipe();',
  ].join("\n");
  const document = TextDocument.create("file:///runtime-member.ts", "typescript", 1, source);
  const sendPosition = { line: 1, character: 67 };
  const sendHover = await getHover(document, sendPosition);
  const sendDefinition = (await getDefinition(document, sendPosition))?.[0];
  const sendPipeHover = await getHover(document, { line: 1, character: 81 });
  const connectPipeDefinition = (await getDefinition(document, { line: 2, character: 46 }))?.[0];

  assert.match(JSON.stringify(sendHover), /send.*Effect\.Effect<void, WebSocketError>/u);
  assert.match(sendDefinition?.targetUri ?? "", /blitz_websocket\.d\.ts$/u);
  assert.match(JSON.stringify(sendPipeHover), /readonly pipe/u);
  assert.match(connectPipeDefinition?.targetUri ?? "", /effect_Effect\.d\.ts$/u);
});

await test("uses unsaved dependency overlays and invalidates importer analysis", async () => {
  const dependencyUri = "file:///tmp/blitz-lsp-overlay/dependency.ts";
  const importerUri = "file:///tmp/blitz-lsp-overlay/importer.ts";
  const importer = TextDocument.create(
    importerUri,
    "typescript",
    1,
    'import { value } from "./dependency.ts";\nvalue;',
  );
  const firstDependency = TextDocument.create(
    dependencyUri,
    "typescript",
    1,
    "export const value = 1;",
  );
  await getDiagnostics(firstDependency);
  const first = (await getDefinition(importer, { line: 1, character: 2 }))?.[0];

  const changedDependency = TextDocument.create(
    dependencyUri,
    "typescript",
    2,
    "// unsaved\nexport const value = 2;",
  );
  await getDiagnostics(changedDependency);
  const changed = (await getDefinition(importer, { line: 1, character: 2 }))?.[0];

  assert.equal(first?.targetUri, dependencyUri);
  assert.equal(first?.targetSelectionRange.start.line, 0);
  assert.equal(changed?.targetUri, dependencyUri);
  assert.equal(changed?.targetSelectionRange.start.line, 1);
  forgetDocument(dependencyUri);
  forgetDocument(importerUri);
});

await test("hovers and navigates to ECMAScript built-in declarations", async () => {
  const document = TextDocument.create(
    "file:///builtins.ts",
    "typescript",
    1,
    "const values = new Map<string, number>();",
  );
  const hover = await getHover(document, { line: 0, character: 20 });
  const definition = (await getDefinition(document, { line: 0, character: 20 }))?.[0];

  assert.deepEqual(await getDiagnostics(document), []);
  assert.match(JSON.stringify(hover), /var Map: MapConstructor/u);
  assert.match(definition?.targetUri ?? "", /lib\.es2023\.d\.ts$/u);
  assert.notEqual(definition?.targetUri, document.uri);
});

await test("reports type errors from the Blitz checker", async () => {
  const source = await readFile(
    new URL("../../test/fixtures/language-service-negative.txt", import.meta.url),
    "utf8",
  );
  const document = TextDocument.create("file:///invalid.ts", "typescript", 1, source);

  assert.match(JSON.stringify(await getDiagnostics(document)), /not assignable/u);
});

stopCompiler();
