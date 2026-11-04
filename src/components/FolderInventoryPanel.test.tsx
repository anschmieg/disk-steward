import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import FolderInventoryPanel, {
  type FolderInventory,
} from "./FolderInventoryPanel";
import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const sample: FolderInventory = {
  rootLabel: "Chosen folder",
  observationId: "observation-1",
  breadcrumbs: ["Chosen folder"],
  canGoBack: false,
  enumerationComplete: false,
  stopReason: "entry_limit",
  unreadableCount: 1,
  directoriesNotTraversed: 1,
  regularFileLogicalBytes: "9007199254741000",
  entries: [
    {
      id: "1",
      name: "Alpha",
      kind: "file",
      logicalBytes: "7",
      hardLinked: true,
      canOpen: false,
    },
    {
      id: "2",
      name: "Zeta",
      kind: "file",
      logicalBytes: "9007199254740993",
      hardLinked: false,
      canOpen: false,
    },
    {
      id: "3",
      name: "Nested",
      kind: "folder",
      logicalBytes: null,
      hardLinked: false,
      canOpen: true,
    },
  ],
};
beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);
test("native selection displays exact sizes, honest incomplete coverage and stable sorting", async () => {
  vi.mocked(invoke).mockResolvedValue(sample);
  const busy = vi.fn();
  render(<FolderInventoryPanel native onBusyChange={busy} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(invoke).toHaveBeenCalledWith("inspect_selected_folder_entries");
  expect(screen.getByText(/Incomplete immediate/)).toHaveTextContent(
    "entry limit was reached",
  );
  expect(screen.getByText(/9007199254741000 bytes/)).toBeVisible();
  const rows = within(screen.getByRole("table")).getAllByRole("row");
  expect(rows[1]).toHaveTextContent("Zeta");
  expect(rows[1]).toHaveTextContent("9007199254740993");
  fireEvent.change(screen.getByRole("combobox"), { target: { value: "name" } });
  expect(
    within(screen.getByRole("table")).getAllByRole("row")[1],
  ).toHaveTextContent("Alpha");
  expect(busy.mock.calls).toEqual([[true], [false]]);
});
test("cancellation and errors preserve the prior observation and release busy state", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce(null)
    .mockRejectedValueOnce(new Error("private path"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  const choose = screen.getByRole("button", { name: "Choose folder" });
  fireEvent.click(choose);
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(choose);
  await screen.findByText(/Folder selection canceled/);
  expect(screen.getByRole("heading", { name: "Chosen folder" })).toBeVisible();
  fireEvent.click(choose);
  await screen.findByText(/folder could not be inspected/);
  expect(screen.queryByText(/private path/)).not.toBeInTheDocument();
  await waitFor(() => expect(choose).toBeEnabled());
});
test("browser cannot invoke native inventory", () => {
  render(<FolderInventoryPanel native={false} onBusyChange={vi.fn()} />);
  expect(screen.getByRole("button", { name: "Choose folder" })).toBeDisabled();
  expect(invoke).not.toHaveBeenCalled();
});

test("folder navigation sends opaque observed IDs, updates breadcrumbs and uses fresh ID for Back", async () => {
  const child: FolderInventory = {
    ...sample,
    rootLabel: "Nested",
    observationId: "observation-2",
    breadcrumbs: ["Chosen folder", "Nested"],
    canGoBack: true,
    entries: [],
    enumerationComplete: true,
    stopReason: null,
  };
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce(child)
    .mockResolvedValueOnce({ ...sample, observationId: "observation-3" });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(
    screen.getByRole("button", { name: "Back to parent folder" }),
  ).toBeDisabled();
  expect(screen.getByRole("heading", { name: "Chosen folder" })).toHaveFocus();
  fireEvent.click(screen.getByRole("button", { name: "Open folder Nested" }));
  await screen.findByRole("heading", { name: "Nested" });
  expect(invoke).toHaveBeenNthCalledWith(2, "navigate_folder_inventory", {
    request: { expectedObservationId: "observation-1", entryId: "3" },
  });
  expect(
    within(
      screen.getByRole("navigation", { name: "Folder location" }),
    ).getAllByRole("listitem"),
  ).toHaveLength(2);
  expect(screen.getByRole("heading", { name: "Nested" })).toHaveFocus();
  fireEvent.click(
    screen.getByRole("button", { name: "Back to parent folder" }),
  );
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(invoke).toHaveBeenNthCalledWith(3, "navigate_folder_inventory", {
    request: { expectedObservationId: "observation-2", entryId: null },
  });
});

test("failed or stale navigation keeps the old observation and busy ownership blocks duplicate calls", async () => {
  let rejectNavigation: (error: Error) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectNavigation = reject;
        }),
    );
  const busy = vi.fn();
  render(<FolderInventoryPanel native onBusyChange={busy} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  const open = await screen.findByRole("button", {
    name: "Open folder Nested",
  });
  fireEvent.click(open);
  expect(open).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Inspecting folder…" }),
  ).toBeDisabled();
  fireEvent.click(open);
  expect(invoke).toHaveBeenCalledTimes(2);
  rejectNavigation(new Error("stale identity /private/path"));
  await screen.findByText(/folder could not be inspected/);
  expect(screen.getByRole("heading", { name: "Chosen folder" })).toBeVisible();
  expect(screen.queryByText(/private\/path/)).not.toBeInTheDocument();
  expect(open).toBeEnabled();
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
});

