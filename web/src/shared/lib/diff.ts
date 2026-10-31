/** A single entry in a line-level diff */
export type DiffLine =
  | { type: "add"; content: string }
  | { type: "remove"; content: string }
  | { type: "unchanged"; before: string; after: string };

/**
 * Computes a line-level diff between two strings using LCS
 * (Longest Common Subsequence) dynamic programming.
 *
 * Returns an ordered array of `DiffLine` entries representing
 * the minimal set of additions and removals to transform
 * `before` into `after`.  Unchanged lines carry both the
 * before and after content for convenient rendering.
 *
 * The table is built only for the lines between the common leading and
 * trailing runs, so an edit inside a long document costs a table proportional
 * to the edited region instead of the whole document.
 *
 * # Edge cases
 * - An empty string is treated as having zero lines.
 * - When a line is replaced (different content at the same
 *   position), the output pairs a `remove` for the old line
 *   followed by an `add` for the new line.
 */
export function computeLineDiff(before: string, after: string): DiffLine[] {
  const parts = splitAroundCommonEdges(before, after);
  const lines = parts.head.map(unchangedLine);
  for (const line of diffMiddle(parts.beforeMiddle, parts.afterMiddle)) lines.push(line);
  for (const line of parts.tail) lines.push(unchangedLine(line));
  return lines;
}

/**
 * Counts added and removed lines between two texts.
 *
 * Only the counts are needed here, so this keeps two DP rows instead of the
 * whole table and never materializes the diff itself.
 */
export function countLineChanges(before: string, after: string): { add: number; remove: number } {
  const parts = splitAroundCommonEdges(before, after);
  // Lines outside the middle are common to both sides by construction; inside
  // it the LCS length is how many lines survive the diff.
  const common =
    parts.head.length + parts.tail.length + lcsLength(parts.beforeMiddle, parts.afterMiddle);
  return {
    add: parts.afterLines.length - common,
    remove: parts.beforeLines.length - common,
  };
}

interface SplitParts {
  beforeLines: string[];
  afterLines: string[];
  /** Lines common to both texts before the first difference. */
  head: string[];
  /** The differing region, which is all the DP has to look at. */
  beforeMiddle: string[];
  afterMiddle: string[];
  /** Lines common to both texts after the last difference. */
  tail: string[];
}

/**
 * Splits both texts into lines and peels off the common leading and trailing
 * runs. A common prefix or suffix is always part of some longest common
 * subsequence, so removing it keeps both the LCS length and the diff intact.
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

/** The length of the longest common subsequence of two line arrays. */
function lcsLength(a: string[], b: string[]): number {
  if (a.length === 0 || b.length === 0) return 0;

  // Iterate over the longer side and keep the row on the shorter one, so the
  // two DP rows stay as small as possible.
  const rows = a.length >= b.length ? a : b;
  const columns = a.length >= b.length ? b : a;

  let previous = new Uint32Array(columns.length + 1);
  let current = new Uint32Array(columns.length + 1);
  for (const line of rows) {
    for (let j = 1; j <= columns.length; j++) {
      current[j] =
        line === columns[j - 1] ? previous[j - 1] + 1 : Math.max(previous[j], current[j - 1]);
    }
    const swap = previous;
    previous = current;
    current = swap;
  }
  return previous[columns.length];
}

/** Diffs two line arrays that share no common prefix or suffix. */
function diffMiddle(beforeLines: string[], afterLines: string[]): DiffLine[] {
  const m = beforeLines.length;
  const n = afterLines.length;
  if (m === 0 && n === 0) return [];

  // ------------------------------------------------------------------
  // LCS table: dp[i][j] = LCS length of beforeLines[..i] & afterLines[..j]
  // ------------------------------------------------------------------
  const dp = Array.from({ length: m + 1 }, () => new Uint32Array(n + 1));

  for (let i = 1; i <= m; i++) {
    const bl = beforeLines[i - 1];
    const row = dp[i - 1];
    const cur = dp[i];
    for (let j = 1; j <= n; j++) {
      if (bl === afterLines[j - 1]) {
        cur[j] = row[j - 1] + 1;
      } else {
        // Explicit branch for clarity — equivalent to Math.max
        cur[j] = row[j] >= cur[j - 1] ? row[j] : cur[j - 1];
      }
    }
  }

  // ------------------------------------------------------------------
  // Backtrack through the DP table to reconstruct the diff.
  // The traversal builds the result in reverse order.
  // ------------------------------------------------------------------
  const result: DiffLine[] = [];
  let i = m;
  let j = n;

  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && beforeLines[i - 1] === afterLines[j - 1]) {
      // Common line — unchanged
      result.push({
        type: "unchanged",
        before: beforeLines[i - 1],
        after: afterLines[j - 1],
      });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
      // Line exists only in `after` → addition
      result.push({ type: "add", content: afterLines[j - 1] });
      j--;
    } else {
      // Line exists only in `before` → removal
      result.push({ type: "remove", content: beforeLines[i - 1] });
      i--;
    }
  }

  result.reverse();
  return result;
}
