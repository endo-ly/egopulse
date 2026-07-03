import { describe, it, expect } from "vitest";
import { render, fireEvent, act } from "@testing-library/react";
import { Timeline } from "../Timeline";

function setScrollProps(el: HTMLElement, h: number, c: number, t: number) {
  Object.defineProperty(el, "scrollHeight", { value: h, configurable: true });
  Object.defineProperty(el, "clientHeight", { value: c, configurable: true });
  Object.defineProperty(el, "scrollTop", { value: t, configurable: true, writable: true });
}

describe("Timeline", () => {
  it("timeline_shows_jump_to_latest_when_scrolled_up", () => {
    const { container } = render(
      <Timeline>
        <div style={{ height: "2000px" }}>tall content</div>
      </Timeline>,
    );

    const tl = container.querySelector(".timeline") as HTMLElement;
    setScrollProps(tl, 2000, 200, 0);

    fireEvent.scroll(tl);

    const btn = container.querySelector(".jump-to-latest");
    expect(btn).toBeTruthy();
  });

  it("timeline_jump_button_click_scrolls_to_bottom", () => {
    const { container } = render(
      <Timeline>
        <div style={{ height: "2000px" }}>tall content</div>
      </Timeline>,
    );

    const tl = container.querySelector(".timeline") as HTMLElement;
    setScrollProps(tl, 2000, 200, 0);

    fireEvent.scroll(tl);

    const btn = container.querySelector(".jump-to-latest") as HTMLButtonElement;
    expect(btn).toBeTruthy();

    btn.click();

    expect(tl.scrollTop).toBe(2000);
  });

  it("timeline_search_highlights_and_navigates_matches", () => {
    const { container } = render(
      <Timeline searchTarget="hello world goodbye world peace">
        <div>messages</div>
      </Timeline>,
    );

    act(() => {
      globalThis.dispatchEvent(new KeyboardEvent("keydown", { key: "f", metaKey: true }));
    });

    const searchInput = container.querySelector(".timeline-search-input") as HTMLInputElement;
    expect(searchInput).toBeTruthy();

    fireEvent.change(searchInput, { target: { value: "world" } });

    const count = container.querySelector(".timeline-search-count");
    expect(count?.textContent).toBe("1 / 2");
  });

  it("timeline_search_escape_closes", () => {
    const { container } = render(
      <Timeline searchTarget="hello world">
        <div>messages</div>
      </Timeline>,
    );

    act(() => {
      globalThis.dispatchEvent(new KeyboardEvent("keydown", { key: "f", metaKey: true }));
    });

    const searchInput = container.querySelector(".timeline-search-input") as HTMLInputElement;
    fireEvent.change(searchInput, { target: { value: "hello" } });

    fireEvent.keyDown(searchInput, { key: "Escape" });

    const searchBar = container.querySelector(".timeline-search-bar");
    expect(searchBar).toBeFalsy();
  });
});
