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
import DuplicateComparisonPanel from "./DuplicateComparisonPanel";
import type { FolderInventory } from "./FolderInventoryPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function makeSnapshot(count = 4): FolderInventory {
  return {
    rootLabel: "Chosen folder",
    pageNumber: 1,
    entriesSeen: String(count),
    canContinue: false,
    canCheckpoint: false,
    observationId: "observation-1",
    breadcrumbs: ["Chosen folder"],
    canGoBack: false,
    enumerationComplete: true,
    stopReason: null,
    unreadableCount: 0,
    directoriesNotTraversed: 0,
    regularFileLogicalBytes: "16384",
    entries: Array.from({ length: count }, (_, index) => ({
      id: `entry-${index + 1}`,
      name: `file-${index + 1}.bin`,
      kind: "file",
      availability: "unknown" as const,
      navigationBlocker: null,
      logicalBytes: "4096",
      allocatedBytes: "4096",
      linkCount: "1",
      hardLinked: false,
      canOpen: false,
    })),
  };
}

const validResult = {
  observationId: "observation-1",
  comparisonId: "comparison-1",
  detectorVersion: "exact-regular-file-v1",
  filesCompared: 2,
  bytesRead: "8192",
  groups: [
    { id: "group-1", entryIds: ["entry-1", "entry-2"], logicalBytes: "4096" },
  ],
};

function selectTwoAndConsent() {
  const files = within(
    screen.getByRole("list", { name: "Regular files on the current page" }),
  );
  fireEvent.click(files.getByRole("checkbox", { name: /file-1\.bin/ }));
  fireEvent.click(files.getByRole("checkbox", { name: /file-2\.bin/ }));
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Allow local content reads for these selected files",
    }),
  );
}

beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);

test("does not compare on mount; explicit selection and per-run consent gate the exact native request", async () => {
  const busy = vi.fn();
  render(
    <DuplicateComparisonPanel
      snapshot={makeSnapshot()}
      native
      busy={false}
      onBusyChange={busy}
    />,
  );
  expect(invoke).not.toHaveBeenCalled();
  expect(
    screen.getByRole("button", { name: "Compare selected files" }),
  ).toBeDisabled();
  selectTwoAndConsent();
  vi.mocked(invoke).mockResolvedValueOnce(validResult);
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByRole("region", { name: "Exact comparison result" });
  const request = vi.mocked(invoke).mock.calls[0][1] as {
    request: Record<string, unknown>;
  };
  expect(vi.mocked(invoke).mock.calls[0][0]).toBe("compare_observed_files");
  expect(request.request).toMatchObject({
    expectedObservationId: "observation-1",
    entryIds: ["entry-1", "entry-2"],
    attemptId: expect.stringMatching(/^disk-steward:summary-attempt:v1:/),
    allowContentRead: true,
  });
  expect(request.request).not.toHaveProperty("path");
  expect(screen.getByText(/Matching bytes: 4,096 bytes/)).toBeVisible();
  expect(screen.getByText(/file contents stay unchanged/)).toBeVisible();
  expect(busy.mock.calls).toEqual([[true], [false]]);
});

test("selection changes clear consent and a prior successful result becomes historical", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockRejectedValueOnce(new Error("private path"));
  render(
    <DuplicateComparisonPanel
      snapshot={makeSnapshot()}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByRole("region", { name: "Exact comparison result" });

  const files = within(
    screen.getByRole("list", { name: "Regular files on the current page" }),
  );
  fireEvent.click(files.getByRole("checkbox", { name: /file-3\.bin/ }));
  expect(
    screen.getByRole("checkbox", {
      name: "Allow local content reads for these selected files",
    }),
  ).not.toBeChecked();
  expect(
    screen.getByRole("heading", {
      name: /Exact comparison result · historical/,
    }),
  ).toBeVisible();
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Allow local content reads for these selected files",
    }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByText(/comparison could not be verified/);
  expect(
    screen.getByRole("heading", {
      name: /Exact comparison result · historical/,
    }),
  ).toBeVisible();
  expect(screen.queryByText(/private path/)).not.toBeInTheDocument();
});

