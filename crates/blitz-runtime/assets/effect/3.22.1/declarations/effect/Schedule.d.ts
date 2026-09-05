export interface Schedule<Output, Input = unknown, Requirements = never> {
  readonly _Output: (_: never) => Output;
  readonly _Input: (_: Input) => void;
  readonly _Requirements: (_: Requirements) => void;
}

export declare const once: Schedule<void>;
export declare const recurs: (times: number) => Schedule<number>;
export declare const spaced: (duration: number | string) => Schedule<number>;
export declare const exponential: (base: number | string) => Schedule<number>;
