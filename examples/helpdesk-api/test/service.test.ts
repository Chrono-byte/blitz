// SPDX-License-Identifier: MPL-2.0
import * as Effect from "effect/Effect";
import { makeInMemory } from "../src/ticket-repository.ts";
import { make as makeTicketService } from "../src/ticket-service.ts";

const tests = makeInMemory.pipe(
  Effect.flatMap((repository) => {
    const service = makeTicketService(repository);

    return service
      .create({
        title: "Customer cannot reset password",
        description: "The reset email never arrives.",
        priority: "high",
      })
      .pipe(
        Effect.flatMap((created) =>
          service.changeStatus(created.id, "in_progress"),
        ),
        Effect.flatMap((updated) => {
          if (updated.id !== "TKT-1" || updated.status !== "in_progress") {
            throw new Error("ticket workflow returned an unexpected result");
          }
          return service.update(updated.id, {
            title: "Customer still cannot reset password",
            description: "The reset email is delayed.",
            priority: "urgent",
          });
        }),
        Effect.flatMap((updated) => {
          if (updated.priority !== "urgent" || updated.status !== "in_progress") {
            throw new Error("ticket update returned an unexpected result");
          }
          return service.remove(updated.id);
        }),
        Effect.flatMap(() => service.list()),
        Effect.map((tickets) => {
          if (tickets.length !== 0) {
            throw new Error("ticket delete returned an unexpected result");
          }
        }),
      );
  }),
);

Effect.runPromise(tests).then(
  () => console.log("service tests passed"),
  (error) => {
    console.error("service tests failed", error);
    process.exitCode = 1;
  },
);
