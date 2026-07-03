import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { Sidebar } from "../app/shell/Sidebar";
import { MarkdownRenderer } from "../features/chat/MarkdownRenderer";
import { Composer } from "../features/chat/Composer";

describe("Sidebar collapse (Step 30)", () => {
  it("sidebar_collapses_to_icon_only_bar", () => {
    const onToggle = vi.fn();
    const { container, rerender } = render(
      <Sidebar onNewSession={vi.fn()} onToggleCollapse={onToggle} collapsed={false} />,
    );

    const collapseBtn = container.querySelector(".sidebar-collapse-btn");
    expect(collapseBtn).toBeTruthy();

    const brandName = container.querySelector(".sidebar-brand-name");
    expect(brandName).toBeTruthy();

    fireEvent.click(collapseBtn!);
    expect(onToggle).toHaveBeenCalled();

    rerender(
      <Sidebar onNewSession={vi.fn()} onToggleCollapse={onToggle} collapsed={true} />,
    );

    const nav = container.querySelector(".sidebar-nav") as HTMLElement;
    expect(nav.classList.contains("collapsed")).toBe(true);

    const hiddenBrand = container.querySelector(".sidebar-brand-name");
    expect(hiddenBrand).toBeFalsy();

    const sessionsSection = container.querySelectorAll(".sidebar-section");
    expect(sessionsSection.length).toBe(1);
  });
});

describe("Code block fold (Step 32)", () => {
  it("code_block_folds_when_longer_than_threshold", () => {
    const longCode = "```\n" + Array.from({ length: 25 }, (_, i) => `line ${i}`).join("\n") + "\n```";
    const { container } = render(<MarkdownRenderer content={longCode} />);

    const foldBtn = container.querySelector(".code-block-fold");
    expect(foldBtn).toBeTruthy();
    expect(foldBtn?.textContent).toContain("Show all (25 lines)");

    fireEvent.click(foldBtn!);

    const updatedBtn = container.querySelector(".code-block-fold") as HTMLElement;
    expect(updatedBtn.textContent).toBe("Collapse");
  });

  it("code_block_short_code_does_not_fold", () => {
    const shortCode = "```\nline1\nline2\n```";
    const { container } = render(<MarkdownRenderer content={shortCode} />);

    const foldBtn = container.querySelector(".code-block-fold");
    expect(foldBtn).toBeFalsy();
  });
});

describe("Composer draft persistence (Step 33)", () => {
  it("composer_draft_persists_to_localstorage", () => {
    localStorage.clear();

    const { unmount } = render(
      <Composer onSubmit={vi.fn()} storageKey="main" />,
    );

    const textarea = document.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "draft text" } });

    unmount();

    expect(localStorage.getItem("egopulse.draft.main")).toBe("draft text");
  });

  it("composer_draft_restores_from_localstorage", () => {
    localStorage.setItem("egopulse.draft.main", "saved draft");

    const { container } = render(
      <Composer onSubmit={vi.fn()} storageKey="main" />,
    );

    const textarea = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    expect(textarea.value).toBe("saved draft");
  });
});
