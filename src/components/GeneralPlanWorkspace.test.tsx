import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import GeneralPlanWorkspace from "./GeneralPlanWorkspace";
import { editorItem } from "../test-fixtures/editor";
import type {
  EditorFrozenReview,
  EditorReviewSummary,
  EditorSnapshot,
  PlanEdit,
} from "../general-plan-contract";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./AdvancedPlanPanel", () => ({
  default: (props: {
    snapshot: EditorSnapshot;
    confirmed: boolean;
    editsConfirmed: boolean;
    readOnly: boolean;
    onEdit: (edit: PlanEdit) => Promise<void>;
    onUndo: () => Promise<void>;
    onRedo: () => Promise<void>;
    onRefresh: () => Promise<void>;
    onFreezeReview: () => Promise<void>;
  }) => (
    <div>
      <span>{props.snapshot.revisionId}</span>
      <span data-testid="panel-state">
        {String(props.confirmed)}:{String(props.editsConfirmed)}:
        {String(props.readOnly)}
      </span>
      <button
        onClick={() =>
          void props.onEdit({
            kind: "setChoice",
            itemId: "item-pilot",
            choice: "include",
          })
        }
      >
        Edit
      </button>
      <button onClick={() => void props.onUndo()}>Undo</button>
      <button onClick={() => void props.onRedo()}>Redo</button>
      <button onClick={() => void props.onFreezeReview()}>Freeze review</button>
      <button onClick={() => void props.onRefresh()}>Refresh editor</button>
    </div>
  ),
}));
vi.mock("./ExclusionsPanel", () => ({ default: () => null }));

const mockedInvoke = vi.mocked(invoke);
const summary = (
  profile: "pilot" | "stress",
  id = `${profile}-r1`,
): EditorSnapshot => ({
  schemaVersion: 1,
  kind: "non_executable_editor",
  executionAvailable: false,
  planId: `plan-${profile}`,
  revisionId: id,
  revisionNumber: 1,
  items: [
    editorItem(`item-${profile}`, {
      suggestion: { choice: "keep", rationale: "Synthetic retained item" },
      userChoice: "keep",
      effectiveSelected: false,
    }),
  ],
  groups: [],
  duplicates: [],
  totals: {
    selectedCount: 0,
    logicalSelectedBytes: 0,
    inPlanLogicalBytes: 1024,
    recoveryLowerBytes: 0,
    recoveryUpperBytes: 0,
  },
  canUndo: false,
  canRedo: false,
});
const reviews: EditorReviewSummary[] = [];
const frozen: EditorFrozenReview = {
  ...summary("pilot", "pilot-r1"),
  reviewId: "saved-1",
  reviewVersion: "disk-steward-general-review-v1",
  contentHash: "a".repeat(64),
};
const noOp = () => undefined;

function mount(native = true) {
  return render(
    <GeneralPlanWorkspace
      native={native}
      onBusyChange={noOp}
      onSummaryChange={noOp}
    />,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockedInvoke.mockImplementation(async (command, args) => {
    const profile =
      (args as { request?: { profile?: "pilot" | "stress" } })?.request
        ?.profile ?? "pilot";
    if (command === "get_general_plan") return summary(profile) as never;
    if (command === "list_general_reviews") return reviews as never;
    if (command === "get_exclusion_registry")
      return {
        schemaVersion: 1,
        revisionId: "exclusions-1",
        entries: [],
      } as never;
    throw new Error(`Unexpected command ${String(command)}`);
  });
});
afterEach(cleanup);

