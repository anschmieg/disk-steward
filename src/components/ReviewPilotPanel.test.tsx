import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import ReviewPilotPanel from "./ReviewPilotPanel";
import type {
  PilotContext,
  ReviewPilotRecorder,
} from "../review-pilot-contract";
import { PILOT_EVENTS } from "../review-pilot-contract";

const context: PilotContext = {
  revisionNumber: 1,
  itemCount: 2,
  groupCount: 1,
  selectedCount: 0,
  logicalSelectedBytes: 0,
  confirmed: true,
};
const recorder = (): ReviewPilotRecorder => ({
  phase: "idle",
  elapsedMs: 0,
  report: null,
  start: vi.fn(() => true),
  finish: vi.fn(),
  discard: vi.fn(),
  record: vi.fn(),
});
describe("ReviewPilotPanel", () => {
  it("starts and finishes through the recorder and presents a manual read-only report", () => {
    const pilot = recorder();
    const { rerender } = render(
      <ReviewPilotPanel
        recorder={pilot}
        context={context}
        busy={false}
        canStart
      />,
    );
    fireEvent.click(screen.getByText("Review pilot recording"));
    fireEvent.change(screen.getByPlaceholderText("P1"), {
      target: { value: "P1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    expect(pilot.start).toHaveBeenCalledWith("P1", context);
    rerender(
      <ReviewPilotPanel
        recorder={{ ...pilot, phase: "recording" }}
        context={context}
        busy={false}
        canStart
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Finish recording" }));
    expect(pilot.finish).toHaveBeenCalledWith(context);
    const report = {
      schemaVersion: 1 as const,
      kind: "synthetic_review_pilot_metrics" as const,
      profile: "pilot" as const,
      participantCode: "P1",
      initial: context,
      final: null,
      elapsedMs: 10,
      counts: Object.fromEntries(
        PILOT_EVENTS.map((event) => [event, 0]),
      ) as Record<(typeof PILOT_EVENTS)[number], number>,
      humanAssessment: "not_recorded" as const,
      executionAvailable: false as const,
    };
    rerender(
      <ReviewPilotPanel
        recorder={{ ...pilot, phase: "finished", report }}
        context={context}
        busy={false}
        canStart
      />,
    );
    expect(screen.getByLabelText("Pilot session report")).toHaveAttribute(
      "readonly",
    );
    expect(screen.getByText("Recorded interactions")).toBeInTheDocument();
    expect(
      screen.getByLabelText("Pilot session report").getAttribute("value"),
    ).toBeNull();
    expect(
      (screen.getByLabelText("Pilot session report") as HTMLTextAreaElement)
        .value,
    ).toContain('"executionAvailable": false');
  });
});
