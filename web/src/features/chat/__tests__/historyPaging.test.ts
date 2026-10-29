import { describe, it, expect } from "vitest";
import type { ChatMessage } from "../../../shared/api/types";
import {
  gapBounds,
  mergeHistoryMessages,
  prependHistoryMessages,
} from "../historyPaging";

function msg(id: string, content = id): ChatMessage {
  return {
    id,
    sender_id: "lyre",
    sender_kind: "assistant",
    content,
    timestamp: "2026-01-01T00:00:00Z",
    message_kind: "message",
  };
}

describe("mergeHistoryMessages", () => {
  it("updates entries in place and appends unknown ids", () => {
    // Arrange
    const loaded = [msg("a", "old a"), msg("b")];
    const incoming = [msg("b", "new b"), msg("c")];

    // Act
    const merged = mergeHistoryMessages(loaded, incoming);

    // Assert
    expect(merged.map((m) => m.id)).toEqual(["a", "b", "c"]);
    expect(merged[0].content).toBe("old a");
    expect(merged[1].content).toBe("new b");
  });

  it("updates an entry when the incoming page repeats its id", () => {
    // Arrange
    const loaded = [msg("a")];

    // Act
    const merged = mergeHistoryMessages(loaded, [msg("a", "updated")]);

    // Assert
    expect(merged.map((m) => m.id)).toEqual(["a"]);
    expect(merged[0].content).toBe("updated");
  });

  it("returns the loaded array unchanged for an empty page", () => {
    const loaded = [msg("a")];
    expect(mergeHistoryMessages(loaded, [])).toBe(loaded);
  });
});

describe("prependHistoryMessages", () => {
  it("prepends older entries without duplicates", () => {
    // Arrange
    const loaded = [msg("c"), msg("d")];

    // Act
    const merged = prependHistoryMessages(loaded, [msg("a"), msg("b"), msg("c")]);

    // Assert
    expect(merged.map((m) => m.id)).toEqual(["a", "b", "c", "d"]);
  });

  it("returns the loaded array unchanged when the page is already covered", () => {
    const loaded = [msg("a")];
    expect(prependHistoryMessages(loaded, [msg("a")])).toBe(loaded);
  });
});

describe("gapBounds", () => {
  it("returns the skipped range when the newest page jumps forward", () => {
    expect(gapBounds(6, 9)).toEqual({ from: 9, downTo: 6 });
  });

  it("accepts overlapping and adjacent windows", () => {
    expect(gapBounds(6, 6)).toBeNull();
    expect(gapBounds(6, 7)).toBeNull();
    expect(gapBounds(6, 8)).toEqual({ from: 8, downTo: 6 });
  });

  it("skips detection without a known top or older cursor", () => {
    expect(gapBounds(null, 9)).toBeNull();
    expect(gapBounds(6, null)).toBeNull();
  });
});
