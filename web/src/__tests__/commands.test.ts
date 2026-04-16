import { describe, expect, it } from "vitest";
import { SLASH_COMMANDS, filterCommands } from "../commands";

describe("filterCommands", () => {
  it("空クエリで全9コマンドが返る", () => {
    // Arrange & Act
    const result = filterCommands("");

    // Assert
    expect(result).toHaveLength(9);
  });

  it('"st" で status のみフィルタされる', () => {
    // Arrange & Act
    const result = filterCommands("st");

    // Assert
    expect(result).toHaveLength(1);
    expect(result[0].name).toBe("status");
  });

  it('"new" で new のみマッチ', () => {
    // Arrange & Act
    const result = filterCommands("new");

    // Assert
    expect(result).toHaveLength(1);
    expect(result[0].name).toBe("new");
  });

  it('"xyz" で空配列', () => {
    // Arrange & Act
    const result = filterCommands("xyz");

    // Assert
    expect(result).toHaveLength(0);
  });
});

describe("SLASH_COMMANDS", () => {
  it("全コマンドの usage が / で始まる", () => {
    // Arrange & Act & Assert
    for (const cmd of SLASH_COMMANDS) {
      expect(cmd.usage.startsWith("/")).toBe(true);
      expect(cmd.name.length).toBeGreaterThan(0);
      expect(cmd.description.length).toBeGreaterThan(0);
    }
  });

  it("name が重複しない", () => {
    // Arrange & Act
    const names = SLASH_COMMANDS.map((c) => c.name);
    const unique = new Set(names);

    // Assert
    expect(names.length).toBe(unique.size);
  });
});
