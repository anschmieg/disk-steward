import { useId, useState } from "react";
import type { ReviewPilotReport } from "../review-pilot-contract";

type Props = {
  report: ReviewPilotReport;
  busy: boolean;
};

type ObservationReport = {
  schemaVersion: 1;
  kind: "synthetic_review_pilot_moderator_observations";
  metricsReport: ReviewPilotReport;
  observations: {
    preservation: "not_recorded" | "correct" | "incorrect";
    comprehension: "not_recorded" | "correct" | "incorrect";
    assistance: "not_recorded" | "none" | "provided";
    confusionCount?: number;
    reversalCount?: number;
  };
};

function parseOptionalCount(value: string): number | null | undefined {
  if (value === "") return null;
  if (value.length > 5 || !/^\d+$/.test(value)) return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed <= 10_000 ? parsed : undefined;
}

export default function ModeratorObservations({ report, busy }: Props) {
  const instanceId = useId();
  const [observedReport, setObservedReport] = useState(report);
  const [preservation, setPreservation] =
    useState<ObservationReport["observations"]["preservation"]>("not_recorded");
  const [comprehension, setComprehension] =
    useState<ObservationReport["observations"]["comprehension"]>(
      "not_recorded",
    );
  const [assistance, setAssistance] =
    useState<ObservationReport["observations"]["assistance"]>("not_recorded");
  const [confusionText, setConfusionText] = useState("");
  const [reversalText, setReversalText] = useState("");

  // Reset before rendering another session's artifact. React retries this
  // component before committing, so old observations never label the new report.
  if (observedReport !== report) {
    setObservedReport(report);
    setPreservation("not_recorded");
    setComprehension("not_recorded");
    setAssistance("not_recorded");
    setConfusionText("");
    setReversalText("");
    return null;
  }

  const confusionCount = parseOptionalCount(confusionText);
  const reversalCount = parseOptionalCount(reversalText);
  const invalidCounts =
    confusionCount === undefined || reversalCount === undefined;
  const observations: ObservationReport["observations"] = {
    preservation,
    comprehension,
    assistance,
    ...(confusionCount !== null && confusionCount !== undefined
      ? { confusionCount }
      : {}),
    ...(reversalCount !== null && reversalCount !== undefined
      ? { reversalCount }
      : {}),
  };
  const sidecar: ObservationReport = {
    schemaVersion: 1,
    kind: "synthetic_review_pilot_moderator_observations",
    metricsReport: report,
    observations,
  };

  return (
    <section
      className="review-pilot-counts moderator-observations"
      aria-labelledby={`${instanceId}-title`}
    >
      <h3 id={`${instanceId}-title`}>Moderator observations</h3>
      <p>
        Manual entries for this report. They are observations, not an app
        assessment or pilot pass.
      </p>
      <p>
        Preservation covers every critical keep, exclusion, survivor, and
        blocked-item decision. Comprehension covers selected, staged, and
        recovered totals. Count occasions of confusion and changes of mind;
        blank counts mean not recorded.
      </p>
      <div className="review-pilot-controls">
        <label>
          Preservation
          <select
            value={preservation}
            disabled={busy}
            onChange={(event) =>
              setPreservation(
                event.target
                  .value as ObservationReport["observations"]["preservation"],
              )
            }
          >
            <option value="not_recorded">Not recorded</option>
            <option value="correct">Correct</option>
            <option value="incorrect">Incorrect</option>
          </select>
        </label>
        <label>
          Comprehension
          <select
            value={comprehension}
            disabled={busy}
            onChange={(event) =>
              setComprehension(
                event.target
                  .value as ObservationReport["observations"]["comprehension"],
              )
            }
          >
            <option value="not_recorded">Not recorded</option>
            <option value="correct">Correct</option>
            <option value="incorrect">Incorrect</option>
          </select>
        </label>
        <label>
          Assistance
          <select
            value={assistance}
            disabled={busy}
            onChange={(event) =>
              setAssistance(
                event.target
                  .value as ObservationReport["observations"]["assistance"],
              )
            }
          >
            <option value="not_recorded">Not recorded</option>
            <option value="none">None</option>
            <option value="provided">Provided</option>
          </select>
        </label>
        <label>
          Confusion count (optional, 0–10,000)
          <input
            type="text"
            inputMode="numeric"
            maxLength={5}
            aria-label="Confusion count (optional, 0–10,000)"
            value={confusionText}
            disabled={busy}
            aria-invalid={confusionCount === undefined}
            aria-describedby={
              confusionCount === undefined
                ? `${instanceId}-confusion-error`
                : undefined
            }
            onChange={(event) => setConfusionText(event.target.value)}
          />
          {confusionCount === undefined && (
            <span id={`${instanceId}-confusion-error`} role="alert">
              Enter a whole number from 0 to 10,000, or leave blank.
            </span>
          )}
        </label>
        <label>
          Reversal count (optional, 0–10,000)
          <input
            type="text"
            inputMode="numeric"
            maxLength={5}
            aria-label="Reversal count (optional, 0–10,000)"
            value={reversalText}
            disabled={busy}
            aria-invalid={reversalCount === undefined}
            aria-describedby={
              reversalCount === undefined
                ? `${instanceId}-reversal-error`
                : undefined
            }
            onChange={(event) => setReversalText(event.target.value)}
          />
          {reversalCount === undefined && (
            <span id={`${instanceId}-reversal-error`} role="alert">
              Enter a whole number from 0 to 10,000, or leave blank.
            </span>
          )}
        </label>
      </div>
      {invalidCounts ? (
        <p role="alert">Correct the count fields to display valid JSON.</p>
      ) : (
        <label className="review-pilot-report">
          Moderator observations JSON — select and copy manually
          <textarea
            aria-label="Moderator observations JSON"
            readOnly
            value={JSON.stringify(sidecar, null, 2)}
          />
        </label>
      )}
    </section>
  );
}
