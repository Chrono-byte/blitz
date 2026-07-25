// SPDX-License-Identifier: MPL-2.0
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Domain from "./domain.ts";
import { TicketRepository } from "./ticket-repository.ts";

export interface TicketService {
  readonly list: () => Effect.Effect<ReadonlyArray<Domain.Ticket>>;
  readonly get: (
    id: Domain.TicketId,
  ) => Effect.Effect<Domain.Ticket, Domain.TicketNotFound>;
  readonly create: (
    input: Domain.CreateTicketInput,
  ) => Effect.Effect<Domain.Ticket>;
  readonly update: (
    id: Domain.TicketId,
    input: Domain.UpdateTicketInput,
  ) => Effect.Effect<Domain.Ticket, Domain.TicketNotFound>;
  readonly changeStatus: (
    id: Domain.TicketId,
    status: Domain.TicketStatus,
  ) => Effect.Effect<Domain.Ticket, Domain.TicketError>;
  readonly remove: (
    id: Domain.TicketId,
  ) => Effect.Effect<void, Domain.TicketNotFound>;
}

export const TicketService = Context.GenericTag<TicketService>(
  "helpdesk/TicketService",
);

export const make = (repository: TicketRepository): TicketService => {
  const get = (id: Domain.TicketId) =>
    repository.findById(id).pipe(
      Effect.flatMap((ticket) =>
        Option.isSome(ticket)
          ? Effect.succeed(ticket.value)
          : Effect.fail(new Domain.TicketNotFound(id)),
      ),
    );

  return {
    list: repository.list,
    get,
    create: (input) => repository.create(input, Date.now()),
    update: (id, input) =>
      repository.update(id, input, Date.now()).pipe(
        Effect.flatMap((updated) =>
          Option.isSome(updated)
            ? Effect.succeed(updated.value)
            : Effect.fail(new Domain.TicketNotFound(id)),
        ),
      ),
    changeStatus: (id, status) =>
      get(id).pipe(
        Effect.flatMap((ticket) => {
          if (!Domain.canTransition(ticket.status, status)) {
            return Effect.fail(
              new Domain.InvalidTransition(id, ticket.status, status),
            );
          }
          return repository.setStatus(id, status, Date.now()).pipe(
            Effect.flatMap((updated) =>
              Option.isSome(updated)
                ? Effect.succeed(updated.value)
                : Effect.fail(new Domain.TicketNotFound(id)),
            ),
          );
        }),
      ),
    remove: (id) =>
      repository.remove(id).pipe(
        Effect.flatMap((removed) =>
          removed
            ? Effect.succeed(undefined)
            : Effect.fail(new Domain.TicketNotFound(id)),
        ),
      ),
  };
};

const makeService = Effect.contextWith(
  (context: Context.Context<TicketRepository>) =>
    make(Context.get(context, TicketRepository)),
);

export const layer = Layer.effect(TicketService, makeService);

export const listTickets = Effect.serviceFunctionEffect(
  TicketService,
  (service) => service.list,
);

export const getTicket = Effect.serviceFunctionEffect(
  TicketService,
  (service) => service.get,
);

export const createTicket = Effect.serviceFunctionEffect(
  TicketService,
  (service) => service.create,
);

export const updateTicket = Effect.serviceFunctionEffect(
  TicketService,
  (service) => service.update,
);

export const changeTicketStatus = Effect.serviceFunctionEffect(
  TicketService,
  (service) => service.changeStatus,
);

export const deleteTicket = Effect.serviceFunctionEffect(
  TicketService,
  (service) => service.remove,
);
