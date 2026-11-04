import { useId, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { EditorSnapshot } from "../general-plan-contract";
import type { FolderInventory } from "./FolderInventoryPanel";
import "./DuplicateComparisonPanel.css";

const MAX_FILES = 32;
const MAX_FILE_BYTES = 16n * 1024n * 1024n;
const MAX_INPUT_BYTES = 64n * 1024n * 1024n;
const MAX_READ_BYTES = 192n * 1024n * 1024n;
const U64_MAX = 18_446_744_073_709_551_615n;
const DETECTOR_VERSION = "exact-regular-file-v1";
const ATTEMPT_PREFIX = "disk-steward:summary-attempt:v1:";

type ComparisonGroup = {
  id: string;
  entryIds: string[];
  logicalBytes: string;
};

type ComparisonResult = {
  observationId: string;
  comparisonId: string;
  detectorVersion: typeof DETECTOR_VERSION;
  filesCompared: number;
  bytesRead: string;
  groups: ComparisonGroup[];
};

type ActiveAttempt = { observationId: string; attemptId: string };

function canonicalU64(value: unknown): bigint | null {
  if (
    typeof value !== "string" ||
    value.length > 20 ||
    !/^(0|[1-9][0-9]*)$/.test(value)
  )
    return null;
  try {
    const parsed = BigInt(value);
    return parsed <= U64_MAX ? parsed : null;
  } catch {
    return null;
  }
}

function formatBytes(value: bigint | string): string {
  const parsed = typeof value === "bigint" ? value : BigInt(value);
  return `${parsed.toLocaleString()} bytes`;
}

function eligibilityReason(
  entry: FolderInventory["entries"][number],
): string | null {
  if (entry.kind !== "file") return null;
  if (entry.availability === "dataless")
    return "Dataless files cannot be compared.";
  if (entry.hardLinked || entry.linkCount !== "1")
    return "Shared or uncertain file links are excluded.";
  if (entry.name.startsWith("."))
    return "Names beginning with a dot are excluded from suggestions.";
  const size = canonicalU64(entry.logicalBytes);
  if (size === null) return "A canonical logical size is required.";
  if (size > MAX_FILE_BYTES) return "The suggested per-file limit is 16 MiB.";
  return null;
}

function validateResult(
  value: unknown,
  snapshot: FolderInventory,
  requestedIds: string[],
): ComparisonResult {
  if (!value || typeof value !== "object") throw new Error("Invalid result");
  const result = value as Record<string, unknown>;
  const selected = new Set(requestedIds);
  const filesById = new Map(snapshot.entries.map((entry) => [entry.id, entry]));
  const bytesRead = canonicalU64(result.bytesRead);
  if (
    result.observationId !== snapshot.observationId ||
    typeof result.comparisonId !== "string" ||
    !result.comparisonId ||
    result.comparisonId.length > 256 ||
    result.detectorVersion !== DETECTOR_VERSION ||
    result.filesCompared !== requestedIds.length ||
    bytesRead === null ||
    bytesRead > MAX_READ_BYTES ||
    !Array.isArray(result.groups) ||
    result.groups.length > requestedIds.length / 2
  )
    throw new Error("Invalid result");

  const groupIds = new Set<string>();
  const comparedIds = new Set<string>();
  const groups: ComparisonGroup[] = [];
  for (const item of result.groups) {
    if (!item || typeof item !== "object") throw new Error("Invalid group");
    const group = item as Record<string, unknown>;
    if (
      typeof group.id !== "string" ||
      !group.id ||
      group.id.length > 256 ||
      groupIds.has(group.id) ||
      !Array.isArray(group.entryIds) ||
      group.entryIds.length < 2
    )
      throw new Error("Invalid group");
    const logicalBytes = canonicalU64(group.logicalBytes);
    if (logicalBytes === null) throw new Error("Invalid group size");
    const ids: string[] = [];
    for (const id of group.entryIds) {
      const entry = typeof id === "string" ? filesById.get(id) : undefined;
      if (
        !entry ||
        !selected.has(id as string) ||
        comparedIds.has(id as string) ||
        canonicalU64(entry.logicalBytes) !== logicalBytes
      )
        throw new Error("Invalid group members");
      comparedIds.add(id as string);
      ids.push(id as string);
    }
    groupIds.add(group.id);
    groups.push({
      id: group.id,
      entryIds: ids,
      logicalBytes: logicalBytes.toString(),
    });
  }
  return {
    observationId: snapshot.observationId,
    comparisonId: result.comparisonId as string,
    detectorVersion: DETECTOR_VERSION,
    filesCompared: requestedIds.length,
    bytesRead: bytesRead.toString(),
    groups,
  };
}

export default function DuplicateComparisonPanel({
  snapshot,
  native,
  busy,
  onBusyChange,
  onOpenPlan,
}: {
  snapshot: FolderInventory;
  native: boolean;
  busy: boolean;
  onBusyChange: (busy: boolean) => void;
  onOpenPlan?: () => void;
}) {
  const headingId = useId();
  const consentId = useId();
  const activeAttempt = useRef<ActiveAttempt | null>(null);
  const importPendingRef = useRef(false);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [contentConsent, setContentConsent] = useState(false);
  const [running, setRunning] = useState(false);
  const [cancelPhase, setCancelPhase] = useState<
    "ready" | "sending" | "acknowledged"
  >("ready");
  const [result, setResult] = useState<ComparisonResult | null>(null);
  const [resultSelection, setResultSelection] = useState<string[]>([]);
  const [resultHistorical, setResultHistorical] = useState(false);
  const [importPending, setImportPending] = useState(false);
  const [importedComparisonId, setImportedComparisonId] = useState<
    string | null
  >(null);
  const [message, setMessage] = useState("");
  const regularFiles = snapshot.entries.filter(
    (entry) => entry.kind === "file",
  );
  const filesById = useMemo(
    () => new Map(regularFiles.map((entry) => [entry.id, entry])),
    [regularFiles],
  );
  const selectedBytes = selectedIds.reduce(
    (sum, id) => sum + (canonicalU64(filesById.get(id)?.logicalBytes) ?? 0n),
    0n,
  );
  const historical =
    result !== null &&
    (resultHistorical ||
      result.observationId !== snapshot.observationId ||
      resultSelection.length !== selectedIds.length ||
      resultSelection.some((id) => !selectedIds.includes(id)));

  const toggleSelection = (id: string, checked: boolean) => {
    if (running || busy || activeAttempt.current || importPendingRef.current)
      return;
    const next = checked
      ? [...selectedIds, id]
      : selectedIds.filter((selected) => selected !== id);
    setSelectedIds(next);
    setContentConsent(false);
    setMessage("");
    if (result) setResultHistorical(true);
  };

  const compare = async () => {
    if (
      !native ||
      activeAttempt.current !== null ||
      importPendingRef.current ||
      busy ||
      running ||
      !contentConsent ||
      selectedIds.length < 2 ||
      selectedIds.length > MAX_FILES ||
      selectedBytes > MAX_INPUT_BYTES
    )
      return;
    const attempt = {
      observationId: snapshot.observationId,
      attemptId: `${ATTEMPT_PREFIX}${crypto.randomUUID()}`,
    };
    activeAttempt.current = attempt;
    setRunning(true);
    setCancelPhase("ready");
    setMessage("");
    setContentConsent(false);
    if (result) setResultHistorical(true);
    setImportedComparisonId(null);
    onBusyChange(true);
    const ids = [...selectedIds];
    try {
      const raw = await invoke<unknown>("compare_observed_files", {
        request: {
          expectedObservationId: attempt.observationId,
          entryIds: ids,
          attemptId: attempt.attemptId,
          allowContentRead: true,
        },
      });
      const verified = validateResult(raw, snapshot, ids);
      setResult(verified);
      setResultSelection(ids);
      setResultHistorical(false);
      setImportedComparisonId(null);
      setMessage(
        verified.groups.length
          ? `Comparison completed for ${verified.filesCompared} explicitly selected files.`
          : `No matching groups were found among these ${verified.filesCompared} explicitly selected files.`,
      );
    } catch {
      setMessage(
        "The comparison could not be verified. Any previous result is retained as historical evidence; review the current folder and selected files before trying again.",
      );
    } finally {
      if (activeAttempt.current === attempt) activeAttempt.current = null;
      setRunning(false);
      setCancelPhase("ready");
      onBusyChange(false);
    }
  };

  const importGroups = async () => {
    if (
      !native ||
      busy ||
      running ||
      importPendingRef.current ||
      activeAttempt.current ||
      !result ||
      historical ||
      result.groups.length === 0 ||
      importedComparisonId === result.comparisonId
    )
      return;
    importPendingRef.current = true;
    setImportPending(true);
    setMessage("");
    onBusyChange(true);
    try {
      const before = await invoke<EditorSnapshot>("get_general_plan", {
        request: { profile: "live" },
      });
      await invoke<EditorSnapshot>("import_duplicate_review_items", {
        request: {
          expectedObservationId: snapshot.observationId,
          comparisonId: result.comparisonId,
          expectedRevisionId: before.revisionId,
        },
      });
      setImportedComparisonId(result.comparisonId);
      setMessage(
        "Matching evidence was saved to your local review plan. New review items default to Keep; existing choices are preserved. No survivor was chosen automatically, duplicate items remain blocked, and no files were changed or executed.",
      );
    } catch {
      setMessage(
        "The plan import could not be verified. Your comparison evidence and selection remain; open your plan to check its latest state before trying again.",
      );
    } finally {
      importPendingRef.current = false;
      setImportPending(false);
      onBusyChange(false);
    }
  };

  const cancel = async () => {
    const attempt = activeAttempt.current;
    if (!attempt || cancelPhase === "sending" || cancelPhase === "acknowledged")
      return;
    setCancelPhase("sending");
    setMessage("");
    try {
      await invoke<void>("cancel_duplicate_comparison", {
        request: {
          expectedObservationId: attempt.observationId,
          attemptId: attempt.attemptId,
        },
      });
      if (activeAttempt.current === attempt) {
        setCancelPhase("acknowledged");
        setMessage(
          "Cancellation signal acknowledged. Waiting for any in-flight file read and comparison to settle.",
        );
      }
    } catch {
      if (activeAttempt.current === attempt) {
        setCancelPhase("ready");
        setMessage(
          "Cancellation could not be confirmed. The comparison remains active until native work settles.",
        );
      }
    }
  };

  return (
    <section
      className="duplicate-comparison"
      aria-labelledby={headingId}
      aria-busy={busy || running || importPending}
    >
      <h3 id={headingId}>Compare selected files for exact matches</h3>
      <p>
        Choose regular files from this page explicitly. Suggestions exclude
        dataless files, shared or uncertain links, hidden names, unknown sizes,
        and files above 16 MiB. The native command checks eligibility again; a
        suggestion does not approve cleanup.
      </p>
      <div
        className="duplicate-comparison__selection-summary"
        aria-live="polite"
      >
        {selectedIds.length} selected · {formatBytes(selectedBytes)} of logical
        input · limits: {MAX_FILES} files, {formatBytes(MAX_INPUT_BYTES)} total
      </div>
      {selectedIds.length > 0 && (
        <p className="duplicate-comparison__selected-names">
          Selected files:{" "}
          {selectedIds
            .map((id) => filesById.get(id)?.name ?? "Observed file")
            .join(", ")}
        </p>
      )}
      {regularFiles.length === 0 ? (
        <p>No regular files appear on this page.</p>
      ) : (
        <ul
          className="duplicate-comparison__files"
          aria-label="Regular files on the current page"
        >
          {regularFiles.map((entry) => {
            const reason = eligibilityReason(entry);
            const bytes = canonicalU64(entry.logicalBytes);
            const chosen = selectedIds.includes(entry.id);
            const wouldExceedTotal =
              !chosen &&
              bytes !== null &&
              selectedBytes + bytes > MAX_INPUT_BYTES;
            const wouldExceedCount = !chosen && selectedIds.length >= MAX_FILES;
            const disabled =
              !native ||
              busy ||
              running ||
              importPending ||
              Boolean(reason) ||
              wouldExceedTotal ||
              wouldExceedCount;
            return (
              <li key={entry.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={chosen}
                    disabled={disabled}
                    onChange={(event) =>
                      toggleSelection(entry.id, event.currentTarget.checked)
                    }
                  />
                  <span className="duplicate-comparison__filename">
                    {entry.name}
                  </span>
                  <span>
                    {bytes === null
                      ? "logical size unknown"
                      : formatBytes(bytes)}
                  </span>
                </label>
                {(reason || wouldExceedTotal || wouldExceedCount) &&
                  !chosen && (
                    <small>
                      {reason ??
                        (wouldExceedCount
                          ? `Selection limit is ${MAX_FILES} files.`
                          : `Selection would exceed ${formatBytes(MAX_INPUT_BYTES)} total.`)}
                    </small>
                  )}
              </li>
            );
          })}
        </ul>
      )}
      <label className="duplicate-comparison__consent" htmlFor={consentId}>
        <input
          id={consentId}
          type="checkbox"
          checked={contentConsent}
          disabled={
            !native ||
            busy ||
            running ||
            importPending ||
            selectedIds.length < 2 ||
            selectedBytes > MAX_INPUT_BYTES
          }
          onChange={(event) => setContentConsent(event.currentTarget.checked)}
        />
        Allow local content reads for these selected files
      </label>
      <p>
        This explicit consent applies to one comparison attempt and clears when
        the selection changes or the attempt starts. Contents and digests are
        not uploaded or saved. Content availability is otherwise unverified:
        selected reads may cause provider I/O, whose traffic the app cannot
        measure. Reads can wait on an in-flight filesystem call; cancellation is
        cooperative with a five-second check and a 192 MiB total actual-read
        limit, including hashing and byte checks. Reads may update filesystem
        access metadata; file contents stay unchanged. A match is not a cleanup
        decision.
      </p>
      <button
        className="button primary"
        type="button"
        disabled={
          !native ||
          busy ||
          running ||
          importPending ||
          !contentConsent ||
          selectedIds.length < 2 ||
          selectedBytes > MAX_INPUT_BYTES
        }
        onClick={() => void compare()}
      >
        {running ? "Comparing selected files…" : "Compare selected files"}
      </button>
      {running && (
        <button
          className="button secondary"
          type="button"
          disabled={cancelPhase !== "ready"}
          onClick={() => void cancel()}
        >
          {cancelPhase === "sending"
            ? "Sending cancellation…"
            : cancelPhase === "acknowledged"
              ? "Cancellation acknowledged"
              : "Cancel comparison"}
        </button>
      )}
      <p role="status">{message}</p>
      {result && (
        <section
          className="duplicate-comparison__result"
          aria-label="Exact comparison result"
        >
          <h4>Exact comparison result{historical ? " · historical" : ""}</h4>
          <p>
            Detector {result.detectorVersion} · {result.filesCompared} files
            compared · {formatBytes(result.bytesRead)} read · comparison{" "}
            {result.comparisonId}
          </p>
          {result.groups.length === 0 ? (
            <p>
              No matching groups among the selected files for that comparison.
            </p>
          ) : (
            <ul>
              {result.groups.map((group) => (
                <li key={group.id}>
                  <strong>
                    Matching bytes: {formatBytes(group.logicalBytes)}
                  </strong>
                  <ul>
                    {group.entryIds.map((id) => (
                      <li key={id}>
                        {filesById.get(id)?.name ?? "Previously selected file"}
                      </li>
                    ))}
                  </ul>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
      {result && result.groups.length > 0 && !historical && (
        <div className="duplicate-comparison__import-actions">
          <p>
            Adding groups saves matching names, logical sizes, and comparison
            evidence locally. It does not save paths, digests, or contents.
            Existing plan choices are preserved; no survivor is selected
            automatically. Native import requires retained comparison proof from
            the last five minutes; changed identity, metadata, or policy may
            also cause refusal. Saved plan evidence remains historical and
            non-executable after that proof expires.
          </p>
          <button
            className="button secondary"
            type="button"
            disabled={
              !native ||
              busy ||
              running ||
              importPending ||
              importedComparisonId === result.comparisonId
            }
            onClick={() => void importGroups()}
          >
            {importPending
              ? "Adding matching groups…"
              : importedComparisonId === result.comparisonId
                ? "Matching groups added to plan"
                : "Add matching groups to plan"}
          </button>
          {importedComparisonId === result.comparisonId && onOpenPlan && (
            <button
              className="button secondary"
              type="button"
              disabled={busy || importPending}
              onClick={onOpenPlan}
            >
              Review matching groups in plan
            </button>
          )}
        </div>
      )}
      <p>
        Matching bytes are historical comparison evidence after native identity
        checks. They do not establish unique physical storage, reclaimable
        space, or APFS clone sharing. Adding evidence does not grant cleanup
        authority. The review plan can record a survivor choice, but duplicate
        items remain blocked and no execution is available.
      </p>
    </section>
  );
}
