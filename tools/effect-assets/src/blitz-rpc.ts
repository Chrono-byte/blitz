export type JsonValue = null | boolean | number | string | JsonValue[] | { readonly [key: string]: JsonValue };

export interface RpcRequest {
  readonly id: string;
  readonly type: "query" | "mutation" | "subscription";
  readonly path: string;
  readonly input: JsonValue;
}

export interface RpcResponse {
  readonly id: string;
  readonly ok: boolean;
  readonly data?: JsonValue;
  readonly error?: JsonValue;
}

export const maxBatchSize = 32 as const;
