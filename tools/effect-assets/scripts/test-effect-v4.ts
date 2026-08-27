import { strict as assert } from "node:assert";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { rollup } from "rollup";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EFFECT_ENTRY = resolve(ROOT, "node_modules/effect-v4/dist/index.js");
const BLITZ = resolve(ROOT, "../../target/debug/blitz");
const MARKER = "BLITZ_EFFECT_V4_RESULT=";

const source = `
import { Chunk, Effect, FileSystem, Ref, Stream } from ${JSON.stringify(EFFECT_ENTRY)};

const refEffect = Ref.make(false);
if (typeof refEffect[Symbol.iterator] !== "function") {
  throw new Error("Ref.make Effect is not iterable");
}
const refIterator = refEffect[Symbol.iterator]();
if (typeof refIterator.next !== "function") {
  throw new Error("Ref.make Effect iterator has no next method");
}

const program = Effect.gen(function*() {
  const finalized = yield* refEffect;
  yield* Effect.scoped(
    Effect.acquireRelease(
      Effect.succeed("resource"),
      () => Ref.set(finalized, true)
    )
  );

  const fileSystem = FileSystem.makeNoop({
    readFileString: () => Effect.succeed("effect-v4")
  });
  const text = yield* fileSystem.readFileString("ignored");
  const values = yield* Stream.runCollect(Stream.make(1, 2, 3));
  yield* Effect.sleep("1 millis");

  return {
    finalized: yield* Ref.get(finalized),
    stream: Chunk.toArray(values),
    text
  };
});

Effect.runPromise(program).then(
  result => console.log(${JSON.stringify(MARKER)} + JSON.stringify(result)),
  error => {
    console.error(error);
    globalThis.process.exitCode = 1;
  }
);
`;

const temporary = mkdtempSync(join(tmpdir(), "blitz-effect-v4-"));

try {
  const entry = join(temporary, "entry.js");
  const outputDirectory = join(temporary, "bundle");
  writeFileSync(entry, source);

  const bundle = await rollup({
    input: entry,
    plugins: [{
      name: "pinned-effect-v4",
      resolveId(specifier) {
        if (specifier === "effect") {
          return EFFECT_ENTRY;
        }
        if (specifier.startsWith("effect/")) {
          return resolve(ROOT, "node_modules/effect-v4/dist", `${specifier.slice("effect/".length)}.js`);
        }
        return null;
      },
    }],
    onwarn(warning) {
      throw new Error(`Effect v4 bundle warning: ${warning.message}`);
    },
    treeshake: { moduleSideEffects: false },
  });
  const written = await bundle.write({
    dir: outputDirectory,
    entryFileNames: "[name].mjs",
    format: "esm",
    preserveModules: true,
  });
  await bundle.close();

  const executable = written.output.find(item => item.type === "chunk" && item.isEntry);
  assert(executable && executable.type === "chunk", "Effect v4 bundle must contain an entry module");
  const output = join(outputDirectory, executable.fileName);
  assert(readFileSync(output, "utf8").length > 0, "Effect v4 entry must not be empty");
  const execution = spawnSync(BLITZ, [output], { encoding: "utf8" });
  assert.equal(execution.status, 0, `${execution.stderr}\n${execution.stdout}`);

  const line = execution.stdout
    .split("\n")
    .find(value => value.startsWith(MARKER));
  assert(line, `missing Effect v4 result marker in stdout:\n${execution.stdout}`);
  assert.deepEqual(JSON.parse(line.slice(MARKER.length)), {
    finalized: true,
    stream: [1, 2, 3],
    text: "effect-v4",
  });
  console.log("Effect v4 executes in Denko with services, scopes, streams, and scheduling.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
