import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App";
import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: () => true,
}));
vi.mock("./components/GeneralPlanWorkspace", () => ({
  default: ({ initialProfile }: { initialProfile: string }) => (
    <p>Active review profile: {initialProfile}</p>
  ),
}));
beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);
test("live explorer and review plan remain usable when the independent example plan fails", async () => {
  vi.mocked(invoke).mockRejectedValue("Example unavailable");
  render(<App />);
  expect(screen.getByRole("button", { name: "Choose folder" })).toBeEnabled();
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("get_example_plan"));
  expect(screen.getByRole("button", { name: "Choose folder" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: /review plan/i }));
  expect(await screen.findByText("Active review profile: live")).toBeVisible();
});

test("returning from review and unavailable example pages retains the bounded explorer without rescanning", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_example_plan") throw new Error("Example unavailable");
    if (command === "inspect_selected_folder_entries")
      return {
        observationId: "retained-observation",
        rootLabel: "Retained folder",
        pageNumber: 3,
        entriesSeen: "1025",
        canContinue: true,
        canCheckpoint: true,
        breadcrumbs: ["Retained folder"],
        canGoBack: false,
        enumerationComplete: false,
        unreadableCount: 0,
        stopReason: "entry_limit",
        directoriesNotTraversed: 0,
        regularFileLogicalBytes: "5",
        entries: [
          {
            id: "retained-entry",
            name: "Retained file",
            kind: "file",
            availability: "unknown",
            navigationBlocker: null,
            logicalBytes: "5",
            allocatedBytes: "4096",
            linkCount: "1",
            hardLinked: false,
            canOpen: false,
          },
        ],
      };
    throw new Error("Unexpected command");
  });
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Retained folder" });
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "Retained" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect Retained file" }),
  );
  const calls = vi.mocked(invoke).mock.calls.length;
  fireEvent.click(
    screen.getByRole("button", { name: "Open your review plan" }),
  );
  expect(await screen.findByText("Active review profile: live")).toBeVisible();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(
    screen.queryByRole("heading", { name: "Retained file" }),
  ).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Explore folder" }));
  expect(screen.getByRole("searchbox")).toHaveValue("Retained");
  expect(screen.getByText(/Page 3 · 1025/)).toBeVisible();
  expect(screen.getByRole("heading", { name: "Retained file" })).toHaveFocus();
  fireEvent.click(screen.getByRole("button", { name: "Fixture demo" }));
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Explore folder" }));
  expect(screen.getByRole("searchbox")).toHaveValue("Retained");
  expect(screen.getByRole("heading", { name: "Retained file" })).toHaveFocus();
  expect(invoke).toHaveBeenCalledTimes(calls);
});
