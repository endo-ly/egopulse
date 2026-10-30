import { describe, it, expect } from "vitest";
import { countLineChanges } from "../diff";

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
});
