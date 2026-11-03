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
  pageNumber: 1,
  entriesSeen: "3",
  canContinue: true,
  canCheckpoint: true,
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
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: "7",
      allocatedBytes: "4096",
      linkCount: "2",
      hardLinked: true,
      canOpen: false,
    },
    {
      id: "2",
      name: "Zeta",
      kind: "file",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: "9007199254740993",
      allocatedBytes: "0",
      linkCount: "1",
      hardLinked: false,
      canOpen: false,
    },
    {
      id: "3",
      name: "Nested",
      kind: "folder",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: null,
      allocatedBytes: null,
      linkCount: null,
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
    screen.getByRole("button", { name: "Add this page to your plan" }),
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
    screen.getByRole("button", { name: "Add this page to your plan" }),
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
    screen.getByRole("button", { name: "Add this page to your plan" }),
  );
  await screen.findByText(/entries could not be added/);
  expect(
    screen.getByRole("button", { name: "Add this page to your plan" }),
  ).toBeEnabled();
  expect(screen.getByText("Zeta")).toBeVisible();
  expect(invoke).toHaveBeenCalledTimes(3);
  expect(screen.queryByText(/private database path/)).not.toBeInTheDocument();
});

test("allocation sort and inspector distinguish exact logical bytes, allocation and unknown directory sizes", async () => {
  const observed = {
    ...sample,
    entries: sample.entries.map((entry) =>
      entry.id === "1"
        ? { ...entry, allocatedBytes: "9007199254740993" }
        : entry,
    ),
  };
  vi.mocked(invoke).mockResolvedValue(observed);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.change(screen.getByRole("combobox"), {
    target: { value: "allocated" },
  });
  expect(
    within(screen.getByRole("table")).getAllByRole("row")[1],
  ).toHaveTextContent("Alpha");
  const inspect = screen.getByRole("button", { name: "Inspect Alpha" });
  fireEvent.click(inspect);
  let evidence = screen.getByRole("region", { name: "Entry evidence" });
  expect(
    within(evidence).getByRole("heading", { name: "Alpha" }),
  ).toHaveFocus();
  expect(within(evidence).getByText("9007199254740993")).toBeVisible();
  expect(within(evidence).getByText("7")).toBeVisible();
  expect(within(evidence).getByText(/Cleanup blocked/)).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Return to entry list" }));
  expect(inspect).toHaveFocus();
  expect(
    screen.queryByRole("region", { name: "Entry evidence" }),
  ).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Inspect Nested" }));
  evidence = screen.getByRole("region", { name: "Entry evidence" });
  expect(within(evidence).getAllByText("Unknown")).toHaveLength(3);
  expect(invoke).toHaveBeenCalledTimes(1);
});

test("filtering does not alter selected evidence and returning falls back to the name filter when row is hidden", async () => {
  vi.mocked(invoke).mockResolvedValue(sample);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(screen.getByRole("button", { name: "Inspect Alpha" }));
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "Zeta" },
  });
  expect(
    within(screen.getByRole("region", { name: "Entry evidence" })).getByRole(
      "heading",
      { name: "Alpha" },
    ),
  ).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Return to entry list" }));
  expect(screen.getByRole("searchbox")).toHaveFocus();
});

test("next page uses only the current observation ID, replaces rows, resets filter and keeps page totals explicit", async () => {
  const next = {
    ...sample,
    observationId: "observation-page-2",
    pageNumber: 2,
    entriesSeen: "4",
    canContinue: false,
    enumerationComplete: true,
    stopReason: null,
    regularFileLogicalBytes: "9",
    entries: [
      {
        ...sample.entries[0],
        id: "page2-file",
        name: "Later file",
        logicalBytes: "9",
      },
    ],
  };
  vi.mocked(invoke).mockResolvedValueOnce(sample).mockResolvedValueOnce(next);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "Alpha" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Inspect Alpha" }));
  fireEvent.click(screen.getByRole("button", { name: "Next page" }));
  await screen.findByText("Later file");
  expect(invoke).toHaveBeenNthCalledWith(2, "continue_folder_inventory", {
    request: { expectedObservationId: "observation-1" },
  });
  expect(screen.queryByText("Zeta")).not.toBeInTheDocument();
  expect(
    screen.queryByRole("region", { name: "Entry evidence" }),
  ).not.toBeInTheDocument();
  expect(screen.getByRole("searchbox")).toHaveValue("");
  expect(screen.getByText(/Page 2 · 4 entries observed/)).toBeVisible();
  expect(screen.getByText(/Reached the end of the directory/)).toBeVisible();
  expect(screen.getByRole("button", { name: "Next page" })).toBeDisabled();
  expect(
    screen.getByText(/Current-page regular-file logical sizes total 9 bytes/),
  ).toBeVisible();
});

