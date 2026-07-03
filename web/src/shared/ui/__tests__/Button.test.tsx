import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { Button } from "../Button";

describe("Button", () => {
  it("button_renders_all_variants_and_states", () => {
    const variants = ["primary", "secondary", "icon", "danger"] as const;

    for (const variant of variants) {
      const label = `btn-${variant}`;
      render(
        <Button variant={variant} onClick={() => {}}>
          {label}
        </Button>,
      );
      const btn = screen.getByText(label).closest("button");
      expect(btn?.className).toContain(`btn-${variant}`);
      cleanup();
    }

    render(
      <Button variant="primary" disabled>
        Disabled
      </Button>,
    );
    expect(screen.getByText("Disabled").closest("button")).toHaveProperty(
      "disabled",
      true,
    );
    cleanup();

    render(
      <Button variant="primary" busy>
        Busy
      </Button>,
    );
    const busyBtn = screen.getByText("Busy").closest("button");
    expect(busyBtn?.getAttribute("aria-busy")).toBe("true");
    expect(busyBtn?.querySelector(".btn-spinner")).toBeTruthy();
  });
});

function cleanup() {
  document.body.innerHTML = "";
}
