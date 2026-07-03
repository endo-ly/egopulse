import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { MarkdownRenderer } from "../MarkdownRenderer";

describe("MarkdownRenderer code block fold", () => {
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
