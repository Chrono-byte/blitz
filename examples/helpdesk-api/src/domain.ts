// SPDX-License-Identifier: MPL-2.0

export type TicketId = string;
export type TicketPriority = "low" | "normal" | "high" | "urgent";
export type TicketStatus = "open" | "in_progress" | "resolved";

export interface Ticket {
  readonly id: TicketId;
  readonly title: string;
  readonly description: string;
  readonly priority: TicketPriority;
  readonly status: TicketStatus;
  readonly createdAt: number;
  readonly updatedAt: number;
}

export interface CreateTicketInput {
  readonly title: string;
  readonly description: string;
  readonly priority: TicketPriority;
}

export interface UpdateTicketInput {
  readonly title: string;
  readonly description: string;
  readonly priority: TicketPriority;
}

export interface ChangeStatusInput {
  readonly status: TicketStatus;
}

export class ValidationError {
  readonly _tag = "ValidationError";

  constructor(
    readonly field: string,
    readonly message: string,
  ) {}
}

export class TicketNotFound {
  readonly _tag = "TicketNotFound";

  constructor(readonly ticketId: TicketId) {}
}

export class InvalidTransition {
  readonly _tag = "InvalidTransition";

  constructor(
    readonly ticketId: TicketId,
    readonly from: TicketStatus,
    readonly to: TicketStatus,
  ) {}
}

export type TicketError = ValidationError | TicketNotFound | InvalidTransition;

const isPriority = (input: unknown): input is TicketPriority =>
  input === "low" ||
  input === "normal" ||
  input === "high" ||
  input === "urgent";

const isStatus = (input: unknown): input is TicketStatus =>
  input === "open" || input === "in_progress" || input === "resolved";

export const decodeCreateTicket = (
  input: unknown,
): CreateTicketInput | ValidationError => {
  if (typeof input !== "object" || input === null) {
    return new ValidationError("body", "Expected a JSON object");
  }

  if (!("title" in input) || typeof input.title !== "string") {
    return new ValidationError("title", "Expected a string");
  }
  const title = input.title.trim();
  if (title.length < 3 || title.length > 120) {
    return new ValidationError("title", "Must contain 3 to 120 characters");
  }

  if (!("description" in input) || typeof input.description !== "string") {
    return new ValidationError("description", "Expected a string");
  }
  const description = input.description.trim();
  if (description.length > 4_000) {
    return new ValidationError("description", "Must not exceed 4000 characters");
  }

  const priority = "priority" in input ? input.priority : "normal";
  if (!isPriority(priority)) {
    return new ValidationError(
      "priority",
      "Expected low, normal, high, or urgent",
    );
  }

  return { title, description, priority };
};

export const decodeUpdateTicket = (
  input: unknown,
): UpdateTicketInput | ValidationError => decodeCreateTicket(input);

export const decodeStatusChange = (
  input: unknown,
): ChangeStatusInput | ValidationError => {
  if (
    typeof input !== "object" ||
    input === null ||
    !("status" in input) ||
    !isStatus(input.status)
  ) {
    return new ValidationError(
      "status",
      "Expected open, in_progress, or resolved",
    );
  }
  return { status: input.status };
};

export const canTransition = (from: TicketStatus, to: TicketStatus) =>
  from === to ||
  (from === "open" && to === "in_progress") ||
  (from === "in_progress" && (to === "open" || to === "resolved"));