test("filtering keeps the full observed logical-size denominator and never changes coverage", async () => {
  vi.mocked(invoke).mockResolvedValue({
    ...sample,
    regularFileLogicalBytes: "100",
    entries: [
      { ...sample.entries[0], logicalBytes: "25" },
      { ...sample.entries[1], logicalBytes: "75" },
    ],
  });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.change(
    screen.getByRole("searchbox", { name: "Filter observed names" }),
    { target: { value: "Alpha" } },
  );
  const rows = within(screen.getByRole("table")).getAllByRole("row");
  expect(rows).toHaveLength(2);
  expect(rows[1].querySelector(".inventory-size-bar")).toHaveStyle({
    width: "25%",
  });
  expect(screen.getByText(/Size bars use/)).toHaveTextContent(
    "100 regular-file logical bytes",
  );
  expect(screen.getByText(/Incomplete immediate/)).toBeVisible();
  expect(screen.getByText(/Showing 1 of 2/)).toBeVisible();
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "missing" },
  });
  expect(
    screen.getByText("No observed names match this filter."),
  ).toBeVisible();
  expect(
    screen.queryByText(/No readable entries were observed/),
  ).not.toBeInTheDocument();
  expect(invoke).toHaveBeenCalledTimes(1);
});

test("explicit plan import uses current observation and live revision without renderer metadata", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce({ revisionId: "live-r7", items: [1] })
    .mockResolvedValueOnce({ revisionId: "live-r8", items: [1, 2, 3, 4] });
  const openPlan = vi.fn();
  render(
    <FolderInventoryPanel
      native
      onBusyChange={vi.fn()}
      onOpenPlan={openPlan}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "Alpha" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Add observed entries to your plan" }),
  );
  await screen.findByText(/Added 3 observed entries/);
  expect(invoke).toHaveBeenNthCalledWith(2, "get_general_plan", {
    request: { profile: "live" },
  });
  expect(invoke).toHaveBeenNthCalledWith(3, "import_observed_review_items", {
    request: {
      expectedObservationId: "observation-1",
      expectedRevisionId: "live-r7",
    },
  });
  expect(
    screen.getByRole("button", { name: "Add observed entries to your plan" }),
  ).toBeDisabled();
  expect(screen.getByRole("heading", { name: "Chosen folder" })).toBeVisible();
  fireEvent.click(
    screen.getByRole("button", { name: "Open your review plan" }),
  );
  expect(openPlan).toHaveBeenCalledOnce();
});

test("failed import preserves observed entries and does not automatically retry", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce({ revisionId: "live-r7", items: [] })
    .mockRejectedValueOnce(new Error("stale private database path"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Add observed entries to your plan" }),
  );
  await screen.findByText(/entries could not be added/);
  expect(
    screen.getByRole("button", { name: "Add observed entries to your plan" }),
  ).toBeEnabled();
  expect(screen.getByText("Zeta")).toBeVisible();
  expect(invoke).toHaveBeenCalledTimes(3);
  expect(screen.queryByText(/private database path/)).not.toBeInTheDocument();
});
