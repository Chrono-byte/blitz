import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const ROOT = resolve(import.meta.dirname, "..");
const REPOSITORY = resolve(ROOT, "../..");
const blitzBinary = join(REPOSITORY, "target/debug/blitz");
const fixture = `
import bufferModule, { Buffer } from "node:buffer";
import { randomFillSync, timingSafeEqual } from "node:crypto";
const assert = (condition, message) => { if (!condition) throw new Error(message); };
assert(bufferModule.Buffer === Buffer && globalThis.Buffer === Buffer, "Buffer identity");
const utf8 = Buffer.from("hé", "utf8");
const alias = utf8.subarray(1);
alias[0] = 0x21;
const copy = Buffer.from(utf8);
copy[0] = 0x41;
const allocated = Buffer.alloc(4, 0x2e);
allocated.write("ok", 1);
const concatenated = Buffer.concat([Buffer.from("a"), Buffer.from("b")]);
const cryptoBacking = new Uint8Array([17, 17, 17, 17, 17, 17]);
const cryptoView = new Uint8Array(cryptoBacking.buffer, 2, 2);
randomFillSync(cryptoView);
const cryptoBuffer = Buffer.from(new ArrayBuffer(6), 2, 2);
randomFillSync(cryptoBuffer);
const output = {
  utf8Hex: utf8.toString("hex"),
  utf8Text: utf8.toString(),
  aliasSharesBuffer: alias.buffer === utf8.buffer,
  copyIsIndependent: utf8[0] !== copy[0],
  allocated: allocated.toString(),
  concatenated: concatenated.toString(),
  byteLength: Buffer.byteLength("💩"),
  compare: Buffer.compare(Buffer.from("a"), Buffer.from("b")),
  json: utf8.toJSON(),
  cryptoOffsetBounds: cryptoBacking[0] === 17 && cryptoBacking[1] === 17 && cryptoBacking[4] === 17 && cryptoBacking[5] === 17,
  cryptoBufferIdentity: timingSafeEqual(cryptoBuffer, Buffer.from(cryptoBuffer)),
};
console.log(JSON.stringify(output));
`;

const run = (command: string, args: readonly string[], cwd: string) => {
  const result = spawnSync(command, [...args], { cwd, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  const line = result.stdout.trim().split("\n").at(-1);
  if (line === undefined) throw new Error(`${command} produced no JSON output`);
  return JSON.parse(line) as unknown;
};

const temporary = mkdtempSync(join(ROOT, ".buffer-smoke-"));
try {
  const script = join(temporary, "fixture.mjs");
  writeFileSync(script, fixture, "utf8");
  const nodeResult = run(process.execPath, [script], temporary);
  const blitzResult = existsSync(blitzBinary)
    ? run(blitzBinary, ["--node-compat", "tooling", "--module", script], REPOSITORY)
    : run("cargo", ["run", "-q", "-p", "blitz-cli", "--", "--node-compat", "tooling", "--module", script], REPOSITORY);
  if (JSON.stringify(blitzResult) !== JSON.stringify(nodeResult)) {
    throw new Error(`Node and Blitz Buffer results differ:\nNode: ${JSON.stringify(nodeResult)}\nBlitz: ${JSON.stringify(blitzResult)}`);
  }
  console.log("node:buffer differential smoke passed against local Node and Blitz.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
