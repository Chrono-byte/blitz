import type {
  FinishedOptions,
  PipelineOptions,
  StreamInstance,
} from "node:stream";

export function finished(stream: StreamInstance, options?: FinishedOptions): Promise<void>;
export function pipeline(...streams: Array<StreamInstance | PipelineOptions>): Promise<void>;

declare const streamPromises: {
  readonly finished: typeof finished;
  readonly pipeline: typeof pipeline;
};

export { streamPromises as default };
