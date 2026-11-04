import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type { EditorSnapshot } from "../general-plan-contract";
import PytestCachePanel from "./PytestCachePanel";
import type { FolderInventory } from "./FolderInventoryPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const snapshot: FolderInventory = {
  rootLabel: "v/cache",
  pageNumber: 1,
  entriesSeen: "4",
  canContinue: false,
  canCheckpoint: false,
  observationId: "observation-pytest-1",
  breadcrumbs: ["Project", ".pytest_cache", "v", "cache"],
  canGoBack: true,
  enumerationComplete: true,
  stopReason: null,
  unreadableCount: 0,
  directoriesNotTraversed: 0,
  regularFileLogicalBytes: "200",
  entries: [
    {
      id: "cache-nodeids",
      name: "nodeids",
      kind: "file",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: "100",
      allocatedBytes: "4096",
      linkCount: "1",
      hardLinked: false,
      canOpen: false,
    },
    {
      id: "cache-lastfailed",
      name: "lastfailed",
      kind: "file",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: "100",
      allocatedBytes: "4096",
      linkCount: "1",
      hardLinked: false,
      canOpen: false,
    },
    {
      id: "folder-nodeids",
      name: "nodeids",
      kind: "folder",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: null,
      allocatedBytes: null,
      linkCount: null,
      hardLinked: false,
      canOpen: true,
    },
    {
      id: "too-large",
      name: "lastfailed",
      kind: "file",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: "65537",
      allocatedBytes: "65536",
      linkCount: "1",
      hardLinked: false,
      canOpen: false,
    },
    {
      id: "linked-nodeids",
      name: "nodeids",
      kind: "file",
      availability: "unknown",
      navigationBlocker: null,
      logicalBytes: "100",
      allocatedBytes: "4096",
      linkCount: "2",
      hardLinked: true,
      canOpen: false,
    },
    {
      id: "dataless-lastfailed",
      name: "lastfailed",
      kind: "file",
      availability: "dataless",
      navigationBlocker: null,
      logicalBytes: "100",
      allocatedBytes: "4096",
      linkCount: "1",
      hardLinked: false,
      canOpen: false,
    },
  ],
};

const validResult = {
  observationId: "observation-pytest-1",
  inspectionId:
    "disk-steward:inventory-item:v1:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  detectorVersion: "pytest-cache-format-v1",
  referenceVersion: "9.2.0",
  bytesRead: "200",
  records: [
    { entryId: "cache-nodeids", kind: "node_ids", recordCount: 2 },
    { entryId: "cache-lastfailed", kind: "last_failed", recordCount: 1 },
  ],
};

const planSnapshot: EditorSnapshot = {
  schemaVersion: 1,
  kind: "non_executable_editor",
  executionAvailable: false,
  planId: "plan-local",
  revisionId: "revision-7",
  revisionNumber: 7,
  items: [],
  groups: [],
  duplicates: [],
  totals: {
    selectedCount: 0,
    logicalSelectedBytes: 0,
    inPlanLogicalBytes: 0,
    recoveryLowerBytes: 0,
    recoveryUpperBytes: null,
  },
  canUndo: false,
  canRedo: false,
};

function selectTwoAndConsent() {
  const list = within(
    screen.getByRole("list", { name: "Pytest cache record candidates" }),
  );
  fireEvent.click(list.getAllByRole("checkbox", { name: /nodeids/ })[0]);
  fireEvent.click(list.getAllByRole("checkbox", { name: /lastfailed/ })[0]);
  fireEvent.click(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  );
}

beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);

