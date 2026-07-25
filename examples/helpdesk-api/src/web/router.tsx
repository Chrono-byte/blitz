import {
  Link,
  Outlet,
  createRootRoute,
  createRoute,
  createRouter,
  useRouter,
} from "@tanstack/react-router";
import { useState } from "react";
import type { TicketStatus } from "../domain";
import { api } from "./api";
import {
  Card,
  ErrorMessage,
  Priority,
  StatusBadge,
  inputClass,
  statusLabel,
} from "./components";
import { TicketForm } from "./ticket-form";

const RootLayout = () => (
  <div className="min-h-screen">
    <header className="border-b border-slate-200 bg-white/90 backdrop-blur">
      <div className="mx-auto flex max-w-6xl items-center justify-between px-5 py-4 sm:px-8">
        <Link to="/" className="flex items-center gap-3">
          <span className="grid size-9 place-items-center rounded-xl bg-indigo-600 text-lg font-black text-white shadow-sm">B</span>
          <span>
            <span className="block text-sm font-bold leading-tight">Blitz Helpdesk</span>
            <span className="block text-xs text-slate-500">Support workspace</span>
          </span>
        </Link>
        <Link
          to="/tickets/new"
          className="rounded-xl bg-slate-950 px-4 py-2.5 text-sm font-semibold text-white shadow-sm hover:bg-slate-800"
        >
          + New ticket
        </Link>
      </div>
    </header>
    <main className="mx-auto max-w-6xl px-5 py-10 sm:px-8">
      <Outlet />
    </main>
  </div>
);

const rootRoute = createRootRoute({
  component: RootLayout,
  notFoundComponent: () => (
    <div className="py-24 text-center">
      <p className="text-sm font-semibold text-indigo-600">404</p>
      <h1 className="mt-2 text-3xl font-bold">Page not found</h1>
      <Link to="/" className="mt-6 inline-block text-sm font-semibold text-indigo-600">Return to tickets</Link>
    </div>
  ),
});

const TicketsPage = () => {
  const tickets = ticketsRoute.useLoaderData();
  const counts = {
    open: tickets.filter((ticket) => ticket.status === "open").length,
    inProgress: tickets.filter((ticket) => ticket.status === "in_progress").length,
    resolved: tickets.filter((ticket) => ticket.status === "resolved").length,
  };

  return (
    <div className="grid gap-7">
      <div>
        <p className="text-sm font-semibold text-indigo-600">Overview</p>
        <h1 className="mt-1 text-3xl font-bold tracking-tight">Support tickets</h1>
        <p className="mt-2 text-slate-500">Track, prioritize, and resolve customer requests.</p>
      </div>
      <div className="grid grid-cols-3 gap-3">
        {[["Open", counts.open], ["In progress", counts.inProgress], ["Resolved", counts.resolved]].map(([label, count]) => (
          <Card key={label}>
            <div className="p-4 sm:p-5">
              <p className="text-xs font-semibold uppercase tracking-wide text-slate-500">{label}</p>
              <p className="mt-1 text-2xl font-bold">{count}</p>
            </div>
          </Card>
        ))}
      </div>
      <Card>
        {tickets.length === 0 ? (
          <div className="px-6 py-16 text-center">
            <div className="mx-auto grid size-12 place-items-center rounded-2xl bg-indigo-50 text-xl">✦</div>
            <h2 className="mt-4 font-bold">Your queue is clear</h2>
            <p className="mt-1 text-sm text-slate-500">Create a ticket to get started.</p>
            <Link to="/tickets/new" className="mt-5 inline-block text-sm font-semibold text-indigo-600">Create a ticket →</Link>
          </div>
        ) : (
          <div className="divide-y divide-slate-100">
            {tickets.map((ticket) => (
              <Link
                key={ticket.id}
                to="/tickets/$ticketId"
                params={{ ticketId: ticket.id }}
                className="grid gap-3 px-5 py-5 transition hover:bg-slate-50 sm:grid-cols-[1fr_auto] sm:items-center sm:px-6"
              >
                <div className="min-w-0">
                  <div className="flex items-center gap-3">
                    <span className="text-xs font-semibold text-slate-400">{ticket.id}</span>
                    <Priority priority={ticket.priority} />
                  </div>
                  <h2 className="mt-1 truncate font-semibold text-slate-900">{ticket.title}</h2>
                  <p className="mt-1 truncate text-sm text-slate-500">{ticket.description || "No description"}</p>
                </div>
                <StatusBadge status={ticket.status} />
              </Link>
            ))}
          </div>
        )}
      </Card>
    </div>
  );
};

const ticketsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  loader: api.list,
  component: TicketsPage,
  pendingComponent: () => <p className="py-24 text-center text-sm text-slate-500">Loading tickets…</p>,
  errorComponent: ({ error }) => <ErrorMessage message={error.message} />,
});

