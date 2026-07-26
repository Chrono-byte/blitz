import type {
  InterfaceOptions,
  PromiseReadlineInterface,
  ReadlineEditor,
} from "node:readline";

export declare const Interface: {
  new (options: InterfaceOptions): PromiseReadlineInterface;
  new (input: import("node:readline").ReadlineInput | AsyncIterable<string | Uint8Array>, output?: import("node:readline").ReadlineOutput, completer?: InterfaceOptions["completer"], terminal?: boolean): PromiseReadlineInterface;
};
export declare const Readline: {
  new (stream: import("node:readline").ReadlineOutput): ReadlineEditor;
};
export declare function createInterface(options: InterfaceOptions): PromiseReadlineInterface;
export declare function createInterface(input: import("node:readline").ReadlineInput | AsyncIterable<string | Uint8Array>, output?: import("node:readline").ReadlineOutput, completer?: InterfaceOptions["completer"], terminal?: boolean): PromiseReadlineInterface;

declare const readlinePromises: {
  readonly Interface: typeof Interface;
  readonly Readline: typeof Readline;
  readonly createInterface: typeof createInterface;
};

export { readlinePromises as default };
