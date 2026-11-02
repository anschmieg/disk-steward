import type {
  EditorGroup,
  EditorItem,
  EditorSnapshot,
} from "../general-plan-contract";

export function editorItem(
  itemId: string,
  overrides: Partial<EditorItem> = {},
): EditorItem {
  return {
    itemId,
    parentId: null,
    label: itemId,
    kind: "file",
    logicalBytes: 1024,
    suggestion: { choice: "include", rationale: "Synthetic fixture" },
    userChoice: null,
    inPlan: true,
    effectiveSelected: !(
      overrides.blockers?.length || overrides.userChoice === "keep"
    ),
    blockers: [],
    evidence: [{ id: `evidence-${itemId}`, summary: "Synthetic evidence" }],
    excludedDescendantIds: [],
    groupIds: [],
    recoveryLowerBytes: 0,
    recoveryUpperBytes: 1024,
    ...overrides,
  };
}

export function editorGroup(
  id: string,
  memberIds: string[],
  overrides: Partial<EditorGroup> = {},
): EditorGroup {
  return {
    id,
    label: id,
    memberIds,
    selectedCount: memberIds.length,
    selectedLogicalBytes: memberIds.length * 1024,
    blockedCount: 0,
    ...overrides,
  };
}

export function editorSnapshot(
  items: EditorItem[],
  groups: EditorGroup[] = [],
  options: Partial<EditorSnapshot> = {},
): EditorSnapshot {
  const included = items.filter((item) => item.effectiveSelected);
  const knownBytes = items.reduce(
    (total, item) => total + (item.logicalBytes ?? 0),
    0,
  );
  return {
    schemaVersion: 1,
    kind: "non_executable_editor",
    executionAvailable: false,
    planId: "fixture-plan",
    revisionId: "fixture-revision-1",
    revisionNumber: 1,
    items,
    groups,
    duplicates: [],
    totals: {
      selectedCount: included.length,
      logicalSelectedBytes: included.reduce(
        (total, item) => total + (item.logicalBytes ?? 0),
        0,
      ),
      inPlanLogicalBytes: knownBytes,
      recoveryLowerBytes: 0,
      recoveryUpperBytes: included.reduce(
        (total, item) => total + (item.logicalBytes ?? 0),
        0,
      ),
    },
    canUndo: false,
    canRedo: false,
    ...options,
  };
}

/** Flat grouped navigation fixture; not the full participant-pilot dataset. */
export function groupedEditorSnapshot(): EditorSnapshot {
  const items = Array.from({ length: 1000 }, (_, index) => {
    const groupIndex = Math.floor(index / 40);
    const id = `finding-${index + 1}`;
    return editorItem(id, {
      label: `Finding ${String(index + 1).padStart(4, "0")}`,
      groupIds: [`group-${groupIndex + 1}`],
      blockers:
        groupIndex < 10 && index % 40 === 0
          ? [{ code: "needs_review", reason: "Requires review" }]
          : [],
      userChoice: index === 999 ? "keep" : null,
      effectiveSelected:
        index !== 999 && !(groupIndex < 10 && index % 40 === 0),
    });
  });
  const groups = Array.from({ length: 25 }, (_, index) => {
    const members = items.slice(index * 40, index * 40 + 40);
    return editorGroup(
      `group-${index + 1}`,
      members.map((item) => item.itemId),
      {
        label: `Review group ${String(index + 1).padStart(2, "0")}`,
        selectedCount: members.filter((item) => item.effectiveSelected).length,
        selectedLogicalBytes: members
          .filter((item) => item.effectiveSelected)
          .reduce((total, item) => total + (item.logicalBytes ?? 0), 0),
        blockedCount: members.filter((item) => item.blockers.length > 0).length,
      },
    );
  });
  return editorSnapshot(items, groups);
}
