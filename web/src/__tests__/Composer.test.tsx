import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { Composer } from "../components/Composer";

describe("Composer", () => {
  afterEach(cleanup);

  const defaultProps = {
    draft: "",
    setDraft: vi.fn(),
    onSubmit: vi.fn(),
  };

  function renderComposer(overrides: Partial<typeof defaultProps> = {}) {
    return render(
      <Composer {...defaultProps} {...overrides} />,
    );
  }

  it("/ 入力でサジェストが表示される", () => {
    // Arrange & Act
    renderComposer({ draft: "/" });

    // Assert: command-suggest が DOM に存在する
    const suggest = document.querySelector(".command-suggest");
    expect(suggest).not.toBeNull();
  });

  it("/st 入力で status のみフィルタされる", () => {
    // Arrange & Act
    renderComposer({ draft: "/st" });

    // Assert
    const items = document.querySelectorAll(".command-suggest-item");
    expect(items).toHaveLength(1);
    expect(items[0].textContent).toContain("status");
  });

  it("通常テキストでサジェスト非表示", () => {
    // Arrange & Act
    renderComposer({ draft: "hello" });

    // Assert
    const suggest = document.querySelector(".command-suggest");
    expect(suggest).toBeNull();
  });

  it("Escape キーでサジェスト非表示", () => {
    // Arrange
    const { container } = renderComposer({ draft: "/" });

    // Act
    const textarea = container.querySelector("textarea")!;
    fireEvent.keyDown(textarea, { key: "Escape" });

    // Assert
    const suggest = document.querySelector(".command-suggest");
    expect(suggest).toBeNull();
  });

  it("Tab キーで選択中コマンドが挿入される", () => {
    // Arrange
    const setDraft = vi.fn();
    const { container } = renderComposer({ draft: "/", setDraft });

    // Act
    const textarea = container.querySelector("textarea")!;
    fireEvent.keyDown(textarea, { key: "Tab" });

    // Assert: setDraft がコマンド usage で呼ばれる
    expect(setDraft).toHaveBeenCalledWith("/new ");
  });

  it("Enter キーで選択中コマンドが挿入される", () => {
    // Arrange
    const setDraft = vi.fn();
    const { container } = renderComposer({ draft: "/", setDraft });

    // Act
    const textarea = container.querySelector("textarea")!;
    fireEvent.keyDown(textarea, { key: "Enter" });

    // Assert
    expect(setDraft).toHaveBeenCalledWith("/new ");
  });

  it("ArrowDown/Up でインデックスが移動する", () => {
    // Arrange
    const { container } = renderComposer({ draft: "/" });

    const textarea = container.querySelector("textarea")!;

    // 初期状態: index=0 が active
    let items = container.querySelectorAll(".command-suggest-item");
    expect(items[0].classList.contains("active")).toBe(true);

    // Act: ArrowDown → index=1 が active
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    items = container.querySelectorAll(".command-suggest-item");
    expect(items[1].classList.contains("active")).toBe(true);

    // Act: ArrowUp → index=0 に戻る
    fireEvent.keyDown(textarea, { key: "ArrowUp" });
    items = container.querySelectorAll(".command-suggest-item");
    expect(items[0].classList.contains("active")).toBe(true);
  });
});
