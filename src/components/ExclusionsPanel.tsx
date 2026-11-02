import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FolderLock, Plus, RotateCw, X } from "lucide-react";

type RegistrySnapshot = {
  schemaVersion: number;
  revisionId: string;
  entries: { id: string; basename: string; status: "saved_unverified" }[];
};
type Change = { snapshot: RegistrySnapshot; canceled: boolean };
type Props = {
  busy: boolean;
  run: <T>(operation: () => Promise<T>) => Promise<T | undefined>;
};

export default function ExclusionsPanel({ busy, run }: Props) {
  const [registry, setRegistry] = useState<RegistrySnapshot | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const perform = useCallback(
    async <T,>(operation: () => Promise<T>) => {
      setError("");
      setNotice("");
      return run(async () => {
        try {
          return await operation();
        } catch (failure) {
          setConfirmed(false);
          setError(
            typeof failure === "object" &&
              failure !== null &&
              "message" in failure &&
              typeof failure.message === "string"
              ? failure.message
              : "The exclusion registry could not be confirmed. Refresh before another change.",
          );
          return undefined;
        }
      });
    },
    [run],
  );
  const refresh = useCallback(async () => {
    const next = await perform(() =>
      invoke<RegistrySnapshot>("get_exclusion_registry"),
    );
    if (next) {
      setRegistry(next);
      setConfirmed(true);
    }
  }, [perform]);
  useEffect(() => {
    void refresh();
  }, [refresh]);
  const change = async (id?: string) => {
    if (!registry || !confirmed) return;
    const result = await perform(() =>
      invoke<Change>(id ? "remove_global_exclusion" : "add_global_exclusion", {
        request: {
          expectedRevisionId: registry.revisionId,
          ...(id ? { exclusionId: id } : {}),
        },
      }),
    );
    if (result) {
      setRegistry(result.snapshot);
      setConfirmed(true);
      setNotice(
        result.canceled
          ? "Canceled. Remembered exclusions are unchanged."
          : id
            ? "Exclusion removed after confirmation."
            : "Exclusion saved for future scans. The example plan is unchanged.",
      );
    }
  };
  return (
    <details className="exclusions-panel">
      <summary>
        <FolderLock size={17} />
        <span>Future scan exclusions</span>
        <span className="exclusions-count">
          {registry?.entries.length ?? "—"} saved
        </span>
      </summary>
      <div className="exclusions-content">
        <p>
          Remember exact folders to leave out of future scans. Each change needs
          a separate confirmation. Scanning is not available in this build;
          these rules do not change the example plan or grant access.
        </p>
        <div className="exclusions-actions">
          <button
            className="ap-button quiet"
            disabled={busy || !confirmed}
            onClick={() => void change()}
          >
            <Plus size={15} />
            Remember a folder…
          </button>
          <button
            className="ap-button quiet"
            disabled={busy}
            onClick={() => void refresh()}
          >
            <RotateCw size={15} />
            Refresh exclusions
          </button>
        </div>
        {error && (
          <p className="exclusions-error" role="alert">
            {error}
          </p>
        )}
        {notice && <p role="status">{notice}</p>}
        {registry && registry.entries.length === 0 && (
          <p className="exclusions-empty">
            No folders remembered. Keeping an item in the plan does not create a
            permanent exclusion.
          </p>
        )}
        {registry && registry.entries.length > 0 && (
          <ul>
            {registry.entries.map((entry) => (
              <li key={entry.id}>
                <div>
                  <b>{entry.basename}</b>
                  <small>Saved · not yet checked by a scan</small>
                </div>
                <button
                  className="ap-button quiet"
                  disabled={busy || !confirmed}
                  aria-label={`Remove remembered exclusion for ${entry.basename}`}
                  onClick={() => void change(entry.id)}
                >
                  <X size={15} />
                  Remove…
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </details>
  );
}
