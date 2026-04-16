import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { CommandSuggest } from "../components/CommandSuggest";
import { SLASH_COMMANDS } from "../commands";

describe("CommandSuggest", () => {
  it("コマンドリストが表示される", () => {
    // Arrange
    const commands = SLASH_COMMANDS.slice(0, 3);

    // Act
    render(
      <CommandSuggest
        commands={commands}
        activeIndex={0}
        onSelect={() => {}}
      />,
    );

    // Assert
    expect(screen.getByRole("listbox")).toBeDefined();
    expect(screen.getAllByRole("option")).toHaveLength(3);
  });

  it("選択中インデックスのアイテムがアクティブ表示", () => {
    // Arrange
    const commands = SLASH_COMMANDS.slice(0, 3);

    // Act
    const { container } = render(
      <CommandSuggest
        commands={commands}
        activeIndex={1}
        onSelect={() => {}}
      />,
    );

    // Assert: DOM クエリで active クラスを確認
    const items = container.querySelectorAll(".command-suggest-item");
    expect(items).toHaveLength(3);
    expect(items[1].classList.contains("active")).toBe(true);
    expect(items[0].classList.contains("active")).toBe(false);
  });

  it("マッチなしで何もレンダリングしない", () => {
    // Arrange & Act
    const { container } = render(
      <CommandSuggest commands={[]} activeIndex={0} onSelect={() => {}} />,
    );

    // Assert
    expect(container.innerHTML).toBe("");
  });

  it("click でコマンドが選択される", () => {
    // Arrange
    const onSelect = vi.fn();
    const commands = SLASH_COMMANDS.slice(0, 2);

    // Act
    const { container } = render(
      <CommandSuggest
        commands={commands}
        activeIndex={0}
        onSelect={onSelect}
      />,
    );

    const items = container.querySelectorAll(".command-suggest-item");
    fireEvent.click(items[1]);

    // Assert
    expect(onSelect).toHaveBeenCalledWith(commands[1]);
  });
});
