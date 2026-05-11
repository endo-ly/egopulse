import { describe, it, expect } from "vitest";
import { computeLineDiff } from "../diff";

describe("computeLineDiff", () => {
  it("diff_lines_identical_content", () => {
    // Arrange
    const content = "line1\nline2\nline3";

    // Act
    const result = computeLineDiff(content, content);

    // Assert
    expect(result).toEqual([
      { type: "unchanged", before: "line1", after: "line1" },
      { type: "unchanged", before: "line2", after: "line2" },
      { type: "unchanged", before: "line3", after: "line3" },
    ]);
  });

  it("diff_lines_added_line", () => {
    // Arrange
    const before = "A";
    const after = "A\nB";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([
      { type: "unchanged", before: "A", after: "A" },
      { type: "add", content: "B" },
    ]);
  });

  it("diff_lines_removed_line", () => {
    // Arrange
    const before = "A\nB";
    const after = "A";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([
      { type: "unchanged", before: "A", after: "A" },
      { type: "remove", content: "B" },
    ]);
  });

  it("diff_lines_changed_line", () => {
    // Arrange
    const before = "A";
    const after = "A'";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([
      { type: "remove", content: "A" },
      { type: "add", content: "A'" },
    ]);
  });

  it("diff_lines_empty_before", () => {
    // Arrange
    const before = "";
    const after = "A\nB";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([
      { type: "add", content: "A" },
      { type: "add", content: "B" },
    ]);
  });

  it("diff_lines_empty_after", () => {
    // Arrange
    const before = "A\nB";
    const after = "";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([
      { type: "remove", content: "A" },
      { type: "remove", content: "B" },
    ]);
  });

  it("diff_lines_both_empty", () => {
    // Arrange
    const before = "";
    const after = "";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([]);
  });

  it("diff_lines_multiline_mixed", () => {
    // Arrange
    const before = "A\nB\nC";
    const after = "A\nC\nD";

    // Act
    const result = computeLineDiff(before, after);

    // Assert
    expect(result).toEqual([
      { type: "unchanged", before: "A", after: "A" },
      { type: "remove", content: "B" },
      { type: "unchanged", before: "C", after: "C" },
      { type: "add", content: "D" },
    ]);
  });
});
