import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  ListMinus,
  ListPlus,
  ChevronDown,
  ChevronRight,
  CircleHelp,
  File,
  Folder,
  History,
  RotateCw,
  ShieldAlert,
  Sparkles,
  Undo2,
} from "lucide-react";
import type {
  EditorItem,
  EditorSnapshot,
  PlanChoice,
  PlanEdit,
} from "../general-plan-contract";
import "./AdvancedPlanPanel.css";

type Props = {
  snapshot: EditorSnapshot;
  busy: boolean;
  confirmed: boolean;
  editsConfirmed?: boolean;
  readOnly?: boolean;
  error?: string;
  onEdit(edit: PlanEdit): Promise<void>;
  onUndo(): Promise<void>;
  onRedo(): Promise<void>;
  onRefresh(): Promise<void>;
  onFreezeReview?(): Promise<void>;
  onReview?(): Promise<void>;
};
type Row = { key: string; groupId: string; itemId?: string; item?: EditorItem };
const ROW_HEIGHT = 58;
const WINDOW_HEIGHT = 464;
const choiceLabel: Record<PlanChoice, string> = {
  include: "Include",
  keep: "Keep",
  investigate: "Investigate",
};
const formatBytes = (value: number | null | undefined) =>
  value == null
    ? "Unknown"
    : value < 1024
      ? `${value} B`
      : value < 1024 ** 2
        ? `${(value / 1024).toFixed(1)} KiB`
        : value < 1024 ** 3
          ? `${(value / 1024 ** 2).toFixed(1)} MiB`
          : `${(value / 1024 ** 3).toFixed(2)} GiB`;

