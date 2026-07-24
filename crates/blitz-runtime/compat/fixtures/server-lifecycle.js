async function observeServerLifecycle(runtime) {
  const server = runtime.serve(
    { hostname: "127.0.0.1", port: 0 },
    () => new Response("ok"),
  );
  const observation = {
    transport: server.addr.transport,
    hostname: server.addr.hostname,
    portIsAssigned: Number.isInteger(server.addr.port) && server.addr.port > 0,
    shutdownReturnsPromise: false,
    finishedSettled: false,
  };
  server.unref();
  server.ref();
  server.finished.then(() => {
    observation.finishedSettled = true;
  });
  const shutdown = server.shutdown();
  observation.shutdownReturnsPromise = shutdown instanceof Promise;
  await shutdown;
  return observation;
}
