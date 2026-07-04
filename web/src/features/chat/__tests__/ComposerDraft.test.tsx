import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { Composer } from "../Composer";

describe("Composer draft persistence", () => {
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
