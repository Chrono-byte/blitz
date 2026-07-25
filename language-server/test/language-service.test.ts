import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { TextDocument } from "vscode-languageserver-textdocument";
import {
  getCompletions,
  getDefinition,
  getDiagnostics,
  getDocumentSymbols,
  getHover,
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
  assert.deepEqual((await getDefinition(document, { line: 2, character: 35 }))?.range.start, {
    line: 1,
    character: 6,
  });
});

await test("reports type errors from the Blitz checker", async () => {
  const source = await readFile(
    new URL("../../test/fixtures/language-service-negative.txt", import.meta.url),
    "utf8",
  );
  const document = TextDocument.create("file:///invalid.ts", "typescript", 1, source);

  assert.match(JSON.stringify(await getDiagnostics(document)), /not assignable/u);
});
