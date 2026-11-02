/** Local, aggregate-only metrics; never approval or a human correctness assessment. */
export const PILOT_EVENTS = [
  "pointer_activation",
  "key_press",
  "item_choice",
  "group_choice",
  "membership_change",
  "descendant_exclusion",
  "duplicate_survivor",
  "undo",
  "redo",
  "freeze_review",
  "open_review",
  "return_to_editor",
  "refresh",
  "add_file_metadata",
  "add_folder_metadata",
  "selection_canceled",
  "request_failed",
] as const;
export type PilotEvent = (typeof PILOT_EVENTS)[number];
export type PilotContext = {
  revisionNumber: number;
  itemCount: number;
  groupCount: number;
  selectedCount: number;
  logicalSelectedBytes: number;
  confirmed: boolean;
};
export type ReviewPilotReport = {
  schemaVersion: 1;
  kind: "synthetic_review_pilot_metrics";
  profile: "pilot";
  participantCode: string;
  initial: PilotContext;
  final: PilotContext | null;
  elapsedMs: number | null;
  counts: Record<PilotEvent, number>;
  humanAssessment: "not_recorded";
  executionAvailable: false;
};
export type ReviewPilotRecorder = {
  phase: "idle" | "recording" | "finished";
  elapsedMs: number | null;
  report: ReviewPilotReport | null;
  start: (participantCode: string, context: PilotContext) => boolean;
  finish: (context: PilotContext | null) => void;
  discard: () => void;
  record: (event: PilotEvent) => void;
};
