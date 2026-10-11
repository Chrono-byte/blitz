export type StringDecoderInput = Uint8Array | readonly number[];

export declare class StringDecoder {
  constructor(encoding?: string | null);
  encoding: string;
  readonly lastNeed: number;
  readonly lastTotal: number;
  readonly lastChar: Uint8Array;
  write(buffer: StringDecoderInput): string;
  end(buffer?: StringDecoderInput): string;
  text(buffer: StringDecoderInput, start?: number, end?: number): string;
}

declare const stringDecoder: {
  StringDecoder: typeof StringDecoder;
};

export { stringDecoder as default };
