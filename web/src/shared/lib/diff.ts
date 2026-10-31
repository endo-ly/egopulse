/** A single entry in a line-level diff */
export type DiffLine =
  | { type: "add"; content: string }
  | { type: "remove"; content: string }
  | { type: "unchanged"; before: string; after: string };

/**
 * Computes a line-level diff between two strings.
 *
 * Returns an ordered array of `DiffLine` entries that turn `before` into
 * `after`. Unchanged lines carry both the before and after content for
 * convenient rendering.
 *
 * # Edge cases
 * - An empty string is treated as having zero lines.
 * - When a line is replaced (different content at the same
 *   position), the output pairs a `remove` for the old line
 *   followed by an `add` for the new line.
 */
export function computeLineDiff(before: string, after: string): DiffLine[] {
  const parts = splitAroundCommonEdges(before, after);
  const lines: DiffLine[] = [];
  for (const line of parts.head) lines.push(unchangedLine(line));
  alignRegion(parts.beforeMiddle, parts.afterMiddle, (line) => lines.push(line));
  for (const line of parts.tail) lines.push(unchangedLine(line));
  return lines;
}

/** Counts added and removed lines between two texts. */
export function countLineChanges(before: string, after: string): { add: number; remove: number } {
  const parts = splitAroundCommonEdges(before, after);
  const counts = { add: 0, remove: 0 };
  alignRegion(parts.beforeMiddle, parts.afterMiddle, (line) => {
    if (line.type === "add") counts.add += 1;
    else if (line.type === "remove") counts.remove += 1;
  });
  return counts;
}

/**
 * Regions up to this many line pairs are aligned exactly, which bounds the
 * table used for one region to a megabyte.
 */
const MAX_TABLE_CELLS = 250_000;

interface SplitParts {
  beforeLines: string[];
  afterLines: string[];
  /** Lines common to both texts before the first difference. */
  head: string[];
  /** The differing region, which is all that has to be aligned. */
  beforeMiddle: string[];
  afterMiddle: string[];
  /** Lines common to both texts after the last difference. */
  tail: string[];
}

/**
 * Splits both texts into lines and peels off the common leading and trailing
 * runs. A common prefix or suffix is always part of some longest common
 * subsequence, so removing it keeps both the subsequence length and the diff
 * intact.
 */