test("does not inspect automatically; requires explicit selection and per-attempt consent", async () => {
  vi.mocked(invoke).mockResolvedValueOnce(validResult);
  const busy = vi.fn();
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={busy}
    />,
  );
  expect(invoke).not.toHaveBeenCalled();
  expect(
    screen.getByRole("heading", {
      name: "Inspect selected pytest cache records",
    }),
  ).toBeVisible();
  expect(
    screen.getByRole("button", { name: "Inspect selected records" }),
  ).toBeDisabled();
  const list = within(
    screen.getByRole("list", { name: "Pytest cache record candidates" }),
  );
  expect(list.getAllByRole("checkbox", { name: /nodeids/ })[0]).toBeEnabled();
  expect(
    list.getAllByRole("checkbox", { name: /lastfailed/ })[0],
  ).toBeEnabled();
  expect(list.getAllByRole("checkbox", { name: /nodeids/ })[1]).toBeDisabled();
  expect(list.getAllByRole("checkbox", { name: /nodeids/ })[2]).toBeDisabled();
  expect(
    list.getAllByRole("checkbox", { name: /lastfailed/ })[1],
  ).toBeDisabled();
  expect(
    list.getAllByRole("checkbox", { name: /lastfailed/ })[2],
  ).toBeDisabled();

  fireEvent.click(list.getAllByRole("checkbox", { name: /nodeids/ })[0]);
  fireEvent.click(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  );
  fireEvent.click(list.getAllByRole("checkbox", { name: /lastfailed/ })[0]);
  expect(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  ).not.toBeChecked();
  expect(invoke).not.toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  expect(invoke).toHaveBeenCalledWith("inspect_pytest_cache_records", {
    request: {
      expectedObservationId: "observation-pytest-1",
      entryIds: ["cache-nodeids", "cache-lastfailed"],
      attemptId: expect.stringMatching(/^disk-steward:summary-attempt:v1:/),
      allowContentRead: true,
    },
  });
  expect(screen.getByText(/node IDs · 2 records/)).toBeVisible();
  expect(screen.getByText(/last failed tests · 1 records/)).toBeVisible();
  expect(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  ).not.toBeChecked();
  expect(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  ).toBeEnabled();
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(busy.mock.calls).toEqual([[true], [false]]);
});

test("cancellation uses the exact active observation and attempt and keeps busy until inspection settles", async () => {
  let finish: (value: unknown) => void = () => {};
  vi.mocked(invoke)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce(undefined);
  const busy = vi.fn();
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={busy}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("button", { name: "Cancel inspection" });
  fireEvent.click(screen.getByRole("button", { name: "Cancel inspection" }));
  await screen.findByText(/Cancellation signal acknowledged/);
  expect(vi.mocked(invoke).mock.calls[1]).toEqual([
    "cancel_pytest_cache_inspection",
    {
      request: {
        expectedObservationId: "observation-pytest-1",
        attemptId: expect.stringMatching(/^disk-steward:summary-attempt:v1:/),
      },
    },
  ]);
  expect(busy.mock.calls).toEqual([[true]]);
  expect(
    screen.getByRole("region", {
      name: "Inspect selected pytest cache records",
    }),
  ).toHaveAttribute("aria-busy", "true");
  finish(validResult);
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  await waitFor(() => expect(busy.mock.calls).toEqual([[true], [false]]));
});

test("invalid results are sanitized and preserve a prior successful result as historical", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockResolvedValueOnce({
      ...validResult,
      bytesRead: "100",
      records: [{ entryId: "outside", kind: "node_ids", recordCount: 2 }],
    });
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  const list = within(
    screen.getByRole("list", { name: "Pytest cache record candidates" }),
  );
  fireEvent.click(list.getAllByRole("checkbox", { name: /lastfailed/ })[0]);
  expect(
    screen.getByRole("heading", {
      name: /Cache records inspected · historical/,
    }),
  ).toBeVisible();
  fireEvent.click(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByText(/inspection could not be verified/);
  expect(
    screen.getByRole("region", { name: "Pytest cache inspection result" }),
  ).toHaveTextContent("nodeids");
  expect(
    screen.getByRole("heading", {
      name: /Cache records inspected · historical/,
    }),
  ).toBeVisible();
  expect(screen.queryByText(/outside/)).not.toBeInTheDocument();
});

test("malformed inspection IDs or oversized read counts cannot replace the result", async () => {
  const oversized = { ...validResult, bytesRead: "131075" };
  vi.mocked(invoke)
    .mockResolvedValueOnce({
      ...validResult,
      inspectionId: "not-a-typed-inspection-id",
    })
    .mockResolvedValueOnce(oversized);
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByText(/inspection could not be verified/);
  expect(
    screen.queryByRole("region", { name: "Pytest cache inspection result" }),
  ).not.toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByText(/inspection could not be verified/);
  expect(screen.queryByText(/131075/)).not.toBeInTheDocument();
});

