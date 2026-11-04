import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
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

const clipboardDescriptor = Object.getOwnPropertyDescriptor(
  navigator,
  "clipboard",
);
afterEach(() => {
  cleanup();
  if (clipboardDescriptor)
    Object.defineProperty(navigator, "clipboard", clipboardDescriptor);
  else Reflect.deleteProperty(navigator, "clipboard");
});

function finishedPilot() {
  const pilot = recorder();
  const report = {
    schemaVersion: 1 as const,
    kind: "synthetic_review_pilot_metrics" as const,
    profile: "pilot" as const,
    participantCode: "P1",
    initial: context,
    final: context,
    elapsedMs: 10,
    counts: Object.fromEntries(
      PILOT_EVENTS.map((event) => [event, 0]),
    ) as Record<(typeof PILOT_EVENTS)[number], number>,
    humanAssessment: "not_recorded" as const,
    executionAvailable: false as const,
  };
  return { ...pilot, phase: "finished" as const, report };
}
function mountFinished(pilot = finishedPilot(), busy = false) {
  render(
    <ReviewPilotPanel
      recorder={pilot}
      context={context}
      busy={busy}
      canStart
    />,
  );
  fireEvent.click(screen.getByText("Review pilot recording"));
  return pilot;
}
function setClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value });
}

describe("explicit pilot report copying", () => {
  it("copies only the displayed finished report after explicit activation", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    setClipboard({ writeText });
    const pilot = mountFinished();
    fireEvent.change(screen.getByLabelText("Preservation"), {
      target: { value: "correct" },
    });
    fireEvent.change(
      screen.getByLabelText("Reversal count (optional, 0–10,000)"),
      {
        target: { value: "3" },
      },
    );
    expect(writeText).not.toHaveBeenCalled();
    const shown = (
      screen.getByLabelText("Pilot session report") as HTMLTextAreaElement
    ).value;
    fireEvent.click(screen.getByRole("button", { name: "Copy report" }));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("Report copied."),
    );
    expect(writeText).toHaveBeenCalledExactlyOnceWith(shown);
    expect(JSON.parse(shown)).toEqual(pilot.report);
    expect(pilot.discard).not.toHaveBeenCalled();
  });

  it.each(["missing", "rejected"])(
    "retains the manual report when clipboard is %s",
    async (kind) => {
      const writeText = vi
        .fn()
        .mockRejectedValue(new Error("synthetic clipboard failure"));
      setClipboard(kind === "missing" ? undefined : { writeText });
      const pilot = mountFinished();
      const shown = (
        screen.getByLabelText("Pilot session report") as HTMLTextAreaElement
      ).value;
      fireEvent.click(screen.getByRole("button", { name: "Copy report" }));
      await waitFor(() =>
        expect(screen.getByRole("status")).toHaveTextContent(
          "Select and copy the report manually.",
        ),
      );
      expect(
        (screen.getByLabelText("Pilot session report") as HTMLTextAreaElement)
          .value,
      ).toBe(shown);
      expect(screen.getByRole("button", { name: "Copy report" })).toBeEnabled();
      expect(pilot.discard).not.toHaveBeenCalled();
    },
  );

  it("prevents repeated writes and discard while copying", async () => {
    let complete!: () => void;
    const writeText = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          complete = resolve;
        }),
    );
    setClipboard({ writeText });
    const pilot = mountFinished();
    fireEvent.click(screen.getByRole("button", { name: "Copy report" }));
    const copying = screen.getByRole("button", { name: "Copying report…" });
    expect(copying).toBeDisabled();
    const discard = screen.getByRole("button", { name: "Discard recording" });
    expect(discard).toBeDisabled();
    fireEvent.click(copying);
    fireEvent.click(discard);
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(pilot.discard).not.toHaveBeenCalled();
    await act(async () => complete());
    expect(screen.getByRole("button", { name: "Copy report" })).toBeEnabled();
    expect(discard).toBeEnabled();
  });

  it("does not copy while the workspace is busy", () => {
    const writeText = vi.fn();
    setClipboard({ writeText });
    mountFinished(undefined, true);
    const button = screen.getByRole("button", { name: "Copy report" });
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(writeText).not.toHaveBeenCalled();
  });
});

describe("moderator observation session isolation", () => {
  it("resets observations for a new report without changing either metrics report", () => {
    const first = finishedPilot();
    const firstMetrics = JSON.stringify(first.report, null, 2);
    const props = {
      recorder: first,
      context,
      busy: false,
      canStart: true,
    };
    const view = render(<ReviewPilotPanel {...props} />);
    fireEvent.click(screen.getByText("Review pilot recording"));

    fireEvent.change(screen.getByLabelText("Preservation"), {
      target: { value: "incorrect" },
    });
    const baseReport = screen.getByLabelText(
      "Pilot session report",
    ) as HTMLTextAreaElement;
    expect(baseReport.value).toBe(firstMetrics);
    expect(JSON.parse(baseReport.value)).toEqual(first.report);

    const secondReport = {
      ...first.report,
      participantCode: "P2",
      elapsedMs: 20,
    };
    view.rerender(
      <ReviewPilotPanel
        {...props}
        recorder={{ ...first, report: secondReport }}
      />,
    );

    expect(screen.getByLabelText("Preservation")).toHaveValue("not_recorded");
    const secondBaseReport = screen.getByLabelText(
      "Pilot session report",
    ) as HTMLTextAreaElement;
    expect(JSON.parse(secondBaseReport.value)).toEqual(secondReport);
    const sidecar = JSON.parse(
      (
        screen.getByLabelText(
          "Moderator observations JSON",
        ) as HTMLTextAreaElement
      ).value,
    );
    expect(sidecar.metricsReport).toEqual(secondReport);
    expect(sidecar.metricsReport.humanAssessment).toBe("not_recorded");
    expect(first.report).toEqual(JSON.parse(firstMetrics));
  });
});

describe("moderator observation lifecycle", () => {
  it("hides observations before finish and discards them when a session is discarded", () => {
    const pilot = finishedPilot();
    const props = { context, busy: false, canStart: true };
    const view = render(
      <ReviewPilotPanel
        {...props}
        recorder={{ ...pilot, phase: "idle", report: null }}
      />,
    );
    fireEvent.click(screen.getByText("Review pilot recording"));
    expect(screen.queryByLabelText("Preservation")).not.toBeInTheDocument();
    view.rerender(
      <ReviewPilotPanel
        {...props}
        recorder={{ ...pilot, phase: "recording", report: null }}
      />,
    );
    expect(
      screen.queryByLabelText("Moderator observations JSON"),
    ).not.toBeInTheDocument();
    view.rerender(<ReviewPilotPanel {...props} recorder={pilot} />);
    fireEvent.change(screen.getByLabelText("Preservation"), {
      target: { value: "incorrect" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Discard recording" }));
    expect(pilot.discard).toHaveBeenCalledExactlyOnceWith();
    view.rerender(
      <ReviewPilotPanel
        {...props}
        recorder={{ ...pilot, phase: "idle", report: null }}
      />,
    );
    expect(
      screen.queryByLabelText("Moderator observations JSON"),
    ).not.toBeInTheDocument();
    // Reusing even the exact previous report cannot revive discarded notes.
    view.rerender(<ReviewPilotPanel {...props} recorder={pilot} />);
    expect(screen.getByLabelText("Preservation")).toHaveValue("not_recorded");
  });
});
