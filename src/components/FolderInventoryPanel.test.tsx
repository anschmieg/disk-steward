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
    },
    {
      id: "2",
      name: "Zeta",
      kind: "file",
      logicalBytes: "9007199254740993",
      hardLinked: false,
    },
    {
      id: "3",
      name: "Nested",
      kind: "folder",
      logicalBytes: null,
      hardLinked: false,
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
