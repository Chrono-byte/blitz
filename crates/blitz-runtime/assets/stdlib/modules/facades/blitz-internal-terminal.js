import { host } from "blitz-internal:host";

const bridge = () => {
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.terminal;
  if (value === undefined) {
    throw new Error("terminal primitive is not installed");
  }
  return value;
};

export const write = (text, complete) =>
  bridge().write(text, complete);

export const writeLine = (text, complete) =>
  bridge().writeLine(text, complete);

export const setPrompt = (prompt, complete) =>
  bridge().setPrompt(prompt, complete);

export const subscribeLines = (onEvent) =>
  bridge().subscribeLines(onEvent);

export const close = (complete) =>
  bridge().close(complete);

export const cancel = (operation) =>
  bridge().cancel(operation);
