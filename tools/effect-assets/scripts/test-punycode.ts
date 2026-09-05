import { blitzCommand } from "./blitz-command.ts";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const ROOT = resolve(import.meta.dirname, "..");
const REPOSITORY = resolve(ROOT, "../..");

const fixture = (specifier: string) => `
import * as p from ${JSON.stringify(specifier)};

const attempt = (operation, ...args) => {
  try {
    return { ok: true, value: operation(...args) };
  } catch (error) {
    return { ok: false, name: error.name, message: error.message };
  }
};
const descriptors = object => Object.keys(object).map(key => {
  const descriptor = Object.getOwnPropertyDescriptor(object, key);
  return [key, descriptor.enumerable, descriptor.writable, descriptor.configurable];
});
const values = ["", "abc", "mañana", "☃-⌘", "💩", "例え.テスト", "a@mañana.com"];
const decoded = ["", "maana-pta", "--dqo34k", "ls8h", "r8jz45g", "zckzah"];
const domains = ["", "example.com", "mañana.com", "☃-⌘.com", "a@mañana.com", "a。b．c｡d"];
const unicodeDomains = ["", "example.com", "xn--maana-pta.com", "xn----dqo34k.com", "a@xn--maana-pta.com"];
const result = {
  namespaceKeys: Object.keys(p),
  defaultKeys: Object.keys(p.default),
  ucs2Keys: Object.keys(p.ucs2),
  defaultDescriptors: descriptors(p.default),
  ucs2Descriptors: descriptors(p.ucs2),
  defaultIsNamed: p.default.encode === p.encode && p.default.ucs2 === p.ucs2,
  version: p.version,
  defaultVersion: p.default.version,
  encode: values.map(value => attempt(p.encode, value)),
  decode: decoded.map(value => attempt(p.decode, value)),
  toASCII: domains.map(value => attempt(p.toASCII, value)),
  toUnicode: unicodeDomains.map(value => attempt(p.toUnicode, value)),
  ucs2Decode: values.map(value => attempt(p.ucs2.decode, value)),
  ucs2Encode: [[0x1f4a9, 0x61, 0x10000], []].map(value => attempt(p.ucs2.encode, value)),
};
console.log(JSON.stringify(result));
`;

const run = (command: string, args: readonly string[], cwd: string) => {
  const result = spawnSync(command, [...args], { cwd, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  const line = result.stdout.trim().split("\n").at(-1);
  if (!line) throw new Error(`${command} produced no JSON output`);
  return JSON.parse(line) as unknown;
};

const temporary = mkdtempSync(join(ROOT, ".punycode-smoke-"));
try {
  const script = join(temporary, "fixture.js");
  writeFileSync(script, fixture("../src/facades/node-punycode.js"), "utf8");
  const reference = join(temporary, "reference.mjs");
  writeFileSync(reference, fixture("node:punycode"), "utf8");
  const nodeResult = run(process.execPath, [reference], temporary);

  const { command, args } = blitzCommand(["--module", script]);
  const blitzResult = run(command, args, REPOSITORY);

  if (JSON.stringify(blitzResult) !== JSON.stringify(nodeResult)) {
    throw new Error(`Native Node and Blitz punycode results differ:\nNode: ${JSON.stringify(nodeResult)}\nBlitz: ${JSON.stringify(blitzResult)}`);
  }
  console.log("node:punycode differential smoke passed against local Node and Blitz.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