test("failed page continuation closes the UI continuation without discarding the historical page", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockRejectedValueOnce(new Error("changed directory private/path"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(screen.getByRole("button", { name: "Next page" }));
  await screen.findByText(/Enumeration could not continue reliably/);
  expect(screen.getByText("Zeta")).toBeVisible();
  expect(screen.getByRole("button", { name: "Next page" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Choose folder" })).toBeEnabled();
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(screen.queryByText(/private\/path/)).not.toBeInTheDocument();
});

test("known dataless folders show a reason and no Open control; absence of the flag remains unknown", async () => {
  vi.mocked(invoke).mockResolvedValue({
    ...sample,
    entries: [
      {
        ...sample.entries[2],
        name: "Remote folder",
        availability: "dataless",
        navigationBlocker: "dataless",
        canOpen: false,
      },
      sample.entries[0],
    ],
  });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(
    screen.queryByRole("button", { name: "Open folder Remote folder" }),
  ).not.toBeInTheDocument();
  expect(
    screen.getByText(/Dataless directory: enumeration is blocked/),
  ).toBeVisible();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect Remote folder" }),
  );
  let evidence = screen.getByRole("region", { name: "Entry evidence" });
  expect(within(evidence).getByText(/Known dataless object/)).toBeVisible();
  expect(within(evidence).getByText(/Cleanup blocked/)).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Return to entry list" }));
  fireEvent.click(screen.getByRole("button", { name: "Inspect Alpha" }));
  evidence = screen.getByRole("region", { name: "Entry evidence" });
  expect(
    within(evidence).getByText(
      /Unknown; local content availability has not been established/,
    ),
  ).toBeVisible();
  expect(invoke).toHaveBeenCalledTimes(1);
});

test("clear releases the current scope only after exact backend success and returns focus to selection", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce(undefined);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(screen.getByRole("button", { name: "Clear folder scope" }));
  await screen.findByText(/Folder scope cleared/);
  expect(invoke).toHaveBeenNthCalledWith(2, "clear_folder_inventory", {
    request: { expectedObservationId: "observation-1" },
  });
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(screen.queryByText("Zeta")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Choose folder" })).toHaveFocus();
  expect(screen.getByText(/Folder scope cleared/)).toHaveTextContent(
    "Saved plan and review metadata remain",
  );
});

test("failed scope clear keeps the observation and does not claim release", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockRejectedValueOnce(new Error("stale private/path"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(screen.getByRole("button", { name: "Clear folder scope" }));
  await screen.findByText(/folder scope could not be cleared/);
  expect(screen.getByText("Zeta")).toBeVisible();
  expect(screen.getByRole("button", { name: "Next page" })).toBeEnabled();
  expect(screen.queryByText(/Folder scope cleared/)).not.toBeInTheDocument();
  expect(screen.queryByText(/private\/path/)).not.toBeInTheDocument();
});

test("subtree summary is explicit, keeps exact totals and leaves page and import scope unchanged", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce({
      observationId: sample.observationId,
      rootLabel: sample.rootLabel,
      entriesObserved: 8,
      filesObserved: 5,
      uniqueFileObjects: 4,
      duplicateHardlinkNames: 1,
      directoriesVisited: 2,
      skippedEntries: 1,
      excludedDirectories: 2,
      unreadableEntries: 0,
      changedDirectories: 0,
      uniqueFileLogicalBytes: "90071992547409930",
      coverageComplete: false,
      stopReason: "depth_limit",
    })
    .mockResolvedValueOnce({
      ...sample,
      observationId: "page-2",
      pageNumber: 2,
    });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(invoke).toHaveBeenCalledTimes(1);
  const rowsBefore = screen.getByRole("table").textContent;
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  const summary = await screen.findByRole("region", {
    name: "Subtree metadata summary",
  });
  expect(invoke).toHaveBeenNthCalledWith(2, "start_subtree_scan", {
    request: {
      expectedObservationId: "observation-1",
      attemptId: expect.stringMatching(/^disk-steward:summary-attempt:v1:/),
    },
  });
  expect(summary).toHaveTextContent("90071992547409930");
  expect(summary).toHaveTextContent("Incomplete subtree coverage");
  expect(
    within(summary).getByText("Remembered excluded directories")
      .nextElementSibling,
  ).toHaveTextContent("2");
  expect(screen.getByRole("table").textContent).toBe(rowsBefore);
  expect(screen.getByText(/Size bars use/)).toHaveTextContent(
    "9007199254741000",
  );
  fireEvent.click(screen.getByRole("button", { name: "Next page" }));
  await waitFor(() =>
    expect(
      screen.queryByRole("region", { name: "Subtree metadata summary" }),
    ).not.toBeInTheDocument(),
  );
});

test("failed subtree summary sanitizes errors and preserves the current page", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockRejectedValueOnce(new Error("/private/secret"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  await screen.findByText(/subtree summary could not be completed reliably/);
  expect(screen.queryByText(/private\/secret/)).not.toBeInTheDocument();
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
  expect(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  ).toBeEnabled();
});

test("cancel binds the exact running attempt and keeps ownership until native completion", async () => {
  let rejectSummary: (error: Error) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectSummary = reject;
        }),
    )
    .mockResolvedValueOnce(undefined);
  const busy = vi.fn();
  render(<FolderInventoryPanel native onBusyChange={busy} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  const startRequest = vi.mocked(invoke).mock.calls[1][1] as {
    request: { expectedObservationId: string; attemptId: string };
  };
  fireEvent.click(
    screen.getByRole("button", { name: "Cancel subtree summary" }),
  );
  await screen.findByRole("button", { name: "Cancellation requested" });
  expect(invoke).toHaveBeenNthCalledWith(
    3,
    "cancel_folder_tree_summary",
    startRequest,
  );
  expect(
    screen.getByRole("button", { name: "Cancellation requested" }),
  ).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  ).toBeDisabled();
  expect(busy.mock.calls).toEqual([[true], [false], [true]]);
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
  rejectSummary(new Error("canceled /private/path"));
  await screen.findByText(
    /ended without a reliable result after cancellation was requested/,
  );
  expect(screen.queryByText(/private\/path/)).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Cancellation requested" }),
  ).not.toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  ).toBeEnabled();
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
});

