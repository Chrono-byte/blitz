import { useState, type FormEvent } from "react";
import type {
  TicketPriority,
  UpdateTicketInput,
} from "../domain";
import { ErrorMessage, Field, inputClass, priorities } from "./components";

interface TicketFormProps {
  readonly initial?: UpdateTicketInput;
  readonly submitLabel: string;
  readonly onSubmit: (input: UpdateTicketInput) => Promise<void>;
  readonly onCancel?: () => void;
}

export const TicketForm = ({
  initial,
  submitLabel,
  onSubmit,
  onCancel,
}: TicketFormProps) => {
  const [title, setTitle] = useState(initial?.title ?? "");
  const [description, setDescription] = useState(initial?.description ?? "");
  const [priority, setPriority] = useState<TicketPriority>(
    initial?.priority ?? "normal",
  );
  const [error, setError] = useState<string>();
  const [submitting, setSubmitting] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setSubmitting(true);
    setError(undefined);
    try {
      await onSubmit({ title, description, priority });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Something went wrong");
      setSubmitting(false);
    }
  };

  return (
    <form className="grid gap-5" onSubmit={submit}>
      {error && <ErrorMessage message={error} />}
      <Field label="Title">
        <input
          className={inputClass}
          value={title}
          onChange={(event) => setTitle(event.target.value)}
          minLength={3}
          maxLength={120}
          placeholder="What needs attention?"
          required
          autoFocus
        />
      </Field>
      <Field label="Description">
        <textarea
          className={`${inputClass} min-h-32 resize-y`}
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          maxLength={4000}
          placeholder="Add useful context for the support team…"
        />
      </Field>
      <Field label="Priority">
        <select
          className={inputClass}
          value={priority}
          onChange={(event) => setPriority(event.target.value as TicketPriority)}
        >
          {priorities.map((item) => (
            <option key={item} value={item}>
              {item[0].toUpperCase() + item.slice(1)}
            </option>
          ))}
        </select>
      </Field>
      <div className="flex justify-end gap-3 border-t border-slate-100 pt-5">
        {onCancel && (
          <button
            type="button"
            className="rounded-xl px-4 py-2.5 text-sm font-semibold text-slate-600 hover:bg-slate-100"
            onClick={onCancel}
          >
            Cancel
          </button>
        )}
        <button
          className="rounded-xl bg-indigo-600 px-4 py-2.5 text-sm font-semibold text-white shadow-sm hover:bg-indigo-500 disabled:opacity-50"
          disabled={submitting}
        >
          {submitting ? "Saving…" : submitLabel}
        </button>
      </div>
    </form>
  );
};
