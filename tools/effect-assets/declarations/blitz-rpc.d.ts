export type JsonValue = null | boolean | number | string | JsonValue[] | { readonly [key: string]: JsonValue };

export interface Router {
  readonly _tag: "BlitzRouter";
}

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
