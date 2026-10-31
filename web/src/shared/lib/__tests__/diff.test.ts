import { describe, it, expect } from "vitest";
import { computeLineDiff, countLineChanges } from "../diff";

describe("countLineChanges", () => {
  it("counts_added_and_removed_lines", () => {
    // Arrange / Act
    const counts = countLineChanges("a\nb\nc", "a\nB\nc\nd");

    // Assert
    expect(counts).toEqual({ add: 2, remove: 1 });
  });

  it("reports_zero_for_identical_texts", () => {
    // Arrange / Act / Assert
    expect(countLineChanges("same", "same")).toEqual({ add: 0, remove: 0 });
  });

  it("counts_a_change_surrounded_by_long_common_runs", () => {
    // Arrange: only one line differs, far from the edges
    const before = text(2000, (index) => `line ${index}`);
    const after = before.replace("line 1000", "line 1000 changed");

    // Act
    const counts = countLineChanges(before, after);

    // Assert
    expect(counts).toEqual({ add: 1, remove: 1 });
  });

  it("aligns_a_large_region_on_the_lines_unique_to_both", () => {
    // Arrange: 2,000 lines with ten edits, far more than one table can hold
    const before = text(2000, (index) => `line ${index}`);
    const after = text(2000, (index) => (index % 200 === 0 ? `changed ${index}` : `line ${index}`));

    // Act
    const counts = countLineChanges(before, after);

    // Assert: the unchanged lines carry the alignment, so only the edits count
    expect(counts).toEqual({ add: 10, remove: 10 });
  });

  it("aligns_a_swapped_block_of_repeated_lines", () => {
    // Arrange: two blocks swapped, with no line unique to either side
    const before = `${text(600, () => "A")}\n${text(600, () => "B")}`;
    const after = `${text(600, () => "B")}\n${text(600, () => "A")}`;

    // Act
    const counts = countLineChanges(before, after);

    // Assert: one block moved, so it counts once as removed and once as added
    expect(counts).toEqual({ add: 600, remove: 600 });
  });

  it("aligns_a_shifted_run_of_repeated_lines", () => {
    // Arrange: two lines alternating in opposite orders
    const before = text(2000, (index) => (index % 2 === 0 ? "x" : "y"));
    const after = text(2000, (index) => (index % 2 === 0 ? "y" : "x"));

    // Act
    const counts = countLineChanges(before, after);

    // Assert: the run is shifted by one line, not rewritten
    expect(counts).toEqual({ add: 1, remove: 1 });
  });

  it("adds_a_large_number_of_lines_without_exceeding_the_argument_limit", () => {
    // Arrange: an insertion big enough that passing it as arguments would throw
    const after = text(130_000, (index) => `added ${index}`);

    // Act
    const diff = computeLineDiff("", after);

    // Assert
    expect(diff).toHaveLength(130_000);
    expect(diff[0]).toEqual({ type: "add", content: "added 0" });
    expect(diff[diff.length - 1]).toEqual({ type: "add", content: "added 129999" });
  });

  it("counts_duplicate_heavy_texts_like_a_naive_lcs", () => {
    // Arrange: repeated lines, where trimming must not change the result
    const cases: Array<[string, string]> = [
      ["a\na\nb\na", "a\nb\na\na"],
      ["x\ny\nx\ny", "y\nx\ny\nx"],
      ["a\nb\nc\na\nb", "a\nc\nb\na\nb"],
      ["same\nsame\nsame", "same\nother\nsame\nsame"],
      ["", "a"],
      ["a", ""],
      ["a\nb", "b\na"],
    ];

    for (const [before, after] of cases) {
      // Act
      const counts = countLineChanges(before, after);

      // Assert: the counts describe a diff of the same length as a plain LCS
      const beforeLines = lines(before);
      const afterLines = lines(after);
      const common = naiveLcsLength(beforeLines, afterLines);
      expect(counts, `${JSON.stringify({ before, after })}`).toEqual({
        add: afterLines.length - common,
        remove: beforeLines.length - common,
      });
    }
  });
});

