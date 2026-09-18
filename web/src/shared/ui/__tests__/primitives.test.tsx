import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Spinner } from "../Spinner";
import { Modal } from "../Modal";

describe("common components", () => {
  it("modal_handles_keyboard_and_backdrop_close_behavior", () => {
    const onClose = vi.fn();
    render(
      <Modal open onClose={onClose} labelledBy="modal-title">
        <h2 id="modal-title">Dialog</h2>
      </Modal>,
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    cleanup();

    const ignoreBackdropClose = vi.fn();
    render(
      <Modal
        open
        onClose={ignoreBackdropClose}
        labelledBy="modal-title"
        closeOnBackdrop={false}
      >
        <h2 id="modal-title">Dialog</h2>
      </Modal>,
    );
    fireEvent.click(document.querySelector(".modal-backdrop") as HTMLElement);
    expect(ignoreBackdropClose).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(ignoreBackdropClose).toHaveBeenCalledTimes(1);
    cleanup();
  });

  it("spinner_exposes_loading_status", () => {
    render(<Spinner />);
    const spinner = screen.getByRole("status");
    expect(spinner.getAttribute("aria-label")).toBe("Loading");
  });
});
