import "./FolderInventoryPanel.css";
import { useEffect, useId, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type FolderInventory = {
  rootLabel: string;
  observationId: string;
  breadcrumbs: string[];
  canGoBack: boolean;
  entries: Array<{
    id: string;
    name: string;
    kind: string;
    logicalBytes: string | null;
    hardLinked: boolean;
    canOpen: boolean;
  }>;
  enumerationComplete: boolean;
  unreadableCount: number;
  stopReason: string | null;
  directoriesNotTraversed: number;
  regularFileLogicalBytes: string;
};

export default function FolderInventoryPanel({
  native,
  onBusyChange,
}: {
  native: boolean;
  onBusyChange: (busy: boolean) => void;
}) {
  const titleId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const activeRequest = useRef(false);
  const [snapshot, setSnapshot] = useState<FolderInventory | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<"name" | "size">("size");
  useEffect(() => {
    if (snapshot) headingRef.current?.focus();
  }, [snapshot]);
  const inspect = async (entryId?: string | null) => {
    if (activeRequest.current || !native) return;
    if (entryId !== undefined && !snapshot) return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      const next =
        entryId === undefined
          ? await invoke<FolderInventory | null>(
              "inspect_selected_folder_entries",
            )
          : await invoke<FolderInventory>("navigate_folder_inventory", {
              request: {
                expectedObservationId: snapshot!.observationId,
                entryId,
              },
            });
      if (next) {
        setSnapshot(next);
        setQuery("");
      } else
        setMessage(
          "Folder selection canceled. The previous observation is unchanged.",
        );
    } catch {
      setMessage(
        "The folder could not be inspected. Choose it again to retry. Any previous observation remains below.",
      );
    } finally {
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const total = BigInt(snapshot?.regularFileLogicalBytes ?? "0");
  const entries = [...(snapshot?.entries ?? [])]
    .filter((entry) =>
      entry.name.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
    )
    .sort((a, b) => {
      if (sort === "size") {
        const left = a.logicalBytes === null ? -1n : BigInt(a.logicalBytes);
        const right = b.logicalBytes === null ? -1n : BigInt(b.logicalBytes);
        if (left !== right) return left > right ? -1 : 1;
      }
      return a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
    });
  return (
    <section
      className="panel folder-inventory"
      aria-labelledby={titleId}
      aria-busy={busy}
    >
      <h2 id={titleId}>Explore a folder</h2>
      <p>
        Choose a local folder to inspect up to 512 immediate entries. Open an
        observed folder explicitly to explore one level deeper. Links and volume
        boundaries are not followed; file contents are not read.
      </p>
      <button
        className="button secondary"
        onClick={() => void inspect()}
        disabled={busy || !native}
      >
        {busy ? "Inspecting folder…" : "Choose folder"}
      </button>
      {!native && <p>Folder inspection is available in the desktop app.</p>}
      <p role="status">{message}</p>
      {snapshot && (
        <>
          <nav aria-label="Folder location">
            <ol>
              {snapshot.breadcrumbs.map((label, index) => (
                <li
                  key={index}
                  aria-current={
                    index === snapshot.breadcrumbs.length - 1
                      ? "location"
                      : undefined
                  }
                >
                  {label}
                </li>
              ))}
            </ol>
          </nav>
          <h3 ref={headingRef} tabIndex={-1}>
            {snapshot.rootLabel}
          </h3>
          <button
            className="button secondary"
            onClick={() => void inspect(null)}
            disabled={busy || !snapshot.canGoBack}
          >
            Back to parent folder
          </button>
          <p role="status">
            {snapshot.enumerationComplete
              ? "Immediate entry enumeration finished."
              : "Incomplete immediate entry enumeration."}{" "}
            {snapshot.stopReason
              ? (coverageReasons[snapshot.stopReason] ??
                "Some metadata could not be observed reliably.")
              : ""}
          </p>
          <p>
            {snapshot.entries.length} observed entries ·{" "}
            {snapshot.unreadableCount} unreadable entries ·{" "}
            {snapshot.directoriesNotTraversed} folders not traversed
          </p>
          <p>
            Observed regular-file logical sizes total{" "}
            {snapshot.regularFileLogicalBytes} bytes. Hard links are counted per
            name. This is neither occupied disk space nor recoverable space;
            nested contents and shared allocation are unknown.
          </p>
          <label>
            Filter observed names{" "}
            <input
              type="search"
              value={query}
              disabled={busy}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <p>
            Showing {entries.length} of {snapshot.entries.length} observed
            entries. Filtering does not change coverage or the logical-size
            denominator.
          </p>
          <label>
            Sort entries{" "}
            <select
              value={sort}
              disabled={busy}
              onChange={(e) => setSort(e.target.value as "name" | "size")}
            >
              <option value="size">Logical size, largest first</option>
              <option value="name">Name</option>
            </select>
          </label>
          <div style={{ overflowX: "auto" }}>
            <table>
              <caption>
                Read-only metadata observation. All entries remain unavailable
                for cleanup. Size bars use the full observation’s{" "}
                {snapshot.regularFileLogicalBytes} regular-file logical bytes as
                their denominator; folders have unknown size.
              </caption>
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Kind</th>
                  <th scope="col">Logical bytes</th>
                  <th scope="col">Links</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((entry) => (
                  <tr key={entry.id}>
                    <th scope="row">
                      {entry.canOpen ? (
                        <button
                          className="button secondary"
                          disabled={busy}
                          onClick={() => void inspect(entry.id)}
                          aria-label={`Open folder ${entry.name}`}
                        >
                          {entry.name}
                        </button>
                      ) : (
                        entry.name
                      )}
                    </th>
                    <td>{entry.kind}</td>
                    <td className="inventory-size">
                      <span
                        className="inventory-size-bar"
                        aria-hidden="true"
                        style={{
                          width: `${entry.logicalBytes !== null && total > 0n ? Number((BigInt(entry.logicalBytes) * 10000n) / total) / 100 : 0}%`,
                        }}
                      />
                      <span className="inventory-size-value">
                        {entry.logicalBytes ?? "Unknown"}
                      </span>
                    </td>
                    <td>{entry.hardLinked ? "Multiple hard links" : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {entries.length === 0 && query && (
            <p>No observed names match this filter.</p>
          )}
          {snapshot.entries.length === 0 && (
            <p>
              No readable entries were observed. This does not establish that
              the folder is empty when coverage is incomplete.
            </p>
          )}
          <p>
            Point-in-time observation; changes after inspection are not tracked.
            Folder identity is rechecked before navigation. Hidden folders,
            package/library boundaries and folders beyond the navigation depth
            limit cannot be opened here. Choose the folder again to refresh. No
            cleanup plan or permission to change files is created.
          </p>
        </>
      )}
    </section>
  );
}

const coverageReasons: Record<string, string> = {
  time_limit: "The observation time limit was reached.",
  entry_limit: "The entry limit was reached; more entries may exist.",
  directory_read_error: "The folder iterator became unreadable.",
  entry_metadata_unavailable: "Some entry metadata was unavailable.",
  logical_size_overflow: "The logical size total exceeded the supported range.",
  root_metadata_changed:
    "The folder changed during inspection; inspect it again.",
  root_metadata_unavailable: "The folder metadata could not be rechecked.",
};