describe("computeLineDiff", () => {
  it("keeps_the_common_lines_around_a_change", () => {
    // Arrange
    const before = "a\nb\nc\nd\ne";
    const after = "a\nb\nC\nd\ne";

    // Act
    const diff = computeLineDiff(before, after);

    // Assert
    expect(diff).toEqual([
      { type: "unchanged", before: "a", after: "a" },
      { type: "unchanged", before: "b", after: "b" },
      { type: "remove", content: "c" },
      { type: "add", content: "C" },
      { type: "unchanged", before: "d", after: "d" },
      { type: "unchanged", before: "e", after: "e" },
    ]);
  });

  it("treats_empty_texts_as_having_no_lines", () => {
    // Act / Assert
    expect(computeLineDiff("", "")).toEqual([]);
    expect(computeLineDiff("", "a")).toEqual([{ type: "add", content: "a" }]);
    expect(computeLineDiff("a", "")).toEqual([{ type: "remove", content: "a" }]);
  });

  it("rebuilds_both_sides_from_the_diff", () => {
    // Arrange
    const before = "keep\nold\nkeep2\nold2\nkeep3";
    const after = "keep\nnew\nkeep2\nkeep3\nnew3";

    // Act
    const diff = computeLineDiff(before, after);

    // Assert: unchanged lines carry both sides, so each side rebuilds exactly
    const rebuiltBefore = diff.flatMap((line) => (line.type === "add" ? [] : [lineContent(line)]));
    const rebuiltAfter = diff.flatMap((line) =>
      line.type === "remove" ? [] : [lineContent(line)],
    );
    expect(rebuiltBefore.join("\n")).toBe(before);
    expect(rebuiltAfter.join("\n")).toBe(after);
  });

  it("rebuilds_both_sides_of_a_region_of_repeated_lines", () => {
    // Arrange: a moved block of repeated lines, so no line is unique
    const before = `${text(600, () => "A")}\n${text(600, () => "B")}`;
    const after = `${text(600, () => "B")}\n${text(600, () => "A")}`;

    // Act
    const diff = computeLineDiff(before, after);

    // Assert
    const rebuiltBefore = diff.flatMap((line) => (line.type === "add" ? [] : [lineContent(line)]));
    const rebuiltAfter = diff.flatMap((line) =>
      line.type === "remove" ? [] : [lineContent(line)],
    );
    expect(rebuiltBefore.join("\n")).toBe(before);
    expect(rebuiltAfter.join("\n")).toBe(after);
  });

  it("rebuilds_both_sides_of_a_large_region", () => {
    // Arrange: a region too large for one table, with edits, deletions and
    // insertions spread through it
    const before = text(2000, (index) => `line ${index}`);
    const after = text(1900, (index) => (index % 150 === 0 ? `changed ${index}` : `line ${index}`));

    // Act
    const diff = computeLineDiff(before, after);

    // Assert
    const rebuiltBefore = diff.flatMap((line) => (line.type === "add" ? [] : [lineContent(line)]));
    const rebuiltAfter = diff.flatMap((line) =>
      line.type === "remove" ? [] : [lineContent(line)],
    );
    expect(rebuiltBefore.join("\n")).toBe(before);
    expect(rebuiltAfter.join("\n")).toBe(after);
  });
});

function text(count: number, mutate: (index: number) => string): string {
  return Array.from({ length: count }, (_, index) => mutate(index)).join("\n");
}

function lineContent(line: ReturnType<typeof computeLineDiff>[number]): string {
  return line.type === "unchanged" ? line.before : line.content;
}

function lines(text: string): string[] {
  return text === "" ? [] : text.split("\n");
}

/** Textbook O(m·n) LCS, used only to check the optimized counting path. */
function naiveLcsLength(a: string[], b: string[]): number {
  const dp = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = 1; i <= a.length; i++) {
    for (let j = 1; j <= b.length; j++) {
      dp[i][j] =
        a[i - 1] === b[j - 1] ? dp[i - 1][j - 1] + 1 : Math.max(dp[i - 1][j], dp[i][j - 1]);
    }
  }
  return dp[a.length][b.length];
}
