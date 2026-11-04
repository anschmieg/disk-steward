import type { EditorSnapshot } from "../general-plan-contract";
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
    allocatedBytes: string | null;
    linkCount: string | null;
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
  onOpenPlan,
}: {
  native: boolean;
  onOpenPlan?: () => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const titleId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const activeRequest = useRef(false);
  const evidenceHeadingRef = useRef<HTMLHeadingElement>(null);
  const inspectorReturnRef = useRef<HTMLButtonElement | null>(null);
  const queryRef = useRef<HTMLInputElement>(null);
  const [snapshot, setSnapshot] = useState<FolderInventory | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [imported, setImported] = useState(false);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [sort, setSort] = useState<"name" | "size" | "allocated">("size");
  useEffect(() => {
    if (snapshot) headingRef.current?.focus();
  }, [snapshot]);
  useEffect(() => {
    if (selectedId !== null) evidenceHeadingRef.current?.focus();
  }, [selectedId]);
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
        inspectorReturnRef.current = null;
        setSelectedId(null);
        setImported(false);
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
  const addToPlan = async () => {
    if (!native || !snapshot || activeRequest.current) return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      const before = await invoke<EditorSnapshot>("get_general_plan", {
        request: { profile: "live" },
      });
      const after = await invoke<EditorSnapshot>(
        "import_observed_review_items",
        {
          request: {
            expectedObservationId: snapshot.observationId,
            expectedRevisionId: before.revisionId,
          },
        },
      );
      setImported(true);
      setMessage(
        `Added ${after.items.length - before.items.length} observed entries to your local review plan. They remain blocked from cleanup.`,
      );
    } catch {
      setMessage(
        "The entries could not be added. Your observation is unchanged. Open your plan to check its latest state before trying again.",
      );
    } finally {
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const selected = snapshot?.entries.find((entry) => entry.id === selectedId);
  const total = BigInt(snapshot?.regularFileLogicalBytes ?? "0");
  const entries = [...(snapshot?.entries ?? [])]
    .filter((entry) =>
      entry.name.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
    )
    .sort((a, b) => {
      if (sort !== "name") {
        const aBytes = sort === "allocated" ? a.allocatedBytes : a.logicalBytes;
        const bBytes = sort === "allocated" ? b.allocatedBytes : b.logicalBytes;
        const left = aBytes === null ? -1n : BigInt(aBytes);
        const right = bBytes === null ? -1n : BigInt(bBytes);
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
          <div className="panel-actions">
            <button
              className="button secondary"
              disabled={
                busy ||
                !native ||
                imported ||
                !snapshot.entries.some(
                  (entry) => entry.kind === "file" || entry.kind === "folder",
                )
              }
              onClick={() => void addToPlan()}
            >
              Add observed entries to your plan
            </button>
            {onOpenPlan && (
              <button
                className="button secondary"
                disabled={busy}
                onClick={onOpenPlan}
              >
                Open your review plan
              </button>
            )}
          </div>
          <p>
            Adding saves the observed file/folder names and metadata locally,
            including entries hidden by the filter. It does not save source
            paths or grant cleanup permission. The separate plan starts empty;
            all imported entries default to Keep and stay blocked.
          </p>
          <label>
            Filter observed names{" "}
            <input
              type="search"
              ref={queryRef}
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
              onChange={(e) =>
                setSort(e.target.value as "name" | "size" | "allocated")
              }
            >
              <option value="size">Logical size, largest first</option>
              <option value="allocated">
                Reported allocation, largest first
              </option>
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
                  <th scope="col">Reported allocated bytes</th>
                  <th scope="col">Links</th>
                  <th scope="col">Evidence</th>
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
                    <td>{entry.allocatedBytes ?? "Unknown"}</td>
                    <td>
                      {entry.linkCount === null
                        ? "Unknown"
                        : `${entry.linkCount}${entry.hardLinked ? " · multiple hard links" : ""}`}
                    </td>
                    <td>
                      <button
                        className="button secondary"
                        aria-label={`Inspect ${entry.name}`}
                        aria-pressed={selectedId === entry.id}
                        onClick={(event) => {
                          inspectorReturnRef.current = event.currentTarget;
                          setSelectedId(entry.id);
                        }}
                      >
                        Inspect
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {selected && (
            <section
              className="inventory-evidence"
              aria-label="Entry evidence"
              aria-live="polite"
            >
              <h4 ref={evidenceHeadingRef} tabIndex={-1}>
                {selected.name}
              </h4>
              <button
                className="button secondary"
                onClick={() => {
                  setSelectedId(null);
                  const target = inspectorReturnRef.current;
                  if (target?.isConnected) target.focus();
                  else queryRef.current?.focus();
                }}
              >
                Return to entry list
              </button>
              <dl>
                <dt>Entry kind</dt>
                <dd>{selected.kind}</dd>
                <dt>Logical bytes</dt>
                <dd>{selected.logicalBytes ?? "Unknown"}</dd>
                <dt>Reported allocated bytes</dt>
                <dd>{selected.allocatedBytes ?? "Unknown"}</dd>
                <dt>Link count</dt>
                <dd>{selected.linkCount ?? "Unknown"}</dd>
              </dl>
              <p>
                Historical no-follow metadata observation. File contents were
                not read. Folder contents and recursive sizes are unknown.
                Reported allocation comes from filesystem block metadata; hard
                links, shared extents, compression and snapshots prevent it from
                establishing uniquely occupied or reclaimable space.
              </p>
              <p>
                Cleanup blocked: metadata alone establishes neither safe removal
                nor a recoverable-space estimate. Importing this entry into your
                plan keeps those blockers.
              </p>
            </section>
          )}
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
