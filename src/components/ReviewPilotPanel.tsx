import { useState } from "react";
import type {
  PilotContext,
  ReviewPilotRecorder,
} from "../review-pilot-contract";
import "./ReviewPilotPanel.css";

type Props = {
  recorder: ReviewPilotRecorder;
  context: PilotContext | null;
  busy: boolean;
  canStart: boolean;
};
const formatTime = (elapsed: number | null) =>
  elapsed === null
    ? "Time unavailable"
    : `${Math.floor(elapsed / 60000)}:${String(Math.floor(elapsed / 1000) % 60).padStart(2, "0")}`;

export default function ReviewPilotPanel({
  recorder,
  context,
  busy,
  canStart,
}: Props) {
  const [code, setCode] = useState("");
  const contextValid =
    !!context &&
    context.confirmed &&
    [
      context.revisionNumber,
      context.itemCount,
      context.groupCount,
      context.selectedCount,
      context.logicalSelectedBytes,
    ].every((value) => Number.isSafeInteger(value) && value >= 0);
  const [message, setMessage] = useState("");
  const begin = () =>
    setMessage(
      recorder.start(code, context!)
        ? "Recording started."
        : "Load the current editor and enter a valid anonymous participant code.",
    );
  return (
    <details className="review-pilot-panel" data-review-pilot-controls>
      <summary>Review pilot recording</summary>
      <p>
        This tab only; no report is sent or saved automatically. A moderator
        must record preservation, comprehension, confusion, and assistance
        separately. Recording does not certify that the pilot passed.
      </p>
      <p>
        Load the current editor before starting. Click and key counts measure
        input events, not unique choices or correctness.
      </p>
      {recorder.phase === "idle" && (
        <div className="review-pilot-controls">
          <label>
            Anonymous participant code{" "}
            <input
              value={code}
              placeholder="P1"
              maxLength={32}
              disabled={busy}
              onChange={(event) => setCode(event.target.value)}
            />
          </label>
          <button
            type="button"
            disabled={
              busy ||
              !canStart ||
              !contextValid ||
              !/^[A-Za-z0-9_-]{1,32}$/.test(code)
            }
            onClick={begin}
          >
            Start recording
          </button>
        </div>
      )}
      {recorder.phase === "recording" && (
        <div className="review-pilot-controls">
          <span aria-live="off">Elapsed: {formatTime(recorder.elapsedMs)}</span>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              recorder.finish(context);
              setMessage("Recording finished.");
            }}
          >
            Finish recording
          </button>
        </div>
      )}
      {recorder.phase === "finished" && recorder.report && (
        <>
          <section
            className="review-pilot-counts"
            aria-label="Recorded interaction counts"
          >
            <h3>Recorded interactions</h3>
            <p>
              Clicks: {recorder.report.counts.pointer_activation} · Key presses:{" "}
              {recorder.report.counts.key_press}
            </p>
            <p>
              Item choices:{" "}
              {recorder.report.counts.item_choice +
                recorder.report.counts.duplicate_survivor}{" "}
              · Group choices: {recorder.report.counts.group_choice}
            </p>
            <p>
              Membership changes:{" "}
              {recorder.report.counts.membership_change +
                recorder.report.counts.descendant_exclusion}{" "}
              · Review navigation:{" "}
              {recorder.report.counts.undo +
                recorder.report.counts.redo +
                recorder.report.counts.freeze_review +
                recorder.report.counts.open_review +
                recorder.report.counts.return_to_editor +
                recorder.report.counts.refresh}
            </p>
            <p>
              Metadata additions:{" "}
              {recorder.report.counts.add_file_metadata +
                recorder.report.counts.add_folder_metadata}{" "}
              · Canceled selections: {recorder.report.counts.selection_canceled}{" "}
              · Failed requests: {recorder.report.counts.request_failed}
            </p>
            <p>
              These counts do not assess correctness, reversals, or moderator
              decisions.
            </p>
          </section>
          <label className="review-pilot-report">
            Pilot session report
            <textarea
              aria-label="Pilot session report"
              readOnly
              value={JSON.stringify(recorder.report, null, 2)}
            />
          </label>
        </>
      )}
      {recorder.phase !== "idle" && (
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            recorder.discard();
            setMessage("Recording discarded.");
          }}
        >
          Discard recording
        </button>
      )}
      <p className="review-pilot-status" role="status">
        {message}
      </p>
    </details>
  );
}
