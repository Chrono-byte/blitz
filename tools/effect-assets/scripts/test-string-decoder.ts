import { blitzCommand } from "./blitz-command.ts";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const ROOT = resolve(import.meta.dirname, "..");
const REPOSITORY = resolve(ROOT, "../..");

const fixture = `
import stringDecoder, { StringDecoder } from "../src/facades/node-string-decoder.js";

const assert = (condition, message) => { if (!condition) throw new Error(message); };
assert(Object.keys(stringDecoder).join(",") === "StringDecoder", "default export shape");
assert(stringDecoder.StringDecoder === StringDecoder, "default/named identity");
assert(Object.keys(StringDecoder.prototype).join(",") === "write,end,text,lastChar,lastNeed,lastTotal", "prototype shape");

const state = decoder => ({
  need: decoder.lastNeed,
  total: decoder.lastTotal,
  char: Array.from(decoder.lastChar),
});
const decoder = new StringDecoder("UTF-8");
const chunks = [new Uint8Array([0xf0]), new Uint8Array([0x9f, 0x92]), new Uint8Array([0xa9])];
const steps = chunks.map(chunk => {
  const output = decoder.write(chunk);
  return { output, state: state(decoder) };
});
const writes = steps.map(step => step.output);
const states = steps.map(step => step.state);
assert(JSON.stringify(states.map(value => value.need)) === "[3,1,0]", "per-write pending byte counts");
assert(JSON.stringify(states.map(value => value.total)) === "[4,4,0]", "per-write sequence lengths");
const ended = decoder.end();

const pending = new StringDecoder();
const pendingWrite = pending.write(new Uint8Array([0xe2, 0x82]));
const pendingState = state(pending);
const pendingEnd = pending.end();
const invalid = new StringDecoder().write(new Uint8Array([0xe2, 0x28, 0xa1]));
const arrayDecoder = new StringDecoder();
const arrayResult = arrayDecoder.write([0xe2]) + arrayDecoder.end([0x82, 0xac]);
const encodings = {
  latin1: new StringDecoder("latin1").write([0xc3, 0xa9]),
  ascii: new StringDecoder("ascii").write([0xc3, 0xa9]),
  utf16le: new StringDecoder("utf16le").write([0x3d, 0xd8, 0x00, 0xde]),
};
const unsupported = (() => {
  try { new StringDecoder("base64"); return null; } catch (error) {
    return { name: error.name, code: error.code, message: error.message };
  }
})();
const text = new StringDecoder().text(new Uint8Array([0xe2, 0x82, 0xac]), 0, 3);
assert(JSON.stringify(writes) === JSON.stringify(["", "", "💩"]), "split UTF-8 writes");
assert(ended === "", "empty end");
assert(pendingWrite === "" && pendingState.need === 1 && pendingState.total === 3 && pendingState.char[0] === 0xe2 && pendingState.char[1] === 0x82, "observable pending state");
assert(pendingEnd === "�" && pending.lastNeed === 0, "pending end");
assert(invalid === "�(�", "invalid UTF-8 recovery");
assert(arrayResult === "€", "array byte chunks");
assert(encodings.latin1 === "Ã©" && encodings.ascii === "C)", "single-byte encodings");
assert(encodings.utf16le === "😀", "utf16le");
assert(unsupported && unsupported.name === "TypeError" && unsupported.code === "ERR_UNKNOWN_ENCODING" && unsupported.message === "Unknown encoding: base64", "stable unknown encoding");
assert(text === "€", "text helper");
console.log(JSON.stringify({ writes, states, ended, pendingWrite, pendingState, pendingEnd, invalid, arrayResult, encodings, unsupported, text }));
`;

const run = (command: string, args: readonly string[], cwd: string) => {
  const result = spawnSync(command, [...args], { cwd, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  const line = result.stdout.trim().split("\n").at(-1);
  if (!line) throw new Error(`${command} produced no JSON output`);
  return JSON.parse(line) as unknown;
};

const temporary = mkdtempSync(join(ROOT, ".string-decoder-smoke-"));
try {
  const script = join(temporary, "fixture.mjs");
  writeFileSync(script, fixture, "utf8");
  const nodeResult = run(process.execPath, [script], temporary);
  const { command, args } = blitzCommand(["--module", script]);
  const blitzResult = run(command, args, REPOSITORY);

  if (JSON.stringify(blitzResult) !== JSON.stringify(nodeResult)) {
    throw new Error(`Node and Blitz string_decoder results differ:\nNode: ${JSON.stringify(nodeResult)}\nBlitz: ${JSON.stringify(blitzResult)}`);
  }
  console.log("node:string_decoder differential smoke passed against local Node and Blitz.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