test("unconfirmed cancellation does not pretend the summary stopped", async () => {
  let rejectSummary: (error: Error) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectSummary = reject;
        }),
    )
    .mockRejectedValueOnce(new Error("stale /private/path"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Cancel subtree summary" }),
  );
  await screen.findByText(/Cancellation could not be confirmed/);
  expect(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  ).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Cancel subtree summary" }),
  ).toBeEnabled();
  rejectSummary(new Error("failed"));
  await screen.findByText(/subtree summary could not be completed reliably/);
});

test("a late cancellation response cannot change a later summary attempt", async () => {
  let rejectFirst: (error: Error) => void = () => {};
  let rejectSecond: (error: Error) => void = () => {};
  let acknowledgeCancel: () => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectFirst = reject;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          acknowledgeCancel = resolve;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectSecond = reject;
        }),
    );
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Cancel subtree summary" }),
  );
  rejectFirst(new Error("finished"));
  await screen.findByText(/subtree summary could not be completed reliably/);
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  const requests = vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name === "start_subtree_scan");
  expect(requests[0][1]).not.toEqual(requests[1][1]);
  acknowledgeCancel();
  await waitFor(() =>
    expect(
      screen.queryByText(/Cancellation requested\. Waiting/),
    ).not.toBeInTheDocument(),
  );
  expect(
    screen.getByRole("button", { name: "Cancel subtree summary" }),
  ).toBeEnabled();
  rejectSecond(new Error("finished"));
  await screen.findByText(/subtree summary could not be completed reliably/);
});

