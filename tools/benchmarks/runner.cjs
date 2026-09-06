// SPDX-License-Identifier: MPL-2.0
// Same checked function body as Denko; timing excludes loading and parsing.
const fs = require("node:fs");
const [file, warmupText, sampleText] = process.argv.slice(2);
const warmups = Number(warmupText);
const samples = Number(sampleText);
if (!Number.isInteger(warmups) || warmups < 0 || !Number.isInteger(samples) || samples < 1) {
  throw new Error("usage: runner.cjs FILE WARMUPS SAMPLES");
}
new Function(fs.readFileSync(file, "utf8"))();
const milliseconds = [];
let checksum;
for (let index = 0; index < warmups + samples; index++) {
  const start = performance.now();
  const value = globalThis.benchmark();
  const elapsed = performance.now() - start;
  if (!Number.isFinite(value) || (checksum !== undefined && checksum !== value)) {
    throw new Error("benchmark checksum changed");
  }
  checksum = value;
  if (index >= warmups) milliseconds.push(elapsed);
}
console.log(JSON.stringify({ milliseconds, checksum }));
