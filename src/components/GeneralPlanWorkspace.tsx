import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import AdvancedPlanPanel from "./AdvancedPlanPanel";
import ExclusionsPanel from "./ExclusionsPanel";
import type {
  EditorFrozenReview,
  EditorReviewSummary,
  EditorSnapshot,
  EditOutcome,
  PlanEdit,
} from "../general-plan-contract";
import { useReviewPilotSession } from "../review-pilot";
import type { PilotContext, PilotEvent } from "../review-pilot-contract";
import ReviewPilotPanel from "./ReviewPilotPanel";

export type GeneralPlanSummary = {
  count: number;
  bytes: number;
  selectedBytes: number;
  selectedCount: number;
};
type Profile = "pilot" | "stress";
type Props = {
  native: boolean;
  onBusyChange: (busy: boolean) => void;
  onSummaryChange: (summary: GeneralPlanSummary | null) => void;
};

export default function GeneralPlanWorkspace({
  native,
  onBusyChange,
  onSummaryChange,
}: Props) {
  const [profile, setProfile] = useState<Profile>("pilot");
  const [snapshot, setSnapshot] = useState<EditorSnapshot | null>(null);
  const [savedReview, setSavedReview] = useState<EditorFrozenReview | null>(
    null,
  );
  const [showReview, setShowReview] = useState(false);
  const [reviewHistory, setReviewHistory] = useState<EditorReviewSummary[]>([]);
  const [busy, setBusy] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const recorder = useReviewPilotSession();
  const pilotContext = useMemo<PilotContext | null>(
    () =>
      snapshot
        ? {
            revisionNumber: snapshot.revisionNumber,
            itemCount: snapshot.items.length,
            groupCount: snapshot.groups.length,
            selectedCount: snapshot.totals.selectedCount,
            logicalSelectedBytes: snapshot.totals.logicalSelectedBytes,
            confirmed,
          }
        : null,
    [snapshot, confirmed],
  );
  const record = recorder.record;
  const recordInteraction = useCallback(
    (event: PilotEvent) => record(event),
    [record],
  );
  const running = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const request = useCallback(
    async <T,>(operation: () => Promise<T>): Promise<T | undefined> => {
      if (!native || running.current) return;
      running.current = true;
      setBusy(true);
      onBusyChange(true);
      setError("");
      setNotice("");
      try {
        return await operation();
      } catch (failure) {
        recordInteraction("request_failed");
        if (mounted.current) {
          setConfirmed(false);
          setError(messageFor(failure));
        }
        return;
      } finally {
        running.current = false;
        if (mounted.current) setBusy(false);
        onBusyChange(false);
      }
    },
    [native, onBusyChange, recordInteraction],
  );

  const refresh = useCallback(async () => {
    const loaded = await request(async () => {
      const next = await invoke<EditorSnapshot>("get_general_plan", {
        request: { profile },
      });
      const reviews = await invoke<EditorReviewSummary[]>(
        "list_general_reviews",
        { request: { profile } },
      );
      return { next, reviews };
    });
    if (loaded && mounted.current) {
      recordInteraction("refresh");
      setSnapshot(loaded.next);
      setReviewHistory(loaded.reviews);
      setConfirmed(true);
      setNotice("");
    }
  }, [profile, request, recordInteraction]);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const display = useMemo<EditorSnapshot | null>(
    () =>
      showReview && savedReview
        ? { ...savedReview, canUndo: false, canRedo: false }
        : snapshot,
    [showReview, savedReview, snapshot],
  );
  useEffect(() => {
    if (display)
      onSummaryChange({
        count: display.items.length,
        bytes: display.totals.inPlanLogicalBytes,
        selectedBytes: display.totals.logicalSelectedBytes,
        selectedCount: display.totals.selectedCount,
      });
    else onSummaryChange(null);
  }, [display, onSummaryChange]);

  const edit = async (change: PlanEdit) => {
    if (!snapshot || !confirmed || showReview) return;
    const next = await request(() =>
      invoke<EditOutcome>("edit_general_plan", {
        request: {
          profile,
          expectedRevisionId: snapshot.revisionId,
          edit: change,
        },
      }),
    );
    if (next) {
      recordInteraction(editEvent(change));
      setSnapshot(next.snapshot);
      setConfirmed(true);
      setNotice(
        next.blocked.length
          ? `${next.blocked.length} ${next.blocked.length === 1 ? "item kept" : "items kept"} because of a blocker or an earlier Keep choice.`
          : next.changed
            ? "Choice saved."
            : "The current choice was preserved.",
      );
    }
  };
  const addSelectedItem = async (kind: "file" | "folder") => {
    if (!snapshot || !confirmed || showReview || profile !== "pilot") return;
    const result = await request(() =>
      invoke<{ snapshot: EditorSnapshot | null; canceled: boolean }>(
        kind === "folder"
          ? "add_selected_general_folder"
          : "add_selected_general_item",
        { request: { profile, expectedRevisionId: snapshot.revisionId } },
      ),
    );
    if (result) {
      if (result.canceled) recordInteraction("selection_canceled");
      else if (result.snapshot)
        recordInteraction(
          kind === "folder" ? "add_folder_metadata" : "add_file_metadata",
        );
      if (result.snapshot) setSnapshot(result.snapshot);
      setConfirmed(true);
      setNotice(
        result.canceled
          ? "Selection canceled. The plan is unchanged."
          : kind === "folder"
            ? "Folder metadata added with Keep. Its contents have not been inventoried; cleanup stays blocked. Find it under Other findings."
            : "Selected file metadata added with Keep. Cleanup stays blocked; its contents were not read. Find it under Other findings.",
      );
    }
  };
  const history = async (direction: "undo" | "redo") => {
    if (!snapshot || !confirmed || showReview) return;
    const next = await request(() =>
      invoke<EditorSnapshot>(`${direction}_general_plan`, {
        request: { profile, expectedRevisionId: snapshot.revisionId },
      }),
    );
    if (next) {
      recordInteraction(direction);
      setSnapshot(next);
      setConfirmed(true);
      setNotice(
        direction === "undo"
          ? "Change undone in a new revision."
          : "Change redone in a new revision.",
      );
    }
  };
  const freeze = async () => {
    if (!snapshot || !confirmed || showReview) return;
    const review = await request(() =>
      invoke<EditorFrozenReview>("freeze_general_review", {
        request: { profile, expectedRevisionId: snapshot.revisionId },
      }),
    );
    if (review) {
      recordInteraction("freeze_review");
      setSavedReview(review);
      setReviewHistory((previous) => [
        {
          reviewId: review.reviewId,
          revisionId: review.revisionId,
          revisionNumber: review.revisionNumber,
          selectedCount: review.totals.selectedCount,
          logicalSelectedBytes: review.totals.logicalSelectedBytes,
        },
        ...previous.filter((row) => row.reviewId !== review.reviewId),
      ]);
      setShowReview(true);
      setNotice("");
    }
  };

  const openReview = async (reviewId: string) => {
    const review = await request(() =>
      invoke<EditorFrozenReview>("get_general_review", {
        request: { profile, reviewId },
      }),
    );
    if (review) {
      recordInteraction("open_review");
      setSavedReview(review);
      setShowReview(true);
      setNotice("");
    }
  };

  if (!native)
    return (
      <section className="general-workspace">
        <p>
          The persistent grouped editor is available in the desktop app. This
          browser preview cannot save a review or change files.
        </p>
      </section>
    );
  return (
    <section
      className="general-workspace"
      onClickCapture={(event) => {
        if (!isPilotControl(event.target)) record("pointer_activation");
      }}
      onKeyDownCapture={(event) => {
        if (!isPilotControl(event.target)) record("key_press");
      }}
    >
      <div className="general-toolbar">
        <label>
          Example workspace{" "}
          <select
            value={profile}
            disabled={busy || showReview || recorder.phase === "recording"}
            onChange={(event) => {
              if (recorder.phase === "recording") return;
              setSnapshot(null);
              setSavedReview(null);
              setReviewHistory([]);
              setShowReview(false);
              setConfirmed(false);
              setNotice("");
              setProfile(event.target.value as Profile);
            }}
          >
            <option value="pilot">1,000 findings · 25 groups</option>
            <option value="stress">10,000 findings · 25 groups</option>
          </select>
        </label>
        <span>
          Example inventory · optional selected-item metadata · no content reads
        </span>
        <button
          className="ap-button quiet"
          disabled={busy || !confirmed || showReview || profile !== "pilot"}
          onClick={() => void addSelectedItem("file")}
          title="Select one file for a metadata-only review entry. Cleanup remains blocked. Available in the 1,000-item workspace."
        >
          Add file metadata…
        </button>
        <button
          className="ap-button quiet"
          disabled={busy || !confirmed || showReview || profile !== "pilot"}
          onClick={() => void addSelectedItem("folder")}
          title="Add one folder's name for review. Children will not be inventoried and cleanup remains blocked. This does not create a remembered exclusion."
        >
          Add folder metadata…
        </button>
        {reviewHistory.length > 0 && (
          <label>
            Saved reviews{" "}
            <select
              value={showReview && savedReview ? savedReview.reviewId : ""}
              disabled={busy}
              onChange={(event) => {
                if (event.target.value) void openReview(event.target.value);
              }}
            >
              <option value="">Choose a saved revision…</option>
              {reviewHistory.map((row) => (
                <option key={row.reviewId} value={row.reviewId}>
                  Revision {row.revisionNumber} · {row.selectedCount} selected
                </option>
              ))}
            </select>
          </label>
        )}
        {savedReview && !showReview && (
          <button
            className="ap-button quiet"
            disabled={busy}
            onClick={() => {
              setShowReview(true);
              record("open_review");
            }}
          >
            View saved review · revision {savedReview.revisionNumber}
          </button>
        )}
      </div>
      {showReview && savedReview && (
        <div className="general-review-banner" role="status">
          <div>
            <b>Saved review · revision {savedReview.revisionNumber}</b>
            <p>
              This immutable snapshot cannot approve or change files. Its
              choices may differ from the current editor.
            </p>
          </div>
          <button
            className="ap-button quiet"
            onClick={() => {
              setShowReview(false);
              record("return_to_editor");
            }}
          >
            Return to editor
          </button>
        </div>
      )}
      {notice && (
        <p className="general-notice" role="status">
          {notice}
        </p>
      )}
      {display && <ExclusionsPanel busy={busy} run={request} />}
      {display ? (
        <AdvancedPlanPanel
          snapshot={display}
          busy={busy}
          confirmed={
            showReview || savedReview?.revisionId === snapshot?.revisionId
          }
          editsConfirmed={confirmed}
          readOnly={showReview}
          error={error}
          onEdit={edit}
          onUndo={() => history("undo")}
          onRedo={() => history("redo")}
          onRefresh={
            showReview
              ? async () => {
                  setShowReview(false);
                  record("return_to_editor");
                }
              : refresh
          }
          onFreezeReview={freeze}
        />
      ) : (
        <div className="general-loading" role={error ? "alert" : "status"}>
          <p>{error || "Loading the example editor…"}</p>
          <button
            className="ap-button quiet"
            disabled={busy}
            onClick={() => void refresh()}
          >
            Refresh example
          </button>
        </div>
      )}
      {profile === "pilot" && (
        <ReviewPilotPanel
          recorder={recorder}
          context={pilotContext}
          busy={busy}
          canStart={!!snapshot && confirmed && !showReview}
        />
      )}
    </section>
  );
}

function isPilotControl(target: EventTarget | null): boolean {
  return (
    target instanceof Element &&
    !!target.closest("[data-review-pilot-controls]")
  );
}

function editEvent(edit: PlanEdit): PilotEvent {
  switch (edit.kind) {
    case "setChoice":
      return "item_choice";
    case "setGroupChoice":
      return "group_choice";
    case "setMembership":
      return "membership_change";
    case "excludeDescendants":
      return "descendant_exclusion";
    case "setDuplicateSurvivor":
      return "duplicate_survivor";
  }
}

function messageFor(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  )
    return error.message;
  return "The saved state could not be confirmed. Refresh the example before continuing.";
}
