import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PlanEdit } from "../general-plan-contract";
import {
  editorGroup,
  editorItem,
  editorSnapshot,
  groupedEditorSnapshot,
} from "../test-fixtures/editor";
import AdvancedPlanPanel from "./AdvancedPlanPanel";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function mount(
  snapshot = editorSnapshot([]),
  overrides: Partial<{
    busy: boolean;
    confirmed: boolean;
    editsConfirmed: boolean;
    readOnly: boolean;
  }> = {},
) {
  const onEdit = vi.fn<(edit: PlanEdit) => Promise<void>>().mockResolvedValue();
  const onUndo = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
  const onRedo = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
  const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
  const onReview = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
  const view = render(
    <AdvancedPlanPanel
      snapshot={snapshot}
      busy={overrides.busy ?? false}
      confirmed={overrides.confirmed ?? false}
      editsConfirmed={overrides.editsConfirmed ?? true}
      readOnly={overrides.readOnly ?? false}
      onEdit={onEdit}
      onUndo={onUndo}
      onRedo={onRedo}
      onRefresh={onRefresh}
      onReview={onReview}
    />,
  );
  return { ...view, onEdit, onUndo, onRedo, onRefresh, onReview };
}

function stubAnimationFrames() {
  const callbacks: FrameRequestCallback[] = [];
  const canceled: number[] = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    callbacks.push(callback);
    return callbacks.length;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => canceled.push(id));
  return {
    callbacks,
    canceled,
    run(id: number) {
      callbacks[id - 1]?.(0);
    },
  };
}

