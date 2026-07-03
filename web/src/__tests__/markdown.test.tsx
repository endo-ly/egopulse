import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
import { MarkdownRenderer } from "../features/chat/MarkdownRenderer";

describe("MarkdownRenderer", () => {
  it("markdown_renders_elements_and_code_block_has_copy", () => {
    const md = "# Title\n\n- item1\n- item2\n\n```js\nconsole.log('hi');\n```";
    const { container } = render(<MarkdownRenderer content={md} />);

    const h1 = container.querySelector("h1");
    expect(h1?.textContent).toBe("Title");

    const ul = container.querySelector("ul");
    expect(ul).toBeTruthy();

    const items = container.querySelectorAll("li");
    expect(items.length).toBe(2);

    const pre = container.querySelector("pre");
    expect(pre).toBeTruthy();

    const copyBtn = pre?.querySelector(".code-block-copy");
    expect(copyBtn).toBeTruthy();
  });

  it("code_block_copy_calls_clipboard", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, {
      clipboard: { writeText },
    });

    const md = "```\nhello world\n```";
    const { container } = render(<MarkdownRenderer content={md} />);

    const copyBtn = container.querySelector(".code-block-copy") as HTMLButtonElement;
    copyBtn.click();

    await new Promise((r) => setTimeout(r, 0));

    expect(writeText).toHaveBeenCalled();
    expect(writeText.mock.calls[0][0]).toContain("hello world");
  });
});
