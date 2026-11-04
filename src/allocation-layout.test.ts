import { describe, expect, test } from "vitest";
import {
  buildAllocationLayout,
  type AllocationInput,
} from "./allocation-layout";

function allocation(weights: string[]): AllocationInput {
  return {
    knownAllocatedBytes: weights
      .reduce((sum, value) => sum + BigInt(value), 0n)
      .toString(),
    tiles: weights.map((allocatedBytes, index) => ({
      entryIds: [`file-${index}`],
      allocatedBytes,
      category: index % 2 ? "document" : "image",
    })),
  };
}

describe("buildAllocationLayout", () => {
  test("covers the map without overlaps for uneven normal weights", () => {
    const result = buildAllocationLayout(allocation(["50", "25", "15", "10"]));
    expect(result.tiles).toHaveLength(4);
    expect(
      result.tiles.every(
        (tile) =>
          tile.x >= 0 &&
          tile.y >= 0 &&
          tile.width >= 0 &&
          tile.height >= 0 &&
          tile.x + tile.width <= 1000 + 1e-8 &&
          tile.y + tile.height <= 1000 + 1e-8,
      ),
    ).toBe(true);
    for (const tile of result.tiles) {
      expect((tile.width * tile.height) / 1_000_000).toBeCloseTo(
        Number(tile.allocatedBytes) / Number(result.totalBytes),
        12,
      );
    }
    for (let i = 0; i < result.tiles.length; i += 1) {
      for (let j = i + 1; j < result.tiles.length; j += 1) {
        const a = result.tiles[i];
        const b = result.tiles[j];
        const overlapX =
          Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x);
        const overlapY =
          Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y);
        expect(overlapX > 1e-8 && overlapY > 1e-8).toBe(false);
      }
    }
    expect(
      result.tiles.reduce((sum, tile) => sum + tile.width * tile.height, 0),
    ).toBeCloseTo(1_000_000, 5);
  });

  test("uses exact BigInt totals with huge u64 and tiny evidence", () => {
    const input = allocation(["18446744073709551614", "1"]);
    const result = buildAllocationLayout(input);
    expect(result.totalBytes).toBe("18446744073709551615");
    expect(
      result.tiles.reduce((sum, tile) => sum + BigInt(tile.allocatedBytes), 0n),
    ).toBe(18_446_744_073_709_551_615n);
    expect(result.tiles).toHaveLength(2);
    expect(
      result.tiles.find((tile) => tile.entryIds[0] === "file-1")?.width,
    ).toBeGreaterThan(0);
  });

  test("aggregates several tiny tiles into one neutral group without losing bytes", () => {
    const result = buildAllocationLayout(allocation(["997", "1", "1", "1"]));
    const other = result.tiles.find((tile) => tile.category === "other");
    expect(other?.entryIds).toEqual(["file-1", "file-2", "file-3"]);
    expect(other?.allocatedBytes).toBe("3");
    expect(
      result.tiles.reduce((sum, tile) => sum + BigInt(tile.allocatedBytes), 0n),
    ).toBe(1000n);
  });

  test("rejects malformed, duplicate, zero, and inconsistent evidence", () => {
    expect(() =>
      buildAllocationLayout({ knownAllocatedBytes: "10x", tiles: [] }),
    ).toThrow();
    expect(() =>
      buildAllocationLayout({
        knownAllocatedBytes: "11",
        tiles: [{ entryIds: ["a"], allocatedBytes: "10", category: null }],
      }),
    ).toThrow(/does not match/);
    expect(() =>
      buildAllocationLayout({
        knownAllocatedBytes: "1",
        tiles: [{ entryIds: ["a", "a"], allocatedBytes: "1", category: null }],
      }),
    ).toThrow(/Duplicate/);
    expect(() =>
      buildAllocationLayout({
        knownAllocatedBytes: "1",
        tiles: [{ entryIds: ["a"], allocatedBytes: "0", category: null }],
      }),
    ).toThrow();
  });

  test("rejects input beyond native tile and page-entry bounds", () => {
    expect(() =>
      buildAllocationLayout(allocation(Array.from({ length: 65 }, () => "1"))),
    ).toThrow(/Too many allocation tiles/);
    expect(() =>
      buildAllocationLayout({
        knownAllocatedBytes: "1",
        tiles: [
          {
            entryIds: Array.from({ length: 513 }, (_, index) => `id-${index}`),
            allocatedBytes: "1",
            category: null,
          },
        ],
      }),
    ).toThrow(/Duplicate or invalid/);
  });

  test("handles an empty current page with no known allocation", () => {
    expect(
      buildAllocationLayout({ knownAllocatedBytes: "0", tiles: [] }),
    ).toEqual({ totalBytes: "0", tiles: [] });
  });
});
