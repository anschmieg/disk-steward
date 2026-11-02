import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertCircle,
  ArrowLeft,
  Check,
  CircleHelp,
  FilePlus2,
  HardDrive,
  RefreshCw,
  ShieldCheck,
} from "lucide-react";
import type {
  DemoExecutionResult,
  DemoPanelMode,
  DemoPlanProjection,
  DemoStatus,
  PreparedDemoReview,
} from "../demo-contract";
import "./DemoLifecyclePanel.css";

type Props = {
  plan: DemoPlanProjection;
  native: boolean;
  planBusy: boolean;
  planConfirmed: boolean;
  onPlanRefresh: () => Promise<void>;
  onBusyChange: (busy: boolean) => void;
  onModeChange: (mode: DemoPanelMode) => void;
};

export default function DemoLifecyclePanel({
  plan,
  native,
  planBusy,
  planConfirmed,
  onPlanRefresh,
  onBusyChange,
  onModeChange,
}: Props) {
  const [status, setStatus] = useState<DemoStatus | null>(null);
  const [prepared, setPrepared] = useState<PreparedDemoReview | null>(null);
  const [result, setResult] = useState<DemoExecutionResult | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [needsRefresh, setNeedsRefresh] = useState(false);
  const lastError = useRef<unknown>(null);
  const busy = pending || planBusy;
  const actionsById = useMemo(
    () => new Map(plan.actions.map((action) => [action.id, action])),
    [plan.actions],
  );
  const selected = plan.actions.filter(
    (action) => action.decision === "include",
  );

  const run = useCallback(
    async <T,>(task: () => Promise<T>): Promise<T | undefined> => {
      if (!native || busy) return undefined;
      setPending(true);
      onBusyChange(true);
      setError("");
      setNotice("");
      lastError.current = null;
      try {
        return await task();
      } catch (e) {
        lastError.current = e;
        setError(messageFor(e));
        return undefined;
      } finally {
        setPending(false);
        onBusyChange(false);
      }
    },
    [busy, native, onBusyChange],
  );

  const refreshStatus = useCallback(
    async (alsoRefreshPlan = false) => {
      const next = await run(() =>
        invoke<DemoStatus>("get_fixture_demo_status"),
      );
      if (next) {
        setStatus(next);
        setPrepared(null);
        if (alsoRefreshPlan) {
          try {
            await onPlanRefresh();
            setNeedsRefresh(false);
            setNotice("Latest plan and demo status loaded.");
          } catch {
            setNeedsRefresh(true);
            setError(
              "The plan could not be refreshed. Refresh both plan and demo status before continuing.",
            );
          }
        } else {
          setNeedsRefresh(false);
          setNotice("Latest demo status loaded.");
        }
      } else setNeedsRefresh(true);
    },
    [onPlanRefresh, run],
  );

  useEffect(() => {
    if (!native) return;
    let active = true;
    invoke<DemoStatus>("get_fixture_demo_status")
      .then((value) => {
        if (active) setStatus(value);
      })
      .catch((e) => {
        if (active) {
          setError(messageFor(e));
          setNeedsRefresh(true);
        }
      });
    return () => {
      active = false;
    };
  }, [native]);

  const start = async () => {
    const next = await run(() => invoke<DemoStatus>("start_fixture_demo"));
    if (next) {
      setStatus(next);
      setPrepared(null);
      setNeedsRefresh(next.state === "needs_inspection");
    } else setNeedsRefresh(true);
  };

  const prepareStage = async () => {
    const next = await run(() =>
      invoke<PreparedDemoReview>("prepare_fixture_stage", {
        request: { expectedRevisionId: plan.revisionId },
      }),
    );
    if (next) {
      setPrepared(next);
      setResult(null);
    } else setNeedsRefresh(true);
  };

  const prepareRestore = async (receiptId: string) => {
    const next = await run(() =>
      invoke<PreparedDemoReview>("prepare_fixture_restore", {
        request: { expectedRevisionId: plan.revisionId, receiptId },
      }),
    );
    if (next) {
      setPrepared(next);
      setResult(null);
    } else setNeedsRefresh(true);
  };

  const confirmWithNativeDialog = async () => {
    if (!prepared || needsRefresh) return;
    const completed = await run(async () => {
      const execution = await invoke<DemoExecutionResult>(
        "request_fixture_execution",
        { request: { reviewId: prepared.reviewId } },
      );
      const latest =
        execution.state === "finished"
          ? await invoke<DemoStatus>("get_fixture_demo_status").catch(
              () => null,
            )
          : null;
      return { execution, latest };
    });
    if (completed) {
      const { execution, latest } = completed;
      setResult(execution);
      setPrepared(null);
      if (execution.state === "uncertain" || execution.state === "partial")
        setNeedsRefresh(true);
      else {
        if (latest) setStatus(latest);
        else setNeedsRefresh(true);
      }
    } else {
      const code = errorCode(lastError.current);
      if (code === "confirmation_cancelled") {
        setPrepared(null);
        setError("");
        setNotice("Confirmation cancelled. No demo files were changed.");
      } else setNeedsRefresh(true);
    }
  };

  if (!native)
    return (
      <section className="demo-panel" aria-labelledby="demo-title">
        <div className="demo-topline">
          <div className="demo-icon">
            <HardDrive size={17} />
          </div>
          <span className="demo-label">DISPOSABLE FILE DEMO</span>
        </div>
        <h2 id="demo-title">File staging is available in the desktop app</h2>
        <p className="demo-intro">
          The browser preview cannot create files, stage them, or restore them.
          Your example plan remains a non-executable preview.
        </p>
        <button
          className="demo-button secondary"
          onClick={() => onModeChange("plan")}
        >
          <ArrowLeft size={15} />
          Return to plan
        </button>
      </section>
    );

  const canPrepareStage =
    status?.state === "ready" &&
    planConfirmed &&
    !needsRefresh &&
    !busy &&
    selected.length > 0 &&
    selected.every((action) => {
      const fixture = status.fixtures.find(
        (item) => item.actionId === action.id,
      );
      return fixture?.status === "available" || fixture?.status === "restored";
    });
  const canStart = status?.state === "not_started" && !needsRefresh && !busy;
  return (
    <section className="demo-panel" aria-labelledby="demo-title">
      <header className="demo-heading">
        <div>
          <div className="demo-topline">
            <div className="demo-icon">
              <HardDrive size={17} />
            </div>
            <span className="demo-label">DISPOSABLE FILE DEMO</span>
          </div>
          <h2 id="demo-title">Try the full review and restore cycle</h2>
          <p className="demo-intro">
            Disk Steward creates five new disposable files inside its private
            demo area. Existing files are never used as targets.
          </p>
        </div>
        <button
          className="demo-button secondary"
          onClick={() => onModeChange("plan")}
          disabled={busy}
        >
          <ArrowLeft size={15} />
          Back to plan
        </button>
      </header>

      {error && (
        <div className="demo-alert" role="alert">
          <AlertCircle size={16} />
          <span>{error}</span>
          <button onClick={() => void refreshStatus(true)} disabled={busy}>
            <RefreshCw size={14} />
            Refresh
          </button>
        </div>
      )}
      {needsRefresh && !error && (
        <div className="demo-alert" role="status">
          <AlertCircle size={16} />
          <span>
            Refresh both plan and demo status before starting another step.
          </span>
          <button onClick={() => void refreshStatus(true)} disabled={busy}>
            <RefreshCw size={14} />
            Refresh
          </button>
        </div>
      )}
      {notice && (
        <p className="demo-notice" role="status">
          {notice}
        </p>
      )}

      {!status ? (
        <div className="demo-state">
          <span>
            {error ? "Demo status needs attention." : "Loading demo status…"}
          </span>
          <button
            className="demo-button secondary"
            onClick={() => void refreshStatus()}
            disabled={busy}
          >
            <RefreshCw size={14} />
            Refresh status
          </button>
        </div>
      ) : (
        <>
          {status.state === "not_started" && (
            <div className="demo-empty">
              <div className="empty-mark">
                <FilePlus2 size={19} />
              </div>
              <div>
                <b>No demo files created yet</b>
                <p>
                  This creates five new files totaling 4.875 MiB. It will not
                  reuse any existing files.
                </p>
              </div>
              <button
                className="demo-button primary"
                onClick={() => void start()}
                disabled={!canStart}
              >
                <FilePlus2 size={15} />
                {pending ? "Creating demo…" : "Create demo files"}
              </button>
            </div>
          )}
          {status.state === "needs_inspection" && (
            <div className="demo-inspection">
              <AlertCircle size={18} />
              <div>
                <b>This demo needs inspection</b>
                <p>
                  Refresh to inspect the latest recorded status. Refreshing will
                  not retry or repair anything; if inspection is still needed,
                  the app will keep file operations unavailable.
                </p>
              </div>
              <button
                className="demo-button secondary"
                onClick={() => void refreshStatus()}
                disabled={busy}
              >
                <RefreshCw size={14} />
                Refresh status
              </button>
            </div>
          )}

          {status.state === "ready" && (
            <>
              <div className="demo-statline">
                <div>
                  <span>Demo files</span>
                  <b>{status.fixtures.length} / 5</b>
                </div>
                <div>
                  <span>Demo size</span>
                  <b>
                    {formatBytes(
                      status.fixtures.reduce(
                        (sum, file) => sum + file.bytes,
                        0,
                      ),
                    )}
                  </b>
                </div>
                <div>
                  <span>Space recovered</span>
                  <b>0 bytes</b>
                </div>
              </div>
              <div className="demo-files" aria-label="Disposable demo files">
                {status.fixtures.map((file) => {
                  const action = actionsById.get(file.actionId);
                  const share =
                    status.fixtures.reduce(
                      (sum, item) => sum + item.bytes,
                      0,
                    ) || 1;
                  return (
                    <div className="demo-file" key={file.actionId}>
                      <span className="demo-category" />
                      <span className="demo-file-name">
                        {action?.label ?? "Demo file"}
                        <small>{humanStatus(file.status)}</small>
                      </span>
                      <span className="demo-size-bar" aria-hidden="true">
                        <i
                          style={{ width: `${(file.bytes / share) * 100}%` }}
                        />
                      </span>
                      <b>{formatBytes(file.bytes)}</b>
                      {file.receiptId && file.status === "staged" && (
                        <>
                          <button
                            className="restore-link"
                            onClick={() => void prepareRestore(file.receiptId!)}
                            disabled={
                              busy ||
                              needsRefresh ||
                              status.state !== "ready" ||
                              !planConfirmed ||
                              action?.decision !== "include"
                            }
                          >
                            Prepare restore
                          </button>
                          {action?.decision !== "include" && (
                            <span className="restore-help">
                              Include this action in the plan first
                            </span>
                          )}
                        </>
                      )}
                    </div>
                  );
                })}
              </div>
              <div className="stage-row">
                <div>
                  <b>Stage the included demo files</b>
                  <span>
                    {selected.length} included ·{" "}
                    {formatBytes(plan.selectedBytes)} · no user files are
                    involved
                  </span>
                  {selected.length === 0 ? (
                    <small className="stage-help">
                      Include at least one action in the plan first.
                    </small>
                  ) : (
                    !canPrepareStage && (
                      <small className="stage-help">
                        Include actions whose demo files are available or
                        restored to prepare staging.
                      </small>
                    )
                  )}
                </div>
                <button
                  className="demo-button primary"
                  onClick={() => void prepareStage()}
                  disabled={!canPrepareStage}
                >
                  {pending ? "Preparing review…" : "Review selected demo files"}
                </button>
              </div>
            </>
          )}
        </>
      )}

      {prepared && (
        <div className="prepared-review" aria-labelledby="prepared-title">
          <div className="prepared-title">
            <ShieldCheck size={17} />
            <div>
              <span>PREPARED {prepared.operation.toUpperCase()} REVIEW</span>
              <h3 id="prepared-title">Check this operation</h3>
            </div>
          </div>
          <p className="prepared-consequence">
            {prepared.operation === "stage"
              ? "These demo files will move into the app's recoverable holding area. Staging does not free space."
              : "This demo file will be restored to its original demo location. Existing files are protected from overwrite."}
          </p>
          <div className="prepared-items">
            {prepared.items.map((item) => (
              <div key={item.actionId}>
                <span>{item.label}</span>
                <b>{formatBytes(item.bytes)}</b>
              </div>
            ))}
          </div>
          <div className="prepared-total">
            <span>
              {prepared.items.length}{" "}
              {prepared.items.length === 1 ? "file" : "files"}
            </span>
            <b>{formatBytes(prepared.selectedBytes)}</b>
          </div>
          <p className="expiry-note">
            Review expires{" "}
            {new Date(prepared.expiresAtMs).toLocaleTimeString([], {
              hour: "numeric",
              minute: "2-digit",
            })}
            . The desktop app will show a native confirmation with the exact
            operation and recovery consequences.
          </p>
          <div className="prepared-actions">
            <button
              className="demo-button secondary"
              onClick={() => setPrepared(null)}
              disabled={busy}
            >
              Cancel
            </button>
            <button
              className="demo-button primary"
              onClick={() => void confirmWithNativeDialog()}
              disabled={busy || needsRefresh}
            >
              {pending
                ? "Waiting for confirmation…"
                : "Continue to native confirmation"}
            </button>
          </div>
        </div>
      )}

      {result && <ExecutionOutcome result={result} labels={actionsById} />}
      {status?.lastExecution && !result && (
        <ExecutionOutcome result={status.lastExecution} labels={actionsById} />
      )}
      {status && (
        <div className="demo-safety">
          <CircleHelp size={15} />
          <span>
            Demo files only · no permanent deletion · restoring does not recover
            disk space
          </span>
        </div>
      )}
    </section>
  );
}

