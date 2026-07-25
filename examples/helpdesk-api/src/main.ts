// SPDX-License-Identifier: MPL-2.0
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Http from "blitz:http";
import * as Runtime from "blitz:runtime";
import { config } from "./config.ts";
import { handler } from "./http.ts";
import { layerInMemory } from "./ticket-repository.ts";
import { layer as ticketServiceLayer } from "./ticket-service.ts";

const domainLayer = ticketServiceLayer.pipe(Layer.provide(layerInMemory));

const program = Effect.scoped(
  Layer.build(domainLayer).pipe(
    Effect.flatMap((services) =>
      Effect.sync(() => {
        console.log(
          `Helpdesk API listening on http://${config.hostname}:${config.port}`,
        );
      }).pipe(
        Effect.zipRight(
          Http.serve({
            hostname: config.hostname,
            port: config.port,
            maxRequestBodyBytes: 8_192,
            requestTimeout: "10 seconds",
            handler: (request, context) =>
              handler(request, context).pipe(Effect.provide(services)),
          }),
        ),
      ),
    ),
  ),
);

Runtime.runMain(Runtime.provideLive(program));
