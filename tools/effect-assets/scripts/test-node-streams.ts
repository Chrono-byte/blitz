import { blitzCommand } from "./blitz-command.ts";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const ROOT = resolve(import.meta.dirname, "..");
const REPOSITORY = resolve(ROOT, "../..");

const sourceRoot = join(ROOT, "src/facades");

const fixture = `
import stream, { Readable, Writable, Duplex, Transform, PassThrough, promises, pipeline, finished } from "./node-stream.js";
import promiseStream, { pipeline as promisePipeline, finished as promiseFinished } from "./node-stream-promises.js";
import consumers, { text, json } from "./node-stream-consumers.js";

const assert = (condition, message) => { if (!condition) throw new Error(message); };
const flags = {
  readable: false,
  callbackPipeline: false,
  passThrough: false,
  unpipe: false,
  promisePipeline: false,
  iterator: false,
  consumers: false,
  callbackFinished: false,
  promiseFinished: false,
  error: false,
};
let failure;
const fail = error => {
  if (failure === undefined) {
    failure = error instanceof Error ? error.message : String(error);
    console.log("Blitz stream smoke operation failure", failure);
  }
};
assert(stream.Readable === Readable && stream.Duplex === Duplex && stream.PassThrough === PassThrough && stream.promises === promises, "node:stream default identity");
assert(promiseStream === promises && promiseStream.pipeline === promisePipeline, "stream/promises identity");
assert(consumers.text === text && consumers.json === json, "consumers identity");
assert(consumers.arrayBuffer === undefined && consumers.buffer === undefined && consumers.bytes === undefined, "consumers must not fabricate byte APIs");

let readableValue = "";
const readable = new Readable({ read() {
  if (this.started) return;
  this.started = true;
  this.push("a");
  this.push("b");
  this.push(null);
} });
readable.on("error", fail);
readable.on("data", chunk => { readableValue += chunk; });
readable.on("end", () => {
  try { assert(readableValue === "ab", "readable push/end"); flags.readable = true; } catch (error) { fail(error); }
});

let pipelineValue = "";
const pipelineSink = new Writable({
  highWaterMark: 1,
  write(chunk, _encoding, callback) {
    setTimeout(() => { pipelineValue += chunk; callback(); }, 1);
  },
});
pipeline(
  Readable.from(["a", "b"]),
  new Transform({ transform(chunk, _encoding, callback) { callback(null, String(chunk).toUpperCase()); } }),
  pipelineSink,
  error => {
    if (error !== undefined) fail(error);
    else {
      try { assert(pipelineValue === "AB", "pipeline/backpressure"); flags.callbackPipeline = true; } catch (pipelineError) { fail(pipelineError); }
    }
  },
);

const iterator = Readable.from(["x", "y"])[Symbol.asyncIterator]();
iterator.next().then(first => iterator.next().then(second => iterator.next().then(last => {
  try {
    assert(first.value === "x" && second.value === "y" && last.done, "async iterator lifecycle");
    flags.iterator = true;
  } catch (error) { fail(error); }
}, fail), fail), fail);

text(Readable.from([new Uint8Array([65, 66])])).then(value => {
  assert(value === "AB", "consumer text");
  return json(Readable.from(["{\\"ok\\":true}"]));
}).then(value => {
  assert(value.ok === true, "consumer json");
  flags.consumers = true;
}, error => { console.log("consumers failure", error && error.message); fail(error); });

const callbackTarget = new Writable({ write(_chunk, _encoding, callback) { callback(); } });
finished(callbackTarget, { readable: false }, error => {
  if (error !== undefined) fail(error);
  else flags.callbackFinished = true;
});
callbackTarget.end("done");

const errorTarget = new Writable({ write(_chunk, _encoding, callback) {
  const error = new Error("expected stream failure");
  error.code = "E_EXPECTED";
  callback(error);
} });
let errorEventSeen = false;
errorTarget.once("error", error => { errorEventSeen = error.code === "E_EXPECTED"; });
errorTarget.write("bad", error => {
  if (error === undefined || error.code !== "E_EXPECTED") fail(new Error("write callback error lifecycle"));
});
setTimeout(() => {
  try {
    assert(errorEventSeen, "error event lifecycle");
    flags.error = true;
  } catch (error) { fail(error); }
}, 200);

const runPromiseForms = () => {
  const passThrough = new PassThrough();
let passThroughValue = "";
passThrough.on("data", chunk => { passThroughValue += chunk; });
passThrough.on("end", () => {
  try { assert(passThroughValue === "pass", "PassThrough state machine"); flags.passThrough = true; } catch (error) { fail(error); }
});
passThrough.end("pass");

const unpipeSource = Readable.from(["unpipe"]);
const unpipeTarget = new Writable({ write(_chunk, _encoding, callback) { callback(); } });
let unpipeSeen = false;
unpipeTarget.once("unpipe", () => { unpipeSeen = true; });
unpipeSource.pipe(unpipeTarget);
unpipeSource.unpipe(unpipeTarget);
setTimeout(() => {
  if (unpipeSeen) flags.unpipe = true;
  else fail(new Error("unpipe lifecycle"));
}, 200);

let promisePipelineValue = "";
let promiseWriteCompleted = false;
const promisePipelineSink = new Writable({ write(chunk, _encoding, callback) {
  setTimeout(() => {
    promisePipelineValue += chunk;
    promiseWriteCompleted = true;
    callback();
  }, 1);
} });
  const promisePipelineResult = promisePipeline(Readable.from(["promise"]), promisePipelineSink);
  promisePipelineResult.then(() => {
    try {
      assert(promiseWriteCompleted && promisePipelineValue === "promise", "promise pipeline resolved before transferring its data");
      flags.promisePipeline = true;
    } catch (error) { fail(error); }
  }, fail);
  const promiseTarget = new Writable({ write(_chunk, _encoding, callback) { callback(); } });
  const promiseFinishedResult = promiseFinished(promiseTarget, { readable: false });
  promiseFinishedResult.then(() => { flags.promiseFinished = true; }, error => { console.log("promiseFinished failure", error && error.message); fail(error); });
  promiseTarget.end("done");
};
setTimeout(runPromiseForms, 400);

setTimeout(() => {
  try {
    if (failure !== undefined) throw new Error(failure);
    for (const name of ["readable", "callbackPipeline", "passThrough", "unpipe", "promisePipeline", "iterator", "consumers", "callbackFinished", "promiseFinished", "error"]) {
      assert(flags[name], name + " smoke did not complete");
    }
    globalThis.streamSmokeResult = "passed";
  } catch (error) {
    globalThis.streamSmokeResult = "failed:" + error.message;
  }
  if (globalThis.streamSmokeResult !== "passed") {
    console.log("Blitz stream smoke failure", globalThis.streamSmokeResult, failure, flags.readable, flags.callbackPipeline, flags.passThrough, flags.unpipe, flags.promisePipeline, flags.iterator, flags.consumers, flags.callbackFinished, flags.promiseFinished, flags.error);
    throw new Error(globalThis.streamSmokeResult);
  }
  console.log("Blitz node stream smoke passed");
}, 2000);
`;