test("refresh retries the exact current folder without a picker and restarts paging after failed continuation", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce({ ...sample, pageNumber: 2 })
    .mockRejectedValueOnce(new Error("directory changed"))
    .mockResolvedValueOnce({
      ...sample,
      observationId: "refreshed",
      pageNumber: 1,
      entries: [{ ...sample.entries[0], name: "Fresh name" }],
    });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(screen.getByRole("button", { name: "Next page" }));
  await screen.findByText(/Enumeration could not continue reliably/);
  expect(screen.getByRole("button", { name: "Next page" })).toBeDisabled();
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh current folder" }),
  );
  await screen.findByText(/Folder refreshed\. Paging restarted/);
  expect(invoke).toHaveBeenNthCalledWith(3, "refresh_folder_inventory", {
    request: { expectedObservationId: "observation-1" },
  });
  expect(screen.getByRole("table")).toHaveTextContent("Fresh name");
  expect(screen.getByRole("button", { name: "Next page" })).toBeEnabled();
  expect(
    vi
      .mocked(invoke)
      .mock.calls.filter(
        ([name]) => name === "inspect_selected_folder_entries",
      ),
  ).toHaveLength(1);
});

test("failed refresh preserves historical page, filter and selected evidence, with no duplicate request", async () => {
  let rejectRefresh: (error: Error) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectRefresh = reject;
        }),
    );
  const busy = vi.fn();
  render(<FolderInventoryPanel native onBusyChange={busy} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "Alpha" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Inspect Alpha" }));
  const refresh = screen.getByRole("button", {
    name: "Refresh current folder",
  });
  fireEvent.click(refresh);
  fireEvent.click(refresh);
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(refresh).toBeDisabled();
  rejectRefresh(new Error("private path"));
  await screen.findByText(/current folder could not be refreshed reliably/);
  expect(screen.getByRole("searchbox")).toHaveValue("Alpha");
  expect(
    screen.getByRole("region", { name: "Entry evidence" }),
  ).toHaveTextContent("Alpha");
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
  expect(screen.queryByText(/private path/)).not.toBeInTheDocument();
  expect(refresh).toBeEnabled();
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
});

const savedCheckpoint = {
  checkpointId: "checkpoint-1",
  rootLabel: "Chosen folder",
  pageNumber: 1,
  entriesSeen: "3",
};

test("checkpoint save is explicit, compares existing revision and leaves the current page unchanged", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce(savedCheckpoint)
    .mockResolvedValueOnce({
      ...savedCheckpoint,
      checkpointId: "checkpoint-2",
    });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  expect(invoke).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(invoke).toHaveBeenCalledTimes(1);
  const before = screen.getByRole("table").textContent;
  fireEvent.click(
    screen.getByRole("button", { name: "Save folder checkpoint" }),
  );
  await screen.findByText(/Folder checkpoint saved locally/);
  expect(invoke).toHaveBeenNthCalledWith(2, "get_folder_checkpoint");
  expect(invoke).toHaveBeenNthCalledWith(3, "save_folder_checkpoint", {
    request: {
      expectedObservationId: "observation-1",
      expectedCheckpointId: "checkpoint-1",
    },
  });
  expect(screen.getByRole("table").textContent).toBe(before);
  expect(
    screen.getByRole("region", { name: "Saved folder checkpoint" }),
  ).toHaveTextContent("page 1");
});

