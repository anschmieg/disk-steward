import { useEffect, useId, useMemo, useRef, useState } from "react";
import { buildAllocationLayout } from "../allocation-layout";
import type { AllocationInput, AllocationRect } from "../allocation-layout";
import "./AllocationMap.css";

export type AllocationModel = AllocationInput & {
  unknownAllocationEntries: number;
  sharedLinkEntries: number;
  zeroAllocationEntries: number;
  unmappedNonFileEntries: number;
  datalessEntries: number;
};

export type AllocationMapEntry = { id: string; name: string };

const categoryLabels: Record<string, string> = {
  image: "Images (.jpg, .png, .gif)",
  document: "Documents (.pdf, .docx, .txt)",
  archive: "Archives (.zip, .7z, .tar)",
  media: "Media (.mp3, .mp4, .mov)",
  code: "Code and data (.js, .py, .json)",
  folder: "Folders",
  other: "Other observed files",
  otherFile: "Other file names (.log, extensionless)",
};
const categoryClasses: Record<string, string> = {
  image: "allocation-map__tile--image",
  document: "allocation-map__tile--document",
  archive: "allocation-map__tile--archive",
  media: "allocation-map__tile--media",
  code: "allocation-map__tile--code",
  folder: "allocation-map__tile--folder",
  other: "allocation-map__tile--other",
  otherFile: "allocation-map__tile--other-file",
};

function formatBytes(decimal: string): string {
  return `${BigInt(decimal).toLocaleString()} bytes`;
}

function titleFor(
  tile: AllocationRect,
  entriesById: Map<string, AllocationMapEntry>,
  visibleIds?: ReadonlySet<string>,
): string {
  const category = tile.category
    ? (categoryLabels[tile.category] ?? tile.category)
    : "Unclassified observed files";
  const allNames = tile.entryIds.map(
    (id) => entriesById.get(id)?.name ?? "Observed entry",
  );
  const matchingCount = visibleIds
    ? tile.entryIds.filter((id) => visibleIds.has(id)).length
    : allNames.length;
  const description =
    allNames.length <= 3
      ? allNames.join(", ")
      : `${allNames.length} observed files`;
  const filteredNote =
    matchingCount < allNames.length
      ? `; ${matchingCount} of ${allNames.length} entries match filters; bytes describe the full unfiltered area`
      : "";
  return `${category}: ${description}; ${formatBytes(tile.allocatedBytes)} allocated${filteredNote}`;
}

function isRenderable(
  tile: AllocationRect,
  plotSize: { width: number; height: number },
): boolean {
  return (
    (tile.width * plotSize.width) / 1000 >= 108 &&
    (tile.height * plotSize.height) / 1000 >= 56
  );
}

