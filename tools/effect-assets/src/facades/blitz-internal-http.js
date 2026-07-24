const bridge = () => {
  const host = globalThis.__blitz_host_v1;
  if (host === undefined || host.abiVersion !== 1) {
    throw new Error("Blitz host ABI v1 is not installed");
  }
  const value = host.http;
  if (value === undefined) {
    throw new Error("blitz:http host primitive is not installed");
  }
  return value;
};

export const openServer = (options) => bridge().openServer(options);
export const closeServer = (server) => bridge().closeServer(server);
export const awaitShutdown = (server) => bridge().awaitShutdown(server);
