import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";

const ROOT = resolve(import.meta.dirname, "..");
const REPOSITORY = resolve(ROOT, "../..");
const blitzBinary = join(REPOSITORY, "target/debug/blitz");

const fixture = `
import constants, * as namedConstants from "../src/facades/node-constants.js";
import tty, { ReadStream, WriteStream, isatty } from "../src/facades/node-tty.js";
import traceEvents, { createTracing, getEnabledCategories } from "../src/facades/node-trace-events.js";
import domain, { Domain, _stack, active as namedDomainActive, create, createDomain } from "../src/facades/node-domain.js";
import { EventEmitter } from "../src/facades/node-events.js";
import sea, { getAsset, getAssetAsBlob, getAssetKeys, getRawAsset, isSea } from "../src/facades/node-sea.js";

if (!Object.isFrozen(constants)) throw new Error("node:constants default must be immutable");
if (constants.COPYFILE_EXCL !== namedConstants.COPYFILE_EXCL) throw new Error("node:constants named/default mismatch");
if ("SIGINT" in constants || "EACCES" in constants) throw new Error("node:constants fabricated unknown host values");
if (tty.ReadStream !== ReadStream || tty.WriteStream !== WriteStream || tty.isatty !== isatty) throw new Error("node:tty named/default mismatch");
if (isatty(0) !== false) throw new Error("node:tty reported an unsupported stdio descriptor");
for (const Constructor of [ReadStream, WriteStream]) {
  try {
    new Constructor(0);
    throw new Error("node:tty constructed without process stdio support");
  } catch (error) {
    if (error.code !== "ERR_NOT_IMPLEMENTED") throw error;
  }
}
const tracing = createTracing({ categories: ["blitz", "smoke"] });
if (tracing.categories !== "blitz,smoke" || tracing.enabled !== false) throw new Error("node:trace_events initial state mismatch");
try {
  tracing.enable();
  throw new Error("node:trace_events silently enabled without a trace sink");
} catch (error) {
  if (error.code !== "ERR_NOT_IMPLEMENTED") throw error;
}
if (tracing.enabled !== false || getEnabledCategories() !== "") throw new Error("node:trace_events claimed an active sink");
tracing.disable();
if (traceEvents.createTracing !== createTracing || traceEvents.getEnabledCategories !== getEnabledCategories) throw new Error("node:trace_events named/default mismatch");
if (domain.Domain !== Domain || domain.create !== create || domain.createDomain !== createDomain) throw new Error("node:domain named/default mismatch");
if (Object.keys(domain).join(",") !== "_stack,Domain,createDomain,create,active") throw new Error("node:domain default shape mismatch");
if (namedDomainActive !== null || domain.active !== null || _stack.length !== 0) throw new Error("node:domain initial state mismatch");
const currentDomain = create();
if (!(currentDomain instanceof Domain) || currentDomain.members.length !== 0) throw new Error("node:domain construction mismatch");
let routed;
currentDomain.on("error", error => { routed = error; });
const emitter = new EventEmitter();
currentDomain.add(emitter);
const domainError = new Error("domain error");
if (emitter.emit("error", domainError) !== false || routed !== domainError || domainError.domainEmitter !== emitter) throw new Error("node:domain error routing mismatch");
let runValue;
currentDomain.run(value => { if (domain.active !== currentDomain) throw new Error("node:domain active mismatch"); runValue = value; }, 42);
if (runValue !== 42 || domain.active !== undefined) throw new Error("node:domain run/exit mismatch");
let intercepted;
currentDomain.intercept(value => { intercepted = value; })(null, "ok");
if (intercepted !== "ok") throw new Error("node:domain intercept mismatch");
currentDomain.dispose();
if (currentDomain.members.length !== 0 || emitter.domain !== null) throw new Error("node:domain dispose mismatch");
if (Object.keys(sea).join(",") !== "isSea,getAsset,getRawAsset,getAssetAsBlob,getAssetKeys") throw new Error("node:sea default shape mismatch");
if (isSea() !== false || sea.isSea !== isSea || sea.getAsset !== getAsset || sea.getRawAsset !== getRawAsset || sea.getAssetAsBlob !== getAssetAsBlob || sea.getAssetKeys !== getAssetKeys) throw new Error("node:sea named/default mismatch");
if ("startupSnapshot" in sea) throw new Error("node:sea fabricated startupSnapshot");
const expectOutsideSea = (operation, label) => {
  try { operation(); throw new Error(label + " did not throw"); } catch (error) {
    if (error.code !== "ERR_NOT_IN_SINGLE_EXECUTABLE_APPLICATION") throw error;
  }
};
expectOutsideSea(() => getAsset("missing"), "getAsset");
expectOutsideSea(() => getRawAsset("missing"), "getRawAsset");
expectOutsideSea(() => getAssetAsBlob("missing"), "getAssetAsBlob");
expectOutsideSea(() => getAssetKeys(), "getAssetKeys");
try { getAsset("missing", true); throw new Error("getAsset accepted an invalid encoding"); } catch (error) {
  if (error.code !== "ERR_INVALID_ARG_TYPE") throw error;
}
console.log("node roots smoke passed");
`;

const temporary = mkdtempSync(join(ROOT, ".node-roots-smoke-"));
try {
  const script = join(temporary, "fixture.mjs");
  writeFileSync(script, fixture, "utf8");
  const command = existsSync(blitzBinary) ? blitzBinary : "cargo";
  const args = existsSync(blitzBinary)
    ? ["--node-compat", "full", "--module", script]
    : ["run", "-q", "-p", "blitz-cli", "--", "--node-compat", "full", "--module", script];
  const result = spawnSync(command, args, { cwd: REPOSITORY, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  if (!result.stdout.includes("node roots smoke passed")) throw new Error("Blitz node roots smoke produced no success marker");
  console.log("Blitz node:domain, node:sea, node:constants, node:tty, and node:trace_events smoke passed.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
