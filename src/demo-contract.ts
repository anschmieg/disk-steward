export type DemoFixtureStatus = {
  actionId: string;
  receiptId?: string | null;
  status: string;
  bytes: number;
  sha256: string;
  identitySha256: string;
};

export type DemoStatus = {
  schemaVersion: 1;
  kind: "disposable_file_demo";
  state: "not_started" | "ready" | "needs_inspection";
  sessionId?: string | null;
  fixtures: DemoFixtureStatus[];
  lastExecution?: DemoExecutionResult | null;
};

export type PreparedDemoReview = {
  schemaVersion: 1;
  kind: "disposable_file_review";
  reviewId: string;
  planId: string;
  revisionId: string;
  sessionId: string;
  operation: "stage" | "restore";
  items: Array<{ actionId: string; label: string; bytes: number }>;
  selectedBytes: number;
  manifestSha256: string;
  expiresAtMs: number;
};

export type DemoExecutionResult = {
  executionId: string;
  operation: "stage" | "restore";
  state: "finished" | "partial" | "uncertain";
  items: Array<{ actionId: string; receiptId?: string | null; status: string }>;
  stagedBytes?: number | null;
  restoredBytes?: number | null;
  recoveredBytes: 0;
};

export type PurgeTargetRequest = {
  actionId: string;
  receiptId: string;
};

export type PurgeReviewTarget = {
  actionId: string;
  receiptId: string;
  label: string;
  logicalBytes: number;
  status: string;
};

export type PreparedPurgeReview = {
  schemaVersion: 1;
  kind: "disposable_fixture_purge_review";
  reviewId: string;
  planId: string;
  revisionId: string;
  sessionId: string;
  operation: "permanently_remove_staged";
  createdAtMs: number;
  expiresAtMs: number;
  manifestSha256: string;
  targets: PurgeReviewTarget[];
  selectedLogicalBytes: number;
};

export type PurgeItemBatchOutcome = {
  attemptId: string;
  outcome:
    | { permanentlyRemoved: { receiptId: string; logicalBytes: number } }
    | { uncertain: { receiptId: string } }
    | { notAttempted: { receiptId: string } };
};

export type PurgeExecutionResult = {
  executionId: string;
  reviewId: string;
  state: "finished" | "partial" | "uncertain";
  items: PurgeItemBatchOutcome[];
  selectedLogicalBytes: number;
  durablyUnlinkedLogicalBytes: number;
};

export type DemoPlanProjection = {
  revisionId: string;
  actions: Array<{
    id: string;
    label: string;
    bytes: number;
    decision: "keep" | "include";
  }>;
  selectedCount: number;
  selectedBytes: number;
  totalBytes: number;
};

export type DemoPanelMode = "plan" | "demo";
