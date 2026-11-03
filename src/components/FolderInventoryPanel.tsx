import "./FolderInventoryPanel.css";
import { useId, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type FolderInventory = {
  rootLabel: string;
  entries: Array<{
    id: string;
    name: string;
    kind: string;
    logicalBytes: string | null;
    hardLinked: boolean;
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
  const [snapshot, setSnapshot] = useState<FolderInventory | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [sort, setSort] = useState<"name" | "size">("size");
  const inspect = async () => {
    if (busy || !native) return;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      const next = await invoke<FolderInventory | null>(
        "inspect_selected_folder_entries",
      );
      if (next) setSnapshot(next);
      else
        setMessage(
          "Folder selection canceled. The previous observation is unchanged.",
        );
    } catch {
      setMessage(
        "The folder could not be inspected. Choose it again to retry. Any previous observation remains below.",
      );
    } finally {
      setBusy(false);
      onBusyChange(false);
    }
  };
  const entries = [...(snapshot?.entries ?? [])].sort((a, b) => {
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
        Choose a local folder to inspect up to 512 immediate entries. Nested
        folders are not scanned, links are not followed, and file contents are
        not read.
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
          <h3>{snapshot.rootLabel}</h3>
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
                for cleanup.
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
                    <th scope="row">{entry.name}</th>
                    <td>{entry.kind}</td>
                    <td>{entry.logicalBytes ?? "Unknown"}</td>
                    <td>{entry.hardLinked ? "Multiple hard links" : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {entries.length === 0 && (
            <p>
              No readable entries were observed. This does not establish that
              the folder is empty when coverage is incomplete.
            </p>
          )}
          <p>
            Point-in-time observation; changes after inspection are not tracked.
            Choose the folder again to refresh. No cleanup plan or permission to
            change files is created.
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