test("resume loads only the typed checkpoint and updates scope only after native success", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(savedCheckpoint)
    .mockResolvedValueOnce({
      ...sample,
      observationId: "resumed",
      pageNumber: 2,
    });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(
    screen.getByRole("button", { name: "Resume saved folder scan" }),
  );
  await screen.findByText(/Resumed at the page after the saved checkpoint/);
  expect(invoke).toHaveBeenNthCalledWith(2, "resume_folder_checkpoint", {
    request: { expectedCheckpointId: "checkpoint-1" },
  });
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
  expect(screen.getByRole("heading", { name: "Chosen folder" })).toHaveFocus();
});

test("canceled resume and failed checkpoint operations preserve historical page and saved checkpoint", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce(savedCheckpoint)
    .mockResolvedValueOnce(null)
    .mockRejectedValueOnce(new Error("private checkpoint path"));
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.change(screen.getByRole("searchbox"), {
    target: { value: "Alpha" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Resume saved folder scan" }),
  );
  await screen.findByText(/Resume selection canceled/);
  expect(screen.getByRole("searchbox")).toHaveValue("Alpha");
  fireEvent.click(
    screen.getByRole("button", { name: "Forget saved checkpoint" }),
  );
  await screen.findByText(
    /checkpoint operation could not be verified reliably/,
  );
  expect(invoke).toHaveBeenLastCalledWith("clear_folder_checkpoint", {
    request: { expectedCheckpointId: "checkpoint-1" },
  });
  expect(
    screen.getByRole("region", { name: "Saved folder checkpoint" }),
  ).toBeVisible();
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
  expect(screen.queryByText(/private checkpoint path/)).not.toBeInTheDocument();
});

test("successful forgetting clears only checkpoint UI while completed pages cannot be saved", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce({ ...sample, canContinue: false })
    .mockResolvedValueOnce(savedCheckpoint)
    .mockResolvedValueOnce(null)
    .mockResolvedValueOnce(undefined);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  expect(
    screen.getByRole("button", { name: "Save folder checkpoint" }),
  ).toBeDisabled();
  fireEvent.click(
    screen.getByRole("button", { name: "Resume saved folder scan" }),
  );
  await screen.findByText(/Resume selection canceled/);
  fireEvent.click(
    screen.getByRole("button", { name: "Forget saved checkpoint" }),
  );
  await screen.findByText(/Saved checkpoint forgotten/);
  expect(
    screen.queryByRole("region", { name: "Saved folder checkpoint" }),
  ).not.toBeInTheDocument();
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
});

test("checkpoint tracking limits disable saving without blocking folder paging", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce({ ...sample, canCheckpoint: false })
    .mockResolvedValueOnce({
      ...sample,
      observationId: "next-page",
      pageNumber: 2,
      canCheckpoint: false,
    });
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  const save = screen.getByRole("button", { name: "Save folder checkpoint" });
  expect(save).toBeDisabled();
  expect(
    screen.getByText(/Checkpoint tracking is unavailable/),
  ).toHaveTextContent("You can continue paging");
  fireEvent.click(save);
  expect(invoke).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Next page" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Next page" })).toBeEnabled(),
  );
  expect(invoke).toHaveBeenLastCalledWith("continue_folder_inventory", {
    request: { expectedObservationId: "observation-1" },
  });
  expect(save).toBeDisabled();
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
});

test("saved checkpoint inspection offers metadata and forgetting without native source access", async () => {
  vi.mocked(invoke).mockResolvedValueOnce(savedCheckpoint);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect saved checkpoint" }),
  );
  await screen.findByText(
    /Saved checkpoint inspected\. No folder access was requested/,
  );
  expect(invoke).toHaveBeenCalledExactlyOnceWith("get_folder_checkpoint");
  expect(
    screen.getByRole("region", { name: "Saved folder checkpoint" }),
  ).toHaveTextContent("Chosen folder");
  expect(
    screen.getByRole("button", { name: "Forget saved checkpoint" }),
  ).toBeEnabled();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
});

const chunkProgress = {
  observationId: sample.observationId,
  rootLabel: sample.rootLabel,
  entriesObserved: 512,
  filesObserved: 500,
  uniqueFileObjects: 500,
  duplicateHardlinkNames: 0,
  directoriesVisited: 2,
  skippedEntries: 0,
  excludedDirectories: 0,
  unreadableEntries: 0,
  changedDirectories: 0,
  uniqueFileLogicalBytes: "500",
  coverageComplete: false,
  stopReason: null,
  scanId: "scan-1",
  canContinue: true,
  scanFinished: false,
  entriesThisBatch: 512,
};