function ExecutionOutcome({
  result,
  labels,
}: {
  result: DemoExecutionResult;
  labels: Map<string, DemoPlanProjection["actions"][number]>;
}) {
  const partial = result.state === "partial";
  const uncertain = result.state === "uncertain";
  return (
    <div
      className={`execution-outcome ${uncertain ? "uncertain" : partial ? "partial" : "finished"}`}
      role="status"
    >
      <div className="outcome-title">
        {uncertain ? <AlertCircle size={16} /> : <Check size={16} />}
        <b>
          {uncertain
            ? "Outcome needs inspection"
            : partial
              ? "Partial result"
              : "Operation complete"}
        </b>
      </div>
      <p>
        {uncertain
          ? "Some results could not be confirmed. Refresh demo status before taking another step; nothing will be retried automatically."
          : partial
            ? "Some demo files completed and others did not. Review each result below before continuing."
            : result.operation === "stage"
              ? "The selected demo files are now staged and can be restored individually."
              : "The selected demo file has been restored to the demo area."}
      </p>
      <div className="outcome-metrics">
        <span>
          {result.operation === "stage" ? "Staged" : "Restored"}:{" "}
          {formatBytes(
            result.operation === "stage"
              ? (result.stagedBytes ?? 0)
              : (result.restoredBytes ?? 0),
          )}
        </span>
        <span>Space recovered: {formatBytes(result.recoveredBytes)}</span>
      </div>
      {(partial || uncertain) && (
        <div className="outcome-items">
          {result.items.map((item) => (
            <div key={item.actionId}>
              <span>{labels.get(item.actionId)?.label ?? "Demo file"}</span>
              <b>{humanStatus(item.status)}</b>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} ${bytes === 1 ? "byte" : "bytes"}`;
  return bytes >= 1_048_576
    ? `${(bytes / 1_048_576).toFixed(3).replace(/0+$/, "").replace(/\.$/, "")} MiB`
    : `${Math.round(bytes / 1024)} KiB`;
}
function humanStatus(status: string): string {
  return status.replaceAll("_", " ").replace(/^./, (c) => c.toUpperCase());
}
function errorCode(error: unknown): string {
  return error &&
    typeof error === "object" &&
    "code" in error &&
    typeof error.code === "string"
    ? error.code
    : "";
}
function messageFor(error: unknown): string {
  const code = errorCode(error);
  if (code === "confirmation_cancelled")
    return "Native confirmation was cancelled.";
  if (code === "stale_revision")
    return "The plan changed before this review could be prepared. Refresh the plan and demo status.";
  if (code === "review_expired")
    return "This prepared review expired. Refresh status and prepare a new review.";
  if (code === "needs_inspection" || code === "uncertain_outcome")
    return "The demo state needs inspection. Refresh status before continuing.";
  if (code === "storage_unavailable")
    return "The demo service is unavailable. Refresh status before continuing.";
  return "The operation could not be confirmed. Refresh demo status before continuing.";
}
