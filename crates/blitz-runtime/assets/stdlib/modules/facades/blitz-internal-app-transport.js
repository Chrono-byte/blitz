import { host } from "blitz-internal:host";

const bridge = () => {
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.appTransport;
  if (value === undefined) {
    throw new Error("blitz:app transport primitive is not installed");
  }
  return value;
};

export const openAppServer = (options) => bridge().openAppServer(options);
export const closeAppServer = (server) => bridge().closeAppServer(server);
export const awaitShutdown = (server) => bridge().awaitShutdown(server);
