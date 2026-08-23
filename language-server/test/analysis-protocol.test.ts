import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import test from "node:test";

import { ANALYSIS_PROTOCOL_VERSION, isCompilerAnalysis } from "../src/compiler-analysis.js";

await test("accepts the Rust analysis protocol fixture", async () => {
  const fixturePath = resolve(process.cwd(), "protocol/analysis-v1.fixture.json");
  const fixture: unknown = JSON.parse(await readFile(fixturePath, "utf8"));
  assert.equal(typeof fixture, "object");
  assert.notEqual(fixture, null);
  if (typeof fixture !== "object" || fixture === null) return;
  if (!("protocol_version" in fixture) || !("result" in fixture)) {
    assert.fail("fixture is missing its protocol envelope");
  }
  assert.equal(fixture.protocol_version, ANALYSIS_PROTOCOL_VERSION);
  assert.equal(isCompilerAnalysis(fixture.result), true);
});