const NewTicketPage = () => {
  const router = useRouter();
  return (
    <div className="mx-auto max-w-2xl">
      <Link to="/" className="text-sm font-semibold text-slate-500 hover:text-slate-900">← All tickets</Link>
      <h1 className="mt-5 text-3xl font-bold tracking-tight">Create a ticket</h1>
      <p className="mt-2 text-slate-500">Share the issue and choose how urgently it needs attention.</p>
      <Card>
        <div className="mt-7 p-6 sm:p-8">
          <TicketForm
            submitLabel="Create ticket"
            onSubmit={async (input) => {
              const ticket = await api.create(input);
              await router.invalidate();
              await router.navigate({ to: "/tickets/$ticketId", params: { ticketId: ticket.id } });
            }}
            onCancel={() => router.navigate({ to: "/" })}
          />
        </div>
      </Card>
    </div>
  );
};

const newTicketRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/tickets/new",
  component: NewTicketPage,
});

const allowedStatuses: Record<TicketStatus, ReadonlyArray<TicketStatus>> = {
  open: ["open", "in_progress"],
  in_progress: ["open", "in_progress", "resolved"],
  resolved: ["resolved"],
};

const TicketPage = () => {
  const ticket = ticketRoute.useLoaderData();
  const router = useRouter();
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  const refresh = async () => {
    await router.invalidate();
    setBusy(false);
  };

  const changeStatus = async (status: TicketStatus) => {
    setBusy(true);
    setError(undefined);
    try {
      await api.changeStatus(ticket.id, status);
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not update ticket");
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!window.confirm(`Delete ${ticket.id}? This cannot be undone.`)) return;
    setBusy(true);
    try {
      await api.remove(ticket.id);
      await router.invalidate();
      await router.navigate({ to: "/" });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not delete ticket");
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto max-w-3xl">
      <Link to="/" className="text-sm font-semibold text-slate-500 hover:text-slate-900">← All tickets</Link>
      <div className="mt-6 flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="flex items-center gap-3"><span className="text-sm font-semibold text-slate-400">{ticket.id}</span><Priority priority={ticket.priority} /></div>
          <h1 className="mt-2 text-3xl font-bold tracking-tight">{ticket.title}</h1>
        </div>
        <StatusBadge status={ticket.status} />
      </div>
      {error && <div className="mt-5"><ErrorMessage message={error} /></div>}
      <Card>
        <div className="mt-7 p-6 sm:p-8">
          {editing ? (
            <TicketForm
              initial={ticket}
              submitLabel="Save changes"
              onCancel={() => setEditing(false)}
              onSubmit={async (input) => {
                await api.update(ticket.id, input);
                setEditing(false);
                await refresh();
              }}
            />
          ) : (
            <div>
              <h2 className="text-xs font-bold uppercase tracking-wider text-slate-400">Description</h2>
              <p className="mt-3 whitespace-pre-wrap leading-7 text-slate-700">{ticket.description || "No description provided."}</p>
              <dl className="mt-8 grid gap-5 border-t border-slate-100 pt-6 sm:grid-cols-2">
                <div><dt className="text-xs font-semibold text-slate-400">Created</dt><dd className="mt-1 text-sm font-medium">{new Date(ticket.createdAt).toLocaleString()}</dd></div>
                <div><dt className="text-xs font-semibold text-slate-400">Last updated</dt><dd className="mt-1 text-sm font-medium">{new Date(ticket.updatedAt).toLocaleString()}</dd></div>
              </dl>
              <div className="mt-8 flex flex-wrap items-center gap-3 border-t border-slate-100 pt-6">
                <select
                  className={`${inputClass} w-auto`}
                  value={ticket.status}
                  disabled={busy || allowedStatuses[ticket.status].length === 1}
                  onChange={(event) => changeStatus(event.target.value as TicketStatus)}
                >
                  {allowedStatuses[ticket.status].map((status) => <option key={status} value={status}>{statusLabel[status]}</option>)}
                </select>
                <button className="rounded-xl border border-slate-300 px-4 py-2.5 text-sm font-semibold hover:bg-slate-50" onClick={() => setEditing(true)}>Edit</button>
                <button className="ml-auto rounded-xl px-4 py-2.5 text-sm font-semibold text-rose-600 hover:bg-rose-50 disabled:opacity-50" disabled={busy} onClick={remove}>Delete</button>
              </div>
            </div>
          )}
        </div>
      </Card>
    </div>
  );
};

const ticketRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/tickets/$ticketId",
  loader: ({ params }) => api.get(params.ticketId),
  component: TicketPage,
  pendingComponent: () => <p className="py-24 text-center text-sm text-slate-500">Loading ticket…</p>,
  errorComponent: ({ error }) => <ErrorMessage message={error.message} />,
});

const routeTree = rootRoute.addChildren([ticketsRoute, newTicketRoute, ticketRoute]);

export const router = createRouter({ routeTree, defaultPreload: "intent" });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
