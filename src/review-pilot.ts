import { useCallback, useEffect, useRef, useState } from "react";
import type {
  PilotContext,
  PilotEvent,
  ReviewPilotRecorder,
  ReviewPilotReport,
} from "./review-pilot-contract";
import { PILOT_EVENTS } from "./review-pilot-contract";

type Session = {
  phase: ReviewPilotRecorder["phase"];
  token: symbol | null;
  startedAt: number | null;
  elapsedMs: number | null;
  report: ReviewPilotReport | null;
  participantCode: string;
  initial: PilotContext | null;
  counts: Record<PilotEvent, number>;
};
type Clock = {
  now: () => number;
  setInterval?: typeof window.setInterval;
  clearInterval?: typeof window.clearInterval;
};
const emptyCounts = (): Record<PilotEvent, number> =>
  Object.fromEntries(PILOT_EVENTS.map((event) => [event, 0])) as Record<
    PilotEvent,
    number
  >;
const emptySession = (): Session => ({
  phase: "idle",
  token: null,
  startedAt: null,
  elapsedMs: null,
  report: null,
  participantCode: "",
  initial: null,
  counts: emptyCounts(),
});
const validNumbers = (context: PilotContext) =>
  [
    context.revisionNumber,
    context.itemCount,
    context.groupCount,
    context.selectedCount,
    context.logicalSelectedBytes,
  ].every((value) => Number.isSafeInteger(value) && value >= 0);
const validStartContext = (context: PilotContext) =>
  context.confirmed && validNumbers(context);

export function useReviewPilotSession(clock?: Clock): ReviewPilotRecorder {
  const clockRef = useRef(clock);
  clockRef.current = clock;
  const [session, setSession] = useState<Session>(emptySession);
  const phaseRef = useRef<Session["phase"]>("idle");
  const startedAtRef = useRef<number | null>(null);
  const tokenRef = useRef<symbol | null>(null);
  const timingRef = useRef<{ lastObservedAt: number | null; invalid: boolean }>(
    { lastObservedAt: null, invalid: false },
  );

  useEffect(() => {
    if (session.phase !== "recording") return;
    const token = session.token;
    const timer = (clockRef.current?.setInterval ?? window.setInterval)(() => {
      if (phaseRef.current !== "recording" || tokenRef.current !== token)
        return;
      const now = (clockRef.current?.now ?? (() => performance.now()))();
      const start = session.startedAt ?? now;
      const elapsed = now - start;
      const timing = timingRef.current;
      if (
        !Number.isFinite(now) ||
        !Number.isFinite(elapsed) ||
        elapsed < 0 ||
        elapsed > Number.MAX_SAFE_INTEGER ||
        (timing.lastObservedAt !== null && now < timing.lastObservedAt)
      )
        timing.invalid = true;
      timing.lastObservedAt = Number.isFinite(now) ? now : null;
      setSession((previous) =>
        previous.phase !== "recording"
          ? previous
          : { ...previous, elapsedMs: timing.invalid ? null : elapsed },
      );
    }, 1000);
    return () =>
      (clockRef.current?.clearInterval ?? window.clearInterval)(timer);
  }, [session.phase, session.startedAt, session.token]);

  const start = useCallback(
    (participantCode: string, context: PilotContext) => {
      if (
        phaseRef.current === "recording" ||
        !/^[A-Za-z0-9_-]{1,32}$/.test(participantCode) ||
        !validStartContext(context)
      )
        return false;
      const startedAt = (clockRef.current?.now ?? (() => performance.now()))();
      if (!Number.isFinite(startedAt) || startedAt < 0) return false;
      phaseRef.current = "recording";
      const token = Symbol();
      tokenRef.current = token;
      startedAtRef.current = startedAt;
      timingRef.current = { lastObservedAt: startedAt, invalid: false };
      setSession({
        ...emptySession(),
        phase: "recording",
        token,
        startedAt,
        elapsedMs: 0,
        participantCode,
        initial: { ...context },
      });
      return true;
    },
    [],
  );

  const record = useCallback((event: PilotEvent) => {
    if (phaseRef.current !== "recording") return;
    setSession((previous) => {
      if (previous.phase !== "recording") return previous;
      const count = previous.counts[event];
      if (count >= Number.MAX_SAFE_INTEGER) return previous;
      return {
        ...previous,
        counts: { ...previous.counts, [event]: count + 1 },
      };
    });
  }, []);

  const finish = useCallback((context: PilotContext | null) => {
    if (phaseRef.current !== "recording") return;
    const now = (clockRef.current?.now ?? (() => performance.now()))();
    const timing = timingRef.current;
    const start = startedAtRef.current ?? now;
    const difference = now - start;
    if (
      !Number.isFinite(now) ||
      !Number.isFinite(difference) ||
      difference < 0 ||
      difference > Number.MAX_SAFE_INTEGER ||
      (timing.lastObservedAt !== null && now < timing.lastObservedAt)
    )
      timing.invalid = true;
    timing.lastObservedAt = Number.isFinite(now) ? now : null;
    const elapsedMs = timing.invalid ? null : difference;
    phaseRef.current = "finished";
    tokenRef.current = null;
    setSession((previous) => {
      if (previous.phase !== "recording" || !previous.initial) return previous;
      const final = context && validNumbers(context) ? { ...context } : null;
      const report: ReviewPilotReport = {
        schemaVersion: 1,
        kind: "synthetic_review_pilot_metrics",
        profile: "pilot",
        participantCode: previous.participantCode,
        initial: previous.initial,
        final,
        elapsedMs,
        counts: previous.counts,
        humanAssessment: "not_recorded",
        executionAvailable: false,
      };
      return {
        ...previous,
        phase: "finished",
        elapsedMs,
        report,
        startedAt: null,
      };
    });
  }, []);

  const discard = useCallback(() => {
    phaseRef.current = "idle";
    tokenRef.current = null;
    startedAtRef.current = null;
    timingRef.current = { lastObservedAt: null, invalid: false };
    setSession(emptySession());
  }, []);
  return {
    phase: session.phase,
    elapsedMs: session.elapsedMs,
    report: session.report,
    start,
    finish,
    discard,
    record,
  };
}
