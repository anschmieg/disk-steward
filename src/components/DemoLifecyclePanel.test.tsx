import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import DemoLifecyclePanel from "./DemoLifecyclePanel";
import type {
  DemoPlanProjection,
  DemoStatus,
  PreparedPurgeReview,
  PurgeExecutionResult,
} from "../demo-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockedInvoke = vi.mocked(invoke);
const plan = (revisionId = "revision-1"): DemoPlanProjection => ({
  revisionId,
  actions: [
    { id: "a1", label: "Cargo build cache", bytes: 2048, decision: "include" },
    { id: "a2", label: "Python cache", bytes: 1024, decision: "keep" },
  ],
  selectedCount: 1,
  selectedBytes: 2048,
  totalBytes: 3072,
});
const readyStatus: DemoStatus = {
  schemaVersion: 1,
  kind: "disposable_file_demo",
  state: "ready",
  sessionId: "session-1",
  fixtures: [
    {
      actionId: "a1",
      receiptId: "receipt-1",
      status: "staged",
      bytes: 2048,
      sha256: "a".repeat(64),
      identitySha256: "b".repeat(64),
    },
    {
      actionId: "a2",
      receiptId: "receipt-2",
      status: "staged",
      bytes: 1024,
      sha256: "c".repeat(64),
      identitySha256: "d".repeat(64),
    },
  ],
};
const prepared: PreparedPurgeReview = {
  schemaVersion: 1,
  kind: "disposable_fixture_purge_review",
  reviewId: "review-1",
  planId: "plan-1",
  revisionId: "revision-1",
  sessionId: "session-1",
  operation: "permanently_remove_staged",
  createdAtMs: 1,
  expiresAtMs: Date.now() + 60_000,
  manifestSha256: "e".repeat(64),
  targets: [
    {
      actionId: "a1",
      receiptId: "receipt-1",
      label: "Cargo build cache",
      logicalBytes: 2048,
      status: "staged",
    },
  ],
  selectedLogicalBytes: 2048,
};
function mount(
  props: {
    native?: boolean;
    projection?: DemoPlanProjection;
    refresh?: () => Promise<void>;
  } = {},
) {
  return render(
    <DemoLifecyclePanel
      plan={props.projection ?? plan()}
      native={props.native ?? true}
      planBusy={false}
      planConfirmed
      onPlanRefresh={props.refresh ?? vi.fn().mockResolvedValue(undefined)}
      onBusyChange={vi.fn()}
      onModeChange={vi.fn()}
    />,
  );
}
beforeEach(() => {
  vi.clearAllMocks();
  mockedInvoke.mockImplementation(async (command) => {
    if (command === "get_fixture_demo_status") return readyStatus as never;
    if (command === "prepare_purge_review") return prepared as never;
    throw new Error(`Unexpected command ${String(command)}`);
  });
});
afterEach(cleanup);

