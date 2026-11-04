/** Advisory device-clock timestamps, never freshness or approval authority. */
export function formatObservationTime(
  value: string | null | undefined,
): string | null {
  if (!value || !/^\d{1,20}$/.test(value)) return null;
  const milliseconds = BigInt(value);
  // ECMAScript Date's supported range is within the exact integer range.
  if (milliseconds > 8_640_000_000_000_000n) return null;
  const date = new Date(Number(milliseconds));
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

export function observationClockReversed(
  start: string | null | undefined,
  finish: string | null | undefined,
): boolean {
  return (
    !!start &&
    !!finish &&
    /^\d{1,20}$/.test(start) &&
    /^\d{1,20}$/.test(finish) &&
    BigInt(finish) < BigInt(start)
  );
}
