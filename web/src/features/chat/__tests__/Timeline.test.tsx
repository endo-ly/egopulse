import { describe, it, expect, vi } from "vitest";
import { act, fireEvent, render } from "@testing-library/react";
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

  it("timeline_highlights_requested_jump_target", () => {
    const { container } = render(
      <Timeline jumpRequest={{ index: 1, seq: 1 }}>
        <div>first message</div>
        <div>second message</div>
      </Timeline>,
    );

    const messages = container.querySelector(".timeline-messages") as HTMLElement;
    expect(messages.children[1].classList.contains("search-highlight")).toBe(true);
  });

  it("timeline_requests_older_history_near_the_top", () => {
    // Arrange
    const onReachTop = vi.fn();
    const { container } = render(
      <Timeline onReachTop={onReachTop}>
        <div style={{ height: "2000px" }}>tall content</div>
      </Timeline>,
    );
    const tl = container.querySelector(".timeline") as HTMLElement;
    setScrollProps(tl, 2000, 200, 100);

    // Act
    fireEvent.scroll(tl);

    // Assert
    expect(onReachTop).toHaveBeenCalledTimes(1);
  });

  it("timeline_fills_a_viewport_shorter_than_the_loaded_page", () => {
    // Arrange: capture the resize callback so the test controls when the
    // browser reports a size change.
    const onReachTop = vi.fn();
    const original = globalThis.ResizeObserver;
    let notify: (() => void) | null = null;
    globalThis.ResizeObserver = class {
      constructor(callback: () => void) {
        notify = callback;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;

    try {
      const { container } = render(
        <Timeline onReachTop={onReachTop}>
          <div>short content</div>
        </Timeline>,
      );
      const tl = container.querySelector(".timeline") as HTMLElement;
      setScrollProps(tl, 100, 800, 0);

      // Act: the container is taller than its content.
      act(() => {
        notify?.();
      });

      // Assert
      expect(onReachTop).toHaveBeenCalled();
    } finally {
      globalThis.ResizeObserver = original;
    }
  });

  it("timeline_anchors_the_viewport_when_older_messages_are_prepended", () => {
    // Arrange
    const { container, rerender } = render(
      <Timeline prependSeq={0}>
        <div>first message</div>
      </Timeline>,
    );
    const tl = container.querySelector(".timeline") as HTMLElement;
    setScrollProps(tl, 1000, 200, 100);
    rerender(
      <Timeline prependSeq={0}>
        <div>first message</div>
      </Timeline>,
    );

    // Act: the older page adds 500px above the current viewport.
    setScrollProps(tl, 1500, 200, 100);
    rerender(
      <Timeline prependSeq={1}>
        <div>older message</div>
        <div>first message</div>
      </Timeline>,
    );

    // Assert
    expect(tl.scrollTop).toBe(600);
  });
});
