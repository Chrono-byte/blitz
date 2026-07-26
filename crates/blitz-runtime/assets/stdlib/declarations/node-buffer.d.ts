export type BufferEncoding = "ascii" | "utf8" | "utf-8" | "utf16le" | "ucs2" | "ucs-2" | "base64" | "base64url" | "latin1" | "binary" | "hex";

export interface Buffer extends Uint8Array {
  [index: number]: number;
  readonly length: number;
  readonly byteLength: number;
  readonly byteOffset: number;
  readonly buffer: ArrayBufferLike;
  toString(...args: any[]): string;
  toJSON(): { type: "Buffer"; data: number[] };
  equals(other: Uint8Array): boolean;
  compare(other: Uint8Array, targetStart?: number, targetEnd?: number, sourceStart?: number, sourceEnd?: number): -1 | 0 | 1;
  subarray(...args: any[]): Buffer;
  slice(...args: any[]): Buffer;
  copy(target: Uint8Array, targetStart?: number, sourceStart?: number, sourceEnd?: number): number;
  write(value: string, offset?: number, length?: number, encoding?: BufferEncoding): number;
  write(value: string, offset?: number, encoding?: BufferEncoding): number;
  readUInt8(offset?: number): number;
  readInt8(offset?: number): number;
  writeUInt8(value: number, offset?: number): number;
  writeInt8(value: number, offset?: number): number;
}

export interface BufferConstructor {
  new(size: number): Buffer;
  new(value: string, encoding?: BufferEncoding): Buffer;
  new(value: ArrayLike<number>): Buffer;
  readonly prototype: Buffer;
  readonly poolSize: number;
  readonly from: (value: unknown, ...args: any[]) => Buffer;
  alloc(size: number, fill?: string | number | Uint8Array, encoding?: BufferEncoding): Buffer;
  allocUnsafe(size: number): Buffer;
  allocUnsafeSlow(size: number): Buffer;
  byteLength(value: string | ArrayBufferView | ArrayBuffer | SharedArrayBuffer, encoding?: BufferEncoding): number;
  compare(left: Uint8Array, right: Uint8Array): -1 | 0 | 1;
  concat(list: readonly Uint8Array[], totalLength?: number): Buffer;
  isBuffer(value: unknown): value is Buffer;
  isEncoding(encoding: string): encoding is BufferEncoding;
}

export declare const Buffer: BufferConstructor;
export declare const SlowBuffer: (size: number) => Buffer;
export declare const INSPECT_MAX_BYTES: number;
export declare const constants: Readonly<{ MAX_LENGTH: number; MAX_STRING_LENGTH: number }>;
export declare const kMaxLength: number;
export declare const kStringMaxLength: number;
export declare function atob(value: string): string;
export declare function btoa(value: string): string;
export declare function isAscii(input: ArrayBuffer | ArrayBufferView): boolean;
export declare function isUtf8(input: ArrayBuffer | ArrayBufferView): boolean;

declare const buffer: {
  readonly Buffer: typeof Buffer;
  readonly SlowBuffer: typeof SlowBuffer;
  readonly INSPECT_MAX_BYTES: typeof INSPECT_MAX_BYTES;
  readonly constants: typeof constants;
  readonly kMaxLength: typeof kMaxLength;
  readonly kStringMaxLength: typeof kStringMaxLength;
  readonly atob: typeof atob;
  readonly btoa: typeof btoa;
  readonly isAscii: typeof isAscii;
  readonly isUtf8: typeof isUtf8;
};
export { buffer as default };
