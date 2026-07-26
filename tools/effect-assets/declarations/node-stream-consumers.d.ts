import type { StreamChunk } from "node:stream";

export function text(stream: AsyncIterable<StreamChunk>): Promise<string>;
export function json(stream: AsyncIterable<StreamChunk>): Promise<unknown>;

declare const consumers: {
  readonly json: typeof json;
  readonly text: typeof text;
};

export { consumers as default };