test("subtree scan advances bounded batches with fresh attempts and preserves the folder page", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockResolvedValueOnce(chunkProgress)
    .mockResolvedValueOnce({
      ...chunkProgress,
      entriesObserved: 1024,
      entriesThisBatch: 512,
      canContinue: false,
      scanFinished: true,
      coverageComplete: true,
    });
  const busy = vi.fn();
  render(<FolderInventoryPanel native onBusyChange={busy} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  const before = screen.getByRole("table").textContent;
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  await screen.findByText(/Traversal finished within its policy/);
  expect(invoke).toHaveBeenNthCalledWith(3, "continue_subtree_scan", {
    request: {
      expectedObservationId: "observation-1",
      scanId: "scan-1",
      attemptId: expect.stringMatching(/^disk-steward:summary-attempt:v1:/),
    },
  });
  const start = vi.mocked(invoke).mock.calls[1][1] as {
    request: { attemptId: string };
  };
  const next = vi.mocked(invoke).mock.calls[2][1] as {
    request: { attemptId: string };
  };
  expect(next.request.attemptId).not.toBe(start.request.attemptId);
  expect(screen.getByRole("table").textContent).toBe(before);
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
});

test("pause waits for a completed batch then resumes the same retained scan", async () => {
  let finishBatch: (value: typeof chunkProgress) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishBatch = resolve;
        }),
    )
    .mockResolvedValueOnce({
      ...chunkProgress,
      canContinue: false,
      scanFinished: true,
    });
  const busy = vi.fn();
  render(<FolderInventoryPanel native onBusyChange={busy} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Pause subtree scan" }));
  expect(
    screen.getByRole("button", { name: "Pause requested" }),
  ).toBeDisabled();
  expect(busy.mock.calls).toEqual([[true], [false], [true]]);
  finishBatch(chunkProgress);
  await screen.findByText(/paused at a batch boundary/);
  expect(invoke).toHaveBeenCalledTimes(2);
  fireEvent.click(screen.getByRole("button", { name: "Resume subtree scan" }));
  await screen.findByText(/Traversal finished within its policy/);
  expect(invoke).toHaveBeenLastCalledWith("continue_subtree_scan", {
    request: {
      expectedObservationId: "observation-1",
      scanId: "scan-1",
      attemptId: expect.any(String),
    },
  });
});

test("discard uses exact scan references and reports release only on native success", async () => {
  let finishBatch: (value: typeof chunkProgress) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(sample)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishBatch = resolve;
        }),
    )
    .mockRejectedValueOnce(new Error("stale /private/path"))
    .mockResolvedValueOnce(undefined);
  render(<FolderInventoryPanel native onBusyChange={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));
  await screen.findByRole("heading", { name: "Chosen folder" });
  fireEvent.click(
    screen.getByRole("button", { name: "Summarize subtree metadata" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Pause subtree scan" }));
  finishBatch(chunkProgress);
  await screen.findByText(/paused at a batch boundary/);
  fireEvent.click(
    screen.getByRole("button", { name: "Discard subtree scan state" }),
  );
  await screen.findByText(/release could not be confirmed/);
  expect(
    screen.getByRole("button", { name: "Resume subtree scan" }),
  ).toBeEnabled();
  fireEvent.click(
    screen.getByRole("button", { name: "Discard subtree scan state" }),
  );
  await screen.findByText(/Scan state released/);
  expect(invoke).toHaveBeenLastCalledWith("discard_subtree_scan", {
    request: { expectedObservationId: "observation-1", scanId: "scan-1" },
  });
  expect(
    screen.queryByRole("button", { name: "Resume subtree scan" }),
  ).not.toBeInTheDocument();
  expect(screen.queryByText(/private\/path/)).not.toBeInTheDocument();
  expect(screen.getByRole("table")).toHaveTextContent("Alpha");
});