describe("AdvancedPlanPanel decisions", () => {
  it("keeps blocked items out of the Include action", () => {
    const blocked = editorItem("blocked", {
      label: "Protected working tree",
      blockers: [{ code: "local_state", reason: "Local work may be unique" }],
    });
    const { onEdit } = mount(
      editorSnapshot(
        [blocked],
        [
          editorGroup("projects", [blocked.itemId], {
            selectedCount: 0,
            selectedLogicalBytes: 0,
            blockedCount: 1,
          }),
        ],
      ),
    );
    const row = screen.getByRole("listitem", {
      name: /Protected working tree/,
    });
    expect(within(row).getByRole("button", { name: "Include" })).toBeDisabled();
    fireEvent.click(within(row).getByRole("button", { name: "Include" }));
    expect(onEdit).not.toHaveBeenCalled();
    expect(within(row).getByText("Blocked")).toBeInTheDocument();
  });

  it("issues a group-target choice command for group changes", () => {
    const one = editorItem("one", { label: "One" });
    const two = editorItem("two", { label: "Two" });
    const group = editorGroup("caches", ["one", "two"], {
      label: "Generated caches",
    });
    const { onEdit } = mount(editorSnapshot([one, two], [group]));
    fireEvent.click(screen.getByRole("button", { name: "Keep group" }));
    expect(onEdit).toHaveBeenCalledExactlyOnceWith({
      kind: "setGroupChoice",
      groupId: "caches",
      choice: "keep",
    });
  });

  it("shows preserved user Keep decisions in both the row and inspector", () => {
    const kept = editorItem("keep-me", {
      label: "Personal project",
      suggestion: { choice: "include", rationale: "Large generated directory" },
      userChoice: "keep",
      effectiveSelected: false,
    });
    mount(editorSnapshot([kept]));
    const row = screen.getByRole("listitem", { name: /Personal project/ });
    expect(
      within(row).getByText("Suggested include · you chose keep"),
    ).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Keep" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    fireEvent.click(row);
    expect(
      screen.getByRole("heading", { name: "Personal project" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Your choice")).toBeInTheDocument();
  });

  it("selects one duplicate survivor by exact set and item identity", () => {
    const a = editorItem("copy-a", {
      label: "Archive A",
      userChoice: "keep",
      effectiveSelected: false,
    });
    const b = editorItem("copy-b", {
      label: "Archive B",
      effectiveSelected: true,
    });
    const c = editorItem("copy-c", {
      label: "Archive C",
      effectiveSelected: true,
    });
    const snapshot = editorSnapshot([a, b, c], [], {
      duplicates: [
        {
          id: "duplicate-1",
          memberIds: [a.itemId, b.itemId, c.itemId],
          survivorId: a.itemId,
        },
      ],
    });
    const { onEdit } = mount(snapshot);
    expect(screen.getByText("Kept copy")).toBeInTheDocument();
    fireEvent.click(
      within(screen.getByRole("listitem", { name: /Archive C/ })).getByRole(
        "button",
        { name: "Keep this copy" },
      ),
    );
    expect(onEdit).toHaveBeenCalledExactlyOnceWith({
      kind: "setDuplicateSurvivor",
      duplicateSetId: "duplicate-1",
      survivorId: "copy-c",
    });
  });

  it("requires hidden removed members to be shown before group-wide choices", () => {
    const visible = editorItem("visible", { label: "Visible item" });
    const removed = editorItem("removed", {
      label: "Removed item",
      inPlan: false,
      effectiveSelected: false,
    });
    const { onEdit } = mount(
      editorSnapshot(
        [visible, removed],
        [editorGroup("mixed", ["visible", "removed"])],
      ),
    );
    expect(
      screen.queryByRole("listitem", { name: /Removed item/ }),
    ).not.toBeInTheDocument();
    for (const name of ["Keep group", "Include group", "Investigate"]) {
      const button = screen.getByRole("button", { name });
      expect(button).toBeDisabled();
      expect(button).toHaveAttribute(
        "title",
        "Show removed items before group-wide changes",
      );
      fireEvent.click(button);
    }
    expect(onEdit).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("checkbox", { name: /Show removed items/ }),
    );
    expect(
      screen.getByRole("listitem", { name: /Removed item/ }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Keep group" }));
    expect(onEdit).toHaveBeenCalledExactlyOnceWith({
      kind: "setGroupChoice",
      groupId: "mixed",
      choice: "keep",
    });
  });

  it.each([false, true])(
    "restores focus after the focused row disappears (group removed: %s)",
    (removeGroup) => {
      const frames: FrameRequestCallback[] = [];
      vi.stubGlobal(
        "requestAnimationFrame",
        (callback: FrameRequestCallback) => {
          frames.push(callback);
          return frames.length;
        },
      );
      vi.stubGlobal("cancelAnimationFrame", vi.fn());
      const original = editorItem("candidate", {
        label: "Candidate",
        groupIds: ["review-group"],
      });
      const other = editorItem("other", {
        label: "Other",
        groupIds: ["other-group"],
      });
      const initial = editorSnapshot(
        [original, other],
        [
          editorGroup("review-group", ["candidate"]),
          editorGroup("other-group", ["other"]),
        ],
      );
      const onEdit = vi
        .fn<(edit: PlanEdit) => Promise<void>>()
        .mockResolvedValue();
      const props = {
        busy: false,
        confirmed: false,
        editsConfirmed: true,
        readOnly: false,
        onEdit,
        onUndo: async () => {},
        onRedo: async () => {},
        onRefresh: async () => {},
        onReview: async () => {},
      };
      const { rerender } = render(
        <AdvancedPlanPanel snapshot={initial} {...props} />,
      );
      Object.defineProperty(
        screen.getByRole("list", { name: "Grouped plan findings" }),
        "clientHeight",
        { configurable: true, value: 464 },
      );
      const remove = screen.getByRole("button", {
        name: "Remove Candidate from plan",
      });
      act(() => remove.focus());
      expect(remove).toHaveFocus();
      fireEvent.click(remove);
      expect(onEdit).toHaveBeenCalledExactlyOnceWith({
        kind: "setMembership",
        itemId: "candidate",
        inPlan: false,
      });
      const next = {
        ...initial,
        revisionId: "revision-after-removal",
        revisionNumber: initial.revisionNumber + 1,
        items: [
          { ...original, inPlan: false, effectiveSelected: false },
          other,
        ],
        groups: removeGroup
          ? [editorGroup("other-group", ["other"])]
          : initial.groups,
      };
      rerender(<AdvancedPlanPanel snapshot={next} {...props} />);
      act(() => frames.splice(0).forEach((callback) => callback(0)));
      expect(
        screen.queryByRole("button", { name: "Remove Candidate from plan" }),
      ).not.toBeInTheDocument();
      expect(
        screen.getByRole("button", {
          name: removeGroup ? "Collapse other-group" : "Collapse review-group",
        }),
      ).toHaveFocus();
    },
  );

  it("ignores a stale restore after a busy revision and newer Search focus", () => {
    const frames = stubAnimationFrames();
    const item = editorItem("focus-item", { label: "Focus item" });
    const initial = editorSnapshot([item]);
    const props = {
      busy: false,
      confirmed: false,
      editsConfirmed: true,
      readOnly: false,
      onEdit: async () => {},
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
      onReview: async () => {},
    };
    const { rerender } = render(
      <AdvancedPlanPanel snapshot={initial} {...props} />,
    );
    act(() => screen.getByRole("button", { name: "Include" }).focus());
    const revision2 = {
      ...initial,
      revisionId: "revision-two",
      revisionNumber: initial.revisionNumber + 1,
    };
    rerender(<AdvancedPlanPanel snapshot={revision2} {...props} />);
    expect(frames.callbacks).toHaveLength(1);

    const revision3 = {
      ...revision2,
      revisionId: "revision-three",
      revisionNumber: revision2.revisionNumber + 1,
    };
    rerender(<AdvancedPlanPanel snapshot={revision3} {...props} busy />);
    expect(frames.canceled).toEqual([1]);
    const search = screen.getByRole("searchbox", {
      name: "Search findings and groups",
    });
    act(() => search.focus());
    act(() => frames.run(1));
    expect(search).toHaveFocus();

    rerender(<AdvancedPlanPanel snapshot={revision3} {...props} />);
    expect(frames.callbacks).toHaveLength(2);
    act(() => frames.run(2));
    expect(search).toHaveFocus();
  });

  it("respects newer Search focus even when the scheduled frame is still current", () => {
    const frames = stubAnimationFrames();
    const snapshot = editorSnapshot([editorItem("current-focus")]);
    const props = {
      busy: false,
      confirmed: false,
      onEdit: async () => {},
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
    };
    const view = render(<AdvancedPlanPanel snapshot={snapshot} {...props} />);
    act(() => screen.getByRole("button", { name: "Include" }).focus());
    view.rerender(
      <AdvancedPlanPanel
        snapshot={{
          ...snapshot,
          revisionId: "current-focus-next",
          revisionNumber: 2,
        }}
        {...props}
      />,
    );
    expect(frames.callbacks).toHaveLength(1);
    const search = screen.getByRole("searchbox", {
      name: "Search findings and groups",
    });
    act(() => search.focus());
    act(() => frames.run(1));
    expect(search).toHaveFocus();
  });

  it("restores focus within the revised panel when another panel shares item IDs", () => {
    const frames = stubAnimationFrames();
    const snapshot = editorSnapshot([
      editorItem("shared-item", { label: "Shared item" }),
    ]);
    const props = {
      busy: false,
      confirmed: false,
      onEdit: async () => {},
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
    };
    const { rerender } = render(
      <>
        <AdvancedPlanPanel key="first" snapshot={snapshot} {...props} />
        <AdvancedPlanPanel key="second" snapshot={snapshot} {...props} />
      </>,
    );
    const [firstInclude, secondInclude] = screen.getAllByRole("button", {
      name: "Include",
    });
    act(() => secondInclude.focus());
    rerender(
      <>
        <AdvancedPlanPanel key="first" snapshot={snapshot} {...props} />
        <AdvancedPlanPanel
          key="second"
          snapshot={{
            ...snapshot,
            revisionId: "second-panel-next",
            revisionNumber: 2,
          }}
          {...props}
        />
      </>,
    );
    expect(frames.callbacks).toHaveLength(1);
    act(() => frames.run(1));
    expect(secondInclude).toHaveFocus();
    expect(firstInclude).not.toHaveFocus();
  });

  it("respects focus moved outside the editor before a current restore runs", () => {
    const frames = stubAnimationFrames();
    const snapshot = editorSnapshot([editorItem("external-focus")]);
    const props = {
      busy: false,
      confirmed: false,
      onEdit: async () => {},
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
    };
    const panel = (next = snapshot) => (
      <>
        <button>Outside editor</button>
        <AdvancedPlanPanel snapshot={next} {...props} />
      </>
    );
    const view = render(panel());
    act(() => screen.getByRole("button", { name: "Include" }).focus());
    view.rerender(
      panel({
        ...snapshot,
        revisionId: "external-focus-next",
        revisionNumber: 2,
      }),
    );
    const outside = screen.getByRole("button", { name: "Outside editor" });
    act(() => outside.focus());
    act(() => frames.run(1));
    expect(outside).toHaveFocus();
  });

  it("reschedules a legitimate restore after busy clears and ignores canceled callbacks", () => {
    const frames = stubAnimationFrames();
    const item = editorItem("resume-item", { label: "Resume item" });
    const initial = editorSnapshot([item]);
    const props = {
      busy: false,
      confirmed: false,
      editsConfirmed: true,
      readOnly: false,
      onEdit: async () => {},
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
      onReview: async () => {},
    };
    const { rerender } = render(
      <AdvancedPlanPanel snapshot={initial} {...props} />,
    );
    act(() => screen.getByRole("button", { name: "Include" }).focus());
    const revision2 = {
      ...initial,
      revisionId: "resume-revision-two",
      revisionNumber: initial.revisionNumber + 1,
    };
    rerender(<AdvancedPlanPanel snapshot={revision2} {...props} />);
    const revision3 = {
      ...revision2,
      revisionId: "resume-revision-three",
      revisionNumber: revision2.revisionNumber + 1,
    };
    rerender(<AdvancedPlanPanel snapshot={revision3} {...props} busy />);
    act(() => frames.run(1));
    expect(frames.callbacks).toHaveLength(1);

    rerender(<AdvancedPlanPanel snapshot={revision3} {...props} />);
    expect(frames.callbacks).toHaveLength(2);
    act(() => frames.run(2));
    expect(screen.getByRole("button", { name: "Include" })).toHaveFocus();
  });

  it("cancels pending restoration on unmount before another editor can match it", () => {
    const frames = stubAnimationFrames();
    const item = editorItem("shared-focus", { label: "Shared focus" });
    const snapshot = editorSnapshot([item]);
    const props = {
      busy: false,
      confirmed: false,
      editsConfirmed: true,
      readOnly: false,
      onEdit: async () => {},
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
      onReview: async () => {},
    };
    const { rerender } = render(
      <>
        <AdvancedPlanPanel key="first" snapshot={snapshot} {...props} />
        <AdvancedPlanPanel key="second" snapshot={snapshot} {...props} />
      </>,
    );
    act(() => screen.getAllByRole("button", { name: "Include" })[0].focus());
    const next = {
      ...snapshot,
      revisionId: "shared-focus-revision-two",
      revisionNumber: snapshot.revisionNumber + 1,
    };
    rerender(
      <>
        <AdvancedPlanPanel key="first" snapshot={next} {...props} />
        <AdvancedPlanPanel key="second" snapshot={snapshot} {...props} />
      </>,
    );
    expect(frames.callbacks).toHaveLength(1);
    rerender(<AdvancedPlanPanel key="second" snapshot={snapshot} {...props} />);
    expect(frames.canceled).toContain(1);
    const search = screen.getByRole("searchbox", {
      name: "Search findings and groups",
    });
    act(() => search.focus());
    act(() => frames.run(1));
    expect(search).toHaveFocus();
  });

  it("sends the exact descendant exclusion identity set", () => {
    const folder = editorItem("project", { label: "Project", kind: "folder" });
    const child = editorItem("valuable", {
      label: "Valuable notes",
      parentId: "project",
    });
    const nested = editorItem("nested", {
      label: "Nested folder",
      parentId: "project",
      kind: "folder",
    });
    const nestedFile = editorItem("nested-file", {
      label: "Nested data",
      parentId: "nested",
    });
    const { onEdit } = mount(
      editorSnapshot([folder, child, nested, nestedFile]),
    );
    fireEvent.click(screen.getByRole("listitem", { name: /Project/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Valuable notes/ }));
    expect(onEdit).toHaveBeenCalledExactlyOnceWith({
      kind: "excludeDescendants",
      parentId: "project",
      itemIds: ["valuable"],
    });
  });

  it("distinguishes same-named descendants by ancestor context and keeps exact identities", () => {
    const { onEdit } = mount(
      editorSnapshot([
        editorItem("project", { label: "Project", kind: "folder" }),
        editorItem("first-folder", {
          label: "Design drafts",
          kind: "folder",
          parentId: "project",
        }),
        editorItem("second-folder", {
          label: "Published notes",
          kind: "folder",
          parentId: "project",
        }),
        editorItem("first-notes", {
          label: "notes.txt",
          parentId: "first-folder",
        }),
        editorItem("second-notes", {
          label: "notes.txt",
          parentId: "second-folder",
        }),
      ]),
    );
    fireEvent.click(screen.getByRole("listitem", { name: /Project/ }));
    const first = screen.getByRole("checkbox", {
      name: /notes.txt.*Design drafts/,
    });
    const second = screen.getByRole("checkbox", {
      name: /notes.txt.*Published notes/,
    });
    expect(first).not.toBe(second);
    expect(first).not.toBeChecked();
    expect(second).not.toBeChecked();
    fireEvent.click(first);
    expect(onEdit).toHaveBeenCalledExactlyOnceWith({
      kind: "excludeDescendants",
      parentId: "project",
      itemIds: ["first-notes"],
    });
  });

  it("bounds descendant pages after snapshots shrink and does not restore an obsolete page", () => {
    const folder = editorItem("paged-project", {
      label: "Paged project",
      kind: "folder",
      excludedDescendantIds: ["child-005", "child-050"],
    });
    const children = Array.from({ length: 120 }, (_, index) =>
      editorItem(`child-${String(index).padStart(3, "0")}`, {
        label: `Child ${String(index).padStart(3, "0")}`,
        parentId: folder.itemId,
      }),
    );
    const snapshot = (count: number, revision: number) =>
      editorSnapshot(
        [
          {
            ...folder,
            excludedDescendantIds: folder.excludedDescendantIds.filter((id) =>
              children.slice(0, count).some((child) => child.itemId === id),
            ),
          },
          ...children.slice(0, count),
        ],
        [],
        {
          revisionId: `descendants-${revision}`,
          revisionNumber: revision,
        },
      );
    const onEdit = vi
      .fn<(edit: PlanEdit) => Promise<void>>()
      .mockResolvedValue();
    const props = {
      busy: false,
      confirmed: false,
      onEdit,
      onUndo: async () => {},
      onRedo: async () => {},
      onRefresh: async () => {},
    };
    const view = render(
      <AdvancedPlanPanel snapshot={snapshot(120, 1)} {...props} />,
    );
    fireEvent.change(
      screen.getByRole("searchbox", { name: "Search findings and groups" }),
      { target: { value: "Paged project" } },
    );
    fireEvent.click(screen.getByRole("listitem", { name: /Paged project/ }));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(screen.getByText("Page 3 of 3")).toBeInTheDocument();
    expect(
      screen.getByRole("checkbox", { name: /Child 100/ }),
    ).toBeInTheDocument();

    view.rerender(<AdvancedPlanPanel snapshot={snapshot(20, 2)} {...props} />);
    expect(screen.getByText("Showing 1–20 of 20")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Next" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: /Child 005/ })).toBeChecked();

    view.rerender(<AdvancedPlanPanel snapshot={snapshot(120, 3)} {...props} />);
    expect(screen.getByText("Page 1 of 3")).toBeInTheDocument();
    expect(
      screen.getByRole("checkbox", { name: /Child 000/ }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(screen.getByText("Page 2 of 3")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(screen.getByText("Page 3 of 3")).toBeInTheDocument();
    view.rerender(<AdvancedPlanPanel snapshot={snapshot(60, 4)} {...props} />);
    expect(screen.getByText("Showing 51–60 of 60")).toBeInTheDocument();
    expect(screen.getByText("Page 2 of 2")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Previous" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /Child 050/ })).toBeChecked();
    fireEvent.click(screen.getByRole("checkbox", { name: /Child 050/ }));
    expect(onEdit).toHaveBeenLastCalledWith({
      kind: "excludeDescendants",
      parentId: "paged-project",
      itemIds: ["child-005"],
    });
    fireEvent.click(screen.getByRole("button", { name: "Previous" }));
    expect(screen.getByText("Page 1 of 2")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(screen.getByText("Page 2 of 2")).toBeInTheDocument();
    const search = screen.getByRole("searchbox", {
      name: "Search descendants",
    });
    fireEvent.change(search, { target: { value: "not a child" } });
    expect(screen.getByText("No matching descendants")).toBeInTheDocument();
    expect(screen.queryAllByRole("checkbox", { name: /^Child / })).toHaveLength(
      0,
    );
    fireEvent.change(search, { target: { value: "" } });
    expect(screen.getByText("Showing 1–50 of 60")).toBeInTheDocument();
    expect(screen.getByText("Page 1 of 2")).toBeInTheDocument();
    expect(screen.getAllByRole("checkbox", { name: /^Child / })).toHaveLength(
      50,
    );
    expect(onEdit).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["busy", { busy: true }],
    ["read only", { readOnly: true }],
    ["unconfirmed edits", { editsConfirmed: false }],
  ] as const)("refuses mutation while %s", (_name, props) => {
    const item = editorItem("item", { label: "Candidate" });
    const snapshot = editorSnapshot(
      [item],
      [editorGroup("group", [item.itemId])],
      {
        canUndo: true,
        canRedo: true,
      },
    );
    const { onEdit, onUndo, onRedo, onReview } = mount(snapshot, props);
    expect(screen.getByRole("button", { name: "Include" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Keep group" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Include" }));
    fireEvent.click(screen.getByRole("button", { name: "Undo last change" }));
    fireEvent.click(screen.getByRole("button", { name: "Redo change" }));
    fireEvent.click(screen.getByRole("button", { name: /Review .*changes/ }));
    expect(onEdit).not.toHaveBeenCalled();
    expect(onUndo).not.toHaveBeenCalled();
    expect(onRedo).not.toHaveBeenCalled();
    expect(onReview).not.toHaveBeenCalled();
  });
});

describe("AdvancedPlanPanel large review navigation", () => {
  it("keeps the 1,000 finding, 25 group review surface bounded and navigable", () => {
    const snapshot = groupedEditorSnapshot();
    mount(snapshot);
    const list = screen.getByRole("list", { name: "Grouped plan findings" });
    expect(screen.getByText(/1,000 items · 25 groups/)).toBeInTheDocument();
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(100);

    const firstGroup = screen.getByRole("button", {
      name: /Collapse Review group 01/,
    });
    firstGroup.focus();
    fireEvent.keyDown(firstGroup, { key: "End" });
    const finalRow = screen.getByRole("listitem", { name: /Finding 1000/ });
    expect(finalRow).toBeInTheDocument();
    expect(finalRow).toHaveFocus();
    fireEvent.keyDown(finalRow, { key: "Enter" });
    expect(
      screen.getByRole("heading", { name: "Finding 1000" }),
    ).toBeInTheDocument();

    fireEvent.keyDown(finalRow, { key: "Home" });
    expect(
      screen.getByRole("button", { name: /Collapse Review group 01/ }),
    ).toHaveFocus();
  });

  it("bounds mounted rows for 10,000 typed items and preserves focus through window moves", () => {
    const items = Array.from({ length: 10_000 }, (_, index) =>
      editorItem(`large-${index + 1}`, {
        label: `Large finding ${String(index + 1).padStart(5, "0")}`,
        groupIds: ["large-group"],
      }),
    );
    const snapshot = editorSnapshot(items, [
      editorGroup(
        "large-group",
        items.map((item) => item.itemId),
      ),
    ]);
    mount(snapshot);
    const list = screen.getByRole("list", { name: "Grouped plan findings" });
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(100);

    const group = screen.getByRole("button", { name: /Collapse large-group/ });
    group.focus();
    fireEvent.keyDown(group, { key: "End" });
    const finalRow = screen.getByRole("listitem", {
      name: /Large finding 10000/,
    });
    expect(finalRow).toBeInTheDocument();
    expect(finalRow).toHaveFocus();
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(100);
    fireEvent.keyDown(finalRow, { key: "ArrowUp" });
    expect(
      screen.getByRole("listitem", { name: /Large finding 09999/ }),
    ).toHaveFocus();
  });

  it("clamps a scrolled window when a revision removes most visible members", () => {
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const items = Array.from({ length: 80 }, (_, index) =>
      editorItem(`revision-${index}`, { groupIds: ["revision-group"] }),
    );
    const groups = [
      editorGroup(
        "revision-group",
        items.map((item) => item.itemId),
      ),
    ];
    const view = mount(editorSnapshot(items, groups));
    const list = screen.getByRole("list", { name: "Grouped plan findings" });
    list.scrollTop = 79 * 58;
    fireEvent.scroll(list);
    expect(
      within(list).queryByRole("listitem", { name: /^revision-0,/ }),
    ).not.toBeInTheDocument();
    view.rerender(
      <AdvancedPlanPanel
        snapshot={editorSnapshot(
          items.map((item, index) => ({ ...item, inPlan: index === 0 })),
          groups,
          {
            revisionId: "fixture-revision-2",
            revisionNumber: 2,
          },
        )}
        busy={false}
        confirmed={false}
        onEdit={view.onEdit}
        onUndo={view.onUndo}
        onRedo={view.onRedo}
        onRefresh={view.onRefresh}
        onReview={view.onReview}
      />,
    );
    expect(
      within(list).getByRole("listitem", { name: /^revision-0,/ }),
    ).toBeInTheDocument();
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(list.scrollTop).toBe(0);
  });

  it("keeps filtered rows visible when a 10,000-item window shrinks from the end", () => {
    const items = Array.from({ length: 10_000 }, (_, index) =>
      editorItem(`shrink-${index + 1}`, {
        label: `Shrink finding ${String(index + 1).padStart(5, "0")}`,
        groupIds: ["shrink-group"],
      }),
    );
    const snapshot = editorSnapshot(items, [
      editorGroup(
        "shrink-group",
        items.map((item) => item.itemId),
      ),
    ]);
    mount(snapshot);
    const list = screen.getByRole("list", { name: "Grouped plan findings" });
    Object.defineProperty(list, "clientHeight", {
      configurable: true,
      value: 464,
    });
    list.scrollTop = 10_000 * 58;
    fireEvent.scroll(list);
    expect(
      within(list).getByRole("listitem", { name: /Shrink finding 10000/ }),
    ).toBeInTheDocument();

    const search = screen.getByRole("searchbox", {
      name: "Search findings and groups",
    });
    search.focus();
    fireEvent.change(search, { target: { value: "Shrink finding 00001" } });
    expect(
      within(list).getByRole("listitem", { name: /Shrink finding 00001/ }),
    ).toBeInTheDocument();
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(search).toHaveFocus();
    expect(list.scrollTop).toBe(0);

    fireEvent.change(search, { target: { value: "" } });
    expect(
      within(list).getByRole("listitem", { name: /Shrink finding 00001/ }),
    ).toBeInTheDocument();
    expect(within(list).getAllByRole("listitem").length).toBeLessThan(100);
    expect(list.scrollTop).toBe(0);
    const first = within(list).getByRole("listitem", {
      name: /Shrink finding 00001/,
    });
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(
      within(list).getByRole("listitem", { name: /Shrink finding 00002/ }),
    ).toHaveFocus();
  });
});
