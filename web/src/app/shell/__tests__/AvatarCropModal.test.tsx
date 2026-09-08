import { useEffect, useRef } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";
import { AvatarCropModal } from "../AvatarCropModal";

const { cropImageToAvatar, resizeImageToAvatar } = vi.hoisted(() => ({
  cropImageToAvatar: vi.fn(),
  resizeImageToAvatar: vi.fn(),
}));

vi.mock("../../../shared/lib/image", () => ({
  cropImageToAvatar,
  resizeImageToAvatar,
}));

// The real cropper needs image decoding; stub it and report a fixed crop area.
vi.mock("react-easy-crop", () => ({
  default: ({
    onCropComplete,
  }: {
    onCropComplete: (area: unknown, pixels: unknown) => void;
  }) => {
    const calledRef = useRef(false);
    useEffect(() => {
      if (calledRef.current) return;
      calledRef.current = true;
      onCropComplete(
        { x: 0, y: 0, width: 100, height: 100 },
        { x: 10, y: 20, width: 120, height: 120 },
      );
    }, [onCropComplete]);
    return <div data-testid="cropper-stub" />;
  },
}));

const FILE = new File(["png"], "photo.png", { type: "image/png" });
const CROPPED = new Blob(["cropped"], { type: "image/webp" });

describe("AvatarCropModal", () => {
  beforeEach(() => {
    cropImageToAvatar.mockReset().mockResolvedValue(CROPPED);
    resizeImageToAvatar.mockReset().mockResolvedValue(new Blob(["resized"]));
    Object.assign(URL, {
      createObjectURL: vi.fn(() => "blob:crop-source"),
      revokeObjectURL: vi.fn(),
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("avatar_crop_applies_selected_region", async () => {
    const onApply = vi.fn();
    render(
      <AvatarCropModal file={FILE} onCancel={vi.fn()} onApply={onApply} />,
    );

    expect(screen.getByTestId("cropper-stub")).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /apply/i }));

    await waitFor(() => expect(onApply).toHaveBeenCalledTimes(1));
    expect(onApply).toHaveBeenCalledWith(CROPPED);
    expect(cropImageToAvatar).toHaveBeenCalledWith(FILE, {
      x: 10,
      y: 20,
      width: 120,
      height: 120,
    });
  });

  it("avatar_crop_cancel_closes_without_uploading", () => {
    const onCancel = vi.fn();
    const onApply = vi.fn();
    render(
      <AvatarCropModal file={FILE} onCancel={onCancel} onApply={onApply} />,
    );

    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onApply).not.toHaveBeenCalled();
  });

  it("avatar_crop_shows_zoom_control", () => {
    render(
      <AvatarCropModal file={FILE} onCancel={vi.fn()} onApply={vi.fn()} />,
    );

    const zoom = screen.getByRole("slider");
    expect(zoom.getAttribute("min")).toBe("1");
    expect(zoom.getAttribute("max")).toBe("10");
  });
});