test("cancellation sends the exact active pair and keeps busy until native comparison settles", async () => {
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
    <DuplicateComparisonPanel
      snapshot={makeSnapshot()}
      native
      busy={false}
      onBusyChange={busy}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByRole("button", { name: "Cancel comparison" });
  fireEvent.click(screen.getByRole("button", { name: "Cancel comparison" }));
  await screen.findByText(/Cancellation signal acknowledged/);
  expect(vi.mocked(invoke).mock.calls[1]).toEqual([
    "cancel_duplicate_comparison",
    {
      request: {
        expectedObservationId: "observation-1",
        attemptId: expect.stringMatching(/^disk-steward:summary-attempt:v1:/),
      },
    },
  ]);
  expect(
    screen
      .getByRole("region", { name: "Compare selected files for exact matches" })
      .getAttribute("aria-busy"),
  ).toBe("true");
  expect(busy.mock.calls).toEqual([[true]]);
  finish(validResult);
  await screen.findByRole("region", { name: "Exact comparison result" });
  await waitFor(() => expect(busy.mock.calls).toEqual([[true], [false]]));
});

test("malformed group evidence is sanitized and cannot replace an older result", async () => {
  const malformed = {
    ...validResult,
    comparisonId: "bad-comparison",
    groups: [
      {
        id: "g",
        entryIds: ["entry-1", "outside-selection"],
        logicalBytes: "4096",
      },
    ],
  };
  vi.mocked(invoke)
    .mockResolvedValueOnce(validResult)
    .mockResolvedValueOnce(malformed);
  render(
    <DuplicateComparisonPanel
      snapshot={makeSnapshot()}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByRole("region", { name: "Exact comparison result" });
  const files = within(
    screen.getByRole("list", { name: "Regular files on the current page" }),
  );
  fireEvent.click(files.getByRole("checkbox", { name: /file-3\.bin/ }));
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Allow local content reads for these selected files",
    }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByText(/comparison could not be verified/);
  expect(
    screen.getByRole("region", { name: "Exact comparison result" }),
  ).toHaveTextContent("comparison-1");
  expect(screen.queryByText("bad-comparison")).not.toBeInTheDocument();
});

test("reports no matching groups only for the explicitly selected files", async () => {
  vi.mocked(invoke).mockResolvedValueOnce({ ...validResult, groups: [] });
  render(
    <DuplicateComparisonPanel
      snapshot={makeSnapshot()}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  selectTwoAndConsent();
  fireEvent.click(
    screen.getByRole("button", { name: "Compare selected files" }),
  );
  await screen.findByText(
    /No matching groups were found among these 2 explicitly selected files/,
  );
  expect(
    screen.getByText(
      /No matching groups among the selected files for that comparison/,
    ),
  ).toBeVisible();
  expect(screen.queryByText(/folder is clean/i)).not.toBeInTheDocument();
});

test("caps explicit selection at 32 files and disables ineligible files", () => {
  const snapshot = makeSnapshot(36);
  snapshot.entries[0].hardLinked = true;
  snapshot.entries[1].name = ".hidden.bin";
  snapshot.entries[2].logicalBytes = (16 * 1024 * 1024 + 1).toString();
  render(
    <DuplicateComparisonPanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  const files = within(
    screen.getByRole("list", { name: "Regular files on the current page" }),
  );
  expect(files.getByRole("checkbox", { name: /file-1\.bin/ })).toBeDisabled();
  expect(files.getByRole("checkbox", { name: /\.hidden\.bin/ })).toBeDisabled();
  expect(files.getByRole("checkbox", { name: /file-3\.bin/ })).toBeDisabled();
  for (let index = 4; index <= 35; index += 1) {
    fireEvent.click(
      files.getByRole("checkbox", { name: new RegExp(`file-${index}\\.bin`) }),
    );
  }
  expect(screen.getByText(/^32 selected/)).toBeVisible();
  expect(files.getByRole("checkbox", { name: /file-36\.bin/ })).toBeDisabled();
});

test("enforces the aggregate 64 MiB logical-input limit before selection", () => {
  const snapshot = makeSnapshot(5);
  snapshot.entries.forEach((entry) => {
    entry.logicalBytes = (16 * 1024 * 1024).toString();
  });
  render(
    <DuplicateComparisonPanel
      snapshot={snapshot}
      native
      busy={false}
      onBusyChange={vi.fn()}
    />,
  );
  const files = within(
    screen.getByRole("list", { name: "Regular files on the current page" }),
  );
  for (let index = 1; index <= 4; index += 1) {
    fireEvent.click(
      files.getByRole("checkbox", { name: new RegExp(`file-${index}\\.bin`) }),
    );
  }
  expect(screen.getByText(/^4 selected · 67,108,864 bytes/)).toBeVisible();
  expect(files.getByRole("checkbox", { name: /file-5\.bin/ })).toBeDisabled();
  expect(invoke).not.toHaveBeenCalled();
});
