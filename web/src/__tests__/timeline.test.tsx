import { describe, it, expect } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { Timeline } from "../components/Timeline";

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
});
