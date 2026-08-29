import { useCallback, useEffect, useRef, useState } from "react";
import { EditorContent, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import DOMPurify from "dompurify";
import { marked } from "marked";
import TurndownService from "turndown";
import { applyTextChange, diffText, rebaseChange } from "../shared/sync";
import type { DocumentSnapshot, VersionedChange } from "../shared/sync";

type Presence = { readonly clientId: string; readonly name: string };
type ConnectionState = "connecting" | "connected" | "offline";
type SaveState = "saved" | "saving" | "conflict";

type ServerMessage =
  | { readonly type: "snapshot"; readonly document: DocumentSnapshot }
  | {
      readonly type: "patch";
      readonly document: DocumentSnapshot;
      readonly change: VersionedChange;
    }
  | {
      readonly type: "conflict";
      readonly document: DocumentSnapshot;
      readonly mutationId: string;
      readonly reason: string;
    }
  | { readonly type: "presence"; readonly users: ReadonlyArray<Presence> }
  | { readonly type: "error"; readonly message: string };

const documentId = "welcome";
const turndown = new TurndownService({
  headingStyle: "atx",
  bulletListMarker: "-",
  codeBlockStyle: "fenced",
});

const markdownToHtml = (markdown: string) =>
  DOMPurify.sanitize(marked.parse(markdown, { async: false }));

const htmlToMarkdown = (html: string) => turndown.turndown(html);

const randomName = () => {
  const adjectives = ["Amber", "Bright", "Calm", "Clever", "Swift", "Vivid"];
  const animals = ["Badger", "Fox", "Heron", "Otter", "Panda", "Wolf"];
  return `${adjectives[Math.floor(Math.random() * adjectives.length)]} ${
    animals[Math.floor(Math.random() * animals.length)]
  }`;
};

const randomId = () =>
  typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;

const readIdentity = () => {
  try {
    const stored = sessionStorage.getItem("blitz-docs-identity");
    if (stored !== null) {
      const separator = stored.indexOf(":");
      if (separator > 0) {
        return {
          clientId: stored.slice(0, separator),
          name: stored.slice(separator + 1),
        };
      }
    }
  } catch {
  }

  const identity = { clientId: randomId(), name: randomName() };
  try {
    sessionStorage.setItem(
      "blitz-docs-identity",
      `${identity.clientId}:${identity.name}`,
    );
  } catch {
  }
  return identity;
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const readVersionedChange = (value: unknown): VersionedChange | null => {
  if (
    !isRecord(value) ||
    typeof value.from !== "number" ||
    typeof value.deleteCount !== "number" ||
    typeof value.insert !== "string" ||
    typeof value.version !== "number" ||
    typeof value.clientId !== "string" ||
    typeof value.mutationId !== "string"
  ) {
    return null;
  }
  return {
    from: value.from,
    deleteCount: value.deleteCount,
    insert: value.insert,
    version: value.version,
    clientId: value.clientId,
    mutationId: value.mutationId,
  };
};

const readDocument = (value: unknown): DocumentSnapshot | null => {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.title !== "string" ||
    typeof value.markdown !== "string" ||
    typeof value.version !== "number" ||
    !Array.isArray(value.changes)
  ) {
    return null;
  }
  const changes: Array<VersionedChange> = [];
  for (const item of value.changes) {
    const change = readVersionedChange(item);
    if (change === null) {
      return null;
    }
    changes.push(change);
  }
  return {
    id: value.id,
    title: value.title,
    markdown: value.markdown,
    version: value.version,
    changes,
  };
};

const readPresence = (value: unknown): Presence | null => {
  if (
    !isRecord(value) ||
    typeof value.clientId !== "string" ||
    typeof value.name !== "string"
  ) {
    return null;
  }
  return { clientId: value.clientId, name: value.name };
};

const parseMessage = (data: string): ServerMessage | null => {
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return null;
  }
  if (!isRecord(value) || typeof value.type !== "string") {
    return null;
  }
  if (value.type === "snapshot") {
    const document = readDocument(value.document);
    return document === null ? null : { type: "snapshot", document };
  }
  if (value.type === "patch") {
    const document = readDocument(value.document);
    const change = readVersionedChange(value.change);
    return document === null || change === null
      ? null
      : { type: "patch", document, change };
  }
  if (value.type === "conflict") {
    const document = readDocument(value.document);
    return document === null ||
      typeof value.mutationId !== "string" ||
      typeof value.reason !== "string"
      ? null
      : {
          type: "conflict",
          document,
          mutationId: value.mutationId,
          reason: value.reason,
        };
  }
  if (value.type === "presence" && Array.isArray(value.users)) {
    const users: Array<Presence> = [];
    for (const item of value.users) {
      const user = readPresence(item);
      if (user === null) {
        return null;
      }
      users.push(user);
    }
    return { type: "presence", users };
  }
  return value.type === "error" && typeof value.message === "string"
    ? { type: "error", message: value.message }
    : null;
};

