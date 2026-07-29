// SPDX-License-Identifier: MPL-2.0

export type TextChange = {
  readonly from: number;
  readonly deleteCount: number;
  readonly insert: string;
};

export type VersionedChange = TextChange & {
  readonly version: number;
  readonly clientId: string;
  readonly mutationId: string;
};

export type DocumentSnapshot = {
  readonly id: string;
  readonly title: string;
  readonly markdown: string;
  readonly version: number;
  readonly changes: ReadonlyArray<VersionedChange>;
};

export type EditRequest = {
  readonly documentId: string;
  readonly clientId: string;
  readonly mutationId: string;
  readonly baseVersion: number;
  readonly change: TextChange;
};

export type ApplyResult =
  | { readonly _tag: "Applied"; readonly document: DocumentSnapshot; readonly change: VersionedChange }
  | { readonly _tag: "Conflict"; readonly document: DocumentSnapshot; readonly reason: string };

export const applyTextChange = (text: string, change: TextChange) =>
  `${text.slice(0, change.from)}${change.insert}${text.slice(change.from + change.deleteCount)}`;

export const diffText = (before: string, after: string): TextChange | null => {
  if (before === after) {
    return null;
  }

  let prefix = 0;
  const sharedLength = Math.min(before.length, after.length);
  while (prefix < sharedLength && before[prefix] === after[prefix]) {
    prefix += 1;
  }

  let suffix = 0;
  while (
    suffix < sharedLength - prefix &&
    before[before.length - 1 - suffix] === after[after.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  return {
    from: prefix,
    deleteCount: before.length - prefix - suffix,
    insert: after.slice(prefix, after.length - suffix),
  };
};

export const rebaseChange = (
  change: TextChange,
  accepted: ReadonlyArray<TextChange>,
): TextChange | null => {
  let rebased = change;

  for (const other of accepted) {
    const end = rebased.from + rebased.deleteCount;
    const otherEnd = other.from + other.deleteCount;

    if (end <= other.from) {
      continue;
    }
    if (rebased.from >= otherEnd) {
      rebased = {
        from: rebased.from + other.insert.length - other.deleteCount,
        deleteCount: rebased.deleteCount,
        insert: rebased.insert,
      };
      continue;
    }

    return null;
  }

  return rebased;
};

export const applyEdit = (
  document: DocumentSnapshot,
  request: EditRequest,
): ApplyResult => {
  const oldestBaseVersion = document.changes.length === 0
    ? document.version
    : document.changes[0].version - 1;
  if (
    request.baseVersion < oldestBaseVersion ||
    request.baseVersion > document.version ||
    request.change.from < 0 ||
    request.change.deleteCount < 0
  ) {
    return { _tag: "Conflict", document, reason: "Unknown base revision" };
  }

  const accepted = document.changes.filter(
    (change) => change.version > request.baseVersion,
  );
  const rebased = rebaseChange(request.change, accepted);
  if (
    rebased === null ||
    rebased.from + rebased.deleteCount > document.markdown.length
  ) {
    return {
      _tag: "Conflict",
      document,
      reason: "Another collaborator changed the same text",
    };
  }

  const version = document.version + 1;
  const versioned: VersionedChange = {
    from: rebased.from,
    deleteCount: rebased.deleteCount,
    insert: rebased.insert,
    version,
    clientId: request.clientId,
    mutationId: request.mutationId,
  };
  const changes = [...document.changes, versioned].slice(-100);
  return {
    _tag: "Applied",
    change: versioned,
    document: {
      id: document.id,
      title: document.title,
      version,
      markdown: applyTextChange(document.markdown, rebased),
      changes,
    },
  };
};
