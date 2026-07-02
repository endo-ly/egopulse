import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { ReadOnlyBanner } from "../components/ReadOnlyBanner";

describe("ReadOnlyBanner", () => {
  it("readonly_session_shows_banner_instead_of_composer", () => {
    const { container } = render(<ReadOnlyBanner channel="discord" />);

    const banner = container.querySelector(".readonly-banner");
    expect(banner).toBeTruthy();

    const text = container.querySelector(".readonly-text");
    expect(text?.textContent).toContain("Discord");
    expect(text?.textContent).toContain("use Discord directly");
  });

  it("readonly_banner_shows_telegram_for_telegram_channel", () => {
    const { container } = render(<ReadOnlyBanner channel="telegram" />);
    const text = container.querySelector(".readonly-text");
    expect(text?.textContent).toContain("Telegram");
  });
});
