import type { PropsWithChildren } from "react";
import type { TicketPriority, TicketStatus } from "../domain";

export const priorities: ReadonlyArray<TicketPriority> = [
  "low",
  "normal",
  "high",
  "urgent",
];

export const statusLabel: Record<TicketStatus, string> = {
  open: "Open",
  in_progress: "In progress",
  resolved: "Resolved",
};

const statusStyles: Record<TicketStatus, string> = {
  open: "bg-sky-50 text-sky-700 ring-sky-600/20",
  in_progress: "bg-amber-50 text-amber-700 ring-amber-600/20",
  resolved: "bg-emerald-50 text-emerald-700 ring-emerald-600/20",
};

const priorityStyles: Record<TicketPriority, string> = {
  low: "text-slate-500",
  normal: "text-indigo-600",
  high: "text-orange-600",
  urgent: "text-rose-600",
};

export const StatusBadge = ({ status }: { status: TicketStatus }) => (
  <span
    className={`inline-flex rounded-full px-2.5 py-1 text-xs font-semibold ring-1 ring-inset ${statusStyles[status]}`}
  >
    {statusLabel[status]}
  </span>
);

export const Priority = ({ priority }: { priority: TicketPriority }) => (
  <span className={`text-xs font-bold uppercase tracking-wider ${priorityStyles[priority]}`}>
    {priority}
  </span>
);

export const Card = ({ children }: PropsWithChildren) => (
  <div className="rounded-2xl border border-slate-200 bg-white shadow-sm">{children}</div>
);

export const Field = ({
  label,
  children,
}: PropsWithChildren<{ readonly label: string }>) => (
  <label className="grid gap-2 text-sm font-medium text-slate-700">
    {label}
    {children}
  </label>
);

export const inputClass =
  "w-full rounded-xl border border-slate-300 bg-white px-3.5 py-2.5 text-sm shadow-sm transition placeholder:text-slate-400 hover:border-slate-400";

export const ErrorMessage = ({ message }: { readonly message: string }) => (
  <div className="rounded-xl bg-rose-50 px-4 py-3 text-sm text-rose-700 ring-1 ring-rose-200">
    {message}
  </div>
);