describe("DemoLifecyclePanel purge safety", () => {
  it("does not invoke Tauri in browser preview", async () => {
    mount({ native: false });
    expect(
      await screen.findByText(/browser preview cannot create files/i),
    ).toBeInTheDocument();
    expect(mockedInvoke).not.toHaveBeenCalled();
  });

  it("prepares purge only for staged receipts in included actions", async () => {
    mount();
    fireEvent.click(
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    await waitFor(() =>
      expect(mockedInvoke).toHaveBeenCalledWith("prepare_purge_review", {
        request: {
          expectedRevisionId: "revision-1",
          targets: [{ actionId: "a1", receiptId: "receipt-1" }],
        },
      }),
    );
  });

  it("reports a native confirmation cancellation as a no-op", async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === "get_fixture_demo_status") return readyStatus as never;
      if (command === "prepare_purge_review") return prepared as never;
      if (command === "confirm_purge_execution")
        throw { code: "confirmation_cancelled" };
      throw new Error(String(command));
    });
    mount();
    fireEvent.click(
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: "Confirm permanent removal" }),
    );
    expect(
      await screen.findByText(
        "Confirmation cancelled. No demo files were changed.",
      ),
    ).toBeInTheDocument();
    expect(mockedInvoke).toHaveBeenCalledTimes(3);
  });

  it("keeps the purge result visible when refreshed status needs inspection", async () => {
    const result: PurgeExecutionResult = {
      executionId: "execution-1",
      reviewId: "review-1",
      state: "finished",
      items: [
        {
          attemptId: "attempt-secret",
          outcome: {
            permanently_removed: {
              receipt_id: "receipt-1",
              logical_bytes: 2048,
            },
          },
        },
      ],
      selectedLogicalBytes: 2048,
      durablyUnlinkedLogicalBytes: 2048,
      volumeObservation: {
        observedFreeSpaceDeltaBytes: 17,
        method: "fstatvfs_bavail_v1",
        volumeBindingSha256: "a".repeat(64),
      },
    };
    const needsInspection: DemoStatus = {
      ...readyStatus,
      state: "needs_inspection",
      fixtures: readyStatus.fixtures.map((fixture) =>
        fixture.actionId === "a1"
          ? { ...fixture, status: "permanently_removed" }
          : fixture,
      ),
    };
    let statusCalls = 0;
    mockedInvoke.mockImplementation(async (command) => {
      if (command === "get_fixture_demo_status") {
        statusCalls += 1;
        return (statusCalls === 1 ? readyStatus : needsInspection) as never;
      }
      if (command === "prepare_purge_review") return prepared as never;
      if (command === "confirm_purge_execution") return result as never;
      throw new Error(String(command));
    });
    mount();
    fireEvent.click(
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: "Confirm permanent removal" }),
    );
    expect(await screen.findByText("Removed permanently")).toBeInTheDocument();
    expect(
      screen.getByRole("status", { name: "Permanent removal result" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Observed volume free-space change: +17 bytes"),
    ).toBeInTheDocument();
    expect(screen.getAllByText("Cargo build cache")[0]).toBeInTheDocument();
    expect(screen.queryByText("attempt-secret")).not.toBeInTheDocument();
    expect(screen.queryByText("receipt-1")).not.toBeInTheDocument();
    expect(
      screen.getByText(
        /Logical bytes are not a promise of free-space recovery/i,
      ),
    ).toBeInTheDocument();
  });

  it("keeps a recorded result visible when status refresh fails", async () => {
    const result: PurgeExecutionResult = {
      executionId: "execution-1",
      reviewId: "review-1",
      state: "finished",
      items: [
        {
          attemptId: "attempt-secret",
          outcome: {
            permanently_removed: {
              receipt_id: "receipt-1",
              logical_bytes: 2048,
            },
          },
        },
      ],
      selectedLogicalBytes: 2048,
      durablyUnlinkedLogicalBytes: 2048,
    };
    let statusCalls = 0;
    mockedInvoke.mockImplementation(async (command) => {
      if (command === "get_fixture_demo_status") {
        statusCalls += 1;
        if (statusCalls > 1) throw new Error("status unavailable");
        return readyStatus as never;
      }
      if (command === "prepare_purge_review") return prepared as never;
      if (command === "confirm_purge_execution") return result as never;
      throw new Error(String(command));
    });
    mount();
    fireEvent.click(
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: "Confirm permanent removal" }),
    );
    expect(await screen.findByText("Removed permanently")).toBeInTheDocument();
    expect(screen.getAllByText("Cargo build cache").length).toBeGreaterThan(1);
    expect(
      screen.getByText(
        /removal result was recorded, but demo status could not be refreshed/i,
      ),
    ).toBeInTheDocument();
  });

  it.each([
    [1800, "+2 KiB"],
    [0, "0 bytes"],
    [-128, "−128 bytes"],
    [undefined, null],
  ] as const)(
    "shows the finished volume observation independently from logical bytes (%s)",
    async (observedFreeSpaceDeltaBytes, expectedObservation) => {
      const result: PurgeExecutionResult = {
        executionId: "execution-1",
        reviewId: "review-1",
        state: "finished",
        items: [
          {
            attemptId: "attempt-1",
            outcome: {
              permanently_removed: {
                receipt_id: "receipt-1",
                logical_bytes: 2048,
              },
            },
          },
        ],
        selectedLogicalBytes: 2048,
        durablyUnlinkedLogicalBytes: 2048,
        ...(observedFreeSpaceDeltaBytes === undefined
          ? {}
          : {
              volumeObservation: {
                observedFreeSpaceDeltaBytes,
                method: "internal-measurement-token",
                volumeBindingSha256: "f".repeat(64),
              },
            }),
      };
      const refreshedStatus: DemoStatus = {
        ...readyStatus,
        fixtures: readyStatus.fixtures.map((fixture) => ({
          ...fixture,
          status: "permanently_removed",
          receiptId: null,
        })),
      };
      let statusCalls = 0;
      mockedInvoke.mockImplementation(async (command) => {
        if (command === "get_fixture_demo_status") {
          statusCalls += 1;
          return statusCalls === 1
            ? (readyStatus as never)
            : (refreshedStatus as never);
        }
        if (command === "prepare_purge_review") return prepared as never;
        if (command === "confirm_purge_execution") return result as never;
        throw new Error(String(command));
      });
      mount();
      fireEvent.click(
        await screen.findByRole("button", {
          name: "Prepare permanent removal",
        }),
      );
      fireEvent.click(
        await screen.findByRole("button", {
          name: "Confirm permanent removal",
        }),
      );
      expect(
        await screen.findByText("Removed permanently"),
      ).toBeInTheDocument();
      expect(
        screen.getByText(
          expectedObservation
            ? `Observed volume free-space change: ${expectedObservation}`
            : "Observed volume free-space change unavailable.",
        ),
      ).toBeInTheDocument();
      expect(
        screen.getByText(/2048 of 2048 logical bytes removed/i),
      ).toBeInTheDocument();
      if (observedFreeSpaceDeltaBytes !== undefined) {
        expect(
          screen.getByText(
            /Volume-wide observation; other writers, snapshots/i,
          ),
        ).toBeInTheDocument();
        expect(
          screen.queryByText(/internal-measurement-token/),
        ).not.toBeInTheDocument();
        expect(screen.queryByText("f".repeat(64))).not.toBeInTheDocument();
      } else {
        expect(
          screen.queryByText(/Volume-wide observation; other writers/i),
        ).not.toBeInTheDocument();
      }
      expect(screen.getAllByText("Permanently removed")).toHaveLength(3);
      expect(mockedInvoke).toHaveBeenCalledWith("confirm_purge_execution", {
        request: { reviewId: "review-1" },
      });
      expect(statusCalls).toBe(2);
    },
  );

  it("invalidates prepared purge review when the plan revision changes", async () => {
    const view = mount();
    fireEvent.click(
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    await screen.findByRole("button", { name: "Confirm permanent removal" });
    view.rerender(
      <DemoLifecyclePanel
        plan={plan("revision-2")}
        native
        planBusy={false}
        planConfirmed
        onPlanRefresh={vi.fn().mockResolvedValue(undefined)}
        onBusyChange={vi.fn()}
        onModeChange={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Confirm permanent removal" }),
      ).not.toBeInTheDocument(),
    );
    expect(
      screen.getByText(/Refresh both plan and demo status/i),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(
      await screen.findByText("Latest plan and demo status loaded."),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Confirm permanent removal" }),
    ).not.toBeInTheDocument();
  });

  it.each(["partial", "uncertain"] as const)(
    "reports %s purge outcomes and avoids claiming no permanent deletion",
    async (state) => {
      const result: PurgeExecutionResult = {
        executionId: "execution-1",
        reviewId: "review-1",
        state,
        items: [
          {
            attemptId: "attempt-1",
            outcome: { uncertain: { receipt_id: "receipt-1" } },
          },
        ],
        selectedLogicalBytes: 2048,
        durablyUnlinkedLogicalBytes: 0,
        volumeObservation: {
          observedFreeSpaceDeltaBytes: 2048,
          method: "hidden-method",
          volumeBindingSha256: "f".repeat(64),
        },
      };
      mockedInvoke.mockImplementation(async (command) => {
        if (command === "get_fixture_demo_status") return readyStatus as never;
        if (command === "prepare_purge_review") return prepared as never;
        if (command === "confirm_purge_execution") return result as never;
        throw new Error(String(command));
      });
      mount();
      fireEvent.click(
        await screen.findByRole("button", {
          name: "Prepare permanent removal",
        }),
      );
      fireEvent.click(
        await screen.findByRole("button", {
          name: "Confirm permanent removal",
        }),
      );
      expect(
        await screen.findByText(
          state === "partial"
            ? "Removal partially completed"
            : "Removal needs inspection",
        ),
      ).toBeInTheDocument();
      expect(
        screen.getByText(
          /Logical bytes are not a promise of free-space recovery/i,
        ),
      ).toBeInTheDocument();
      expect(
        screen.getByText(
          /permanent removal requires separate review and native confirmation/i,
        ),
      ).toBeInTheDocument();
      expect(
        screen.queryByText(/Observed volume free-space change/i),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByText(/no permanent deletion/i),
      ).not.toBeInTheDocument();
    },
  );

  it("shows each snake_case item outcome with labels frozen at confirmation", async () => {
    const mixedPrepared: PreparedPurgeReview = {
      ...prepared,
      targets: [
        ...prepared.targets,
        {
          actionId: "a2",
          receiptId: "receipt-2",
          label: "Reviewed Python cache",
          logicalBytes: 1024,
          status: "staged",
        },
        {
          actionId: "a3",
          receiptId: "receipt-3",
          label: "Reviewed shader cache",
          logicalBytes: 512,
          status: "staged",
        },
      ],
    };
    const mixedResult: PurgeExecutionResult = {
      executionId: "execution-secret",
      reviewId: "review-1",
      state: "partial",
      items: [
        {
          attemptId: "attempt-secret-1",
          outcome: {
            permanently_removed: {
              receipt_id: "receipt-1",
              logical_bytes: 2048,
            },
          },
        },
        {
          attemptId: "attempt-secret-2",
          outcome: { uncertain: { receipt_id: "receipt-2" } },
        },
        {
          attemptId: "attempt-secret-3",
          outcome: { not_attempted: { receipt_id: "receipt-3" } },
        },
      ],
      selectedLogicalBytes: 3584,
      durablyUnlinkedLogicalBytes: 2048,
    };
    mockedInvoke.mockImplementation(async (command) => {
      if (command === "get_fixture_demo_status") return readyStatus as never;
      if (command === "prepare_purge_review") return mixedPrepared as never;
      if (command === "confirm_purge_execution") return mixedResult as never;
      throw new Error(String(command));
    });
    const view = mount();
    fireEvent.click(
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: "Confirm permanent removal" }),
    );
    expect(
      await screen.findByText("Reviewed Python cache"),
    ).toBeInTheDocument();
    expect(screen.getByText("Permanently removed")).toBeInTheDocument();
    expect(screen.getByText("Needs inspection")).toBeInTheDocument();
    expect(screen.getByText("Not attempted")).toBeInTheDocument();

    view.rerender(
      <DemoLifecyclePanel
        plan={{
          ...plan("revision-1"),
          actions: [
            {
              id: "a1",
              label: "Changed current label",
              bytes: 2048,
              decision: "include",
            },
          ],
        }}
        native
        planBusy={false}
        planConfirmed
        onPlanRefresh={vi.fn().mockResolvedValue(undefined)}
        onBusyChange={vi.fn()}
        onModeChange={vi.fn()}
      />,
    );
    expect(screen.getByText("Reviewed Python cache")).toBeInTheDocument();
    expect(screen.getAllByText("Changed current label")).toHaveLength(1);
    expect(
      screen.queryByText(/attempt-secret|execution-secret|receipt-[123]/),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText(/Observed volume free-space change/),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(
        /Logical bytes are not a promise of free-space recovery/i,
      ),
    ).toBeInTheDocument();
  });
});