describe("GeneralPlanWorkspace revision and review boundaries", () => {
  it("does not invoke Tauri in browser preview", () => {
    mount(false);
    expect(
      screen.getByText(/browser preview cannot save a review/i),
    ).toBeInTheDocument();
    expect(mockedInvoke).not.toHaveBeenCalled();
  });

  it("loads the active profile and its review list", async () => {
    mount();
    await screen.findByText("pilot-r1");
    // Loaded and editable is distinct from an immutable saved review.
    expect(screen.getByTestId("panel-state")).toHaveTextContent(
      "false:true:false",
    );
    expect(mockedInvoke).toHaveBeenCalledWith("get_general_plan", {
      request: { profile: "pilot" },
    });
    expect(mockedInvoke).toHaveBeenCalledWith("list_general_reviews", {
      request: { profile: "pilot" },
    });
  });

  it("requires explicit refresh after a stale revision and sends the current revision with edits, history, and freeze", async () => {
    mount();
    await screen.findByText("pilot-r1");
    mockedInvoke.mockImplementation(async (command, args) => {
      const request = (args as { request?: { expectedRevisionId?: string } })
        ?.request;
      if (command === "edit_general_plan")
        if (request?.expectedRevisionId === "pilot-r1")
          throw { code: "stale_revision", message: "stale" };
        else
          return {
            snapshot: { ...summary("pilot", "pilot-r3"), canUndo: true },
            blocked: [],
            changed: true,
          } as never;
      if (command === "get_general_plan")
        return summary("pilot", "pilot-r2") as never;
      if (command === "list_general_reviews") return reviews as never;
      if (command === "undo_general_plan")
        return { ...summary("pilot", "pilot-r4"), canRedo: true } as never;
      if (command === "redo_general_plan")
        return { ...summary("pilot", "pilot-r5"), canUndo: true } as never;
      if (command === "freeze_general_review")
        return {
          ...frozen,
          reviewId: "review-after-edit",
          revisionId: "pilot-r5",
        } as never;
      throw new Error(String(command));
    });
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    await waitFor(() =>
      expect(screen.getByTestId("panel-state")).toHaveTextContent(
        "false:false:false",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    expect(
      mockedInvoke.mock.calls.filter(
        ([command]) => command === "edit_general_plan",
      ),
    ).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Refresh editor" }));
    await screen.findByText("pilot-r2");
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    await waitFor(() =>
      expect(mockedInvoke).toHaveBeenCalledWith("edit_general_plan", {
        request: {
          profile: "pilot",
          expectedRevisionId: "pilot-r2",
          edit: { kind: "setChoice", itemId: "item-pilot", choice: "include" },
        },
      }),
    );
    await screen.findByText("pilot-r3");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() =>
      expect(mockedInvoke).toHaveBeenCalledWith("undo_general_plan", {
        request: { profile: "pilot", expectedRevisionId: "pilot-r3" },
      }),
    );
    await screen.findByText("pilot-r4");
    fireEvent.click(screen.getByRole("button", { name: "Redo" }));
    await waitFor(() =>
      expect(mockedInvoke).toHaveBeenCalledWith("redo_general_plan", {
        request: { profile: "pilot", expectedRevisionId: "pilot-r4" },
      }),
    );
    await screen.findByText("pilot-r5");
    fireEvent.click(screen.getByRole("button", { name: "Freeze review" }));
    await waitFor(() =>
      expect(mockedInvoke).toHaveBeenCalledWith("freeze_general_review", {
        request: { profile: "pilot", expectedRevisionId: "pilot-r5" },
      }),
    );
  });

  it("keeps a frozen review immutable and read-only, then reloads the selected profile", async () => {
    mockedInvoke.mockImplementation(async (command, args) => {
      const profile =
        (args as { request?: { profile?: "pilot" | "stress" } })?.request
          ?.profile ?? "pilot";
      if (command === "get_general_plan") return summary(profile) as never;
      if (command === "list_general_reviews") return reviews as never;
      if (command === "get_exclusion_registry")
        return {
          schemaVersion: 1,
          revisionId: "exclusions-1",
          entries: [],
        } as never;
      if (command === "freeze_general_review") return frozen as never;
      throw new Error(String(command));
    });
    mount();
    await screen.findByText("pilot-r1");
    fireEvent.click(screen.getByRole("button", { name: "Freeze review" }));
    await waitFor(() =>
      expect(screen.getByTestId("panel-state")).toHaveTextContent(
        "true:true:true",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    expect(mockedInvoke).not.toHaveBeenCalledWith(
      "edit_general_plan",
      expect.anything(),
    );
    expect(
      screen.getByText(/cannot approve or change files/i),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Return to editor" }));
    const selector = screen.getByLabelText(/example workspace/i);
    fireEvent.change(selector, { target: { value: "stress" } });
    await screen.findByText("stress-r1");
    expect(mockedInvoke).toHaveBeenCalledWith("get_general_plan", {
      request: { profile: "stress" },
    });
    expect(mockedInvoke).toHaveBeenCalledWith("list_general_reviews", {
      request: { profile: "stress" },
    });
  });

  it("leaves the plan untouched when metadata selection is canceled", async () => {
    mockedInvoke.mockImplementation(async (command) => {
      if (command === "get_general_plan") return summary("pilot") as never;
      if (command === "list_general_reviews") return reviews as never;
      if (command === "get_exclusion_registry")
        return {
          schemaVersion: 1,
          revisionId: "exclusions-1",
          entries: [],
        } as never;
      if (command === "add_selected_general_item")
        return { snapshot: null, canceled: true } as never;
      throw new Error(String(command));
    });
    mount();
    await screen.findByText("pilot-r1");
    fireEvent.click(screen.getByRole("button", { name: /add file metadata/i }));
    expect(
      await screen.findByText("Selection canceled. The plan is unchanged."),
    ).toBeInTheDocument();
    expect(mockedInvoke).toHaveBeenCalledWith("add_selected_general_item", {
      request: { profile: "pilot", expectedRevisionId: "pilot-r1" },
    });
    expect(screen.getByText("pilot-r1")).toBeInTheDocument();
  });
});
