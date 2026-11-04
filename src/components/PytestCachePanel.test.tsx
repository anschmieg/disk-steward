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
  detectorVersion: "pytest-cache-format-v1",
  referenceVersion: "9.2.0",
  bytesRead: "200",
  records: [
    { entryId: "cache-nodeids", kind: "node_ids", recordCount: 2 },
    { entryId: "cache-lastfailed", kind: "last_failed", recordCount: 1 },
  ],
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

test("unverified format versions or oversized read counts cannot replace the result", async () => {
  const oversized = { ...validResult, bytesRead: "131075" };
  vi.mocked(invoke)
    .mockResolvedValueOnce({ ...validResult, detectorVersion: "unknown" })
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
