/**
 * The initial Blitz Node 24 crypto slice.
 *
 * randomBytes currently returns Uint8Array. randomFill and timingSafeEqual
 * accept Uint8Array, including Buffer and ArrayBuffer-backed subviews. Broader
 * Buffer-shaped crypto results and Web Crypto exports remain outside this slice.
 */

export interface RandomUUIDOptions {
  readonly disableEntropyCache?: boolean;
}

export interface NodeCryptoError extends Error {
  readonly code: string;
  readonly operation?: string;
  readonly syscall?: string;
}

export type CryptoByteArray = Uint8Array;

export declare function randomUUID(options?: RandomUUIDOptions): string;

/** Random byte allocations are bounded at 16 MiB by the Blitz host. */
export declare function randomBytes(size: number): CryptoByteArray;
export declare function randomBytes(
  size: number,
  callback: (error: NodeCryptoError | null, buffer?: CryptoByteArray) => void,
): void;

export declare function randomFillSync(
  buffer: CryptoByteArray,
  offset?: number,
  size?: number,
): CryptoByteArray;
export declare function randomFill(
  buffer: CryptoByteArray,
  callback: (error: NodeCryptoError | null, buffer?: CryptoByteArray) => void,
): void;
export declare function randomFill(
  buffer: CryptoByteArray,
  offset: number,
  callback: (error: NodeCryptoError | null, buffer?: CryptoByteArray) => void,
): void;
export declare function randomFill(
  buffer: CryptoByteArray,
  offset: number,
  size: number,
  callback: (error: NodeCryptoError | null, buffer?: CryptoByteArray) => void,
): void;

export declare function randomInt(max: number): number;
export declare function randomInt(min: number, max: number): number;
export declare function randomInt(
  max: number,
  callback: (error: NodeCryptoError | null, value?: number) => void,
): void;
export declare function randomInt(
  min: number,
  max: number,
  callback: (error: NodeCryptoError | null, value?: number) => void,
): void;

export declare function timingSafeEqual(a: CryptoByteArray, b: CryptoByteArray): boolean;

declare const crypto: {
  readonly randomUUID: typeof randomUUID;
  readonly randomBytes: typeof randomBytes;
  readonly randomFill: typeof randomFill;
  readonly randomFillSync: typeof randomFillSync;
  readonly randomInt: typeof randomInt;
  readonly timingSafeEqual: typeof timingSafeEqual;
};

export { crypto as default };