export default function AllocationMap({
  allocation,
  entries,
  visibleIds,
  selectedId,
  busy,
  onSelect,
  onGroup,
}: {
  allocation: AllocationModel | undefined;
  entries: AllocationMapEntry[];
  visibleIds: ReadonlySet<string>;
  selectedId: string | null;
  busy: boolean;
  onSelect: (id: string, returnTarget: HTMLButtonElement) => void;
  onGroup: (ids: string[]) => void;
}) {
  const titleId = useId();
  const plotRef = useRef<HTMLDivElement>(null);
  const [plotSize, setPlotSize] = useState({ width: 0, height: 0 });
  const entriesById = useMemo(
    () => new Map(entries.map((entry) => [entry.id, entry])),
    [entries],
  );
  const layout = useMemo(() => {
    if (!allocation) return null;
    try {
      if (
        entries.length > 512 ||
        new Set(entries.map((entry) => entry.id)).size !== entries.length
      )
        return null;
      const built = buildAllocationLayout(allocation);
      if (
        built.tiles.some((tile) =>
          tile.entryIds.some((id) => !entriesById.has(id)),
        )
      )
        return null;
      return built;
    } catch {
      return null;
    }
  }, [allocation, entries, entriesById]);
  const countsValid =
    allocation &&
    [
      allocation.unknownAllocationEntries,
      allocation.sharedLinkEntries,
      allocation.zeroAllocationEntries,
      allocation.unmappedNonFileEntries,
      allocation.datalessEntries,
    ].every(
      (count) => Number.isSafeInteger(count) && count >= 0 && count <= 512,
    ) &&
    layout !== null &&
    layout.tiles.reduce((sum, tile) => sum + tile.entryIds.length, 0) +
      allocation.unknownAllocationEntries +
      allocation.sharedLinkEntries +
      allocation.zeroAllocationEntries +
      allocation.unmappedNonFileEntries +
      allocation.datalessEntries ===
      entries.length;

  const usable = Boolean(allocation && layout && countsValid);
  useEffect(() => {
    const plot = plotRef.current;
    if (!plot || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      setPlotSize({
        width: entry.contentRect.width,
        height: entry.contentRect.height,
      });
    });
    observer.observe(plot);
    return () => observer.disconnect();
  }, [usable]);
  const activate = (tile: AllocationRect, target: HTMLButtonElement) => {
    const ids = tile.entryIds.filter((id) => visibleIds.has(id));
    if (!ids.length || busy) return;
    if (ids.length === 1) onSelect(ids[0], target);
    else onGroup(ids);
  };

  if (!allocation || !layout || !countsValid) {
    return (
      <section className="allocation-map" aria-labelledby={titleId}>
        <h3 id={titleId}>Allocated space map</h3>
        <p role="status">
          Allocated-space map unavailable because its evidence could not be
          verified.
        </p>
      </section>
    );
  }

  const mappedIds = new Set(layout.tiles.flatMap((tile) => tile.entryIds));
  return (
    <section className="allocation-map" aria-labelledby={titleId}>
      <header className="allocation-map__header">
        <div>
          <h3 id={titleId}>Allocated space map</h3>
          <p>
            Current page · {formatBytes(layout.totalBytes)} of known reported
            allocation
          </p>
        </div>
        <span className="allocation-map__selection" aria-live="polite">
          {selectedId && mappedIds.has(selectedId)
            ? `Selected: ${entriesById.get(selectedId)?.name ?? "observed entry"} (${layout.tiles.find((tile) => tile.entryIds.includes(selectedId))?.entryIds.length === 1 ? "mapped tile" : "inside its mapped group"})`
            : "Select a map tile or list entry to inspect it"}
        </span>
      </header>
      <div
        ref={plotRef}
        className="allocation-map__plot"
        role="group"
        aria-label="Proportional allocated space tiles"
      >
        {layout.tiles.map((tile, index) => {
          const key = `${tile.entryIds.join(",")}-${index}`;
          const renderable = isRenderable(tile, plotSize);
          const matches = tile.entryIds.some((id) => visibleIds.has(id));
          const allMatch = tile.entryIds.every((id) => visibleIds.has(id));
          const isSelected =
            selectedId !== null && tile.entryIds.includes(selectedId);
          const style = {
            left: `${tile.x / 10}%`,
            top: `${tile.y / 10}%`,
            width: `${tile.width / 10}%`,
            height: `${tile.height / 10}%`,
          };
          return (
            <button
              key={key}
              type="button"
              className={`allocation-map__tile ${categoryClasses[tile.category ?? "other"] ?? "allocation-map__tile--other"}${matches && !allMatch ? " allocation-map__tile--partial" : matches ? "" : " allocation-map__tile--dimmed"}${isSelected ? " allocation-map__tile--selected" : ""}`}
              style={style}
              aria-label={titleFor(tile, entriesById, visibleIds)}
              aria-hidden={!renderable}
              aria-pressed={isSelected}
              disabled={busy || !matches || !renderable}
              tabIndex={renderable ? 0 : -1}
              onClick={(event) => activate(tile, event.currentTarget)}
            >
              {renderable && (
                <span className="allocation-map__tile-label">
                  <strong>
                    {tile.entryIds.length > 1
                      ? `${tile.entryIds.length} files`
                      : (entriesById.get(tile.entryIds[0])?.name ??
                        "Observed file")}
                  </strong>
                  <span>{formatBytes(tile.allocatedBytes)}</span>
                </span>
              )}
            </button>
          );
        })}
      </div>
      <ul
        className="allocation-map__legend"
        aria-label="File category color key"
      >
        {Object.entries(categoryLabels).map(([category, label]) => (
          <li key={category}>
            <span
              className={`allocation-map__swatch ${categoryClasses[category]}`}
              aria-hidden="true"
            />
            {label}
          </li>
        ))}
        <li>
          <span
            className="allocation-map__swatch allocation-map__tile--other"
            aria-hidden="true"
          />
          Unclassified observed files
        </li>
      </ul>
      <h4>Mapped groups and small tiles</h4>
      <ul
        className="allocation-map__accessible-list"
        aria-label="Mapped files and groups"
      >
        {layout.tiles.map((tile, index) => {
          const key = `${tile.entryIds.join(",")}-${index}`;
          const matches = tile.entryIds.filter((id) => visibleIds.has(id));
          const isSelected =
            selectedId !== null && tile.entryIds.includes(selectedId);
          return (
            <li key={`list-${key}`}>
              {isRenderable(tile, plotSize) ? (
                <div className="allocation-map__group-description">
                  <span>{titleFor(tile, entriesById, visibleIds)}</span>
                  {isSelected && tile.entryIds.length > 1 && (
                    <span>Selected item is within this group.</span>
                  )}
                </div>
              ) : (
                <>
                  <button
                    type="button"
                    disabled={busy || !matches.length}
                    aria-pressed={isSelected}
                    onClick={(event) => activate(tile, event.currentTarget)}
                  >
                    {titleFor(tile, entriesById, visibleIds)}
                  </button>
                  <span className="allocation-map__small-note">
                    Small tile; use this control to select its{" "}
                    {matches.length === 1 ? "file" : "group"}.
                    {isSelected && tile.entryIds.length > 1
                      ? " Selected item is within this group."
                      : ""}
                  </span>
                </>
              )}
            </li>
          );
        })}
      </ul>
      <p className="allocation-map__omissions">
        This map covers the current page only. Directories, shared links,
        dataless files, unknown allocation, zero allocation, and other non-file
        entries are shown separately. Known allocation is reported metadata, not
        unique physical storage or reclaimable space; APFS clones remain
        uncertain.
      </p>
      <dl className="allocation-map__counts">
        <div>
          <dt>Unknown allocation</dt>
          <dd>{allocation.unknownAllocationEntries}</dd>
        </div>
        <div>
          <dt>Shared-link entries</dt>
          <dd>{allocation.sharedLinkEntries}</dd>
        </div>
        <div>
          <dt>Zero allocation</dt>
          <dd>{allocation.zeroAllocationEntries}</dd>
        </div>
        <div>
          <dt>Unmapped non-file entries</dt>
          <dd>{allocation.unmappedNonFileEntries}</dd>
        </div>
        <div>
          <dt>Dataless entries</dt>
          <dd>{allocation.datalessEntries}</dd>
        </div>
      </dl>
    </section>
  );
}
