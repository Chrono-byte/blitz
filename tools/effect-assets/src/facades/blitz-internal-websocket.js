import { host } from "blitz-internal:host";

const bridge = () => {
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.webSocket;
  if (value === undefined) {
    throw new Error("blitz:websocket host primitive is not installed");
  }
  return value;
};

export const connect = (options, complete) =>
  bridge().connect(options, complete);

export const prepareUpgrade = (request, options, complete) =>
  bridge().prepareUpgrade(request, options, complete);

export const accept = (upgrade, complete) =>
  bridge().accept(upgrade, complete);

export const send = (connection, message, complete) =>
  bridge().send(connection, message, complete);

export const close = (connection, options, complete) =>
  bridge().close(connection, options, complete);

export const subscribe = (connection, onEvent) =>
  bridge().subscribe(connection, onEvent);

export const cancel = (operation) =>
  bridge().cancel(operation);
