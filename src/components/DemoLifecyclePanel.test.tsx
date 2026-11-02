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

  it("blocks operations when purge succeeds but status refresh fails", async () => {
    const result: PurgeExecutionResult = {
      executionId: "execution-1",
      reviewId: "review-1",
      state: "finished",
      items: [],
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
    expect(
      await screen.findByText(
        /removal result was recorded, but demo status could not be refreshed/i,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Prepare permanent removal" }),
    ).toBeDisabled();
    expect(
      screen.getByText(
        /Logical bytes are not a promise of free-space recovery/i,
      ),
    ).toBeInTheDocument();
  });

  it("confirms only the prepared review and displays refreshed successful status", async () => {
    const result: PurgeExecutionResult = {
      executionId: "execution-1",
      reviewId: "review-1",
      state: "finished",
      items: [
        {
          attemptId: "attempt-1",
          outcome: {
            permanentlyRemoved: {
              receiptId: "receipt-1",
              logicalBytes: 2048,
            },
          },
        },
      ],
      selectedLogicalBytes: 2048,
      durablyUnlinkedLogicalBytes: 2048,
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
      await screen.findByRole("button", { name: "Prepare permanent removal" }),
    );
    fireEvent.click(
      await screen.findByRole("button", { name: "Confirm permanent removal" }),
    );
    expect(await screen.findByText("Removed permanently")).toBeInTheDocument();
    expect(screen.getAllByText("Permanently removed")).toHaveLength(2);
    expect(mockedInvoke).toHaveBeenCalledWith("confirm_purge_execution", {
      request: { reviewId: "review-1" },
    });
    expect(statusCalls).toBe(2);
  });

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
            outcome: { uncertain: { receiptId: "receipt-1" } },
          },
        ],
        selectedLogicalBytes: 2048,
        durablyUnlinkedLogicalBytes: 0,
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
        screen.queryByText(/no permanent deletion/i),
      ).not.toBeInTheDocument();
    },
  );
});
