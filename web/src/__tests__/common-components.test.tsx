import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Badge } from "../shared/ui/Badge";
import { StatusDot } from "../shared/ui/StatusDot";
import { Spinner } from "../shared/ui/Spinner";
import { Card } from "../shared/ui/Card";
import { Modal } from "../shared/ui/Modal";

describe("common components", () => {
  it("common_components_render_according_to_spec", () => {
    render(<Badge kind="channel">discord</Badge>);
    expect(screen.getByText("discord").className).toContain("badge-channel");
    cleanup();

    render(<StatusDot tone="live" />);
    expect(document.querySelector(".dot-live")).not.toBeNull();
    cleanup();

    render(<StatusDot tone="idle" />);
    expect(document.querySelector(".dot-idle")).not.toBeNull();
    cleanup();

    const onClose = vi.fn();
    render(
      <Modal open onClose={onClose} labelledBy="modal-title">
        <h2 id="modal-title">Dialog</h2>
      </Modal>,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    cleanup();

    render(<Spinner />);
    const spinner = screen.getByRole("status");
    expect(spinner.getAttribute("aria-label")).toBe("Loading");
    expect(spinner.className).toContain("spinner");
    cleanup();

    render(<Card>plain</Card>);
    const plainCard = screen.getByText("plain");
    expect(plainCard.className).toContain("card");
    expect(plainCard.className).not.toContain("card-active");
    cleanup();

    render(<Card active>selected</Card>);
    expect(screen.getByText("selected").className).toContain("card-active");
  });
});
