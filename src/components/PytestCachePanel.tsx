import { useId, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { FolderInventory } from "./FolderInventoryPanel";

const MAX_RECORD_BYTES = 65_536n;
const MAX_READ_BYTES = 131_074n;
const MAX_RECORDS = 512;
const DETECTOR_VERSION = "pytest-cache-format-v1";
const REFERENCE_VERSION = "9.2.0";
const ATTEMPT_PREFIX = "disk-steward:summary-attempt:v1:";
const CACHE_NAMES = ["nodeids", "lastfailed"] as const;

type CacheKind = "node_ids" | "last_failed";
type CacheRecord = { entryId: string; kind: CacheKind; recordCount: number };
type InspectionResult = {
  observationId: string;
  detectorVersion: typeof DETECTOR_VERSION;
  referenceVersion: typeof REFERENCE_VERSION;
  bytesRead: string;
  records: CacheRecord[];
};
type ActiveAttempt = { observationId: string; attemptId: string };

function canonicalBytes(value: unknown): bigint | null {
  if (
    typeof value !== "string" ||
    value.length > 20 ||
    !/^(0|[1-9][0-9]*)$/.test(value)
  )
    return null;
  try {
    return BigInt(value);
  } catch {
    return null;
  }
}

function formatBytes(value: bigint | string): string {
  return `${typeof value === "bigint" ? value : BigInt(value)} bytes`;
}

function eligibilityReason(
  entry: FolderInventory["entries"][number],
): string | null {
  if (!CACHE_NAMES.includes(entry.name as (typeof CACHE_NAMES)[number]))
    return "Only nodeids and lastfailed records are supported.";
  if (entry.kind !== "file") return "The cache record must be a regular file.";
  if (entry.availability === "dataless")
    return "Dataless records cannot be inspected.";
  if (entry.hardLinked || entry.linkCount !== "1")
    return "Shared or uncertain file links are not supported.";
  const bytes = canonicalBytes(entry.logicalBytes);
  if (bytes === null) return "A canonical logical size is required.";
  if (bytes > MAX_RECORD_BYTES) return "The per-record limit is 64 KiB.";
  return null;
}

function isCandidate(entry: FolderInventory["entries"][number]): boolean {
  return eligibilityReason(entry) === null;
}

function validateResult(
  value: unknown,
  snapshot: FolderInventory,
  selectedIds: string[],
): InspectionResult {
  if (!value || typeof value !== "object") throw new Error("Invalid result");
  const result = value as Record<string, unknown>;
  const bytesRead = canonicalBytes(result.bytesRead);
  const selected = new Set(selectedIds);
  const entries = new Map(snapshot.entries.map((entry) => [entry.id, entry]));
  let expectedBytes = 0n;
  for (const id of selectedIds) {
    const entry = entries.get(id);
    const bytes = canonicalBytes(entry?.logicalBytes);
    if (!entry || !isCandidate(entry) || bytes === null)
      throw new Error("Invalid selected records");
    expectedBytes += bytes;
  }
  if (
    result.observationId !== snapshot.observationId ||
    result.detectorVersion !== DETECTOR_VERSION ||
    result.referenceVersion !== REFERENCE_VERSION ||
    bytesRead === null ||
    bytesRead > MAX_READ_BYTES ||
    bytesRead !== expectedBytes ||
    !Array.isArray(result.records) ||
    result.records.length !== selectedIds.length ||
    result.records.length > 2
  )
    throw new Error("Invalid result");

  const seenIds = new Set<string>();
  const seenKinds = new Set<CacheKind>();
  const records: CacheRecord[] = [];
  for (const item of result.records) {
    if (!item || typeof item !== "object") throw new Error("Invalid record");
    const record = item as Record<string, unknown>;
    const kind = record.kind;
    if (
      typeof record.entryId !== "string" ||
      !selected.has(record.entryId) ||
      seenIds.has(record.entryId) ||
      (kind !== "node_ids" && kind !== "last_failed") ||
      seenKinds.has(kind) ||
      !Number.isSafeInteger(record.recordCount) ||
      (record.recordCount as number) < 0 ||
      (record.recordCount as number) > MAX_RECORDS
    )
      throw new Error("Invalid record");
    const entry = entries.get(record.entryId);
    const expectedName = kind === "node_ids" ? "nodeids" : "lastfailed";
    if (!entry || entry.name !== expectedName || !isCandidate(entry))
      throw new Error("Unexpected record");
    seenIds.add(record.entryId);
    seenKinds.add(kind);
    records.push({
      entryId: record.entryId,
      kind,
      recordCount: record.recordCount as number,
    });
  }
  return {
    observationId: snapshot.observationId,
    detectorVersion: DETECTOR_VERSION,
    referenceVersion: REFERENCE_VERSION,
    bytesRead: bytesRead.toString(),
    records,
  };
}

export default function PytestCachePanel({
  snapshot,
  native,
  busy,
  onBusyChange,
}: {
  snapshot: FolderInventory;
  native: boolean;
  busy: boolean;
  onBusyChange: (busy: boolean) => void;
}) {
  const headingId = useId();
  const consentId = useId();
  const activeAttempt = useRef<ActiveAttempt | null>(null);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [contentConsent, setContentConsent] = useState(false);
  const [running, setRunning] = useState(false);
  const [cancelPhase, setCancelPhase] = useState<
    "ready" | "sending" | "acknowledged"
  >("ready");
  const [result, setResult] = useState<InspectionResult | null>(null);
  const [resultSelection, setResultSelection] = useState<string[]>([]);
  const [resultHistorical, setResultHistorical] = useState(false);
  const [message, setMessage] = useState("");
  const candidates = snapshot.entries.filter((entry) =>
    CACHE_NAMES.includes(entry.name as (typeof CACHE_NAMES)[number]),
  );
  const entryById = new Map(snapshot.entries.map((entry) => [entry.id, entry]));
  const historical =
    result !== null &&
    (resultHistorical ||
      result.observationId !== snapshot.observationId ||
      resultSelection.length !== selectedIds.length ||
      resultSelection.some((id) => !selectedIds.includes(id)));

  const select = (entryId: string, checked: boolean) => {
    if (busy || running || activeAttempt.current) return;
    setSelectedIds((current) =>
      checked ? [...current, entryId] : current.filter((id) => id !== entryId),
    );
    setContentConsent(false);
    setMessage("");
    if (result) setResultHistorical(true);
  };

  const inspect = async () => {
    if (
      !native ||
      busy ||
      running ||
      activeAttempt.current ||
      !contentConsent ||
      selectedIds.length < 1 ||
      selectedIds.length > 2
    )
      return;
    const attempt: ActiveAttempt = {
      observationId: snapshot.observationId,
      attemptId: `${ATTEMPT_PREFIX}${crypto.randomUUID()}`,
    };
    activeAttempt.current = attempt;
    const ids = [...selectedIds];
    setRunning(true);
    setCancelPhase("ready");
    setContentConsent(false);
    setMessage("");
    if (result) setResultHistorical(true);
    onBusyChange(true);
    try {
      const raw = await invoke<unknown>("inspect_pytest_cache_records", {
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
      setMessage("Selected pytest cache records were inspected locally.");
    } catch {
      setMessage(
        "The cache inspection could not be verified. Any earlier result remains historical; check the current folder and selected records before trying again.",
      );
    } finally {
      if (activeAttempt.current === attempt) activeAttempt.current = null;
      setRunning(false);
      setCancelPhase("ready");
      onBusyChange(false);
    }
  };

  const cancel = async () => {
    const attempt = activeAttempt.current;
    if (!attempt || cancelPhase !== "ready") return;
    setCancelPhase("sending");
    setMessage("");
    try {
      await invoke<void>("cancel_pytest_cache_inspection", {
        request: {
          expectedObservationId: attempt.observationId,
          attemptId: attempt.attemptId,
        },
      });
      if (activeAttempt.current === attempt) {
        setCancelPhase("acknowledged");
        setMessage(
          "Cancellation signal acknowledged. Waiting for an in-flight record read and inspection to settle.",
        );
      }
    } catch {
      if (activeAttempt.current === attempt) {
        setCancelPhase("ready");
        setMessage(
          "Cancellation could not be confirmed. The inspection remains active until native work settles.",
        );
      }
    }
  };

  if (candidates.length === 0) return null;

  return (
    <section
      className="pytest-cache-panel"
      aria-labelledby={headingId}
      aria-busy={busy || running}
    >
      <h3 id={headingId}>Inspect selected pytest cache records</h3>
      <p>
        To inspect cache records, navigate explicitly into the project’s
        <code> .pytest_cache/v/cache </code> directory and select{" "}
        <code>nodeids</code>
        and/or <code>lastfailed</code>. Nothing is selected automatically. Each
        record is limited to 64 KiB and at most two may be selected.
      </p>
      <ul aria-label="Pytest cache record candidates">
        {candidates.map((entry) => {
          const reason = eligibilityReason(entry);
          const eligible = reason === null;
          const checked = selectedIds.includes(entry.id);
          const disabled =
            !native ||
            busy ||
            running ||
            !eligible ||
            (!checked && selectedIds.length >= 2);
          const bytes = canonicalBytes(entry.logicalBytes);
          return (
            <li key={entry.id}>
              <label>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={disabled}
                  onChange={(event) =>
                    select(entry.id, event.currentTarget.checked)
                  }
                />
                <span>{entry.name}</span>
                <span>
                  {bytes === null ? "logical size unknown" : formatBytes(bytes)}
                </span>
              </label>
              {!eligible && <small>{reason}</small>}
            </li>
          );
        })}
      </ul>
      <p aria-live="polite">
        {selectedIds.length} selected · limits: 2 records, 64 KiB per record
        {selectedIds.length > 0
          ? ` · ${selectedIds.map((id) => entryById.get(id)?.name ?? "record").join(", ")}`
          : ""}
      </p>
      <label htmlFor={consentId}>
        <input
          id={consentId}
          type="checkbox"
          checked={contentConsent}
          disabled={!native || busy || running || selectedIds.length === 0}
          onChange={(event) => setContentConsent(event.currentTarget.checked)}
        />
        Allow local content reads for these selected records for this inspection
      </label>
      <p>
        Consent is required for each attempt and clears when selection changes
        or inspection starts. Names and record contents stay local; no tools or
        plugins run. The format check does not prove project ownership, pytest
        version, idle state, or regeneration safety, and it does not approve
        cleanup. Reads may trigger provider I/O. This inspection does not write
        to or change cache record contents.
      </p>
      <button
        className="button primary"
        type="button"
        disabled={
          !native ||
          busy ||
          running ||
          !contentConsent ||
          selectedIds.length === 0 ||
          selectedIds.length > 2
        }
        onClick={() => void inspect()}
      >
        {running ? "Inspecting selected records…" : "Inspect selected records"}
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
              : "Cancel inspection"}
        </button>
      )}
      <p role="status">{message}</p>
      {result && (
        <section aria-label="Pytest cache inspection result">
          <h4>Cache records inspected{historical ? " · historical" : ""}</h4>
          <p>
            {result.detectorVersion} · pytest reference{" "}
            {result.referenceVersion} · {formatBytes(result.bytesRead)} read
          </p>
          {result.records.map((record) => (
            <p key={record.entryId}>
              <strong>
                {entryById.get(record.entryId)?.name ??
                  "Previously selected record"}
              </strong>
              {record.kind === "node_ids"
                ? " · node IDs"
                : " · last failed tests"}
              {` · ${record.recordCount} records`}
            </p>
          ))}
        </section>
      )}
      <p>
        A valid format does not show who owns this project, which pytest version
        produced the record, whether the project is idle, or whether the cache
        can be regenerated safely. This inspection does not authorize staging or
        cleanup.
      </p>
    </section>
  );
}
