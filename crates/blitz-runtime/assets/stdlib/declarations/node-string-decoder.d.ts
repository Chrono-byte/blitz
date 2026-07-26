/**
 * Pure-JavaScript incremental string decoding for Blitz.
 *
 * Blitz does not provide Node's Buffer, so this facade accepts Uint8Array and
 * readonly number[] byte chunks. It intentionally does not claim Buffer
 * compatibility.
 */
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
