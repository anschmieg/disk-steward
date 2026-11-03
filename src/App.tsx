import FolderInventoryPanel from "./components/FolderInventoryPanel";
import GeneralPlanWorkspace, {
  type GeneralPlanSummary,
} from "./components/GeneralPlanWorkspace";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import {
  Archive,
  ArrowLeft,
  Check,
  ChevronRight,
  CircleAlert,
  FilePlus2,
  HardDrive,
  RefreshCw,
  ShieldCheck,
} from "lucide-react";
import DemoLifecyclePanel from "./components/DemoLifecyclePanel";

type Decision = "keep" | "include";
type Category = "build" | "cache";
type Evidence = { id: string; summary: string };
type Action = {
  id: string;
  label: string;
  category: Category;
  bytes: number;
  decision: Decision;
  evidence: Evidence[];
};
type Plan = {
  schemaVersion: 1;
  kind: "non_executable_example";
  planId: string;
  revisionId: string;
  revisionNumber: number;
  actions: Action[];
  totalBytes: number;
  selectedBytes: number;
  selectedCount: number;
  executionAvailable: false;
};
type Review = {
  schemaVersion: 1;
  kind: "non_executable_example";
  reviewId: string;
  planId: string;
  revisionId: string;
  revisionNumber: number;
  manifestSha256: string;
  actions: Action[];
  totalBytes: number;
  selectedBytes: number;
  selectedCount: number;
  executionAvailable: false;
};

// Browser fixture mirrors the documented five-row contract and lives only in memory.
const browserPlan: Plan = {
  schemaVersion: 1,
  kind: "non_executable_example",
  planId: "browser-preview",
  revisionId: "preview-revision-1",
  revisionNumber: 1,
  actions: [
    {
      id: "compiler-output",
      label: "Compiler output",
      category: "build",
      bytes: 3_145_728,
      decision: "keep",
      evidence: [
        {
          id: "e-compiler",
          summary: "Synthetic disposable compiler output for this example.",
        },
      ],
    },
    {
      id: "test-cache",
      label: "Test cache",
      category: "cache",
      bytes: 1_048_576,
      decision: "keep",
      evidence: [
        {
          id: "e-test",
          summary: "Synthetic disposable test cache for this example.",
        },
      ],
    },
    {
      id: "preview-cache",
      label: "Preview cache",
      category: "cache",
      bytes: 524_288,
      decision: "keep",
      evidence: [
        {
          id: "e-preview",
          summary: "Synthetic disposable preview cache for this example.",
        },
      ],
    },
    {
      id: "package-index",
      label: "Package index",
      category: "cache",
      bytes: 262_144,
      decision: "keep",
      evidence: [
        {
          id: "e-package",
          summary: "Synthetic disposable package index for this example.",
        },
      ],
    },
    {
      id: "incremental-output",
      label: "Incremental output",
      category: "build",
      bytes: 131_072,
      decision: "keep",
      evidence: [
        {
          id: "e-incremental",
          summary: "Synthetic disposable incremental output for this example.",
        },
      ],
    },
  ],
  totalBytes: 5_111_808,
  selectedBytes: 0,
  selectedCount: 0,
  executionAvailable: false,
};
const categoryName = (category: Category) =>
  category === "build" ? "Build output" : "Cache";
