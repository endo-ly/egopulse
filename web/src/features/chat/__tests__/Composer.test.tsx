import { describe, it, expect, vi } from "vitest";
import { render, fireEvent, waitFor, act } from "@testing-library/react";
import { Composer } from "../Composer";

describe("Composer", () => {
  it("composer_enter_submits_and_clears_on_accept", async () => {
    const onSubmit = vi.fn().mockResolvedValue(true);
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "hello" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });

    expect(onSubmit).toHaveBeenCalledWith("hello");
    await waitFor(() => expect(ta.value).toBe(""));
  });

  it("composer_enter_keeps_text_on_reject", async () => {
    const onSubmit = vi.fn().mockResolvedValue(false);
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "hello" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });

    expect(onSubmit).toHaveBeenCalledWith("hello");
    await act(async () => {});
    expect(ta.value).toBe("hello");
  });

  it("composer_disables_input_until_acceptance", async () => {
    let resolveSubmit!: (ok: boolean) => void;
    const onSubmit = vi.fn(
      () => new Promise<boolean>((resolve) => (resolveSubmit = resolve)),
    );
    const { container } = render(<Composer onSubmit={onSubmit} />);

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    const button = container.querySelector(".composer-send") as HTMLButtonElement;
    fireEvent.change(ta, { target: { value: "hello" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });

    expect(ta.disabled).toBe(true);
    expect(button.disabled).toBe(true);

    await act(async () => {
      resolveSubmit(true);
    });
    expect(ta.disabled).toBe(false);
    expect(ta.value).toBe("");
  });

  it("composer_submit_after_session_switch_keeps_new_draft", async () => {
    let resolveSubmit!: (ok: boolean) => void;
    const onSubmit = vi.fn(
      () => new Promise<boolean>((resolve) => (resolveSubmit = resolve)),
    );
    globalThis.localStorage?.setItem("egopulse.draft.s2", "keep me");
    const { container, rerender } = render(
      <Composer onSubmit={onSubmit} storageKey="s1" />,
    );

    const ta = container.querySelector(".composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "hello" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: false });
    expect(onSubmit).toHaveBeenCalledWith("hello");

    // Arrange: switch sessions while the ack is in flight.
    rerender(<Composer onSubmit={onSubmit} storageKey="s2" />);
    expect(ta.value).toBe("keep me");

    // A return to the original session must also not revive the old clear.
    globalThis.localStorage?.setItem("egopulse.draft.s1", "new draft");
    rerender(<Composer onSubmit={onSubmit} storageKey="s1" />);
    expect(ta.value).toBe("new draft");

    // Act
    await act(async () => {
      resolveSubmit(true);
    });

    // Assert: the newer draft survives.
    expect(ta.value).toBe("new draft");
    globalThis.localStorage?.clear();
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
