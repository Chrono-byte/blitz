import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";
import type * as Scope from "effect/Scope";
import type * as Stream from "effect/Stream";

export interface TextMessage {
  readonly _tag: "Text";
  readonly data: string;
}

export interface BinaryMessage {
  readonly _tag: "Binary";
  readonly data: Uint8Array;
}

export type Message = TextMessage | BinaryMessage;
export type SendMessage = string | Uint8Array;

export interface CloseOptions {
  readonly code?: number;
  readonly reason?: string;
}

export interface ConnectOptions {
  readonly url: string | URL;
  readonly protocols?: ReadonlyArray<string>;
  readonly queueCapacity?: number;
  readonly maxMessageBytes?: number;
}

export interface UpgradeOptions {
  readonly protocols?: ReadonlyArray<string>;
  readonly queueCapacity?: number;
  readonly maxMessageBytes?: number;
}

export interface WebSocketError {
  readonly _tag: "WebSocketError";
  readonly reason:
    | "InvalidOptions"
    | "PermissionDenied"
    | "Dns"
    | "Connect"
    | "UpgradeRejected"
    | "Protocol"
    | "Overflow"
    | "Closed"
    | "Host";
  readonly operation: "connect" | "prepareUpgrade" | "accept" | "send" | "close" | "messages";
  readonly message: string;
}

export interface Connection {
  readonly messages: Stream.Stream<Message, WebSocketError>;
  readonly send: (
    message: SendMessage,
  ) => Effect.Effect<void, WebSocketError>;
  readonly close: (
    options?: CloseOptions,
  ) => Effect.Effect<void, WebSocketError>;
}

export interface WebSocketUpgrade {
  readonly _tag: "WebSocketUpgrade";
  readonly response: Response;
  readonly accept: Effect.Effect<
    Connection,
    WebSocketError,
    Scope.Scope
  >;
}

export interface WebSocketService {
  readonly connect: (
    options: ConnectOptions,
  ) => Effect.Effect<
    Connection,
    WebSocketError,
    Scope.Scope
  >;
  readonly upgrade: (
    request: Request,
    options?: UpgradeOptions,
  ) => Effect.Effect<WebSocketUpgrade, WebSocketError>;
}

export type WebSocket = "blitz:websocket/WebSocket";

export declare const WebSocket: Context.Tag<WebSocket, WebSocketService>;

export declare const connect: (
  options: ConnectOptions,
) => Effect.Effect<
  Connection,
  WebSocketError,
  WebSocket | Scope.Scope
>;

export declare const upgrade: (
  request: Request,
  options?: UpgradeOptions,
) => Effect.Effect<WebSocketUpgrade, WebSocketError, WebSocket>;

export declare const layerLive: Layer.Layer<WebSocket>;
export declare const layer: Layer.Layer<WebSocket>;