const fmt = (bytes: number) => {
  if (bytes < 1024) return `${bytes} ${bytes === 1 ? "byte" : "bytes"}`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(3).replace(/0+$/, "").replace(/\.$/, "")} ${units[unit]}`;
};

export default function App() {
  const native = isTauri();
  const [plan, setPlan] = useState<Plan | null>(native ? null : browserPlan);
  const [review, setReview] = useState<Review | null>(null);
  const [selectedId, setSelectedId] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [needsRefresh, setNeedsRefresh] = useState(false);
  const [nativePreviewMessage, setNativePreviewMessage] = useState("");
  const [page, setPage] = useState<
    "edit" | "review" | "demo" | "advanced" | "explore"
  >(native ? "explore" : "edit");
  const [demoBusy, setDemoBusy] = useState(false);
  const [generalBusy, setGeneralBusy] = useState(false);
  const [generalSummary, setGeneralSummary] =
    useState<GeneralPlanSummary | null>(null);
  const [inventoryBusy, setInventoryBusy] = useState(false);
  const workspaceBusy = demoBusy || generalBusy || inventoryBusy;
  const actionRefs = useRef<Array<HTMLButtonElement | null>>([]);

  const refresh = useCallback(
    async (preservePage = false): Promise<boolean> => {
      if (!native) return false;
      setPending(true);
      setError("");
      try {
        const next = await invoke<Plan>("get_example_plan");
        setPlan(next);
        setReview(null);
        setNativePreviewMessage("");
        if (!preservePage) setPage("edit");
        setNeedsRefresh(false);
        return true;
      } catch (e) {
        setError(`Could not load the example plan. ${friendlyError(e)}`);
        setNeedsRefresh(true);
        return false;
      } finally {
        setPending(false);
      }
    },
    [native],
  );
  const refreshDemoPlan = useCallback(async () => {
    if (!(await refresh(true))) throw new Error("Plan refresh failed");
  }, [refresh]);
  useEffect(() => {
    if (native) void refresh(true);
  }, [native, refresh]);
  useEffect(() => {
    if (plan && !plan.actions.some((a) => a.id === selectedId))
      setSelectedId(plan.actions[0]?.id ?? "");
  }, [plan, selectedId]);

  const selected =
    plan?.actions.find((a) => a.id === selectedId) ?? plan?.actions[0];
  const denominator = plan?.totalBytes ?? 1;
  const updateDecision = async (action: Action, decision: Decision) => {
    if (
      !plan ||
      pending ||
      workspaceBusy ||
      needsRefresh ||
      action.decision === decision
    )
      return;
    setPending(true);
    setError("");
    try {
      const next = native
        ? await invoke<Plan>("edit_example_plan", {
            request: {
              expectedRevisionId: plan.revisionId,
              actionId: action.id,
              decision,
            },
          })
        : previewEdit(plan, action.id, decision);
      setPlan(next);
    } catch (e) {
      setError(`${friendlyError(e)} Refresh to load the latest plan.`);
      setNeedsRefresh(true);
    } finally {
      setPending(false);
    }
  };
  const freeze = async () => {
    if (
      !plan ||
      pending ||
      workspaceBusy ||
      needsRefresh ||
      plan.selectedCount === 0
    )
      return;
    setPending(true);
    setError("");
    try {
      const result = native
        ? await invoke<Review>("freeze_example_review", {
            request: { expectedRevisionId: plan.revisionId },
          })
        : previewReview(plan);
      setReview(result);
      setNativePreviewMessage("");
      setPage("review");
    } catch (e) {
      setError(`${friendlyError(e)} Refresh to load the latest plan.`);
      setNeedsRefresh(true);
    } finally {
      setPending(false);
    }
  };
  const previewNativeReview = async () => {
    if (!native || !review || pending || workspaceBusy || needsRefresh) return;
    setPending(true);
    setError("");
    setNativePreviewMessage("");
    try {
      setNativePreviewMessage(
        await invoke<string>("preview_saved_example_review", {
          request: { expectedRevisionId: review.revisionId },
        }),
      );
    } catch (e) {
      setError(`${friendlyError(e)} Refresh to load the latest plan.`);
      setNeedsRefresh(true);
    } finally {
      setPending(false);
    }
  };
  const keySelect = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (!plan || !["ArrowDown", "ArrowUp"].includes(event.key)) return;
    event.preventDefault();
    const index = plan.actions.findIndex((a) => a.id === selectedId);
    const nextIndex =
      (index + (event.key === "ArrowDown" ? 1 : -1) + plan.actions.length) %
      plan.actions.length;
    setSelectedId(plan.actions[nextIndex].id);
    actionRefs.current[nextIndex]?.focus();
  };
  const reviewActions = useMemo(() => review?.actions ?? [], [review]);

  if (!plan && page !== "explore" && page !== "advanced")
    return (
      <div className="loading-shell">
        <HardDrive />
        <p>
          {pending
            ? "Loading example plan…"
            : "The example plan could not be loaded."}
        </p>
        {error && <p role="alert">{error}</p>}
        <button
          className="button secondary"
          onClick={() => void refresh()}
          disabled={pending}
        >
          <RefreshCw size={15} /> Refresh plan
        </button>
      </div>
    );

  const displayedExample = plan ?? browserPlan;
  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand-mark">
          <HardDrive size={17} />
        </div>
        <span className="brand-name">Disk Steward</span>
        <span className="top-divider" />
        <span className="eyebrow">Storage review</span>
        <span className="top-spacer" />
        <span
          className={`example-chip ${page === "demo" ? "demo-chip-active" : ""}`}
        >
          <i />
          {page === "explore"
            ? "Read-only folder observation"
            : page === "advanced"
              ? "Metadata review plan"
              : page === "demo"
                ? "Disposable file demo"
                : "Non-executable example"}
        </span>
      </header>
      <aside className="sidebar">
        <div className="nav-label">WORKSPACE</div>
        <button
          className={`nav-item ${page === "edit" || page === "review" ? "active" : ""}`}
          disabled={workspaceBusy}
          onClick={() => setPage("edit")}
        >
          <Archive size={16} />
          Five-action example
        </button>
        <button
          className={`nav-item ${page === "demo" ? "active" : ""}`}
          disabled={workspaceBusy || !native}
          onClick={() => setPage("demo")}
        >
          <FilePlus2 size={16} />
          Fixture demo
        </button>
        <button
          className={`nav-item ${page === "advanced" ? "active" : ""}`}
          disabled={workspaceBusy || !native}
          onClick={() => setPage("advanced")}
        >
          <Archive size={16} />
          Review plan
        </button>
        <button
          className={`nav-item ${page === "explore" ? "active" : ""}`}
          disabled={workspaceBusy}
          onClick={() => setPage("explore")}
        >
          <HardDrive size={16} />
          Explore folder
        </button>
        <div className="side-rule" />
        {page !== "explore" && (
          <>
            <div className="side-metric">
              <span>
                {page === "advanced" ? "Known logical size" : "Plan size"}
              </span>
              <b>
                {page === "advanced"
                  ? generalSummary
                    ? fmt(generalSummary.bytes)
                    : "Loading…"
                  : fmt(displayedExample.totalBytes)}
              </b>
              <small>
                {page === "advanced"
                  ? `${generalSummary?.count ?? "—"} review entries`
                  : "5 synthetic actions"}
              </small>
            </div>
            <div className="side-metric selected-metric">
              <span>Included</span>
              <b>
                {page === "advanced"
                  ? generalSummary
                    ? fmt(generalSummary.selectedBytes)
                    : "—"
                  : fmt(displayedExample.selectedBytes)}
              </b>
              <small>
                {page === "advanced"
                  ? (generalSummary?.selectedCount ?? "—")
                  : displayedExample.selectedCount}{" "}
                {page === "advanced"
                  ? "items"
                  : displayedExample.selectedCount === 1
                    ? "action"
                    : "actions"}
              </small>
            </div>
          </>
        )}
        <div className="side-note">
          <ShieldCheck size={15} />
          <span>
            {page === "explore"
              ? "Read-only metadata. No file operation authority is created."
              : page === "demo"
                ? "Only app-created demo fixtures can be staged, restored, or permanently removed."
                : "This plan is metadata only and cannot authorize file operations."}
          </span>
        </div>
      </aside>
      <main className="main-content">
        <div className="main-head">
          <div>
            <div className="crumb">
              {page === "explore" || page === "advanced"
                ? "LOCAL WORKSPACE"
                : "EXAMPLE WORKSPACE"}{" "}
              <ChevronRight size={12} />{" "}
              {page === "explore"
                ? "FOLDER EXPLORER"
                : page === "advanced"
                  ? "GROUPED EDITOR"
                  : page === "demo"
                    ? "FIXTURE DEMO"
                    : page === "edit"
                      ? "PLAN EDITOR"
                      : native
                        ? "SAVED REVIEW"
                        : "PREVIEW"}
            </div>
            <h1>
              {page === "explore"
                ? "See what is in a folder."
                : page === "advanced"
                  ? "Make the plan yours."
                  : page === "demo"
                    ? "Test with disposable files."
                    : page === "edit"
                      ? "Review each action."
                      : native
                        ? "Review saved."
                        : "Preview decisions."}
            </h1>
            <p>
              {page === "explore"
                ? "Inspect immediate entries and their logical sizes before planning any changes."
                : page === "advanced"
                  ? "Review groups and exceptions, keep what matters, and inspect the evidence before saving a snapshot."
                  : page === "demo"
                    ? "Review, restore, or permanently remove files created only for this demo. Each operation needs its own native confirmation."
                    : page === "edit"
                      ? "Choose what to include. The plan totals update from the current revision."
                      : native
                        ? "This immutable snapshot records the decisions you reviewed."
                        : "This unsaved preview shows how the current decisions fit together."}
            </p>
          </div>
          {page === "review" && (
            <button
              className="button secondary"
              disabled={pending || workspaceBusy}
              onClick={() => setPage("edit")}
            >
              <ArrowLeft size={15} />
              Return to edit
            </button>
          )}
        </div>
        {!native && (
          <div className="preview-banner">
            <span>Browser preview</span> Unsaved example fixture · changes
            disappear when this page reloads
          </div>
        )}
        {error && page !== "explore" && page !== "advanced" && (
          <div className="error-banner" role="alert">
            <CircleAlert size={16} />
            <span>{error}</span>
            <button
              onClick={() => void refresh(page === "demo")}
              disabled={pending || workspaceBusy}
            >
              <RefreshCw size={14} />
              Refresh plan
            </button>
          </div>
        )}
        {page === "explore" ? (
          <FolderInventoryPanel
            onOpenPlan={() => setPage("advanced")}
            native={native}
            onBusyChange={setInventoryBusy}
          />
        ) : page === "advanced" ? (
          <GeneralPlanWorkspace
            initialProfile="live"
            native={native}
            onBusyChange={setGeneralBusy}
            onSummaryChange={setGeneralSummary}
          />
        ) : page === "demo" ? (
          <DemoLifecyclePanel
            plan={{
              revisionId: displayedExample.revisionId,
              actions: displayedExample.actions.map(
                ({ id, label, bytes, decision }) => ({
                  id,
                  label,
                  bytes,
                  decision,
                }),
              ),
              selectedCount: displayedExample.selectedCount,
              selectedBytes: displayedExample.selectedBytes,
              totalBytes: displayedExample.totalBytes,
            }}
            native={native}
            planBusy={pending}
            planConfirmed={!needsRefresh}
            onPlanRefresh={refreshDemoPlan}
            onBusyChange={setDemoBusy}
            onModeChange={(mode) => setPage(mode === "demo" ? "demo" : "edit")}
          />
        ) : page === "edit" ? (
          <section className="review-layout" aria-label="Example plan editor">
            <div className="action-panel">
              <div className="panel-heading">
                <div>
                  <h2>Cleanup actions</h2>
                  <p>
                    {displayedExample.actions.length} actions · Revision{" "}
                    {displayedExample.revisionNumber}
                  </p>
                </div>
                <span
                  className={`revision-tag ${needsRefresh ? "unconfirmed" : ""}`}
                >
                  {needsRefresh ? "Unconfirmed" : "Current"}
                </span>
              </div>
              <div
                className="action-list"
                role="group"
                aria-label="Actions"
                onKeyDown={keySelect}
              >
                {displayedExample.actions.map((action, index) => (
                  <button
                    key={action.id}
                    ref={(node) => {
                      actionRefs.current[index] = node;
                    }}
                    tabIndex={action.id === selected?.id ? 0 : -1}
                    aria-pressed={action.id === selected?.id}
                    className={`action-row category-${action.category} ${action.id === selected?.id ? "selected" : ""}`}
                    style={
                      {
                        "--bar-width": `${(action.bytes / denominator) * 100}%`,
                      } as React.CSSProperties
                    }
                    onClick={() => setSelectedId(action.id)}
                    disabled={pending || workspaceBusy}
                  >
                    <span className={`category-dot ${action.category}`} />
                    <span className="action-label">
                      {action.label}
                      <small>
                        {action.category === "build" ? "Build output" : "Cache"}
                      </small>
                    </span>
                    <span className="action-size">{fmt(action.bytes)}</span>
                    <span className={`decision-state ${action.decision}`}>
                      {action.decision === "include" ? "Included" : "Keep"}
                    </span>
                  </button>
                ))}
              </div>
              <div className="list-foot">
                <span>
                  <i className="legend-dot build" />
                  Build
                </span>
                <span>
                  <i className="legend-dot cache" />
                  Cache
                </span>
                <span>Bars share the full plan total</span>
              </div>
              <div className="totals-row" role="status" aria-live="polite">
                <div>
                  <small>Included in example</small>
                  <strong>{fmt(displayedExample.selectedBytes)}</strong>
                </div>
                <span>
                  {displayedExample.selectedCount} of{" "}
                  {displayedExample.actions.length} actions
                </span>
              </div>
              <div className="panel-actions">
                <button
                  className="button secondary"
                  onClick={() => void refresh()}
                  disabled={!native || pending || workspaceBusy}
                >
                  <RefreshCw className={pending ? "spin" : ""} size={15} />
                  Refresh
                </button>
                <button
                  className="button primary"
                  onClick={() => void freeze()}
                  disabled={
                    pending ||
                    workspaceBusy ||
                    needsRefresh ||
                    displayedExample.selectedCount === 0
                  }
                >
                  {native ? "Save review" : "Preview review"}
                  <ChevronRight size={15} />
                </button>
              </div>
              {needsRefresh && (
                <p className="inline-help" role="status">
                  Refresh the plan before making another change or saving a
                  review.
                </p>
              )}
              {displayedExample.selectedCount === 0 && (
                <p className="inline-help">
                  Include at least one action to save a review.
                </p>
              )}
            </div>
            <aside className="evidence-panel">
              <div className="inspector-label">SELECTED ACTION</div>
              {selected ? (
                <>
                  <div className={`evidence-icon ${selected.category}`}>
                    <Archive size={17} />
                  </div>
                  <h2>{selected.label}</h2>
                  <span className={`category-label ${selected.category}`}>
                    {categoryName(selected.category).toUpperCase()}
                  </span>
                  <div className="evidence-size">
                    {fmt(selected.bytes)}
                    <small>example size</small>
                  </div>
                  <div className="evidence-block">
                    <h3>Evidence</h3>
                    {selected.evidence.map((ev) => (
                      <p key={ev.id}>
                        <Check size={14} />
                        {ev.summary}
                      </p>
                    ))}
                  </div>
                  <div className="decision-control">
                    <span>Your decision</span>
                    <div className="decision-buttons">
                      <button
                        aria-pressed={selected.decision === "keep"}
                        className={
                          selected.decision === "keep" ? "chosen keep" : ""
                        }
                        onClick={() => void updateDecision(selected, "keep")}
                        disabled={pending || workspaceBusy || needsRefresh}
                      >
                        <Check size={14} />
                        Keep
                      </button>
                      <button
                        aria-pressed={selected.decision === "include"}
                        className={
                          selected.decision === "include"
                            ? "chosen include"
                            : ""
                        }
                        onClick={() => void updateDecision(selected, "include")}
                        disabled={pending || workspaceBusy || needsRefresh}
                      >
                        <Archive size={14} />
                        Include
                      </button>
                    </div>
                  </div>
                  {needsRefresh && (
                    <p className="evidence-foot">
                      Refresh the plan to confirm the latest decisions before
                      editing or saving.
                    </p>
                  )}
                  <p className="evidence-foot">
                    Synthetic disposable example. No files were scanned or
                    changed.
                  </p>
                  {native && (
                    <p className="evidence-foot">
                      This plan cannot stage files. A separate demo workflow is
                      limited to files created by Disk Steward and requires its
                      own review and native confirmation.
                    </p>
                  )}
                </>
              ) : (
                <p>No action selected.</p>
              )}
            </aside>
          </section>
        ) : (
          review && (
            <section className="saved-panel">
              <div className="saved-header">
                <div>
                  <span className="saved-kicker">
                    <ShieldCheck size={15} />{" "}
                    {native ? "IMMUTABLE REVIEW" : "UNSAVED PREVIEW"} · REVISION{" "}
                    {review.revisionNumber}
                  </span>
                  <h2>
                    {native
                      ? "Saved decision snapshot"
                      : "Example decision snapshot"}
                  </h2>
                </div>
                <div className="saved-total">
                  <strong>{fmt(review.selectedBytes)}</strong>
                  <span>
                    {review.selectedCount} included{" "}
                    {review.selectedCount === 1 ? "action" : "actions"}
                  </span>
                </div>
              </div>
              <div className="saved-list">
                {reviewActions.map((a) => (
                  <div className="saved-row" key={a.id}>
                    <span className={`category-dot ${a.category}`} />
                    <span>
                      {a.label}
                      <small>
                        {categoryName(a.category)} ·{" "}
                        {a.decision === "include" ? "Included" : "Keep"}
                      </small>
                    </span>
                    <b>{fmt(a.bytes)}</b>
                  </div>
                ))}
              </div>
              {native && (
                <div className="panel-actions">
                  <button
                    className="button secondary"
                    onClick={() => void previewNativeReview()}
                    disabled={pending || workspaceBusy || needsRefresh}
                  >
                    Preview native review
                  </button>
                </div>
              )}
              {nativePreviewMessage && (
                <p className="inline-help" role="status">
                  {nativePreviewMessage}
                </p>
              )}
              {native && (
                <details className="review-details">
                  <summary>Review details</summary>
                  <div className="hash-row">
                    <span>Snapshot digest</span>
                    <code>{review.manifestSha256}</code>
                  </div>
                </details>
              )}
              <div className="nonexec-callout">
                <ShieldCheck size={17} />
                <div>
                  <b>Example review only</b>
                  <span>
                    This {native ? "saved snapshot" : "browser preview"} cannot
                    approve, stage, or remove files. It contains synthetic
                    example metadata.
                  </span>
                </div>
              </div>
            </section>
          )
        )}
        <footer className="footnote">
          {page === "explore"
            ? "Read-only immediate folder entries · nested contents and recoverable space remain unknown"
            : page === "demo"
              ? "Only app-created demo fixtures · permanent removal requires separate review and native confirmation"
              : "Metadata review only · this plan cannot authorize staging or cleanup"}
        </footer>
      </main>
    </div>
  );
}

function previewEdit(plan: Plan, id: string, decision: Decision): Plan {
  const actions = plan.actions.map((a) =>
    a.id === id ? { ...a, decision } : a,
  );
  const selected = actions.filter((a) => a.decision === "include");
  return {
    ...plan,
    revisionNumber: plan.revisionNumber + 1,
    revisionId: `preview-revision-${plan.revisionNumber + 1}`,
    actions,
    selectedBytes: selected.reduce((n, a) => n + a.bytes, 0),
    selectedCount: selected.length,
  };
}
function previewReview(plan: Plan): Review {
  return {
    schemaVersion: 1,
    kind: "non_executable_example",
    reviewId: `preview-review-${plan.revisionNumber}`,
    planId: plan.planId,
    revisionId: plan.revisionId,
    revisionNumber: plan.revisionNumber,
    manifestSha256: "0".repeat(64),
    actions: plan.actions.map((a) => ({
      ...a,
      evidence: [...a.evidence],
    })),
    totalBytes: plan.totalBytes,
    selectedBytes: plan.selectedBytes,
    selectedCount: plan.selectedCount,
    executionAvailable: false,
  };
}
function friendlyError(error: unknown): string {
  if (
    error &&
    typeof error === "object" &&
    "message" in error &&
    typeof error.message === "string"
  )
    return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return "The plan could not be updated.";
}
