import {
  formatObservationTime,
  observationClockReversed,
} from "../observation-window";
import { buildAllocationLayout } from "../allocation-layout";
import AllocationMap, {
  type AllocationModel as AllocationMapData,
} from "./AllocationMap";
import type { EditorSnapshot } from "../general-plan-contract";
import "./FolderInventoryPanel.css";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type NameCategory =
  | "image"
  | "document"
  | "archive"
  | "media"
  | "code"
  | "otherFile"
  | "folder"
  | "link"
  | "other"
  | "unreadable";
const nameCategoryLabels: Record<NameCategory, string> = {
  image: "Image extensions",
  document: "Document extensions",
  archive: "Archive extensions",
  media: "Media extensions",
  code: "Code and data extensions",
  otherFile: "Other file names",
  folder: "Folders",
  link: "Symbolic links",
  other: "Other entry kinds",
  unreadable: "Unreadable entries",
};

export type FolderInventory = {
  observationWindow?: {
    startedUnixMs: string | null;
    finishedUnixMs: string | null;
    elapsedMillis: string;
  };
  rootLabel: string;
  allocationMap?: AllocationMapData | null;
  nameCategories?: Array<{
    category: NameCategory;
    entryCount: number;
    knownLogicalBytes: string;
    unknownLogicalSizeCount: number;
  }>;
  pageNumber: number;
  entriesSeen: string;
  canContinue: boolean;
  canCheckpoint: boolean;
  observationId: string;
  breadcrumbs: string[];
  canGoBack: boolean;
  entries: Array<{
    id: string;
    name: string;
    kind: string;
    nameCategory?: NameCategory;
    availability: "dataless" | "unknown";
    navigationBlocker: string | null;
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

export type FolderCheckpoint = {
  checkpointId: string;
  rootLabel: string;
  pageNumber: number;
  entriesSeen: string;
};

export type FolderTreeSummary = {
  observationId: string;
  rootLabel: string;
  entriesObserved: number;
  filesObserved: number;
  uniqueFileObjects: number;
  duplicateHardlinkNames: number;
  directoriesVisited: number;
  skippedEntries: number;
  excludedDirectories: number;
  unreadableEntries: number;
  changedDirectories: number;
  uniqueFileLogicalBytes: string;
  coverageComplete: boolean;
  stopReason: string | null;
};

type SubtreeProgress = FolderTreeSummary & {
  scanId: string;
  canContinue: boolean;
  scanFinished: boolean;
  entriesThisBatch: number;
  coverageBreakdown: {
    hiddenEntries: number;
    symbolicLinks: number;
    nonregularEntries: number;
    crossVolumeEntries: number;
    datalessEntries: number;
    managedBoundaries: number;
    systemBoundaries: number;
    depthLimitedDirectories: number;
    pendingDirectories: number;
    inaccessibleDirectories: number;
  };
  scanReleased?: boolean;
};

type SubtreeCheckpointInfo = {
  checkpointId: string;
  rootLabel: string;
  entriesObserved: number;
  batches: number;
};

function allocationShare(bytes: string, total: bigint): string {
  const basisPoints = (BigInt(bytes) * 10000n) / total;
  return basisPoints === 0n ? "<0.01%" : `${Number(basisPoints) / 100}%`;
}

export default function FolderInventoryPanel({
  native,
  active = true,
  onBusyChange,
  onOpenPlan,
}: {
  native: boolean;
  active?: boolean;
  onOpenPlan?: () => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const titleId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const activeRequest = useRef(false);
  const summaryJobRef = useRef<{
    attemptId: string;
    observationId: string;
    cancelRequested: boolean;
  } | null>(null);
  const cancelInFlight = useRef(false);
  const continueScanRef = useRef(false);
  const [pauseRequested, setPauseRequested] = useState(false);
  const [summaryRunning, setSummaryRunning] = useState(false);
  const [cancelPhase, setCancelPhase] = useState<
    "ready" | "sending" | "requested"
  >("ready");
  const evidenceHeadingRef = useRef<HTMLHeadingElement>(null);
  const inspectorReturnRef = useRef<HTMLButtonElement | null>(null);
  const queryRef = useRef<HTMLInputElement>(null);
  const chooseRef = useRef<HTMLButtonElement>(null);
  const treeHeadingRef = useRef<HTMLHeadingElement>(null);
  const focusChooseAfterClear = useRef(false);
  const [treeSummary, setTreeSummary] = useState<SubtreeProgress | null>(null);
  const [checkpoint, setCheckpoint] = useState<FolderCheckpoint | null>(null);
  const [subtreeCheckpoint, setSubtreeCheckpoint] =
    useState<SubtreeCheckpointInfo | null>(null);
  const [snapshot, setSnapshot] = useState<FolderInventory | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [imported, setImported] = useState(false);
  const [query, setQuery] = useState("");
  const [nameCategory, setNameCategory] = useState<NameCategory | null>(null);
  const [mapGroupIds, setMapGroupIds] = useState<Set<string> | null>(null);
  const listHeadingRef = useRef<HTMLHeadingElement>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [sort, setSort] = useState<"name" | "size" | "allocated">("size");
  useEffect(() => {
    if (active && snapshot) headingRef.current?.focus();
    else if (active && focusChooseAfterClear.current) {
      chooseRef.current?.focus();
      focusChooseAfterClear.current = false;
    }
  }, [snapshot, active]);
  useEffect(() => {
    if (active && selectedId !== null) evidenceHeadingRef.current?.focus();
  }, [selectedId, active]);
  useEffect(() => {
    if (active && treeSummary && !summaryRunning)
      treeHeadingRef.current?.focus();
  }, [treeSummary, active, summaryRunning]);
  const inspect = async (
    entryId?: string | null,
    pageAction?: "next" | "refresh",
  ) => {
    const nextPage = pageAction === "next";
    const refreshing = pageAction === "refresh";
    if (activeRequest.current || !native) return;
    if ((pageAction || entryId !== undefined) && !snapshot) return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      const next =
        entryId === undefined && !pageAction
          ? await invoke<FolderInventory | null>(
              "inspect_selected_folder_entries",
            )
          : refreshing
            ? await invoke<FolderInventory>("refresh_folder_inventory", {
                request: { expectedObservationId: snapshot!.observationId },
              })
            : nextPage
              ? await invoke<FolderInventory>("continue_folder_inventory", {
                  request: { expectedObservationId: snapshot!.observationId },
                })
              : await invoke<FolderInventory>("navigate_folder_inventory", {
                  request: {
                    expectedObservationId: snapshot!.observationId,
                    entryId,
                  },
                });
      if (next) {
        setTreeSummary(null);
        inspectorReturnRef.current = null;
        setSelectedId(null);
        setImported(false);
        setSnapshot(next);
        setQuery("");
        setNameCategory(null);
        setMapGroupIds(null);
        if (refreshing)
          setMessage(
            "Folder refreshed. Paging restarted at the first page; saved plan and review metadata are unchanged.",
          );
      } else
        setMessage(
          "Folder selection canceled. The previous observation is unchanged.",
        );
    } catch {
      if (nextPage) setTreeSummary(null);
      if (nextPage)
        setSnapshot((previous) =>
          previous
            ? { ...previous, canContinue: false, canCheckpoint: false }
            : previous,
        );
      setMessage(
        nextPage
          ? "Enumeration could not continue reliably. The previous page is historical; refresh this folder or choose it again to restart."
          : refreshing
            ? "The current folder could not be refreshed reliably. Its previous page and scope remain unchanged. Choose the folder again if access or identity changed."
            : "The folder could not be inspected. Choose it again to retry. Any previous observation remains below.",
      );
    } finally {
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const checkpointAction = async (
    action: "save" | "resume" | "forget" | "inspect",
  ) => {
    if (
      !native ||
      activeRequest.current ||
      (action === "save" && (!snapshot?.canContinue || !snapshot.canCheckpoint))
    )
      return;
    if (action === "forget" && !checkpoint) return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      if (action === "forget") {
        await invoke("clear_folder_checkpoint", {
          request: { expectedCheckpointId: checkpoint!.checkpointId },
        });
        setCheckpoint(null);
        setMessage(
          "Saved checkpoint forgotten. The current folder and saved review metadata are unchanged.",
        );
      } else {
        const saved = await invoke<FolderCheckpoint | null>(
          "get_folder_checkpoint",
        );
        setCheckpoint(saved);
        if (action === "inspect") {
          setMessage(
            saved
              ? "Saved checkpoint inspected. No folder access was requested."
              : "No saved folder checkpoint is available. Choose a folder to start a new observation.",
          );
        } else if (action === "save") {
          const next = await invoke<FolderCheckpoint>(
            "save_folder_checkpoint",
            {
              request: {
                expectedObservationId: snapshot!.observationId,
                expectedCheckpointId: saved?.checkpointId ?? null,
              },
            },
          );
          setCheckpoint(next);
          setMessage(
            "Folder checkpoint saved locally. Resume requires choosing the same unchanged folder again; your current page remains available.",
          );
        } else if (!saved) {
          setMessage(
            "No saved folder checkpoint is available. Choose a folder to start a new observation.",
          );
        } else {
          const next = await invoke<FolderInventory | null>(
            "resume_folder_checkpoint",
            { request: { expectedCheckpointId: saved.checkpointId } },
          );
          if (next) {
            setTreeSummary(null);
            inspectorReturnRef.current = null;
            setSelectedId(null);
            setImported(false);
            setSnapshot(next);
            setQuery("");
            setNameCategory(null);
            setMapGroupIds(null);
            setMessage(
              "Resumed at the page after the saved checkpoint. Save again to advance the checkpoint; saved plan decisions are unchanged.",
            );
          } else {
            setMessage(
              "Resume selection canceled. The saved checkpoint and any current folder are unchanged.",
            );
          }
        }
      }
    } catch {
      setMessage(
        "The checkpoint operation could not be verified reliably. The current folder view is unchanged. Check the saved checkpoint before trying again; changed folders or exclusions may require a new observation.",
      );
    } finally {
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const subtreeCheckpointAction = async (
    action: "save" | "resume" | "forget" | "inspect",
  ) => {
    if (!native || activeRequest.current) return;
    if (
      action === "save" &&
      (!snapshot ||
        !treeSummary?.canContinue ||
        summaryRunning ||
        treeSummary.observationId !== snapshot.observationId)
    )
      return;
    if (action === "forget" && !subtreeCheckpoint) return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      if (action === "inspect" || action === "resume" || action === "save") {
        const saved = await invoke<SubtreeCheckpointInfo | null>(
          "get_subtree_checkpoint",
        );
        setSubtreeCheckpoint(saved);
        if (action === "inspect") {
          setMessage(
            saved
              ? "Saved recursive checkpoint inspected. No folder access was requested."
              : "No saved recursive checkpoint is available.",
          );
        } else if (action === "save") {
          const next = await invoke<SubtreeCheckpointInfo>(
            "save_subtree_checkpoint",
            {
              request: {
                expectedObservationId: snapshot!.observationId,
                scanId: treeSummary!.scanId,
                expectedCheckpointId: saved?.checkpointId ?? null,
              },
            },
          );
          setSubtreeCheckpoint(next);
          setMessage(
            "Paused recursive scan checkpoint saved. The current folder view is unchanged.",
          );
        } else if (!saved) {
          setMessage("No saved recursive checkpoint is available.");
        } else {
          const resumed = await invoke<{
            snapshot: FolderInventory;
            progress: SubtreeProgress;
          } | null>("resume_subtree_checkpoint", {
            request: { expectedCheckpointId: saved.checkpointId },
          });
          if (!resumed) {
            setMessage(
              "Recursive resume selection canceled. The saved checkpoint and current folder are unchanged.",
            );
          } else if (
            resumed.progress.observationId !== resumed.snapshot.observationId ||
            !resumed.progress.canContinue ||
            !resumed.progress.scanId
          ) {
            setMessage(
              "The returned recursive scan references could not be matched reliably. This historical view is retained; choose the folder again before continuing.",
            );
          } else {
            setTreeSummary(resumed.progress);
            inspectorReturnRef.current = null;
            setSelectedId(null);
            setImported(false);
            setSnapshot(resumed.snapshot);
            setQuery("");
            setNameCategory(null);
            setMapGroupIds(null);
            setMessage(
              "Recursive scan resumed paused. Review its coverage, then explicitly continue scanning when ready.",
            );
          }
        }
      } else {
        await invoke<void>("clear_subtree_checkpoint", {
          request: { expectedCheckpointId: subtreeCheckpoint!.checkpointId },
        });
        setSubtreeCheckpoint(null);
        setMessage(
          "Saved recursive checkpoint forgotten. The current folder and source files are unchanged.",
        );
      }
    } catch {
      setMessage(
        action === "save"
          ? "The recursive checkpoint save could not be verified. The current folder view is unchanged; inspect the saved checkpoint before trying again."
          : "The recursive checkpoint operation could not be verified. The current folder view is unchanged; inspect the saved checkpoint before retrying.",
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
  const clearFolder = async () => {
    if (!native || !snapshot || activeRequest.current) return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    try {
      await invoke<void>("clear_folder_inventory", {
        request: { expectedObservationId: snapshot.observationId },
      });
      focusChooseAfterClear.current = true;
      inspectorReturnRef.current = null;
      setTreeSummary(null);
      setSnapshot(null);
      setSelectedId(null);
      setImported(false);
      setQuery("");
      setNameCategory(null);
      setMapGroupIds(null);
      setMessage(
        "Folder scope cleared. Retained directory handles and enumeration state were released. Saved plan and review metadata remain.",
      );
    } catch {
      setMessage(
        "The folder scope could not be cleared. The historical view is retained; choose the folder again to refresh its state.",
      );
    } finally {
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const summarizeTree = async (resume = false) => {
    if (!native || !snapshot || activeRequest.current) return;
    if (resume && !treeSummary?.canContinue) return;
    activeRequest.current = true;
    continueScanRef.current = true;
    setPauseRequested(false);
    setBusy(true);
    onBusyChange(true);
    setMessage("");
    if (!resume) setTreeSummary(null);
    setSummaryRunning(true);
    let retained = resume ? treeSummary : null;
    try {
      while (continueScanRef.current) {
        const job = {
          attemptId: `disk-steward:summary-attempt:v1:${crypto.randomUUID()}`,
          observationId: snapshot.observationId,
          cancelRequested: false,
        };
        summaryJobRef.current = job;
        setCancelPhase("ready");
        const result = await invoke<SubtreeProgress>(
          retained ? "continue_subtree_scan" : "start_subtree_scan",
          {
            request: {
              expectedObservationId: job.observationId,
              attemptId: job.attemptId,
              ...(retained ? { scanId: retained.scanId } : {}),
            },
          },
        );
        if (
          result.observationId !== snapshot.observationId ||
          (retained && result.scanId !== retained.scanId) ||
          (result.canContinue && !result.scanId)
        )
          throw new Error("Scan identity mismatch");
        setTreeSummary(result);
        retained = result;
        if (job.cancelRequested)
          setMessage(
            "The summary batch completed before cancellation took effect. Remaining progress is paused.",
          );
        if (!result.canContinue) break;
      }
      if (retained?.canContinue && !continueScanRef.current)
        setMessage(
          "Subtree scan paused at a batch boundary. Progress is retained in memory; resume or discard it explicitly.",
        );
    } catch {
      setTreeSummary((previous) =>
        previous ? { ...previous, canContinue: false } : previous,
      );
      setMessage(
        summaryJobRef.current?.cancelRequested
          ? "The summary ended without a reliable result after cancellation was requested. The folder page is unchanged."
          : "The subtree summary could not be completed reliably. The folder page is unchanged. Prior scan counts are historical; start again or discard the scan state.",
      );
    } finally {
      continueScanRef.current = false;
      summaryJobRef.current = null;
      setSummaryRunning(false);
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const pauseScan = () => {
    continueScanRef.current = false;
    setPauseRequested(true);
    setMessage(
      "Pause requested. Waiting for the current metadata batch to finish; an in-flight filesystem call may still finish.",
    );
  };
  const discardScan = async () => {
    if (!native || !snapshot || !treeSummary?.scanId || activeRequest.current)
      return;
    activeRequest.current = true;
    setBusy(true);
    onBusyChange(true);
    try {
      await invoke("discard_subtree_scan", {
        request: {
          expectedObservationId: snapshot.observationId,
          scanId: treeSummary.scanId,
        },
      });
      setTreeSummary((previous) =>
        previous
          ? { ...previous, canContinue: false, scanReleased: true }
          : previous,
      );
      setMessage(
        "Scan state released. Historical counts remain; no source files or saved review metadata changed.",
      );
    } catch {
      setMessage(
        "Scan-state release could not be confirmed. The historical view remains; clear the folder scope to release its retained state.",
      );
    } finally {
      activeRequest.current = false;
      setBusy(false);
      onBusyChange(false);
    }
  };
  const cancelSummary = async () => {
    const job = summaryJobRef.current;
    if (!native || !job || cancelInFlight.current || job.cancelRequested)
      return;
    continueScanRef.current = false;
    cancelInFlight.current = true;
    setCancelPhase("sending");
    try {
      await invoke("cancel_folder_tree_summary", {
        request: {
          expectedObservationId: job.observationId,
          attemptId: job.attemptId,
        },
      });
      if (summaryJobRef.current === job) {
        job.cancelRequested = true;
        setCancelPhase("requested");
        setMessage(
          "Cancellation requested. Waiting for the native summary to stop; an in-flight filesystem call may still finish.",
        );
      }
    } catch {
      if (summaryJobRef.current === job) {
        setCancelPhase("ready");
        setMessage(
          "Cancellation could not be confirmed. Waiting for the summary result; the folder page is unchanged.",
        );
      }
    } finally {
      cancelInFlight.current = false;
    }
  };
  const selected = snapshot?.entries.find((entry) => entry.id === selectedId);
  const entries = [...(snapshot?.entries ?? [])]
    .filter(
      (entry) =>
        entry.name.toLocaleLowerCase().includes(query.toLocaleLowerCase()) &&
        (nameCategory === null || entry.nameCategory === nameCategory) &&
        (mapGroupIds === null || mapGroupIds.has(entry.id)),
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
  const visibleIds = new Set(entries.map((entry) => entry.id));
  const mapEvidence = useMemo(() => {
    const model = snapshot?.allocationMap;
    if (!model || !snapshot) return null;
    try {
      const layout = buildAllocationLayout(model);
      const ids = new Set(layout.tiles.flatMap((tile) => tile.entryIds));
      const observedIds = new Set(snapshot.entries.map((entry) => entry.id));
      const counts = [
        model.unknownAllocationEntries,
        model.sharedLinkEntries,
        model.zeroAllocationEntries,
        model.unmappedNonFileEntries,
        model.datalessEntries,
      ];
      if (
        observedIds.size !== snapshot.entries.length ||
        [...ids].some((id) => !observedIds.has(id)) ||
        counts.some(
          (count) => !Number.isSafeInteger(count) || count < 0 || count > 512,
        ) ||
        counts.reduce((sum, count) => sum + count, ids.size) !==
          snapshot.entries.length
      )
        return null;
      return { ids, total: BigInt(layout.totalBytes) };
    } catch {
      return null;
    }
  }, [snapshot]);
  const mappedIds = mapEvidence?.ids ?? new Set<string>();
  const allocationTotal = mapEvidence?.total ?? null;
  return (
    <section
      className="panel folder-inventory"
      aria-labelledby={titleId}
      aria-busy={busy}
    >
      <h2 id={titleId}>Explore a folder</h2>
      <p>
        Choose a local folder to inspect up to 512 immediate entries per page.
        Open an observed folder explicitly to explore one level deeper. Links
        and volume boundaries are not followed; file contents are not read.
      </p>
      <button
        className="button secondary"
        ref={chooseRef}
        onClick={() => void inspect()}
        disabled={busy || !native}
      >
        {busy ? "Inspecting folder…" : "Choose folder"}
      </button>
      <button
        className="button secondary"
        disabled={busy || !native}
        onClick={() => void checkpointAction("inspect")}
      >
        Inspect saved checkpoint
      </button>
      <button
        className="button secondary"
        disabled={busy || !native}
        onClick={() => void checkpointAction("resume")}
      >
        Resume saved folder scan
      </button>
      <p>
        Explicit checkpoints save a private ordered list of observed entry names
        and directory identity locally, without file contents or an absolute
        folder path. Resume asks you to select the same folder again, validates
        its unchanged identity and enumeration prefix, and starts the next page.
        Changed metadata, entry order or exclusions may refuse resume.
        Checkpoints cover this folder’s paging, not recursive scans or automatic
        access after restart.
      </p>
      <section aria-label="Recursive scan checkpoint">
        <button
          className="button secondary"
          disabled={busy || !native}
          onClick={() => void subtreeCheckpointAction("inspect")}
        >
          Inspect saved recursive checkpoint
        </button>
        <button
          className="button secondary"
          disabled={busy || !native}
          onClick={() => void subtreeCheckpointAction("resume")}
        >
          Resume saved recursive scan
        </button>
        {subtreeCheckpoint && (
          <>
            <p>
              Saved recursive checkpoint: {subtreeCheckpoint.rootLabel} ·{" "}
              {subtreeCheckpoint.entriesObserved} entries across{" "}
              {subtreeCheckpoint.batches} batches.
            </p>
            <button
              className="button secondary"
              disabled={busy || !native}
              onClick={() => void subtreeCheckpointAction("forget")}
            >
              Forget saved recursive checkpoint
            </button>
            <p>
              Forgetting removes only this app’s checkpoint metadata. It does
              not erase source files or change the separate paging checkpoint.
              Forgotten metadata is not securely erased from local storage.
            </p>
          </>
        )}
        <p>
          Resume replays and rechecks the entire saved prefix within a
          cooperative two second limit. It may refuse a changed, reordered, or
          too large tree. The metadata fingerprint does not prove file contents
          or a consistent filesystem snapshot, and it is not cleanup approval.
          The checkpoint stores no source paths. Saving explicitly advances this
          separate recursive checkpoint.
        </p>
      </section>
      {checkpoint && (
        <section aria-label="Saved folder checkpoint">
          <p>
            Saved checkpoint: {checkpoint.rootLabel} · page{" "}
            {checkpoint.pageNumber} · {checkpoint.entriesSeen} observed entry
            records.
          </p>
          <button
            className="button secondary"
            disabled={busy || !native}
            onClick={() => void checkpointAction("forget")}
          >
            Forget saved checkpoint
          </button>
          <p>
            Forgetting removes only this app’s saved scan checkpoint; source
            files and saved plan/review metadata remain.
          </p>
        </section>
      )}
      {!native && <p>Folder inspection is available in the desktop app.</p>}
      <p role="status">{message}</p>
      {snapshot && (
        <>
          <button
            className="button secondary"
            disabled={
              busy ||
              !native ||
              !snapshot.canContinue ||
              !snapshot.canCheckpoint
            }
            onClick={() => void checkpointAction("save")}
          >
            Save folder checkpoint
          </button>
          <p>
            Save only an incomplete, stable paging observation. Checkpoint name
            and memory limits are independent of folder paging. Saving replaces
            the previous checkpoint after a revision check. It does not add
            entries to your plan or save subtree summaries.
          </p>
          {snapshot.canContinue && !snapshot.canCheckpoint && (
            <p>
              Checkpoint tracking is unavailable because its name or memory
              limits were reached. You can continue paging; a refresh starts a
              new observation.
            </p>
          )}
          <button
            className="button secondary"
            disabled={busy || !native}
            onClick={() => void inspect(undefined, "refresh")}
          >
            Refresh current folder
          </button>
          <p>
            Refresh retries metadata access for this retained folder and
            restarts paging at page one. It clears the old subtree summary and
            inspection on success; saved plan decisions and review metadata stay
            unchanged.
          </p>
          <button
            className="button secondary"
            disabled={busy || !native}
            onClick={() => void clearFolder()}
          >
            Clear folder scope
          </button>
          <p>
            Clearing releases this app’s retained directory handles and
            in-memory observations. Previously saved plan and review metadata
            remain.
          </p>
          <button
            className="button secondary"
            disabled={busy || !native}
            onClick={() => void summarizeTree()}
          >
            Summarize subtree metadata
          </button>
          {summaryRunning && (
            <button
              className="button secondary"
              disabled={pauseRequested}
              onClick={pauseScan}
            >
              {pauseRequested ? "Pause requested" : "Pause subtree scan"}
            </button>
          )}
          {summaryRunning && (
            <button
              className="button secondary"
              disabled={cancelPhase !== "ready"}
              onClick={() => void cancelSummary()}
            >
              {cancelPhase === "requested"
                ? "Cancellation requested"
                : cancelPhase === "sending"
                  ? "Requesting cancellation…"
                  : "Cancel subtree summary"}
            </button>
          )}
          <p>
            Cancellation is cooperative; the page remains busy until the native
            summary finishes. It cannot interrupt an in-flight filesystem call.
          </p>
          <p>
            Explicit read-only subtree scan: sequential batches of up to 512
            entries, at most 65,536 entries and 16 levels, with cooperative
            two-second checks per batch. Continuation expires after 15 minutes;
            idle scan state remains until discarded, the folder scope changes,
            or the app exits. Pause waits for a batch boundary; retained subtree
            progress does not survive app restart. File contents are not read.
            Links and unsupported boundaries are skipped; coverage gaps remain
            visible. Remembered exclusions are verified for this summary;
            uncertain rules or an excluded selected scope refuse the summary.
            The immediate-page metadata view remains separate from this scan
            policy.
          </p>
          {treeSummary && (
            <section
              className="inventory-evidence"
              aria-label="Subtree metadata summary"
            >
              <h3 ref={treeHeadingRef} tabIndex={-1}>
                Subtree observation · {treeSummary.rootLabel}
              </h3>
              <p role="status">
                {treeSummary.coverageComplete
                  ? "Traversal finished within the bounded policy."
                  : "Incomplete subtree coverage."}{" "}
                {treeSummary.stopReason
                  ? (summaryReasons[treeSummary.stopReason] ??
                    "Some metadata could not be observed reliably.")
                  : ""}
              </p>
              <p>
                {treeSummary.canContinue
                  ? summaryRunning
                    ? "Scanning; more metadata remains."
                    : "Paused; more metadata remains."
                  : treeSummary.scanFinished
                    ? "Traversal finished within its policy; coverage gaps may remain."
                    : "Scan stopped or ended without resumable progress."}
              </p>
              <button
                className="button secondary"
                disabled={
                  busy ||
                  !native ||
                  !treeSummary.canContinue ||
                  summaryRunning ||
                  treeSummary.observationId !== snapshot.observationId
                }
                onClick={() => void subtreeCheckpointAction("save")}
              >
                Save paused recursive scan
              </button>
              {treeSummary.canContinue && !summaryRunning && (
                <button
                  className="button secondary"
                  disabled={busy || !native}
                  onClick={() => void summarizeTree(true)}
                >
                  Resume subtree scan
                </button>
              )}
              {treeSummary.scanId &&
                !treeSummary.scanFinished &&
                !treeSummary.scanReleased &&
                !summaryRunning && (
                  <button
                    className="button secondary"
                    disabled={busy || !native}
                    onClick={() => void discardScan()}
                  >
                    Discard subtree scan state
                  </button>
                )}
              <dl>
                <dt>Entries in latest batch</dt>
                <dd>{treeSummary.entriesThisBatch}</dd>
                <dt>Observed entry records</dt>
                <dd>{treeSummary.entriesObserved}</dd>
                <dt>Regular-file names</dt>
                <dd>{treeSummary.filesObserved}</dd>
                <dt>Unique observed file objects</dt>
                <dd>{treeSummary.uniqueFileObjects}</dd>
                <dt>Repeated hard-link names</dt>
                <dd>{treeSummary.duplicateHardlinkNames}</dd>
                <dt>Directories visited</dt>
                <dd>{treeSummary.directoriesVisited}</dd>
                <dt>Remembered excluded directories</dt>
                <dd>{treeSummary.excludedDirectories}</dd>
                <dt>Skipped entries, including remembered exclusions</dt>
                <dd>{treeSummary.skippedEntries}</dd>
                <dt>Unreadable entries</dt>
                <dd>{treeSummary.unreadableEntries}</dd>
                <dt>Changed directories</dt>
                <dd>{treeSummary.changedDirectories}</dd>
                <dt>Observed file-object logical bytes</dt>
                <dd>{treeSummary.uniqueFileLogicalBytes}</dd>
              </dl>
              {treeSummary.coverageBreakdown && (
                <section aria-label="Subtree coverage details">
                  <h4>Why entries were skipped</h4>
                  <dl>
                    <dt>Hidden entries</dt>
                    <dd>{treeSummary.coverageBreakdown.hiddenEntries}</dd>
                    <dt>Symbolic links</dt>
                    <dd>{treeSummary.coverageBreakdown.symbolicLinks}</dd>
                    <dt>Other nonregular entries</dt>
                    <dd>{treeSummary.coverageBreakdown.nonregularEntries}</dd>
                    <dt>Volume crossings</dt>
                    <dd>{treeSummary.coverageBreakdown.crossVolumeEntries}</dd>
                    <dt>Known dataless entries</dt>
                    <dd>{treeSummary.coverageBreakdown.datalessEntries}</dd>
                    <dt>Managed directory boundaries</dt>
                    <dd>{treeSummary.coverageBreakdown.managedBoundaries}</dd>
                    <dt>System directory boundaries</dt>
                    <dd>{treeSummary.coverageBreakdown.systemBoundaries}</dd>
                    <dt>Directories beyond the depth limit</dt>
                    <dd>
                      {treeSummary.coverageBreakdown.depthLimitedDirectories}
                    </dd>
                    <dt>Directories denied at open</dt>
                    <dd>
                      {treeSummary.coverageBreakdown.inaccessibleDirectories}
                    </dd>
                    <dt>Unfinished directory streams</dt>
                    <dd>{treeSummary.coverageBreakdown.pendingDirectories}</dd>
                  </dl>
                  <p>
                    Each skipped entry has one primary reason. Remembered
                    exclusions and unreadable entries are counted separately
                    above. These counts describe encountered boundaries, not
                    their hidden contents. Unfinished streams count only the
                    known traversal frontier, not all remaining directories or
                    files. A zero count does not prove that an area is clean.
                  </p>
                  {treeSummary.coverageBreakdown.inaccessibleDirectories >
                    0 && (
                    <p>
                      Some child directories denied read access and were not
                      opened. Their contents remain unknown. The scan continued
                      through readable siblings without retrying denied folders.
                      After changing access, refresh the current folder and
                      start a new subtree scan to retry.
                    </p>
                  )}
                </section>
              )}
              <p>
                Historical metadata observation, not a consistent filesystem
                snapshot. Native file-object identities deduplicate hard-link
                names only within this bounded walk. Shared extents, clones,
                snapshots and skipped contents remain unknown. This is not
                occupied or reclaimable space and creates no cleanup
                eligibility.
              </p>
              <p>
                The current-page list, size-bar denominator and metadata import
                remain unchanged. Summary totals do not add items to your plan.
              </p>
            </section>
          )}
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
          <section aria-label="Observation timing">
            <h4>Observation timing</h4>
            <dl>
              <dt>Scan started (UTC)</dt>
              <dd>
                {formatObservationTime(
                  snapshot.observationWindow?.startedUnixMs,
                ) ?? "Unavailable"}
              </dd>
              <dt>Scan finished (UTC)</dt>
              <dd>
                {formatObservationTime(
                  snapshot.observationWindow?.finishedUnixMs,
                ) ?? "Unavailable"}
              </dd>
              <dt>Measured page duration</dt>
              <dd>
                {snapshot.observationWindow
                  ? `${snapshot.observationWindow.elapsedMillis} milliseconds`
                  : "Unavailable"}
              </dd>
            </dl>
            <p>
              Times use this device’s clock and describe this page only. Clock
              settings can change; timestamps do not guarantee that evidence is
              current. Files may change after observation. These times stay
              fixed while reviewing and accompany imported metadata.
            </p>
            {observationClockReversed(
              snapshot.observationWindow?.startedUnixMs,
              snapshot.observationWindow?.finishedUnixMs,
            ) && (
              <p>
                The device clock moved backward during observation. Use the
                measured duration; the recorded clock window is uncertain.
              </p>
            )}
          </section>
          <button
            className="button secondary"
            onClick={() => void inspect(null)}
            disabled={busy || !snapshot.canGoBack}
          >
            Back to parent folder
          </button>
          <p role="status">
            {snapshot.enumerationComplete
              ? snapshot.pageNumber > 1
                ? "Reached the end of the directory. Only the current page is displayed."
                : "Immediate entry enumeration finished."
              : "Incomplete immediate entry enumeration."}{" "}
            {snapshot.stopReason
              ? (coverageReasons[snapshot.stopReason] ??
                "Some metadata could not be observed reliably.")
              : ""}
          </p>
          <p>
            Page {snapshot.pageNumber} · {snapshot.entriesSeen} entries observed
            across pages. Each page is a separate historical observation;
            earlier pages are not retained in this list.
          </p>
          <button
            className="button secondary"
            disabled={busy || !snapshot.canContinue}
            onClick={() => void inspect(undefined, "next")}
          >
            Next page
          </button>
          <p>
            {snapshot.entries.length} observed entries on this page ·{" "}
            {snapshot.unreadableCount} unreadable entries ·{" "}
            {snapshot.directoriesNotTraversed} folders not traversed
          </p>
          <p>
            Current-page regular-file logical sizes total{" "}
            {snapshot.regularFileLogicalBytes} bytes. Hard links are counted per
            name. This is neither occupied disk space nor recoverable space;
            nested contents and shared allocation are unknown.
          </p>
          {snapshot.nameCategories && snapshot.nameCategories.length > 0 && (
            <section aria-label="Current-page name categories">
              <h4>Current-page name categories</h4>
              <p>
                File categories use name extensions, not content inspection.
                Counts and known logical bytes include the whole current page
                and stay fixed while filtering or sorting. Hard links count per
                name; unknown sizes are listed separately. Categories do not
                establish occupied space, savings or safe removal.
              </p>
              <button
                className="button secondary"
                disabled={busy}
                aria-pressed={nameCategory === null}
                onClick={() => setNameCategory(null)}
              >
                Show all name categories
              </button>
              <dl>
                {snapshot.nameCategories.map((group) => (
                  <div key={group.category}>
                    <dt>
                      <button
                        className="button secondary"
                        disabled={busy}
                        aria-pressed={nameCategory === group.category}
                        onClick={() => setNameCategory(group.category)}
                      >
                        {nameCategoryLabels[group.category]}
                      </button>
                    </dt>
                    <dd>
                      {group.entryCount} entries · {group.knownLogicalBytes}{" "}
                      known logical bytes · {group.unknownLogicalSizeCount}{" "}
                      entries with unknown logical size
                    </dd>
                  </div>
                ))}
              </dl>
              {nameCategory !== null && (
                <p role="status">
                  List filtered to {nameCategoryLabels[nameCategory]}. Text
                  search also applies. Page import still includes the whole
                  page; category filtering does not change your plan or evidence
                  selection.
                </p>
              )}
            </section>
          )}
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
              Add this page to your plan
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
            from this page, including entries hidden by the filter. It does not
            save source paths or grant cleanup permission. The separate plan
            starts empty; all imported entries default to Keep and stay blocked.
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
            entries. Filtering does not change coverage or the allocation
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
          <AllocationMap
            allocation={
              mapEvidence ? (snapshot.allocationMap ?? undefined) : undefined
            }
            entries={snapshot.entries}
            visibleIds={visibleIds}
            selectedId={selectedId}
            busy={busy}
            onSelect={(id, target) => {
              inspectorReturnRef.current = target;
              setSelectedId(id);
            }}
            onGroup={(ids) => {
              setQuery("");
              setNameCategory(null);
              setMapGroupIds(new Set(ids));
              listHeadingRef.current?.focus();
            }}
          />
          <h4 ref={listHeadingRef} tabIndex={-1}>
            Observed entries
          </h4>
          {mapGroupIds !== null && (
            <p>
              Showing a grouped map area in the list. Other filters also apply.{" "}
              <button
                className="button secondary"
                disabled={busy}
                onClick={() => setMapGroupIds(null)}
              >
                Show all map entries
              </button>
            </p>
          )}
          <div style={{ overflowX: "auto" }}>
            <table>
              <caption>
                Read-only metadata observation. All entries remain unavailable
                for cleanup. Allocation bars use the current page’s{" "}
                {allocationTotal === null
                  ? "unavailable"
                  : allocationTotal.toString()}{" "}
                eligible reported allocated bytes as their unfiltered
                denominator. Shared links, unknown allocation and folder
                contents have no bar. This is not unique occupied or reclaimable
                space.
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
                  <tr
                    key={entry.id}
                    className={
                      selectedId === entry.id
                        ? "inventory-row-selected"
                        : undefined
                    }
                  >
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
                      {entry.navigationBlocker && (
                        <small className="inventory-boundary-reason">
                          {navigationReasons[entry.navigationBlocker] ??
                            "Folder traversal is unavailable."}
                        </small>
                      )}
                    </th>
                    <td>
                      {entry.kind}
                      {entry.availability === "dataless" && (
                        <small> · dataless object</small>
                      )}
                    </td>
                    <td>{entry.logicalBytes ?? "Unknown"}</td>
                    <td className="inventory-size">
                      {mappedIds.has(entry.id) &&
                        allocationTotal !== null &&
                        allocationTotal > 0n &&
                        entry.allocatedBytes !== null && (
                          <span
                            className="inventory-size-bar"
                            aria-hidden="true"
                            style={{
                              width: `${Number((BigInt(entry.allocatedBytes) * 10000n) / allocationTotal) / 100}%`,
                            }}
                          />
                        )}
                      <span className="inventory-size-value">
                        {entry.allocatedBytes ?? "Unknown"}
                        {mappedIds.has(entry.id) &&
                          allocationTotal !== null &&
                          allocationTotal > 0n &&
                          entry.allocatedBytes !== null && (
                            <small>
                              {" "}
                              ·{" "}
                              {allocationShare(
                                entry.allocatedBytes,
                                allocationTotal,
                              )}{" "}
                              of mapped page allocation
                            </small>
                          )}
                        {!mappedIds.has(entry.id) && (
                          <small> · not represented by area</small>
                        )}
                      </span>
                    </td>
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
                Return to selected entry
              </button>
              <dl>
                <dt>Entry kind</dt>
                <dd>{selected.kind}</dd>
                <dt>Content availability</dt>
                <dd>
                  {selected.availability === "dataless"
                    ? "Known dataless object; provider and hydration behavior unverified."
                    : "Unknown; local content availability has not been established."}
                </dd>
                <dt>Folder traversal</dt>
                <dd>
                  {selected.navigationBlocker
                    ? (navigationReasons[selected.navigationBlocker] ??
                      "Folder traversal is unavailable.")
                    : selected.canOpen
                      ? "Explicit folder navigation available."
                      : "Not a navigable folder entry."}
                </dd>
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
          {entries.length === 0 && (query || nameCategory) && (
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
  page_limit:
    "The page limit was reached. Coverage remains incomplete; choose a narrower folder to inspect further.",
  time_limit: "The observation time limit was reached.",
  entry_limit: "The entry limit was reached; more entries may exist.",
  directory_read_error: "The folder iterator became unreadable.",
  entry_metadata_unavailable: "Some entry metadata was unavailable.",
  logical_size_overflow: "The logical size total exceeded the supported range.",
  root_metadata_changed:
    "The folder changed during inspection; inspect it again.",
  root_metadata_unavailable: "The folder metadata could not be rechecked.",
};

const navigationReasons: Record<string, string> = {
  dataless:
    "Dataless directory: enumeration is blocked to avoid requesting its contents.",
  cross_device: "Different volume: traversal is blocked.",
  hidden: "Hidden folder: traversal is blocked by default.",
  managed_boundary:
    "Application or managed library boundary: traversal is blocked.",
  depth_limit: "Navigation depth limit reached.",
};

const summaryReasons: Record<string, string> = {
  entry_limit: "The entry limit was reached.",
  depth_limit: "The depth limit was reached.",
  time_limit: "The observation time limit was reached.",
  logical_size_overflow: "The logical-byte total exceeded the supported range.",
  directory_changed: "A directory changed during inspection.",
  directory_access_denied:
    "Some child directories denied read access; their contents remain unknown.",
};
