import type {
  CreateTicketInput,
  Ticket,
  TicketStatus,
  UpdateTicketInput,
} from "../domain";

interface ApiErrorBody {
  readonly error?: {
    readonly message?: string;
  };
}

const request = async <T>(path: string, init?: RequestInit) => {
  const response = await fetch(path, {
    ...init,
    headers: init?.body
      ? { "content-type": "application/json", ...init.headers }
      : init?.headers,
  });

  if (!response.ok) {
    const body = (await response.json().catch(() => ({}))) as ApiErrorBody;
    throw new Error(body.error?.message ?? `Request failed (${response.status})`);
  }

  return (response.status === 204 ? undefined : await response.json()) as T;
};

export const api = {
  list: async () => {
    const result = await request<{ readonly tickets: ReadonlyArray<Ticket> }>(
      "/tickets",
    );
    return result.tickets;
  },
  get: (id: string) => request<Ticket>(`/tickets/${id}`),
  create: (input: CreateTicketInput) =>
    request<Ticket>("/tickets", {
      method: "POST",
      body: JSON.stringify(input),
    }),
  update: (id: string, input: UpdateTicketInput) =>
    request<Ticket>(`/tickets/${id}`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),
  changeStatus: (id: string, status: TicketStatus) =>
    request<Ticket>(`/tickets/${id}/status`, {
      method: "PATCH",
      body: JSON.stringify({ status }),
    }),
  remove: (id: string) => request<void>(`/tickets/${id}`, { method: "DELETE" }),
};
