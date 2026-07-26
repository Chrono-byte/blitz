import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const ROOT = new URL("..", import.meta.url).pathname;
const REPOSITORY = new URL("../..", import.meta.url).pathname;
const blitzBinary = join(REPOSITORY, "target/debug/blitz");

const fixture = `
import EventEmitter from "node:events";
import readline, { clearLine, clearScreenDown, createInterface, cursorTo, emitKeypressEvents, moveCursor } from "node:readline";
import promises, { Readline, createInterface as createPromiseInterface } from "node:readline/promises";

const input = new EventEmitter();
const writes = [];
const output = { write(value, callback) { writes.push(value); if (callback !== undefined) callback(); return true; } };
const rl = createInterface({ input, output, prompt: "$ " });
if (readline.Interface !== rl.constructor || readline.createInterface !== createInterface) throw new Error("readline default/named shape mismatch");
if (rl.getPrompt() !== "$ " || rl.closed) throw new Error("readline initial lifecycle mismatch");
let callbackAnswer;
const lineIterator = rl[Symbol.asyncIterator]();
rl.question("answer: ", answer => { callbackAnswer = answer; });
input.emit("data", "ok\\nfirst\\nsecond");
input.emit("end");
if (callbackAnswer !== "ok") throw new Error("readline question callback mismatch");
if (writes[0] !== "answer: ") throw new Error("readline question did not write query");
const firstLine = await lineIterator.next();
const secondLine = await lineIterator.next();
const endLine = await lineIterator.next();
if (firstLine.value !== "first" || secondLine.value !== "second" || endLine.done !== true) throw new Error("readline async iteration mismatch");

const pausedInput = new EventEmitter();
const paused = createInterface({ input: pausedInput });
const pausedLines = [];
paused.on("line", line => pausedLines.push(line));
paused.pause();
pausedInput.emit("data", "held\\n");
if (pausedLines.length !== 0) throw new Error("readline pause emitted a line");
paused.resume();
if (pausedLines[0] !== "held") throw new Error("readline resume did not flush a line");
pausedInput.emit("close");
if (!paused.closed) throw new Error("readline close lifecycle mismatch");

const keys = new EventEmitter();
let keyName;
keys.on("keypress", (_value, key) => { keyName = key.name; });
emitKeypressEvents(keys);
keys.emit("data", "\\u001b[A");
if (keyName !== "up") throw new Error("readline keypress mismatch");
clearLine(output, -1); clearScreenDown(output); cursorTo(output, 1, 2); moveCursor(output, -1, 2);
if (writes.slice(-4).join("") !== "\\u001b[1K\\u001b[0J\\u001b[3;2H\\u001b[1D\\u001b[2B") throw new Error("readline cursor helper mismatch");

const promiseInput = new EventEmitter();
const promiseRl = createPromiseInterface({ input: promiseInput, output });
if (promises.Interface !== promiseRl.constructor || promises.createInterface !== createPromiseInterface) throw new Error("readline/promises default/named shape mismatch");
const answer = promiseRl.question("promise: ");
promiseInput.emit("data", "yes\\n");
if (await answer !== "yes") throw new Error("readline promise question mismatch");
const editor = new Readline(output).cursorTo(1).moveCursor(1, 2).clearLine(0).clearScreenDown();
if (editor.rollback() !== editor || await editor.commit() !== null) throw new Error("readline editor rollback mismatch");
new Readline(output).cursorTo(1).moveCursor(1, 2).clearLine(0).clearScreenDown();
await new Readline(output).commit();
if (readline.promises !== promises) throw new Error("readline promises namespace mismatch");
promiseRl.close();
console.log("readline smoke passed");
`;

const temporary = mkdtempSync(join(ROOT, ".readline-smoke-"));
try {
  const script = join(temporary, "fixture.mjs");
  writeFileSync(script, fixture, "utf8");
  const command = existsSync(blitzBinary) ? blitzBinary : "cargo";
  const args = command === blitzBinary
    ? ["--node-compat", "tooling", "--module", script]
    : ["run", "-q", "-p", "blitz-cli", "--", "--node-compat", "tooling", "--module", script];
  const result = spawnSync(command, args, { cwd: REPOSITORY, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stdout}\n${result.stderr}`);
  if (!result.stdout.includes("readline smoke passed")) throw new Error("Blitz readline smoke produced no success marker");
  console.log("Blitz node:readline and node:readline/promises smoke passed.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
