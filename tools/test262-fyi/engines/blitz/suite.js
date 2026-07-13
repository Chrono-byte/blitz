import fs from 'node:fs';
import { dirname, relative, resolve, sep } from 'node:path';
import { spawnSync } from 'node:child_process';
import setup, { test262Bin } from './setup.js';

const metadata = await setup();
const args = process.argv.slice(2);
let output = resolve('results', 'blitz.json');
const outputIndex = args.indexOf('--output');
if (outputIndex !== -1) {
  if (!args[outputIndex + 1]) throw new Error('--output requires a path');
  output = resolve(args[outputIndex + 1]);
  args.splice(outputIndex, 2);
}

if (!fs.existsSync(resolve('test262', 'test'))) {
  throw new Error(
    `Run this command from a test262-fyi working directory containing ./test262 (cwd: ${process.cwd()})`,
  );
}

fs.mkdirSync(dirname(output), { recursive: true });
const rawReport = resolve(dirname(output), `.blitz-raw-${process.pid}.json`);
const started = performance.now();
const result = spawnSync(test262Bin, [...args, '--report-json', rawReport], {
  cwd: process.cwd(),
  encoding: 'utf8',
  stdio: 'inherit',
});

if (result.status !== 0 && result.status !== 1) {
  fs.rmSync(rawReport, { force: true });
  throw new Error(`Blitz Test262 runner exited with status ${result.status}`);
}

const report = JSON.parse(fs.readFileSync(rawReport, 'utf8'));
fs.rmSync(rawReport, { force: true });

const passes = {};
for (const [, file, testResult] of report.results) {
  const normalized = file.split(sep).join('/').replace(/^test262\/test\//, '');
  const passed = testResult === 'Pass';
  passes[normalized] = (passes[normalized] ?? true) && passed;
}

const payload = {
  passes,
  time: performance.now() - started,
  ...metadata,
};
fs.writeFileSync(output, JSON.stringify(payload));

const passed = Object.values(passes).filter(Boolean).length;
console.log(`blitz passes: ${passed} / ${Object.keys(passes).length}`);
console.log(`wrote ${relative(process.cwd(), output)}`);
