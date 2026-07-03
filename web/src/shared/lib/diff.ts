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
 * # Edge cases
 * - An empty string is treated as having zero lines.
 * - When a line is replaced (different content at the same
 *   position), the output pairs a `remove` for the old line
 *   followed by an `add` for the new line.
 */
export function computeLineDiff(before: string, after: string): DiffLine[] {
  const beforeLines = before === "" ? [] : before.split("\n");
  const afterLines = after === "" ? [] : after.split("\n");

  const m = beforeLines.length;
  const n = afterLines.length;

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
