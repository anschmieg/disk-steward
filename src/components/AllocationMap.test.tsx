import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import AllocationMap, { type AllocationModel } from "./AllocationMap";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const model: AllocationModel = {
  knownAllocatedBytes: "1000",
  tiles: [
    { entryIds: ["a"], allocatedBytes: "700", category: "image" },
    { entryIds: ["b", "c"], allocatedBytes: "300", category: "document" },
  ],
  unknownAllocationEntries: 1,
  sharedLinkEntries: 1,
  zeroAllocationEntries: 2,
  unmappedNonFileEntries: 1,
  datalessEntries: 0,
};
const entries = [
  { id: "a", name: "photo.jpg" },
  { id: "b", name: "report.pdf" },
  { id: "c", name: "notes.txt" },
  ...Array.from({ length: 5 }, (_, n) => ({
    id: `omitted-${n}`,
    name: `Unmapped entry ${n}`,
  })),
];
const defaults = {
  allocation: model,
  entries,
  visibleIds: new Set(entries.map(({ id }) => id)),
  selectedId: null,
  busy: false,
  onSelect: vi.fn(),
  onGroup: vi.fn(),
};

test("selects an individual tile and the current filtered members of a group", () => {
  const onSelect = vi.fn();
  const onGroup = vi.fn();
  const { rerender } = render(
    <AllocationMap {...defaults} onSelect={onSelect} onGroup={onGroup} />,
  );
  const photo = screen.getByRole("button", { name: /photo\.jpg.*700 bytes/ });
  fireEvent.click(photo);
  expect(onSelect).toHaveBeenCalledWith("a", photo);
  const reportGroup = screen.getByRole("button", {
    name: /report\.pdf, notes\.txt.*300 bytes/,
  });
  fireEvent.click(reportGroup);
  expect(onGroup).toHaveBeenCalledWith(["b", "c"]);

  const filtered = new Set(["c"]);
  rerender(
    <AllocationMap
      {...defaults}
      visibleIds={filtered}
      onSelect={onSelect}
      onGroup={onGroup}
    />,
  );
  fireEvent.click(
    screen.getByRole("button", { name: /notes\.txt.*300 bytes/ }),
  );
  expect(onSelect).toHaveBeenLastCalledWith("c", expect.any(HTMLButtonElement));
  expect(onGroup).toHaveBeenCalledTimes(1);
});

test("filters dim and disable tiles without changing geometry or denominator", () => {
  const { rerender, container } = render(<AllocationMap {...defaults} />);
  const tile = () =>
    container.querySelector<HTMLButtonElement>(
      ".allocation-map__tile[aria-label*='photo.jpg']",
    )!;
  const before = tile();
  const beforeStyle = {
    left: before.style.left,
    top: before.style.top,
    width: before.style.width,
    height: before.style.height,
  };
  rerender(<AllocationMap {...defaults} visibleIds={new Set(["b"])} />);
  const after = tile();
  expect(after).toHaveClass("allocation-map__tile--dimmed");
  expect(after).toBeDisabled();
  expect({
    left: after.style.left,
    top: after.style.top,
    width: after.style.width,
    height: after.style.height,
  }).toEqual(beforeStyle);
  expect(
    screen.getByText(/1,000 bytes of known reported allocation/),
  ).toBeVisible();
});

test("shows separate exclusions and selected-member context without implying solo geometry", () => {
  render(<AllocationMap {...defaults} selectedId="b" />);
  expect(
    screen.getByText(/Selected: report\.pdf \(inside its mapped group\)/),
  ).toBeVisible();
  expect(screen.getAllByText(/Selected item is within this group/).length).toBe(
    1,
  );
  const counts = screen.getByRole("group", {
    name: "Proportional allocated space tiles",
  }).parentElement!;
  expect(
    within(counts).getByText("Shared-link entries").nextElementSibling,
  ).toHaveTextContent("1");
  expect(screen.getByText(/APFS clones remain uncertain/)).toBeVisible();
});

test("invalid evidence is unavailable and empty state reports zero without invented files", () => {
  const invalid = { ...model, knownAllocatedBytes: "999" };
  const { rerender } = render(
    <AllocationMap {...defaults} allocation={invalid} />,
  );
  expect(screen.getByText(/evidence could not be verified/)).toBeVisible();
  rerender(
    <AllocationMap
      {...defaults}
      allocation={{
        ...model,
        knownAllocatedBytes: "0",
        tiles: [],
        unknownAllocationEntries: 0,
        sharedLinkEntries: 0,
        zeroAllocationEntries: 0,
        unmappedNonFileEntries: 0,
        datalessEntries: 0,
      }}
      entries={[]}
      visibleIds={new Set()}
    />,
  );
  expect(
    screen.getByText(/0 bytes of known reported allocation/),
  ).toBeVisible();
  expect(screen.getByText(/Current page/)).toBeVisible();
  expect(screen.queryByText(/fallback|logical size/i)).not.toBeInTheDocument();
});

test("valid evidence arriving after unavailable evidence attaches size observation and restores usable tiles", () => {
  const observe = vi.fn();
  const disconnect = vi.fn();
  class Observer {
    constructor(private callback: ResizeObserverCallback) {}
    observe(target: Element) {
      observe(target);
      this.callback(
        [
          {
            target,
            contentRect: { width: 1000, height: 562.5 },
          } as ResizeObserverEntry,
        ],
        this as unknown as ResizeObserver,
      );
    }
    disconnect() {
      disconnect();
    }
    unobserve() {}
  }
  vi.stubGlobal("ResizeObserver", Observer);
  const { rerender } = render(
    <AllocationMap {...defaults} allocation={undefined} />,
  );
  expect(observe).not.toHaveBeenCalled();
  rerender(<AllocationMap {...defaults} />);
  expect(observe).toHaveBeenCalledTimes(1);
  const plot = screen.getByRole("group", {
    name: "Proportional allocated space tiles",
  });
  expect(
    within(plot).getByRole("button", { name: /photo.jpg.*700 bytes/ }),
  ).toBeEnabled();
  rerender(<AllocationMap {...defaults} allocation={undefined} />);
  expect(disconnect).toHaveBeenCalledTimes(1);
});

test("inconsistent omission counts cannot masquerade as a verified partition", () => {
  render(
    <AllocationMap
      {...defaults}
      allocation={{ ...model, sharedLinkEntries: 2 }}
    />,
  );
  expect(screen.getByText(/evidence could not be verified/)).toBeVisible();
});