const temporary = mkdtempSync(join(ROOT, ".node-stream-smoke-"));
try {
  writeFileSync(join(temporary, "node-stream.js"), readFileSync(join(sourceRoot, "node-stream.js")));
  writeFileSync(join(temporary, "node-stream-consumers.js"), readFileSync(join(sourceRoot, "node-stream-consumers.js")));
  const promisesSource = readFileSync(join(sourceRoot, "node-stream-promises.js"), "utf8")
    .replace('from "node:stream"', 'from "./node-stream.js"');
  writeFileSync(join(temporary, "node-stream-promises.js"), promisesSource, "utf8");
  const entry = join(temporary, "fixture.mjs");
  writeFileSync(entry, fixture, "utf8");

  const { command, args } = blitzCommand(["--module", entry]);
  // A clean Cargo target also needs time to compile the executable.
  const result = spawnSync(command, args, { cwd: REPOSITORY, encoding: "utf8", timeout: command === "cargo" ? 300_000 : 30_000 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  if (!result.stdout.includes("Blitz node stream smoke passed")) {
    throw new Error(`Blitz stream smoke produced no success marker:\n${result.stdout}\n${result.stderr}`);
  }
  console.log("Blitz node:stream, node:stream/promises, and node:stream/consumers smoke passed.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