function splitAroundCommonEdges(before: string, after: string): SplitParts {
  const beforeLines = splitLines(before);
  const afterLines = splitLines(after);

  const shortest = Math.min(beforeLines.length, afterLines.length);
  let prefix = 0;
  while (prefix < shortest && beforeLines[prefix] === afterLines[prefix]) prefix += 1;

  let suffix = 0;
  while (
    suffix < shortest - prefix &&
    beforeLines[beforeLines.length - 1 - suffix] === afterLines[afterLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  return {
    beforeLines,
    afterLines,
    head: beforeLines.slice(0, prefix),
    beforeMiddle: beforeLines.slice(prefix, beforeLines.length - suffix),
    afterMiddle: afterLines.slice(prefix, afterLines.length - suffix),
    tail: beforeLines.slice(beforeLines.length - suffix),
  };
}

function splitLines(text: string): string[] {
  return text === "" ? [] : text.split("\n");
}

function unchangedLine(line: string): DiffLine {
  return { type: "unchanged", before: line, after: line };
}

interface Range {
  start: number;
  end: number;
}

interface Segment {
  before: Range;
  after: Range;
}

/**
 * Emits the diff of two line arrays that share no common leading or trailing
 * lines.
 *
 * A region small enough to compare line by line is aligned exactly. A larger
 * one is split on the lines that appear exactly once on each side, because a
 * line without duplicates is the context a reader recognizes as unchanged;
 * the pieces between them are then small enough to align exactly. A region
 * without such a line is halved until its pieces can be aligned exactly, and
 * one that shares no line at all is reported as replaced.
 *
 * The diff and the line counts both come from this walk, so the counts shown
 * for a change always describe the diff it expands to.
 */
function alignRegion(before: string[], after: string[], emit: (line: DiffLine) => void): void {
  const pending: Segment[] = [
    {
      before: { start: 0, end: before.length },
      after: { start: 0, end: after.length },
    },
  ];

  while (pending.length > 0) {
    const { before: beforeRange, after: afterRange } = pending.pop()!;
    const rows = beforeRange.end - beforeRange.start;
    const columns = afterRange.end - afterRange.start;

    if (rows === 0) {
      for (let index = afterRange.start; index < afterRange.end; index += 1) {
        emit({ type: "add", content: after[index] });
      }
      continue;
    }
    if (columns === 0) {
      for (let index = beforeRange.start; index < beforeRange.end; index += 1) {
        emit({ type: "remove", content: before[index] });
      }
      continue;
    }
    if (rows * columns <= MAX_TABLE_CELLS) {
      emitAlignedRegion(before, beforeRange, after, afterRange, emit);
      continue;
    }

    const anchors = uniqueLineAnchors(before, beforeRange, after, afterRange);
    if (anchors.length === 0) {
      if (!sharesAnyLine(before, beforeRange, after, afterRange)) {
        for (let index = beforeRange.start; index < beforeRange.end; index += 1) {
          emit({ type: "remove", content: before[index] });
        }
        for (let index = afterRange.start; index < afterRange.end; index += 1) {
          emit({ type: "add", content: after[index] });
        }
        continue;
      }
      // Only repeated lines in common: halve the region so each half becomes
      // small enough to align exactly.
      const beforeMiddle = beforeRange.start + (rows >> 1);
      const afterMiddle = afterRange.start + (columns >> 1);
      pending.push({
        before: { start: beforeMiddle, end: beforeRange.end },
        after: { start: afterMiddle, end: afterRange.end },
      });
      pending.push({
        before: { start: beforeRange.start, end: beforeMiddle },
        after: { start: afterRange.start, end: afterMiddle },
      });
      continue;
    }

    // Queue the pieces around the anchors in reverse, so they emit in order.
    const lastAnchor = anchors[anchors.length - 1];
    pending.push({
      before: { start: lastAnchor.before + 1, end: beforeRange.end },
      after: { start: lastAnchor.after + 1, end: afterRange.end },
    });
    for (let index = anchors.length - 1; index >= 0; index -= 1) {
      const anchor = anchors[index];
      pending.push({
        before: { start: anchor.before, end: anchor.before + 1 },
        after: { start: anchor.after, end: anchor.after + 1 },
      });
      const previous = anchors[index - 1];
      pending.push({
        before: {
          start: previous === undefined ? beforeRange.start : previous.before + 1,
          end: anchor.before,
        },
        after: {
          start: previous === undefined ? afterRange.start : previous.after + 1,
          end: anchor.after,
        },
      });
    }
  }
}

interface Anchor {
  before: number;
  after: number;
}

/**
 * The lines that occur exactly once on each side, chained into the longest run
 * that keeps their order on both sides. These are the positions a reader would
 * agree are unchanged, so they make stable split points for a large region.
 */
function uniqueLineAnchors(
  before: string[],
  beforeRange: Range,
  after: string[],
  afterRange: Range,
): Anchor[] {
  const beforeIndexes = uniqueIndexes(before, beforeRange);
  const afterIndexes = uniqueIndexes(after, afterRange);

  const candidates: Anchor[] = [];
  for (const [line, beforeIndex] of beforeIndexes) {
    const afterIndex = afterIndexes.get(line);
    if (afterIndex !== undefined) candidates.push({ before: beforeIndex, after: afterIndex });
  }
  candidates.sort((left, right) => left.before - right.before);

  // Keep only the candidates whose `after` positions increase, so the chain
  // runs forward in both texts.
  const tails: number[] = [];
  const tailCandidate: number[] = [];
  const previous: number[] = [];
  for (let index = 0; index < candidates.length; index += 1) {
    const afterIndex = candidates[index].after;
    const length = firstTailNotBelow(tails, afterIndex);
    previous.push(length === 0 ? -1 : tailCandidate[length - 1]);
    if (length === tails.length) {
      tails.push(afterIndex);
      tailCandidate.push(index);
    } else {
      tails[length] = afterIndex;
      tailCandidate[length] = index;
    }
  }

  const chain: Anchor[] = [];
  let index = tailCandidate[tails.length - 1] ?? -1;
  while (index >= 0) {
    chain.push(candidates[index]);
    index = previous[index];
  }
  return chain.reverse();
}

/** Whether the two ranges have at least one line in common. */
function sharesAnyLine(
  before: string[],
  beforeRange: Range,
  after: string[],
  afterRange: Range,
): boolean {
  const afterLines = new Set<string>();
  for (let index = afterRange.start; index < afterRange.end; index += 1) {
    afterLines.add(after[index]);
  }
  for (let index = beforeRange.start; index < beforeRange.end; index += 1) {
    if (afterLines.has(before[index])) return true;
  }
  return false;
}

/** Index of each line that occurs once in the range; repeated lines are absent. */
function uniqueIndexes(lines: string[], range: Range): Map<string, number> {
  const seen = new Map<string, number>();
  for (let index = range.start; index < range.end; index += 1) {
    const line = lines[index];
    const existing = seen.get(line);
    // -1 marks a line that repeats, which cannot serve as an anchor.
    seen.set(line, existing === undefined ? index : -1);
  }
  for (const [line, index] of seen) {
    if (index < 0) seen.delete(line);
  }
  return seen;
}

/** The first index in `tails` whose value is at least `value`. */
function firstTailNotBelow(tails: number[], value: number): number {
  let low = 0;
  let high = tails.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if (tails[middle] < value) low = middle + 1;
    else high = middle;
  }
  return low;
}

/**
 * Aligns one region with a longest-common-subsequence table. Callers keep the
 * region within [`MAX_TABLE_CELLS`], so the table stays small.
 */
function emitAlignedRegion(
  before: string[],
  beforeRange: Range,
  after: string[],
  afterRange: Range,
  emit: (line: DiffLine) => void,
): void {
  const rows = beforeRange.end - beforeRange.start;
  const columns = afterRange.end - afterRange.start;

  // table[i][j] = LCS length of the first i before lines and j after lines.
  const table = Array.from({ length: rows + 1 }, () => new Uint32Array(columns + 1));
  for (let i = 1; i <= rows; i += 1) {
    const line = before[beforeRange.start + i - 1];
    const previous = table[i - 1];
    const current = table[i];
    for (let j = 1; j <= columns; j += 1) {
      current[j] =
        line === after[afterRange.start + j - 1]
          ? previous[j - 1] + 1
          : Math.max(previous[j], current[j - 1]);
    }
  }

  // Walk back from the end, then emit the reversed result in document order.
  const lines: DiffLine[] = [];
  let i = rows;
  let j = columns;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && before[beforeRange.start + i - 1] === after[afterRange.start + j - 1]) {
      lines.push(unchangedLine(before[beforeRange.start + i - 1]));
      i -= 1;
      j -= 1;
    } else if (j > 0 && (i === 0 || table[i][j - 1] >= table[i - 1][j])) {
      lines.push({ type: "add", content: after[afterRange.start + j - 1] });
      j -= 1;
    } else {
      lines.push({ type: "remove", content: before[beforeRange.start + i - 1] });
      i -= 1;
    }
  }
  for (let index = lines.length - 1; index >= 0; index -= 1) emit(lines[index]);
}
