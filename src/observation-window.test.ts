import { expect, test } from "vitest";
import {
  formatObservationTime,
  observationClockReversed,
} from "./observation-window";

test("device-clock display preserves exact range checks and does not turn unknown into epoch", () => {
  expect(formatObservationTime("1700000000000")).toBe(
    "2023-11-14T22:13:20.000Z",
  );
  expect(formatObservationTime("0")).toBe("1970-01-01T00:00:00.000Z");
  for (const value of [
    null,
    undefined,
    "",
    "-1",
    "1e3",
    "NaN",
    "18446744073709551615",
    "8640000000000001",
  ]) {
    expect(formatObservationTime(value)).toBeNull();
  }
  expect(observationClockReversed("1700000000001", "1700000000000")).toBe(true);
  expect(observationClockReversed(null, "0")).toBe(false);
  expect(observationClockReversed("0", "0")).toBe(false);
  expect(observationClockReversed("bad", "0")).toBe(false);
});
