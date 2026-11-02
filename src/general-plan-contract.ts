/** Frozen camelCase DTOs for the non-executable general plan editor. */
export type PlanChoice = "keep" | "include" | "investigate";
export type EditorBlocker = { code: string; reason: string };
export type EditorEvidence = { id: string; summary: string };
export type EditorItem = {
  itemId: string;
  parentId: string | null;
  label: string;
  kind: "file" | "folder";
  logicalBytes: number | null;
  suggestion: { choice: PlanChoice; rationale: string };
  userChoice: PlanChoice | null;
  inPlan: boolean;
  effectiveSelected: boolean;
  blockers: EditorBlocker[];
  evidence: EditorEvidence[];
  excludedDescendantIds: string[];
  groupIds: string[];
  recoveryLowerBytes: number | null;
  recoveryUpperBytes: number | null;
};
export type EditorGroup = {
  id: string;
  label: string;
  memberIds: string[];
  selectedCount: number;
  selectedLogicalBytes: number;
  blockedCount: number;
};
export type EditorDuplicateSet = {
  id: string;
  memberIds: string[];
  survivorId: string | null;
};
export type EditorTotals = {
  selectedCount: number;
  logicalSelectedBytes: number;
  inPlanLogicalBytes: number;
  recoveryLowerBytes: number;
  recoveryUpperBytes: number | null;
};
export type EditorSnapshot = {
  schemaVersion: number;
  kind: "non_executable_editor";
  executionAvailable: false;
  planId: string;
  revisionId: string;
  revisionNumber: number;
  items: EditorItem[];
  groups: EditorGroup[];
  duplicates: EditorDuplicateSet[];
  totals: EditorTotals;
  canUndo: boolean;
  canRedo: boolean;
};
export type PlanEdit =
  | { kind: "setChoice"; itemId: string; choice: PlanChoice }
  | { kind: "setMembership"; itemId: string; inPlan: boolean }
  | { kind: "setGroupChoice"; groupId: string; choice: PlanChoice }
  | { kind: "excludeDescendants"; parentId: string; itemIds: string[] }
  | {
      kind: "setDuplicateSurvivor";
      duplicateSetId: string;
      survivorId: string;
    };
export type EditOutcome = {
  snapshot: EditorSnapshot;
  blocked: Array<{ itemId: string; reason: string }>;
  changed: boolean;
};

export type EditorFrozenReview = Omit<EditorSnapshot, "canUndo" | "canRedo"> & {
  reviewId: string;
  reviewVersion: string;
  contentHash: string;
};

export type EditorReviewSummary = {
  reviewId: string;
  revisionId: string;
  revisionNumber: number;
  selectedCount: number;
  logicalSelectedBytes: number;
};
