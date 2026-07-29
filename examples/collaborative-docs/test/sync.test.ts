// SPDX-License-Identifier: MPL-2.0
import {
  applyEdit,
  applyTextChange,
  diffText,
  rebaseChange,
} from "../src/shared/sync.ts";

const initial = {
  id: "test",
  title: "Test",
  markdown: "alpha beta gamma",
  version: 0,
  changes: [],
};

const first = applyEdit(initial, {
  documentId: "test",
  clientId: "alice",
  mutationId: "one",
  baseVersion: 0,
  change: { from: 6, deleteCount: 4, insert: "bravo" },
});
if (first._tag !== "Applied") {
  throw new Error("first edit should apply");
}
if (first.document.markdown !== "alpha bravo gamma") {
  throw new Error("edit should replace text");
}

const concurrent = applyEdit(first.document, {
  documentId: "test",
  clientId: "bob",
  mutationId: "two",
  baseVersion: 0,
  change: { from: 16, deleteCount: 0, insert: "!" },
});
if (concurrent._tag !== "Applied") {
  throw new Error("non-overlapping stale edit should rebase");
}
if (concurrent.document.markdown !== "alpha bravo gamma!") {
  throw new Error("rebased edit should account for changed length");
}

const conflict = applyEdit(first.document, {
  documentId: "test",
  clientId: "bob",
  mutationId: "three",
  baseVersion: 0,
  change: { from: 7, deleteCount: 2, insert: "XX" },
});
if (conflict._tag !== "Conflict") {
  throw new Error("overlapping edits should conflict");
}

const change = diffText("hello world", "hello brave world");
if (change === null) {
  throw new Error("diff should produce a change");
}
if (applyTextChange("hello world", change) !== "hello brave world") {
  throw new Error("diff should round-trip");
}
const rebased = rebaseChange({ from: 10, deleteCount: 0, insert: "!" }, [
  { from: 2, deleteCount: 3, insert: "longer" },
]);
if (rebased === null || rebased.from !== 13) {
  throw new Error("rebase should shift edits after changed text");
}

console.log("collaborative sync tests passed");
