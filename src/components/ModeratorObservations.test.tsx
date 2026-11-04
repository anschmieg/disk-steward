import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { PILOT_EVENTS } from "../review-pilot-contract";
import type { ReviewPilotReport } from "../review-pilot-contract";
import ModeratorObservations from "./ModeratorObservations";

afterEach(cleanup);

function metricsReport(): ReviewPilotReport {
  return {
    schemaVersion: 1,
    kind: "synthetic_review_pilot_metrics",
    profile: "pilot",
    participantCode: "P1",
    initial: {
      revisionNumber: 1,
      itemCount: 2,
      groupCount: 1,
      selectedCount: 0,
      logicalSelectedBytes: 0,
      confirmed: true,
    },
    final: null,
    elapsedMs: 10,
    counts: Object.fromEntries(
      PILOT_EVENTS.map((event) => [event, 0]),
    ) as Record<(typeof PILOT_EVENTS)[number], number>,
    humanAssessment: "not_recorded",
    executionAvailable: false,
  };
}

function json(): Record<string, unknown> {
  return JSON.parse(
    (
      screen.getByLabelText(
        "Moderator observations JSON",
      ) as HTMLTextAreaElement
    ).value,
  ) as Record<string, unknown>;
}

describe("ModeratorObservations", () => {
  it("starts with explicit not-recorded values and embeds the exact metrics report", () => {
    const report = metricsReport();
    render(<ModeratorObservations report={report} busy={false} />);

    expect(screen.getByLabelText("Preservation")).toHaveValue("not_recorded");
    expect(screen.getByLabelText("Comprehension")).toHaveValue("not_recorded");
    expect(screen.getByLabelText("Assistance")).toHaveValue("not_recorded");
    const sidecar = json();
    expect(sidecar).toEqual({
      schemaVersion: 1,
      kind: "synthetic_review_pilot_moderator_observations",
      metricsReport: report,
      observations: {
        preservation: "not_recorded",
        comprehension: "not_recorded",
        assistance: "not_recorded",
      },
    });
    expect(report.humanAssessment).toBe("not_recorded");
    expect(
      screen.getByLabelText("Moderator observations JSON"),
    ).toHaveAttribute("readonly");
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("records fixed choices and valid optional integer counts", () => {
    render(<ModeratorObservations report={metricsReport()} busy={false} />);
    fireEvent.change(screen.getByLabelText("Preservation"), {
      target: { value: "incorrect" },
    });
    fireEvent.change(screen.getByLabelText("Comprehension"), {
      target: { value: "correct" },
    });
    fireEvent.change(screen.getByLabelText("Assistance"), {
      target: { value: "provided" },
    });
    fireEvent.change(
      screen.getByLabelText("Confusion count (optional, 0–10,000)"),
      {
        target: { value: "0" },
      },
    );
    fireEvent.change(
      screen.getByLabelText("Reversal count (optional, 0–10,000)"),
      {
        target: { value: "10000" },
      },
    );

    expect(json().observations).toEqual({
      preservation: "incorrect",
      comprehension: "correct",
      assistance: "provided",
      confusionCount: 0,
      reversalCount: 10000,
    });
  });

  it.each([
    "-1",
    "1.5",
    "1e2",
    "words",
    "10001",
    " 1",
    "Infinity",
    "000001",
    "9007199254740993",
  ])("rejects invalid count input %s and hides the JSON sidecar", (value) => {
    render(<ModeratorObservations report={metricsReport()} busy={false} />);
    fireEvent.change(
      screen.getByLabelText("Confusion count (optional, 0–10,000)"),
      { target: { value } },
    );
    expect(
      screen.getByLabelText("Confusion count (optional, 0–10,000)"),
    ).toHaveAttribute("aria-invalid", "true");
    expect(
      screen.getByText("Correct the count fields to display valid JSON."),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Moderator observations JSON"),
    ).not.toBeInTheDocument();
  });

  it("recovers from invalid reversal input and resets on a distinct report object", () => {
    const report = metricsReport();
    const view = render(<ModeratorObservations report={report} busy={false} />);
    const reversal = screen.getByLabelText(
      "Reversal count (optional, 0–10,000)",
    );
    fireEvent.change(reversal, { target: { value: "-2" } });
    expect(
      screen.queryByLabelText("Moderator observations JSON"),
    ).not.toBeInTheDocument();
    fireEvent.change(reversal, { target: { value: "7" } });
    expect(json().observations).toMatchObject({ reversalCount: 7 });
    fireEvent.change(screen.getByLabelText("Assistance"), {
      target: { value: "none" },
    });
    view.rerender(
      <ModeratorObservations report={{ ...report }} busy={false} />,
    );
    expect(screen.getByLabelText("Assistance")).toHaveValue("not_recorded");
    expect(
      screen.getByLabelText("Reversal count (optional, 0–10,000)"),
    ).toHaveValue("");
    expect(json().observations).not.toHaveProperty("reversalCount");
  });

  it("disables all observation inputs while the workspace is busy", () => {
    render(<ModeratorObservations report={metricsReport()} busy />);
    expect(screen.getByLabelText("Preservation")).toBeDisabled();
    expect(screen.getByLabelText("Comprehension")).toBeDisabled();
    expect(screen.getByLabelText("Assistance")).toBeDisabled();
    expect(
      screen.getByLabelText("Confusion count (optional, 0–10,000)"),
    ).toBeDisabled();
    expect(
      screen.getByLabelText("Reversal count (optional, 0–10,000)"),
    ).toBeDisabled();
  });
});
