import assert from "node:assert/strict";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const frame = (message: object): string => {
  const body = JSON.stringify(message);
  return `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
};

const waitForOutput = (child: ChildProcessWithoutNullStreams, pattern: RegExp): Promise<string> =>
  new Promise((resolve, reject) => {
    let output = "";
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error(`Timed out waiting for ${String(pattern)}: ${output}`));
    }, 10_000);
    const onData = (chunk: Buffer): void => {
      output += chunk.toString();
      if (!pattern.test(output)) return;
      cleanup();
      resolve(output);
    };
    const cleanup = (): void => {
      clearTimeout(timeout);
      child.stdout.off("data", onData);
    };
    child.stdout.on("data", onData);
  });

await test("language server completes an LSP handshake", async () => {
  const serverPath = fileURLToPath(new URL("../src/lsp.js", import.meta.url));
  const child = spawn(process.execPath, [serverPath]);
  let stderr = "";
  child.stderr.on("data", (chunk: Buffer) => {
    stderr += chunk.toString();
  });

  child.stdin.write(
    frame({ jsonrpc: "2.0", id: 1, method: "initialize", params: { capabilities: {} } }),
  );
  const output = await waitForOutput(child, /"id":1/u);
  assert.match(output, /blitz-language-server/u);
  assert.match(output, /documentSymbolProvider/u);
  assert.match(output, /definitionProvider/u);
  assert.match(output, /inlayHintProvider/u);

  child.stdin.write(frame({ jsonrpc: "2.0", id: 2, method: "shutdown", params: null }));
  await waitForOutput(child, /"id":2/u);
  child.stdin.end(frame({ jsonrpc: "2.0", method: "exit", params: null }));
  const code = await new Promise<number | null>((resolve) => child.once("exit", resolve));
  assert.equal(code, 0, stderr);
});