const ToolbarButton = ({
  active = false,
  label,
  onClick,
}: {
  readonly active?: boolean;
  readonly label: string;
  readonly onClick: () => void;
}) => (
  <button
    className={`toolbar-button ${active ? "toolbar-button-active" : ""}`}
    type="button"
    title={label}
    aria-label={label}
    onClick={onClick}
  >
    {label}
  </button>
);

export function App() {
  const identity = useRef(readIdentity()).current;
  const socketRef = useRef<WebSocket | null>(null);
  const reconnectRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const syncedRef = useRef("");
  const currentRef = useRef("");
  const submittedRef = useRef("");
  const versionRef = useRef(0);
  const pendingRef = useRef<string | null>(null);
  const applyingRemoteRef = useRef(false);
  const [markdown, setMarkdown] = useState("");
  const [mode, setMode] = useState<"rich" | "markdown">("rich");
  const [connection, setConnection] = useState<ConnectionState>("connecting");
  const [saveState, setSaveState] = useState<SaveState>("saved");
  const [presence, setPresence] = useState<ReadonlyArray<Presence>>([]);
  const [notice, setNotice] = useState<string | null>(null);

  const editor = useEditor({
    extensions: [StarterKit],
    content: "",
    editorProps: {
      attributes: {
        class: "document-surface",
        spellcheck: "true",
      },
    },
    onUpdate: ({ editor: activeEditor }) => {
      if (applyingRemoteRef.current) {
        return;
      }
      const next = htmlToMarkdown(activeEditor.getHTML());
      currentRef.current = next;
      setMarkdown(next);
      setSaveState("saving");
    },
  });

  const applyLocalMarkdown = useCallback(
    (next: string) => {
      currentRef.current = next;
      setMarkdown(next);
      if (editor !== null) {
        applyingRemoteRef.current = true;
        editor.commands.setContent(markdownToHtml(next), { emitUpdate: false });
        applyingRemoteRef.current = false;
      }
    },
    [editor],
  );

  const flush = useCallback(() => {
    const socket = socketRef.current;
    if (
      socket === null ||
      socket.readyState !== WebSocket.OPEN ||
      pendingRef.current !== null
    ) {
      return;
    }
    const change = diffText(syncedRef.current, currentRef.current);
    if (change === null) {
      setSaveState("saved");
      return;
    }

    const mutationId = randomId();
    pendingRef.current = mutationId;
    submittedRef.current = currentRef.current;
    setSaveState("saving");
    socket.send(
      JSON.stringify({
        type: "edit",
        documentId,
        clientId: identity.clientId,
        mutationId,
        baseVersion: versionRef.current,
        change,
      }),
    );
  }, [identity.clientId]);

  useEffect(() => {
    if (saveState !== "saving" || pendingRef.current !== null) {
      return;
    }
    if (saveTimerRef.current !== null) {
      clearTimeout(saveTimerRef.current);
    }
    saveTimerRef.current = setTimeout(flush, 350);
    return () => {
      if (saveTimerRef.current !== null) {
        clearTimeout(saveTimerRef.current);
      }
    };
  }, [flush, markdown, saveState]);

  useEffect(() => {
    let disposed = false;

    const connect = () => {
      if (disposed) {
        return;
      }
      setConnection("connecting");
      const protocol = location.protocol === "https:" ? "wss:" : "ws:";
      const socket = new WebSocket(`${protocol}//${location.host}/sync`);
      socketRef.current = socket;

      socket.addEventListener("open", () => {
        reconnectRef.current = 0;
        setConnection("connected");
        socket.send(
          JSON.stringify({
            type: "subscribe",
            documentId,
            clientId: identity.clientId,
            name: identity.name,
          }),
        );
      });

      socket.addEventListener("message", (event) => {
        if (typeof event.data !== "string") {
          return;
        }
        const message = parseMessage(event.data);
        if (message === null) {
          return;
        }
        if (message.type === "presence") {
          setPresence(message.users);
          return;
        }
        if (message.type === "error") {
          setNotice(message.message);
          return;
        }
        if (message.type === "conflict") {
          pendingRef.current = null;
          syncedRef.current = message.document.markdown;
          versionRef.current = message.document.version;
          applyLocalMarkdown(message.document.markdown);
          setSaveState("conflict");
          setNotice(`${message.reason}. The latest server version was restored.`);
          return;
        }
        if (message.type === "snapshot") {
          const hadLocalChanges =
            syncedRef.current.length > 0 && currentRef.current !== syncedRef.current;
          syncedRef.current = message.document.markdown;
          versionRef.current = message.document.version;
          pendingRef.current = null;
          if (!hadLocalChanges) {
            applyLocalMarkdown(message.document.markdown);
            setSaveState("saved");
          } else {
            setSaveState("saving");
            queueMicrotask(flush);
          }
          return;
        }

        const ownMutation = message.change.mutationId === pendingRef.current;
        if (pendingRef.current !== null && !ownMutation) {
          return;
        }

        const previousSynced = syncedRef.current;
        syncedRef.current = message.document.markdown;
        versionRef.current = message.document.version;

        if (ownMutation) {
          pendingRef.current = null;
          const laterChange = diffText(submittedRef.current, currentRef.current);
          const merged = laterChange === null
            ? message.document.markdown
            : applyTextChange(message.document.markdown, laterChange);
          applyLocalMarkdown(merged);
          setSaveState(laterChange === null ? "saved" : "saving");
          if (laterChange !== null) {
            queueMicrotask(flush);
          }
          return;
        }

        const localChange = diffText(previousSynced, currentRef.current);
        if (localChange === null) {
          applyLocalMarkdown(message.document.markdown);
          setSaveState("saved");
          return;
        }
        const rebased = rebaseChange(localChange, [message.change]);
        if (rebased === null) {
          applyLocalMarkdown(message.document.markdown);
          setSaveState("conflict");
          setNotice("A collaborator edited the same text. Their version was kept.");
          return;
        }
        applyLocalMarkdown(applyTextChange(message.document.markdown, rebased));
        setSaveState("saving");
      });

      socket.addEventListener("close", () => {
        if (disposed) {
          return;
        }
        setConnection("offline");
        reconnectRef.current += 1;
        const delay = Math.min(1_000 * 2 ** reconnectRef.current, 10_000);
        reconnectTimerRef.current = setTimeout(connect, delay);
      });
    };

    connect();
    return () => {
      disposed = true;
      if (reconnectTimerRef.current !== null) {
        clearTimeout(reconnectTimerRef.current);
      }
      socketRef.current?.close();
    };
  }, [applyLocalMarkdown, flush, identity.clientId, identity.name]);

  const switchMode = (nextMode: "rich" | "markdown") => {
    if (nextMode === "rich") {
      applyLocalMarkdown(markdown);
    }
    setMode(nextMode);
  };

  const saveLabel = saveState === "saved"
    ? "Saved"
    : saveState === "saving"
      ? "Saving…"
      : "Sync conflict";

  return (
    <div className="min-h-screen bg-[#f8f9fa] text-slate-800">
      <header className="app-header">
        <div className="brand-mark" aria-hidden="true">B</div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-3">
            <input
              className="title-input"
              aria-label="Document title"
              defaultValue="Untitled document"
              readOnly
            />
            <span className={`connection-dot connection-${connection}`} />
            <span className="save-label">{saveLabel}</span>
          </div>
          <nav className="menu-row" aria-label="Document menu">
            <button type="button">File</button>
            <button type="button">Edit</button>
            <button type="button">View</button>
            <button type="button">Insert</button>
            <button type="button">Format</button>
          </nav>
        </div>
        <div className="presence-row">
          {presence.slice(0, 4).map((user, index) => (
            <div
              key={user.clientId}
              className="avatar"
              style={{ backgroundColor: ["#7c3aed", "#0284c7", "#059669", "#dc2626"][index % 4] }}
              title={`${user.name}${user.clientId === identity.clientId ? " (you)" : ""}`}
            >
              {user.name.slice(0, 1).toUpperCase()}
            </div>
          ))}
          <span className="presence-label">
            {presence.length} {presence.length === 1 ? "editor" : "editors"}
          </span>
          <button className="share-button" type="button">Share</button>
        </div>
      </header>

      <div className="toolbar" role="toolbar" aria-label="Formatting">
        <ToolbarButton label="↶" onClick={() => editor?.chain().focus().undo().run()} />
        <ToolbarButton label="↷" onClick={() => editor?.chain().focus().redo().run()} />
        <span className="toolbar-divider" />
        <select
          aria-label="Text style"
          value={editor?.isActive("heading", { level: 1 }) ? "h1" : editor?.isActive("heading", { level: 2 }) ? "h2" : "p"}
          onChange={(event) => {
            if (event.target.value === "h1") editor?.chain().focus().toggleHeading({ level: 1 }).run();
            else if (event.target.value === "h2") editor?.chain().focus().toggleHeading({ level: 2 }).run();
            else editor?.chain().focus().setParagraph().run();
          }}
        >
          <option value="p">Normal text</option>
          <option value="h1">Heading 1</option>
          <option value="h2">Heading 2</option>
        </select>
        <span className="toolbar-divider" />
        <ToolbarButton active={editor?.isActive("bold")} label="B" onClick={() => editor?.chain().focus().toggleBold().run()} />
        <ToolbarButton active={editor?.isActive("italic")} label="I" onClick={() => editor?.chain().focus().toggleItalic().run()} />
        <ToolbarButton active={editor?.isActive("strike")} label="S" onClick={() => editor?.chain().focus().toggleStrike().run()} />
        <ToolbarButton active={editor?.isActive("bulletList")} label="• List" onClick={() => editor?.chain().focus().toggleBulletList().run()} />
        <ToolbarButton active={editor?.isActive("orderedList")} label="1. List" onClick={() => editor?.chain().focus().toggleOrderedList().run()} />
        <ToolbarButton active={editor?.isActive("blockquote")} label="❝" onClick={() => editor?.chain().focus().toggleBlockquote().run()} />
        <span className="toolbar-spacer" />
        <div className="mode-switch" aria-label="Editor mode">
          <button className={mode === "rich" ? "active" : ""} type="button" onClick={() => switchMode("rich")}>Rich text</button>
          <button className={mode === "markdown" ? "active" : ""} type="button" onClick={() => switchMode("markdown")}>Markdown</button>
        </div>
      </div>

      {notice !== null && (
        <div className="notice" role="status">
          <span>{notice}</span>
          <button type="button" onClick={() => setNotice(null)}>Dismiss</button>
        </div>
      )}

      <main className="workspace">
        <div className="page-shell">
          {mode === "rich" ? (
            <EditorContent editor={editor} />
          ) : (
            <textarea
              className="markdown-editor"
              aria-label="Markdown source"
              value={markdown}
              spellCheck
              onChange={(event) => {
                const next = event.target.value;
                currentRef.current = next;
                setMarkdown(next);
                setSaveState("saving");
              }}
            />
          )}
        </div>
      </main>
      <footer className="status-bar">
        <span>Markdown-backed</span>
        <span>Revision {versionRef.current}</span>
        <span>{markdown.trim() === "" ? 0 : markdown.trim().split(/\s+/).length} words</span>
      </footer>
    </div>
  );
}
