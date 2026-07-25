// SPDX-License-Identifier: MPL-2.0
import * as Effect from "effect/Effect";
import type * as Http from "blitz:http";
import * as Domain from "./domain.ts";
import {
  changeTicketStatus,
  createTicket,
  deleteTicket,
  getTicket,
  listTickets,
  type TicketService,
  updateTicket,
} from "./ticket-service.ts";

class RequestBodyError {
  readonly _tag = "RequestBodyError";
  readonly message = "Request body must be valid JSON";
}

type ApiError = Domain.TicketError | RequestBodyError;

const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });

const readJson = (request: Request) =>
  Effect.async<unknown, RequestBodyError>((resume) => {
    request.json().then(
      (value) => resume(Effect.succeed(value)),
      () => resume(Effect.fail(new RequestBodyError())),
    );
  });

const validationFailure = (error: Domain.ValidationError) =>
  Effect.fail<ApiError>(error);

const create = (request: Request) =>
  readJson(request).pipe(
    Effect.flatMap((body) => {
      const input = Domain.decodeCreateTicket(body);
      return input instanceof Domain.ValidationError
        ? validationFailure(input)
        : createTicket(input);
    }),
    Effect.map((ticket) => json(ticket, 201)),
  );

const update = (request: Request, ticketId: string) =>
  readJson(request).pipe(
    Effect.flatMap((body) => {
      const input = Domain.decodeUpdateTicket(body);
      return input instanceof Domain.ValidationError
        ? validationFailure(input)
        : updateTicket(ticketId, input);
    }),
    Effect.map((ticket) => json(ticket)),
  );

const changeStatus = (request: Request, ticketId: string) =>
  readJson(request).pipe(
    Effect.flatMap((body) => {
      const input = Domain.decodeStatusChange(body);
      return input instanceof Domain.ValidationError
        ? validationFailure(input)
        : changeTicketStatus(ticketId, input.status);
    }),
    Effect.map((ticket) => json(ticket)),
  );

const route = (request: Request) => {
  const url = new URL(request.url);
  const parts = url.pathname.split("/").filter((part) => part.length > 0);

  if (request.method === "GET" && url.pathname === "/health") {
    return Effect.succeed(json({ status: "ok" }));
  }
  if (request.method === "GET" && url.pathname === "/tickets") {
    return listTickets().pipe(Effect.map((tickets) => json({ tickets })));
  }
  if (request.method === "POST" && url.pathname === "/tickets") {
    return create(request);
  }
  if (request.method === "GET" && parts.length === 2 && parts[0] === "tickets") {
    return getTicket(parts[1]).pipe(Effect.map((ticket) => json(ticket)));
  }
  if (request.method === "PUT" && parts.length === 2 && parts[0] === "tickets") {
    return update(request, parts[1]);
  }
  if (
    request.method === "DELETE" &&
    parts.length === 2 &&
    parts[0] === "tickets"
  ) {
    return deleteTicket(parts[1]).pipe(
      Effect.map(() => new Response(null, { status: 204 })),
    );
  }
  if (
    request.method === "PATCH" &&
    parts.length === 3 &&
    parts[0] === "tickets" &&
    parts[2] === "status"
  ) {
    return changeStatus(request, parts[1]);
  }

  return Effect.succeed(
    json({ error: { code: "NOT_FOUND", message: "Route not found" } }, 404),
  );
};

const errorResponse = (error: ApiError) => {
  if (error._tag === "ValidationError") {
    return json(
      {
        error: {
          code: "VALIDATION_ERROR",
          field: error.field,
          message: error.message,
        },
      },
      400,
    );
  }
  if (error._tag === "TicketNotFound") {
    return json(
      {
        error: {
          code: "TICKET_NOT_FOUND",
          message: `Ticket ${error.ticketId} was not found`,
        },
      },
      404,
    );
  }
  if (error._tag === "InvalidTransition") {
    return json(
      {
        error: {
          code: "INVALID_TRANSITION",
          message: `Cannot move ${error.ticketId} from ${error.from} to ${error.to}`,
        },
      },
      409,
    );
  }
  return json(
    { error: { code: "INVALID_JSON", message: error.message } },
    400,
  );
};

export const handler: Http.Handler<never, TicketService> = (request, context) =>
  route(request).pipe(
    Effect.catchAll((error) => Effect.succeed(errorResponse(error))),
    Effect.tap((response) =>
      Effect.sync(() => {
        console.log(
          `${context.requestId} ${request.method} ${new URL(request.url).pathname} ${response.status}`,
        );
      }),
    ),
  );
