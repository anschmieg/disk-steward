export type AllocationTileInput = {
  entryIds: string[];
  allocatedBytes: string;
  category: string | null;
};

export type AllocationInput = {
  knownAllocatedBytes: string;
  tiles: AllocationTileInput[];
};

export type AllocationRect = {
  entryIds: string[];
  allocatedBytes: string;
  category: string | null;
  x: number;
  y: number;
  width: number;
  height: number;
};

export type AllocationLayout = {
  totalBytes: string;
  tiles: AllocationRect[];
};

const U64_MAX = 18_446_744_073_709_551_615n;
const EDGE = 1000;

function parseU64(value: string, label: string, allowZero = false): bigint {
  if (
    typeof value !== "string" ||
    value.length > 20 ||
    !/^(0|[1-9][0-9]*)$/.test(value)
  )
    throw new Error(`Invalid ${label}`);
  const parsed = BigInt(value);
  if ((!allowZero && parsed <= 0n) || parsed > U64_MAX)
    throw new Error(`Invalid ${label}`);
  return parsed;
}

function ratio(part: bigint, whole: bigint): number {
  const scale = 10n ** 24n;
  return Number((part * scale) / whole) / Number(scale);
}

function stableKey(tile: AllocationTileInput): string {
  return [...tile.entryIds].sort().join("\u0000");
}

function sumBytes(tiles: AllocationTileInput[]): bigint {
  return tiles.reduce((sum, tile) => sum + BigInt(tile.allocatedBytes), 0n);
}

function mergeAsOther(tiles: AllocationTileInput[]): AllocationTileInput {
  return {
    entryIds: tiles.flatMap((tile) => tile.entryIds).sort(),
    allocatedBytes: sumBytes(tiles).toString(),
    category: "other",
  };
}

function groupTinyTiles(
  tiles: AllocationTileInput[],
  total: bigint,
): AllocationTileInput[] {
  if (tiles.length < 3) return tiles;
  const tiny = tiles.filter(
    (tile) => BigInt(tile.allocatedBytes) * 100n < total,
  );
  if (tiny.length < 2) return tiles;
  const tinyKeys = new Set(tiny.map(stableKey));
  return [
    ...tiles.filter((tile) => !tinyKeys.has(stableKey(tile))),
    mergeAsOther(tiny),
  ];
}

function split(
  tiles: AllocationTileInput[],
  rect: { x: number; y: number; width: number; height: number },
  output: AllocationRect[],
): void {
  if (tiles.length === 1) {
    const tile = tiles[0];
    output.push({ ...tile, ...rect });
    return;
  }
  const total = sumBytes(tiles);
  let accumulated = 0n;
  let splitAt = 1;
  let bestDifference: bigint | null = null;
  for (let index = 1; index < tiles.length; index += 1) {
    accumulated += BigInt(tiles[index - 1].allocatedBytes);
    const difference =
      accumulated * 2n > total
        ? accumulated * 2n - total
        : total - accumulated * 2n;
    if (bestDifference === null || difference < bestDifference) {
      bestDifference = difference;
      splitAt = index;
    }
  }
  const first = tiles.slice(0, splitAt);
  const second = tiles.slice(splitAt);
  const firstWeight = sumBytes(first);
  if (rect.width >= rect.height) {
    let firstWidth: number;
    let secondWidth: number;
    if (firstWeight * 2n <= total) {
      firstWidth = rect.width * ratio(firstWeight, total);
      secondWidth = rect.width - firstWidth;
    } else {
      secondWidth = rect.width * ratio(total - firstWeight, total);
      firstWidth = rect.width - secondWidth;
    }
    split(first, { ...rect, width: firstWidth }, output);
    split(
      second,
      { ...rect, x: rect.x + firstWidth, width: secondWidth },
      output,
    );
  } else {
    let firstHeight: number;
    let secondHeight: number;
    if (firstWeight * 2n <= total) {
      firstHeight = rect.height * ratio(firstWeight, total);
      secondHeight = rect.height - firstHeight;
    } else {
      secondHeight = rect.height * ratio(total - firstWeight, total);
      firstHeight = rect.height - secondHeight;
    }
    split(first, { ...rect, height: firstHeight }, output);
    split(
      second,
      { ...rect, y: rect.y + firstHeight, height: secondHeight },
      output,
    );
  }
}

/** Bounded proportional geometry; byte evidence stays exact and separate. */
export function buildAllocationLayout(
  input: AllocationInput,
): AllocationLayout {
  if (!Array.isArray(input.tiles)) throw new Error("Invalid allocation tiles");
  if (input.tiles.length > 64) throw new Error("Too many allocation tiles");
  const total = parseU64(input.knownAllocatedBytes, "allocation total", true);
  if (total === 0n && input.tiles.length === 0)
    return { totalBytes: "0", tiles: [] };
  if (total === 0n) throw new Error("Zero allocation cannot contain tiles");
  const ids = new Set<string>();
  let idCount = 0;
  const validated = input.tiles.map((tile) => {
    if (!Array.isArray(tile.entryIds) || tile.entryIds.length === 0)
      throw new Error("Invalid tile entries");
    for (const id of tile.entryIds) {
      idCount += 1;
      if (
        typeof id !== "string" ||
        id.length === 0 ||
        id.length > 256 ||
        /[\u0000-\u001f\u007f]/.test(id) ||
        idCount > 512 ||
        ids.has(id)
      )
        throw new Error("Duplicate or invalid allocation entry");
      ids.add(id);
    }
    const bytes = parseU64(tile.allocatedBytes, "tile allocation");
    if (bytes > total) throw new Error("Tile exceeds allocation total");
    if (
      tile.category !== null &&
      (typeof tile.category !== "string" ||
        ![
          "image",
          "document",
          "archive",
          "media",
          "code",
          "otherFile",
          "folder",
          "link",
          "other",
          "unreadable",
        ].includes(tile.category))
    )
      throw new Error("Invalid tile category");
    return {
      ...tile,
      entryIds: [...tile.entryIds],
      allocatedBytes: bytes.toString(),
    };
  });
  if (sumBytes(validated) !== total)
    throw new Error("Allocation total does not match its tiles");
  const grouped = groupTinyTiles(validated, total).sort((a, b) => {
    const left = BigInt(a.allocatedBytes);
    const right = BigInt(b.allocatedBytes);
    return left === right
      ? stableKey(a).localeCompare(stableKey(b))
      : left > right
        ? -1
        : 1;
  });
  const tiles: AllocationRect[] = [];
  split(grouped, { x: 0, y: 0, width: EDGE, height: EDGE }, tiles);
  return { totalBytes: total.toString(), tiles };
}
