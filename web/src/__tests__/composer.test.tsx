import { describe, it, expect, vi } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { Composer } from "../components/Composer";

describe("Composer", () => {
  it("composer_enter_submits_and_clears", () => {
    const onSubmit = vi.fn();
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "hello" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });

    expect(onSubmit).toHaveBeenCalledWith("hello");
    expect(ta.value).toBe("");
  });

  it("composer_shift_enter_inserts_newline", () => {
    const onSubmit = vi.fn();
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "hello" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: true });

    expect(onSubmit).not.toHaveBeenCalled();
    expect(ta.value).toBe("hello");
  });

  it("composer_empty_enter_does_not_submit", () => {
    const onSubmit = vi.fn();
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "   " } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });

    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("composer_slash_shows_suggest", () => {
    const onSubmit = vi.fn();
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "/" } });

    const suggest = container.querySelector(".command-suggest");
    expect(suggest).toBeTruthy();
    const items = suggest?.querySelectorAll(".suggest-item");
    expect((items ?? []).length).toBeGreaterThan(0);
  });

  it("composer_escape_clears_suggest", () => {
    const onSubmit = vi.fn();
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "/re" } });
    fireEvent.keyDown(ta, { key: "Escape" });

    const suggest = container.querySelector(".command-suggest");
    expect(suggest).toBeFalsy();
  });
});