test("imports only the latest inspection ID and live plan revision", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockResolvedValueOnce(planSnapshot)
    .mockResolvedValueOnce({
      ...planSnapshot,
      revisionId: "revision-8",
      revisionNumber: 8,
    });
  const openPlan = vi.fn();
  const busy = vi.fn();
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={busy}
      onOpenPlan={openPlan}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  ).toBeEnabled();
  fireEvent.click(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  );
  await screen.findByText(/records were saved to the read-only plan/);
  expect(invoke).toHaveBeenNthCalledWith(2, "get_general_plan", {
    request: { profile: "live" },
  });
  expect(invoke).toHaveBeenNthCalledWith(3, "import_pytest_review_items", {
    request: {
      expectedObservationId: "observation-pytest-1",
      inspectionId: validResult.inspectionId,
      expectedRevisionId: "revision-7",
    },
  });
  const importRequest = vi.mocked(invoke).mock.calls[2][1] as {
    request: Record<string, unknown>;
  };
  expect(Object.keys(importRequest.request).sort()).toEqual([
    "expectedObservationId",
    "expectedRevisionId",
    "inspectionId",
  ]);
  expect(importRequest.request).not.toHaveProperty("records");
  expect(importRequest.request).not.toHaveProperty("entryIds");
  expect(importRequest.request).not.toHaveProperty("path");
  expect(
    screen.getByRole("button", { name: "Cache records added to plan" }),
  ).toBeDisabled();
  fireEvent.click(
    screen.getByRole("button", { name: "Review cache records in plan" }),
  );
  expect(openPlan).toHaveBeenCalledOnce();
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
});

test("keeps parent busy through revision read and import and blocks duplicate clicks", async () => {
  let finishPlan: (value: EditorSnapshot) => void = () => {};
  let finishImport: (value: EditorSnapshot) => void = () => {};
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishPlan = resolve;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishImport = resolve;
        }),
    );
  const busy = vi.fn();
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={busy}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  fireEvent.click(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  );
  await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
  expect(busy.mock.calls).toEqual([[true], [false], [true]]);
  expect(
    screen.getByRole("region", {
      name: "Inspect selected pytest cache records",
    }),
  ).toHaveAttribute("aria-busy", "true");
  expect(
    screen.getByRole("button", { name: "Adding cache records…" }),
  ).toBeDisabled();
  finishPlan(planSnapshot);
  await waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
  expect(busy.mock.calls).toEqual([[true], [false], [true]]);
  finishImport(planSnapshot);
  await screen.findByText(/records were saved to the read-only plan/);
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
  expect(
    screen.getByRole("region", {
      name: "Inspect selected pytest cache records",
    }),
  ).toHaveAttribute("aria-busy", "false");
  expect(invoke).toHaveBeenCalledTimes(3);
});

test("failed import preserves current result, releases busy, and allows only an explicit retry", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockResolvedValueOnce(planSnapshot)
    .mockRejectedValueOnce(new Error("private path"));
  const busy = vi.fn();
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={busy}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  fireEvent.click(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  );
  await screen.findByText(
    /open your plan to check its latest state before trying again/,
  );
  expect(
    screen.getByRole("region", { name: "Pytest cache inspection result" }),
  ).toHaveTextContent("nodeids");
  expect(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  ).toBeEnabled();
  expect(screen.queryByText(/private path/)).not.toBeInTheDocument();
  expect(invoke).toHaveBeenCalledTimes(3);
  expect(busy.mock.calls).toEqual([[true], [false], [true], [false]]);
});

test("historical results cannot be imported and a fresh inspection resets the import marker", async () => {
  const nextResult = {
    ...validResult,
    inspectionId:
      "disk-steward:inventory-item:v1:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    bytesRead: "100",
    records: [{ entryId: "cache-nodeids", kind: "node_ids", recordCount: 1 }],
  };
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockResolvedValueOnce(planSnapshot)
    .mockResolvedValueOnce(planSnapshot)
    .mockResolvedValueOnce(nextResult);
  render(
    <PytestCachePanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByRole("region", { name: "Pytest cache inspection result" });
  fireEvent.click(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  );
  await screen.findByText(/records were saved to the read-only plan/);
  const list = within(
    screen.getByRole("list", { name: "Pytest cache record candidates" }),
  );
  fireEvent.click(list.getAllByRole("checkbox", { name: /lastfailed/ })[0]);
  expect(
    screen.queryByRole("button", { name: /Add inspected records to plan/ }),
  ).not.toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("checkbox", { name: /Allow local content reads/ }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Inspect selected records" }),
  );
  await screen.findByText(/records were inspected locally/);
  expect(
    screen.getByRole("heading", { name: "Cache records inspected" }),
  ).toBeVisible();
  expect(
    screen.getByRole("button", { name: "Add inspected records to plan" }),
  ).toBeEnabled();
  expect(invoke).toHaveBeenCalledTimes(4);
});
