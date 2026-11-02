import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useReviewPilotSession } from "./review-pilot";
import type { PilotContext } from "./review-pilot-contract";

const context: PilotContext = {
  revisionNumber: 1,
  itemCount: 3,
  groupCount: 1,
  selectedCount: 0,
  logicalSelectedBytes: 0,
  confirmed: true,
};
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useReviewPilotSession", () => {
  it("uses clock time despite delayed ticks, rejects invalid start, records and freezes counts", () => {
    let now = 100;
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() =>
      useReviewPilotSession({ now: () => now }),
    );
    const startFn = result.current.start;
    const finishFn = result.current.finish;
    expect(result.current.start("bad code", context)).toBe(false);
    expect(result.current.start("P1", { ...context, confirmed: false })).toBe(
      false,
    );
    act(() => {
      result.current.start("P1", context);
    });
    act(() => {
      expect(result.current.start("P2", context)).toBe(false);
    });
    expect(result.current.start).toBe(startFn);
    expect(result.current.finish).toBe(finishFn);
    now = 5400;
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(result.current.elapsedMs).toBe(5300);
    act(() => {
      result.current.record("pointer_activation");
      result.current.finish({ ...context, confirmed: false });
    });
    expect(result.current.report?.counts.pointer_activation).toBe(1);
    expect(result.current.report?.final).toEqual({
      ...context,
      confirmed: false,
    });
    expect(result.current.report?.humanAssessment).toBe("not_recorded");
    expect(result.current.report?.executionAvailable).toBe(false);
    act(() => result.current.record("pointer_activation"));
    expect(result.current.report?.counts.pointer_activation).toBe(1);
    act(() => result.current.discard());
    expect(result.current.report).toBeNull();
    unmount();
    vi.useRealTimers();
  });
  it("keeps clock failures null", () => {
    let now = 50;
    vi.useFakeTimers();
    const { result } = renderHook(() =>
      useReviewPilotSession({ now: () => now }),
    );
    act(() => result.current.start("P2", context));
    now = 70;
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(result.current.elapsedMs).toBe(20);
    now = 60;
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(result.current.elapsedMs).toBeNull();
    now = 500;
    act(() => result.current.finish(context));
    expect(result.current.report?.elapsedMs).toBeNull();
    vi.useRealTimers();
  });

  it("cleans up the interval on unmount", () => {
    vi.useFakeTimers();
    const clear = vi.spyOn(window, "clearInterval");
    const { result, unmount } = renderHook(() => useReviewPilotSession());
    act(() => result.current.start("P3", context));
    unmount();
    expect(clear).toHaveBeenCalled();
    vi.useRealTimers();
  });
  it("ignores stale timer callbacks and preserves an unknown final snapshot", () => {
    let now = 100;
    const callbacks: Array<() => void> = [];
    const setInterval = vi
      .fn<typeof window.setInterval>()
      .mockImplementation((callback) => {
        callbacks.push(callback as () => void);
        return callbacks.length;
      });
    const clearInterval = vi.fn<typeof window.clearInterval>();
    const clock = { now: () => now, setInterval, clearInterval };
    const { result } = renderHook(() => useReviewPilotSession(clock));
    act(() => result.current.record("key_press"));
    expect(result.current.report).toBeNull();
    act(() => {
      result.current.start("P4", context);
    });
    const oldTick = callbacks[0];
    act(() => {
      result.current.discard();
      result.current.start("P5", context);
    });
    now = 5100;
    act(() => oldTick());
    expect(result.current.elapsedMs).toBe(0);
    act(() => callbacks[1]());
    expect(result.current.elapsedMs).toBe(5000);
    act(() => result.current.finish(null));
    expect(result.current.report?.elapsedMs).toBe(5000);
    expect(result.current.report?.final).toBeNull();
    expect(result.current.report?.initial.itemCount).toBe(3);
    expect(result.current.report?.counts.key_press).toBe(0);
  });
});
