// SPDX-License-Identifier: MPL-2.0
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Domain from "./domain.ts";

interface RepositoryState {
  readonly nextId: number;
  readonly tickets: ReadonlyArray<Domain.Ticket>;
}

export interface TicketRepository {
  readonly list: () => Effect.Effect<ReadonlyArray<Domain.Ticket>>;
  readonly findById: (
    id: Domain.TicketId,
  ) => Effect.Effect<Option.Option<Domain.Ticket>>;
  readonly create: (
    input: Domain.CreateTicketInput,
    now: number,
  ) => Effect.Effect<Domain.Ticket>;
  readonly update: (
    id: Domain.TicketId,
    input: Domain.UpdateTicketInput,
    now: number,
  ) => Effect.Effect<Option.Option<Domain.Ticket>>;
  readonly setStatus: (
    id: Domain.TicketId,
    status: Domain.TicketStatus,
    now: number,
  ) => Effect.Effect<Option.Option<Domain.Ticket>>;
  readonly remove: (id: Domain.TicketId) => Effect.Effect<boolean>;
}

export const TicketRepository = Context.GenericTag<TicketRepository>(
  "helpdesk/TicketRepository",
);

export const makeInMemory = Ref.make<RepositoryState>({
  nextId: 1,
  tickets: [],
}).pipe(
  Effect.map((state): TicketRepository => ({
    list: () => Ref.get(state).pipe(Effect.map((current) => current.tickets)),
    findById: (id) =>
      Ref.get(state).pipe(
        Effect.map((current) =>
          Option.fromNullable(current.tickets.find((ticket) => ticket.id === id)),
        ),
      ),
    create: (input, now) =>
      Ref.modify(state, (current) => {
        const ticket: Domain.Ticket = {
          id: `TKT-${current.nextId}`,
          title: input.title,
          description: input.description,
          priority: input.priority,
          status: "open",
          createdAt: now,
          updatedAt: now,
        };
        return [
          ticket,
          {
            nextId: current.nextId + 1,
            tickets: [...current.tickets, ticket],
          },
        ];
      }),
    update: (id, input, now) =>
      Ref.modify(state, (current) => {
        const existing = current.tickets.find((ticket) => ticket.id === id);
        if (existing === undefined) {
          return [Option.none<Domain.Ticket>(), current];
        }
        const updated: Domain.Ticket = {
          id: existing.id,
          title: input.title,
          description: input.description,
          priority: input.priority,
          status: existing.status,
          createdAt: existing.createdAt,
          updatedAt: now,
        };
        return [
          Option.some(updated),
          {
            nextId: current.nextId,
            tickets: current.tickets.map((ticket) =>
              ticket.id === id ? updated : ticket,
            ),
          },
        ];
      }),
    setStatus: (id, status, now) =>
      Ref.modify(state, (current) => {
        const existing = current.tickets.find((ticket) => ticket.id === id);
        if (existing === undefined) {
          return [Option.none<Domain.Ticket>(), current];
        }
        const updated: Domain.Ticket = {
          id: existing.id,
          title: existing.title,
          description: existing.description,
          priority: existing.priority,
          status,
          createdAt: existing.createdAt,
          updatedAt: now,
        };
        return [
          Option.some(updated),
          {
            nextId: current.nextId,
            tickets: current.tickets.map((ticket) =>
              ticket.id === id ? updated : ticket,
            ),
          },
        ];
      }),
    remove: (id) =>
      Ref.modify(state, (current) => {
        const tickets = current.tickets.filter((ticket) => ticket.id !== id);
        return [
          tickets.length !== current.tickets.length,
          { nextId: current.nextId, tickets },
        ];
      }),
  })),
);

export const layerInMemory = Layer.effect(TicketRepository, makeInMemory);