export default function AdvancedPlanPanel({
  snapshot,
  busy,
  confirmed,
  editsConfirmed = true,
  readOnly = false,
  error,
  onEdit,
  onUndo,
  onRedo,
  onRefresh,
  onFreezeReview,
  onReview,
}: Props) {
  const editsDisabled = busy || !editsConfirmed || readOnly;
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [showRemoved, setShowRemoved] = useState(false);
  const [findingSearch, setFindingSearch] = useState("");
  const [scrollTop, setScrollTop] = useState(0);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [activeItemId, setActiveItemId] = useState<string | null>(null);
  const [inspectedId, setInspectedId] = useState<string | null>(null);
  const [descendantSearch, setDescendantSearch] = useState("");
  const [descendantPage, setDescendantPage] = useState(0);
  const scroller = useRef<HTMLDivElement>(null);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  const pendingFocusRow = useRef<string | null>(null);
  const focusContext = useRef<{
    focusKey: string | null;
    itemId: string | null;
    rowKey: string | null;
  }>({ focusKey: null, itemId: null, rowKey: null });
  const lastRevision = useRef(snapshot.revisionId);
  const pendingFocusRestore = useRef(false);
  const items = useMemo(
    () => new Map(snapshot.items.map((item) => [item.itemId, item])),
    [snapshot.items],
  );
  const hiddenRemovedItemIds = useMemo(
    () =>
      new Set(
        snapshot.items
          .filter((item) => !item.inPlan)
          .map((item) => item.itemId),
      ),
    [snapshot.items],
  );
  const hasHiddenRemovedGroupMembers = useMemo(
    () =>
      !showRemoved &&
      snapshot.groups.some((group) =>
        group.memberIds.some((itemId) => hiddenRemovedItemIds.has(itemId)),
      ),
    [showRemoved, snapshot.groups, hiddenRemovedItemIds],
  );
  const maxKnownLogicalBytes = useMemo(
    () => Math.max(0, ...snapshot.items.map((item) => item.logicalBytes ?? 0)),
    [snapshot.items],
  );
  const unknownLeafCounts = useMemo(() => {
    const hasChildren = new Set<string>();
    for (const item of snapshot.items)
      if (item.parentId) hasChildren.add(item.parentId);
    let inPlan = 0;
    let selected = 0;
    for (const item of snapshot.items) {
      const isLeaf = item.kind === "file" || !hasChildren.has(item.itemId);
      if (!isLeaf || item.logicalBytes != null) continue;
      if (item.inPlan) inPlan += 1;
      if (item.effectiveSelected) selected += 1;
    }
    return { inPlan, selected };
  }, [snapshot.items]);
  const editsBlocked = editsDisabled;
  const collator = useMemo(
    () => new Intl.Collator(undefined, { numeric: true, sensitivity: "base" }),
    [],
  );
  const normalizedQuery = findingSearch.trim().toLocaleLowerCase();
  const rows = useMemo(() => {
    const output: Row[] = [];
    const addItem = (groupId: string, item: EditorItem) => {
      if (!showRemoved && !item.inPlan) return;
      output.push({
        key: `item:${groupId}:${item.itemId}`,
        groupId,
        itemId: item.itemId,
        item,
      });
    };
    const groups = [...snapshot.groups].sort((a, b) =>
      collator.compare(a.label, b.label),
    );
    const grouped = new Set(groups.flatMap((group) => group.memberIds));
    for (const group of groups) {
      const groupMatches =
        !normalizedQuery ||
        group.label.toLocaleLowerCase().includes(normalizedQuery);
      const members = group.memberIds
        .map((id) => items.get(id))
        .filter((item): item is EditorItem => Boolean(item))
        .sort((a, b) => collator.compare(a.label, b.label));
      const matchingMembers = members.filter((item) =>
        item.label.toLocaleLowerCase().includes(normalizedQuery),
      );
      if (!groupMatches && matchingMembers.length === 0) continue;
      output.push({ key: `group:${group.id}`, groupId: group.id });
      if (!collapsed.has(group.id)) {
        for (const item of groupMatches || !normalizedQuery
          ? members
          : matchingMembers)
          addItem(group.id, item);
      }
    }
    const unassigned = snapshot.items
      .filter((item) => !grouped.has(item.itemId))
      .filter(
        (item) =>
          !normalizedQuery ||
          item.label.toLocaleLowerCase().includes(normalizedQuery),
      )
      .sort((a, b) => collator.compare(a.label, b.label));
    if (unassigned.length) {
      output.push({ key: "group:unassigned", groupId: "unassigned" });
      if (!collapsed.has("unassigned"))
        for (const item of unassigned) addItem("unassigned", item);
    }
    return output;
  }, [
    snapshot.groups,
    snapshot.items,
    items,
    collapsed,
    showRemoved,
    normalizedQuery,
    collator,
  ]);
  const visibleRows = useMemo(() => {
    const start = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 3);
    return {
      start,
      rows: rows.slice(
        start,
        start + Math.ceil(WINDOW_HEIGHT / ROW_HEIGHT) + 6,
      ),
    };
  }, [rows, scrollTop]);
  const requestFocusRow = (index: number) => {
    const boundedIndex = Math.max(0, Math.min(rows.length - 1, index));
    const next = rows[boundedIndex];
    if (!next) return;
    setActiveId(next.key);
    setActiveItemId(next.itemId ?? null);
    const container = scroller.current;
    const rowTop = boundedIndex * ROW_HEIGHT;
    const visible =
      container &&
      rowTop >= container.scrollTop &&
      rowTop + ROW_HEIGHT <= container.scrollTop + container.clientHeight;
    const mounted = rowRefs.current.get(next.key);
    if (visible && mounted) {
      mounted.focus({ preventScroll: true });
      return;
    }
    pendingFocusRow.current = next.key;
    const top = Math.max(
      0,
      rowTop - (container?.clientHeight ?? WINDOW_HEIGHT) / 2,
    );
    setScrollTop(top);
    if (container) container.scrollTop = top;
  };
  useLayoutEffect(() => {
    const key = pendingFocusRow.current;
    if (!key) return;
    const mounted = rowRefs.current.get(key);
    if (mounted) {
      mounted.focus({ preventScroll: true });
      pendingFocusRow.current = null;
    }
  }, [visibleRows]);
  const inspected = inspectedId ? items.get(inspectedId) : undefined;
  useEffect(() => {
    if (activeId && !rows.some((row) => row.key === activeId)) {
      const alternate = rows.find((row) => row.itemId === activeItemId);
      setActiveId(alternate?.key ?? null);
    }
  }, [snapshot.revisionId, activeId, activeItemId, rows]);
  useEffect(() => {
    if (lastRevision.current !== snapshot.revisionId) {
      lastRevision.current = snapshot.revisionId;
      pendingFocusRestore.current = true;
    }
    if (busy || !pendingFocusRestore.current) return;
    pendingFocusRestore.current = false;
    const previous = focusContext.current;
    requestAnimationFrame(() => {
      if (previous.focusKey) {
        const eligible = Array.from(
          document.querySelectorAll<HTMLElement>("[data-editor-focus]"),
        ).find((element) => element.dataset.editorFocus === previous.focusKey);
        if (eligible && !eligible.matches(":disabled")) {
          eligible.focus({ preventScroll: true });
          return;
        }
      }
      if (previous.itemId) {
        const candidate = rows.find((row) => row.itemId === previous.itemId);
        if (candidate) {
          setActiveId(candidate.key);
          setActiveItemId(previous.itemId);
          const index = rows.indexOf(candidate);
          requestFocusRow(index);
          return;
        }
      }
      if (previous.rowKey) {
        const row = rows.find((entry) => entry.key === previous.rowKey);
        if (row) {
          const index = rows.indexOf(row);
          requestFocusRow(index);
        }
      }
    });
  }, [snapshot.revisionId, busy, rows]);
  const descendants = useMemo(() => {
    if (!inspected || inspected.kind !== "folder") return [] as EditorItem[];
    const children = new Map<string, EditorItem[]>();
    for (const item of snapshot.items) {
      if (!item.parentId) continue;
      const list = children.get(item.parentId) ?? [];
      list.push(item);
      children.set(item.parentId, list);
    }
    for (const siblings of children.values())
      siblings.sort((a, b) => collator.compare(a.label, b.label));
    const result: EditorItem[] = [];
    const seen = new Set<string>([inspected.itemId]);
    const stack = (children.get(inspected.itemId) ?? [])
      .slice()
      .reverse()
      .map((item) => ({ item, depth: 1 }));
    while (stack.length) {
      const current = stack.pop()!;
      if (seen.has(current.item.itemId) || current.depth > 64) continue;
      seen.add(current.item.itemId);
      result.push(current.item);
      for (const child of (children.get(current.item.itemId) ?? [])
        .slice()
        .reverse())
        stack.push({ item: child, depth: current.depth + 1 });
    }
    return result;
  }, [inspected, snapshot.items, collator]);
  const filteredDescendants = useMemo(() => {
    const query = descendantSearch.trim().toLocaleLowerCase();
    return query
      ? descendants.filter((item) =>
          item.label.toLocaleLowerCase().includes(query),
        )
      : descendants;
  }, [descendants, descendantSearch]);
  const descendantsPageCount = Math.max(
    1,
    Math.ceil(filteredDescendants.length / 50),
  );
  const pagedDescendants = filteredDescendants.slice(
    descendantPage * 50,
    descendantPage * 50 + 50,
  );
  // Only the visible page needs display context. Keep the existing traversal
  // depth bound and avoid materializing paths for the whole 10,000-item plan.
  const descendantContexts = new Map<string, string>();
  for (const child of pagedDescendants) {
    const ancestors: string[] = [];
    const seen = new Set<string>([child.itemId]);
    let parentId = child.parentId;
    while (
      parentId &&
      parentId !== inspected?.itemId &&
      ancestors.length < 64
    ) {
      if (seen.has(parentId)) break;
      seen.add(parentId);
      const parent = items.get(parentId);
      if (!parent) break;
      ancestors.unshift(parent.label);
      parentId = parent.parentId;
    }
    if (parentId !== inspected?.itemId)
      ancestors.unshift("Incomplete location");
    descendantContexts.set(child.itemId, ancestors.join(" › "));
  }
  useEffect(() => setDescendantPage(0), [inspectedId, descendantSearch]);
  const captureFocus = (event: React.FocusEvent<HTMLElement>) => {
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;
    const keyed = target.closest<HTMLElement>("[data-editor-focus]");
    const row = target.closest<HTMLElement>("[data-editor-row]");
    focusContext.current = {
      focusKey: keyed?.dataset.editorFocus ?? null,
      itemId: row?.dataset.itemId ?? null,
      rowKey: row?.dataset.editorRow ?? null,
    };
  };
  const setChoice = (item: EditorItem, choice: PlanChoice) =>
    onEdit({ kind: "setChoice", itemId: item.itemId, choice });
  const groupFor = (id: string) =>
    snapshot.groups.find((group) => group.id === id);
  const logicalMax = Math.max(1, snapshot.totals.inPlanLogicalBytes);
  const includePct = snapshot.totals.inPlanLogicalBytes
    ? Math.min(100, (snapshot.totals.logicalSelectedBytes / logicalMax) * 100)
    : 0;
  return (
    <section
      className="advanced-plan"
      aria-labelledby="advanced-plan-title"
      onFocusCapture={captureFocus}
    >
      <header className="ap-head">
        <div>
          <div className="ap-kicker">
            <span className="ap-dot" />
            GENERAL PLAN · REVISION {snapshot.revisionNumber}
          </div>
          <h2 id="advanced-plan-title">Review storage choices</h2>
          <p>
            Suggestions are starting points. Your choices and the evidence stay
            visible together.
          </p>
        </div>
        <div className="ap-head-actions">
          <button
            className="ap-icon-button"
            onClick={() => void onUndo()}
            disabled={editsBlocked || !snapshot.canUndo}
            aria-label="Undo last change"
          >
            <Undo2 size={16} />
          </button>
          <button
            className="ap-icon-button"
            onClick={() => void onRedo()}
            disabled={editsBlocked || !snapshot.canRedo}
            aria-label="Redo change"
          >
            <RotateCw size={15} />
          </button>
          <button
            className="ap-button quiet"
            onClick={() => void onRefresh()}
            disabled={busy}
          >
            {readOnly ? "Return to editor" : "Refresh"}
          </button>
        </div>
      </header>
      <div className="ap-status">
        <div className="ap-revision">
          <History size={14} />{" "}
          {snapshot.canUndo ? "Changes can be undone" : "Latest plan revision"}
          <span>·</span>
          {snapshot.canRedo ? "Redo available" : "No redo changes"}
        </div>
        <div className="ap-nonexec">
          <ShieldAlert size={14} />
          <span>Review only · no changes are carried out here</span>
        </div>
      </div>
      {!editsConfirmed && (
        <div className="ap-error" role="status">
          Changes need refreshing before editing or reviewing this plan.
          <button onClick={() => void onRefresh()} disabled={busy}>
            Refresh plan
          </button>
        </div>
      )}
      {error && (
        <div className="ap-error" role="alert">
          {error}
          <button onClick={() => void onRefresh()} disabled={busy}>
            Try refresh
          </button>
        </div>
      )}
      <div className="ap-workspace">
        <main className="ap-list-panel">
          <div className="ap-summary">
            <div>
              <span className="ap-summary-label">Selected items</span>
              <strong>{snapshot.totals.selectedCount.toLocaleString()}</strong>
            </div>
            <div>
              <span className="ap-summary-label">
                Logical size selected · known portion
              </span>
              <strong>
                {formatBytes(snapshot.totals.logicalSelectedBytes)}
              </strong>
              {unknownLeafCounts.selected > 0 && (
                <small className="ap-unknown-size">
                  {unknownLeafCounts.selected} selected{" "}
                  {unknownLeafCounts.selected === 1 ? "size" : "sizes"} unknown
                  {unknownLeafCounts.inPlan > unknownLeafCounts.selected
                    ? ` · ${unknownLeafCounts.inPlan} in plan`
                    : ""}
                </small>
              )}
            </div>
            <div>
              <span className="ap-summary-label">Estimated recovery</span>
              <strong>
                {formatBytes(snapshot.totals.recoveryLowerBytes)}
                {snapshot.totals.recoveryUpperBytes != null
                  ? `–${formatBytes(snapshot.totals.recoveryUpperBytes)}`
                  : "–unknown"}
              </strong>
            </div>
          </div>
          <div
            className="ap-meter"
            aria-label={`Selected logical size ${includePct.toFixed(0)} percent of items currently in the plan`}
          >
            <span style={{ width: `${includePct}%` }} />
          </div>
          <div className="ap-meter-caption">
            <span>Logical size selected</span>
            <span>Share of known in-plan logical size</span>
          </div>
          <div className="ap-list-toolbar">
            <div>
              <b>Findings</b>
              <span>
                Whole plan · {snapshot.items.length.toLocaleString()} items ·{" "}
                {snapshot.groups.length} groups
              </span>
              {normalizedQuery && (
                <small className="ap-filter-count">
                  Visible in filtered view:{" "}
                  {rows.filter((row) => row.item).length.toLocaleString()}{" "}
                  {rows.filter((row) => row.item).length === 1
                    ? "finding"
                    : "findings"}
                </small>
              )}
            </div>
            <label className="ap-finding-search">
              Search findings and groups
              <input
                type="search"
                value={findingSearch}
                onChange={(event) => setFindingSearch(event.target.value)}
                placeholder="Name or group"
                aria-label="Search findings and groups"
              />
            </label>
            <label>
              <input
                type="checkbox"
                checked={showRemoved}
                onChange={(e) => setShowRemoved(e.target.checked)}
              />{" "}
              Show removed items
            </label>
          </div>
          {hasHiddenRemovedGroupMembers && (
            <p className="ap-window-note" aria-live="polite">
              Show removed items before changing a group that contains them.
            </p>
          )}
          {normalizedQuery && (
            <p className="ap-window-note">
              Summary totals include the whole plan, including findings hidden
              by search.
            </p>
          )}
          {(snapshot.items.length > 400 || normalizedQuery) && (
            <p className="ap-window-note" aria-live="polite">
              {normalizedQuery
                ? "Search filters this view only. Clear search before using group-wide changes."
                : "Showing nearby findings as you move through the list."}
            </p>
          )}
          <div
            className="ap-window"
            ref={scroller}
            role="list"
            aria-label="Grouped plan findings"
            tabIndex={-1}
            onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
          >
            <div
              style={{ height: rows.length * ROW_HEIGHT, position: "relative" }}
            >
              {visibleRows.rows.map((row, offset) => {
                const index = visibleRows.start + offset;
                const group = groupFor(row.groupId);
                const item = row.item;
                if (!item) {
                  const label = group?.label ?? "Other findings";
                  const count = group?.memberIds.length ?? 0;
                  const hasHiddenRemovedMembers = Boolean(
                    group &&
                    !showRemoved &&
                    group.memberIds.some((itemId) =>
                      hiddenRemovedItemIds.has(itemId),
                    ),
                  );
                  const groupChoiceDisabled =
                    editsBlocked ||
                    Boolean(normalizedQuery) ||
                    hasHiddenRemovedMembers;
                  const groupChoiceTitle = normalizedQuery
                    ? "Clear search before group-wide changes"
                    : hasHiddenRemovedMembers
                      ? "Show removed items before group-wide changes"
                      : undefined;
                  return (
                    <div
                      key={row.key}
                      role="listitem"
                      aria-posinset={index + 1}
                      aria-setsize={rows.length}
                      className="ap-group-row"
                      style={{
                        position: "absolute",
                        top: index * ROW_HEIGHT,
                        height: ROW_HEIGHT,
                      }}
                    >
                      <button
                        data-editor-focus={`group:${row.groupId}`}
                        ref={(node) => {
                          if (node) rowRefs.current.set(row.key, node);
                          else rowRefs.current.delete(row.key);
                        }}
                        className="ap-group-toggle"
                        aria-expanded={!collapsed.has(row.groupId)}
                        aria-label={`${collapsed.has(row.groupId) ? "Expand" : "Collapse"} ${label}`}
                        onClick={() =>
                          setCollapsed((previous) => {
                            const next = new Set(previous);
                            if (next.has(row.groupId)) next.delete(row.groupId);
                            else next.add(row.groupId);
                            return next;
                          })
                        }
                        onFocus={() => setActiveId(row.key)}
                        onKeyDown={(e) => {
                          if (e.key === "Home" || e.key === "End") {
                            e.preventDefault();
                            requestFocusRow(
                              e.key === "Home" ? 0 : rows.length - 1,
                            );
                            return;
                          }
                          if (e.key === "ArrowDown") {
                            e.preventDefault();
                            requestFocusRow(index + 1);
                          }
                          if (e.key === "ArrowUp") {
                            e.preventDefault();
                            requestFocusRow(index - 1);
                          }
                        }}
                      >
                        {collapsed.has(row.groupId) ? (
                          <ChevronRight size={15} />
                        ) : (
                          <ChevronDown size={15} />
                        )}
                        <span>{label}</span>
                        <small>
                          {count} items
                          {group
                            ? ` · ${group.selectedCount} selected · ${group.blockedCount} blocked`
                            : ""}
                        </small>
                      </button>
                      {group && (
                        <div
                          className="ap-group-choice"
                          aria-label={`Set all eligible ${label} items`}
                        >
                          <button
                            disabled={groupChoiceDisabled}
                            title={groupChoiceTitle}
                            onClick={() =>
                              void onEdit({
                                kind: "setGroupChoice",
                                groupId: group.id,
                                choice: "include",
                              })
                            }
                          >
                            Include group
                          </button>
                          <button
                            disabled={groupChoiceDisabled}
                            title={groupChoiceTitle}
                            onClick={() =>
                              void onEdit({
                                kind: "setGroupChoice",
                                groupId: group.id,
                                choice: "keep",
                              })
                            }
                          >
                            Keep group
                          </button>
                          <button
                            disabled={groupChoiceDisabled}
                            title={groupChoiceTitle}
                            onClick={() =>
                              void onEdit({
                                kind: "setGroupChoice",
                                groupId: group.id,
                                choice: "investigate",
                              })
                            }
                          >
                            Investigate
                          </button>
                        </div>
                      )}
                    </div>
                  );
                }
                const duplicate = snapshot.duplicates.find((d) =>
                  d.memberIds.includes(item.itemId),
                );
                const blocker = item.blockers.length > 0;
                const bar =
                  item.logicalBytes == null || maxKnownLogicalBytes === 0
                    ? 0
                    : Math.max(
                        0,
                        Math.min(
                          100,
                          (item.logicalBytes / maxKnownLogicalBytes) * 100,
                        ),
                      );
                return (
                  <div
                    key={row.key}
                    data-editor-row={row.key}
                    data-item-id={item.itemId}
                    data-editor-focus={`row:${row.key}`}
                    ref={(node) => {
                      if (node) rowRefs.current.set(row.key, node);
                      else rowRefs.current.delete(row.key);
                    }}
                    role="listitem"
                    aria-label={`${item.label}, ${group?.label ?? "Other findings"}, ${duplicate?.survivorId === item.itemId ? "retained duplicate copy" : item.effectiveSelected ? "included" : "not included"}`}
                    aria-posinset={index + 1}
                    aria-setsize={rows.length}
                    aria-current={
                      inspectedId === item.itemId ? "true" : undefined
                    }
                    tabIndex={
                      activeId === row.key || (!activeId && index === 0)
                        ? 0
                        : -1
                    }
                    className={`ap-item-row${inspectedId === item.itemId ? " is-inspected" : ""}${!item.inPlan ? " is-removed" : ""}`}
                    style={
                      {
                        position: "absolute",
                        top: index * ROW_HEIGHT,
                        height: ROW_HEIGHT,
                        "--row-bar": `${bar}%`,
                      } as React.CSSProperties
                    }
                    onFocus={() => {
                      setActiveId(row.key);
                      setActiveItemId(item.itemId);
                    }}
                    onClick={() => setInspectedId(item.itemId)}
                    onKeyDown={(e) => {
                      if (e.target !== e.currentTarget) return;
                      if (e.key === "Home" || e.key === "End") {
                        e.preventDefault();
                        requestFocusRow(e.key === "Home" ? 0 : rows.length - 1);
                        return;
                      }
                      if (e.key === "ArrowDown") {
                        e.preventDefault();
                        requestFocusRow(index + 1);
                      } else if (e.key === "ArrowUp") {
                        e.preventDefault();
                        requestFocusRow(index - 1);
                      } else if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setInspectedId(item.itemId);
                      }
                    }}
                  >
                    <span className={`ap-kind ${item.kind}`}>
                      {item.kind === "folder" ? (
                        <Folder size={15} />
                      ) : (
                        <File size={14} />
                      )}
                    </span>
                    <span className="ap-item-name">
                      <b>{item.label}</b>
                      <small>
                        {item.inPlan
                          ? `Suggested ${choiceLabel[item.suggestion.choice].toLowerCase()}${item.userChoice ? ` · you chose ${choiceLabel[item.userChoice].toLowerCase()}` : ""}`
                          : "Removed from plan"}
                      </small>
                    </span>
                    <span className="ap-item-size">
                      {formatBytes(item.logicalBytes)}
                      <small>logical size</small>
                    </span>
                    {blocker ? (
                      <span
                        className="ap-blocked"
                        title={item.blockers.map((b) => b.reason).join("; ")}
                      >
                        <ShieldAlert size={13} />
                        Blocked
                      </span>
                    ) : null}
                    <button
                      data-editor-focus={`choice:include:${item.itemId}`}
                      className={`ap-choice ${item.effectiveSelected ? "chosen" : ""}`}
                      disabled={editsBlocked || !item.inPlan || blocker}
                      onClick={(e) => {
                        e.stopPropagation();
                        void setChoice(item, "include");
                      }}
                      aria-pressed={item.effectiveSelected}
                    >
                      Include
                    </button>
                    <button
                      data-editor-focus={`choice:keep:${item.itemId}`}
                      className={`ap-choice ${!item.effectiveSelected && item.userChoice === "keep" ? "chosen keep" : ""}`}
                      disabled={editsBlocked || !item.inPlan}
                      onClick={(e) => {
                        e.stopPropagation();
                        void setChoice(item, "keep");
                      }}
                      aria-pressed={
                        !item.effectiveSelected && item.userChoice === "keep"
                      }
                    >
                      Keep
                    </button>
                    <button
                      data-editor-focus={`choice:investigate:${item.itemId}`}
                      className={`ap-choice ${item.userChoice === "investigate" ? "chosen inspect" : ""}`}
                      disabled={editsBlocked || !item.inPlan}
                      onClick={(e) => {
                        e.stopPropagation();
                        void setChoice(item, "investigate");
                      }}
                      aria-pressed={item.userChoice === "investigate"}
                    >
                      Look into
                    </button>
                    <button
                      data-editor-focus={`membership:${item.itemId}`}
                      className="ap-membership"
                      disabled={editsBlocked}
                      title={
                        item.inPlan ? "Remove from plan" : "Add back to plan"
                      }
                      aria-label={
                        item.inPlan
                          ? `Remove ${item.label} from plan`
                          : `Add ${item.label} back to plan`
                      }
                      onClick={(e) => {
                        e.stopPropagation();
                        void onEdit({
                          kind: "setMembership",
                          itemId: item.itemId,
                          inPlan: !item.inPlan,
                        });
                      }}
                    >
                      {item.inPlan ? (
                        <ListMinus size={15} />
                      ) : (
                        <ListPlus size={15} />
                      )}
                    </button>
                    {duplicate && (
                      <span className="ap-survivor">
                        {duplicate.survivorId === item.itemId ? (
                          "Kept copy"
                        ) : (
                          <button
                            data-editor-focus={`survivor:${duplicate.id}`}
                            disabled={editsBlocked}
                            onClick={(e) => {
                              e.stopPropagation();
                              void onEdit({
                                kind: "setDuplicateSurvivor",
                                duplicateSetId: duplicate.id,
                                survivorId: item.itemId,
                              });
                            }}
                          >
                            Keep this copy
                          </button>
                        )}
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
          <footer className="ap-list-foot">
            <span>
              <i className="ap-legend-include" />
              Selected logical size
            </span>
            <span>
              Bar length is each item's share of the largest known logical size;
              unknown sizes have no bar.
            </span>
          </footer>
        </main>
        <aside className="ap-inspector" aria-label="Finding details">
          <div className="ap-inspector-head">
            <span className="ap-inspector-kicker">DETAILS</span>
            {inspected && (
              <span className="ap-item-kind">{inspected.kind}</span>
            )}
          </div>
          {inspected ? (
            <>
              <h3>{inspected.label}</h3>
              <p className="ap-inspector-size">
                {formatBytes(inspected.logicalBytes)}{" "}
                <small>logical size</small>
              </p>
              <div className="ap-inspector-estimate">
                <span>
                  {inspected.blockers.some(
                    (blocker) => blocker.code === "native_metadata_only",
                  )
                    ? "Recovery estimate"
                    : "Synthetic recovery estimate"}
                </span>
                <b>
                  {inspected.recoveryLowerBytes === null &&
                  inspected.recoveryUpperBytes === null ? (
                    "Unavailable"
                  ) : (
                    <>
                      {formatBytes(inspected.recoveryLowerBytes)} –{" "}
                      {formatBytes(inspected.recoveryUpperBytes)}
                    </>
                  )}
                </b>
                <small>
                  {inspected.blockers.some(
                    (blocker) => blocker.code === "native_metadata_only",
                  )
                    ? "Metadata alone cannot establish reclaimable space."
                    : "Illustrative only. This is not recovered space or a live-storage estimate."}
                </small>
              </div>
              <section className="ap-detail-section">
                <h4>
                  <Sparkles size={14} /> Suggestion
                </h4>
                <p>
                  <b>{choiceLabel[inspected.suggestion.choice]}</b> ·{" "}
                  {inspected.suggestion.rationale}
                </p>
              </section>
              {inspected.userChoice && (
                <section className="ap-detail-section ap-user-choice">
                  <h4>Your choice</h4>
                  <p>{choiceLabel[inspected.userChoice]}</p>
                </section>
              )}
              {inspected.blockers.length > 0 && (
                <section className="ap-detail-section ap-blocker-list">
                  <h4>
                    <ShieldAlert size={14} /> Needs attention
                  </h4>
                  {inspected.blockers.map((b) => (
                    <p key={`${b.code}:${b.reason}`}>{b.reason}</p>
                  ))}
                </section>
              )}
              {inspected.evidence.length > 0 && (
                <section className="ap-detail-section">
                  <h4>Evidence</h4>
                  {inspected.evidence.map((e) => (
                    <p className="ap-evidence" key={e.id}>
                      <CircleHelp size={13} />
                      {e.summary}
                    </p>
                  ))}
                </section>
              )}
              {inspected.kind === "folder" && descendants.length > 0 && (
                <section className="ap-detail-section">
                  <h4>Keep items inside</h4>
                  <p className="ap-help">
                    Choose descendants to keep even when this folder is
                    selected.
                  </p>
                  <label className="ap-descendant-search">
                    Search descendants
                    <input
                      data-editor-focus={`descendant-search:${inspected.itemId}`}
                      type="search"
                      value={descendantSearch}
                      onChange={(event) =>
                        setDescendantSearch(event.target.value)
                      }
                      placeholder="Find an item"
                    />
                  </label>
                  <div className="ap-descendant-count" aria-live="polite">
                    {filteredDescendants.length === 0
                      ? "No matching descendants"
                      : `Showing ${descendantPage * 50 + 1}–${Math.min((descendantPage + 1) * 50, filteredDescendants.length)} of ${filteredDescendants.length}`}
                  </div>
                  <div className="ap-descendants">
                    {pagedDescendants.map((child) => {
                      const checked = inspected.excludedDescendantIds.includes(
                        child.itemId,
                      );
                      return (
                        <label key={child.itemId}>
                          <input
                            data-editor-focus={`exclude:${child.itemId}`}
                            type="checkbox"
                            checked={checked}
                            disabled={editsBlocked}
                            onChange={(event) => {
                              const next = event.target.checked
                                ? [
                                    ...inspected.excludedDescendantIds,
                                    child.itemId,
                                  ]
                                : inspected.excludedDescendantIds.filter(
                                    (id) => id !== child.itemId,
                                  );
                              void onEdit({
                                kind: "excludeDescendants",
                                parentId: inspected.itemId,
                                itemIds: next,
                              });
                            }}
                          />
                          <span>
                            {child.label}
                            <small>{formatBytes(child.logicalBytes)}</small>
                            {descendantContexts.get(child.itemId) && (
                              <small>
                                {descendantContexts.get(child.itemId)}
                              </small>
                            )}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                  {descendantsPageCount > 1 && (
                    <div className="ap-descendant-pages">
                      <button
                        data-editor-focus={`descendant-previous:${inspected.itemId}`}
                        type="button"
                        disabled={descendantPage === 0}
                        onClick={() =>
                          setDescendantPage((page) => Math.max(0, page - 1))
                        }
                      >
                        Previous
                      </button>
                      <span>
                        Page {descendantPage + 1} of {descendantsPageCount}
                      </span>
                      <button
                        data-editor-focus={`descendant-next:${inspected.itemId}`}
                        type="button"
                        disabled={descendantPage + 1 >= descendantsPageCount}
                        onClick={() =>
                          setDescendantPage((page) =>
                            Math.min(descendantsPageCount - 1, page + 1),
                          )
                        }
                      >
                        Next
                      </button>
                    </div>
                  )}
                </section>
              )}
            </>
          ) : (
            <div className="ap-inspector-empty">
              <CircleHelp size={22} />
              <b>Select a finding</b>
              <p>
                See why it was suggested, what you chose, and which evidence
                informs it.
              </p>
            </div>
          )}
        </aside>
      </div>
      <footer className="ap-review-footer">
        <div>
          <b>
            {readOnly
              ? "Immutable saved review"
              : confirmed
                ? "This revision has been reviewed"
                : "Review this revision when ready"}
          </b>
          <span>
            Any change creates a new revision and needs a fresh review.
          </span>
        </div>
        <button
          className="ap-button review"
          disabled={editsDisabled || snapshot.totals.selectedCount === 0}
          onClick={() => void (onFreezeReview ?? onReview)?.()}
        >
          {confirmed ? "Review latest revision" : "Review selected changes"}
          <ChevronRight size={15} />
        </button>
      </footer>
    </section>
  );
}
