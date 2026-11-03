import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@testing-library/react";
import { AttachmentCard } from "../AttachmentCard";
import { apiFetchBlob } from "../../../shared/api/client";
import type { AttachmentInfo } from "../../../shared/api/types";

vi.mock("../../../shared/api/client", () => ({
  apiFetchBlob: vi.fn(),
}));

const fetchBlob = vi.mocked(apiFetchBlob);
const AUDIO: AttachmentInfo = {
  id: "00000000-0000-4000-8000-000000000000.mp3",
  filename: "reply.mp3",
  content_type: "audio/mpeg",
  size_bytes: 1234,
};

function audioBlob(): Blob {
  return new Blob(["audio"], { type: "audio/mpeg" });
}

describe("AttachmentCard", () => {
  let anchorClick: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    fetchBlob.mockReset();
    URL.createObjectURL = vi.fn(() => "blob:attachment") as never;
    URL.revokeObjectURL = vi.fn() as never;
    anchorClick = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
  });

  afterEach(() => {
    anchorClick.mockRestore();
    vi.clearAllMocks();
  });

  it("renders_filename_size_and_caption", () => {
    // Arrange / Act
    const { container } = render(
      <AttachmentCard attachment={AUDIO} caption="hear this" authToken="token" />,
    );

    // Assert
    expect(container.querySelector(".attachment-card-title")?.textContent).toBe(
      "reply.mp3",
    );
    expect(container.querySelector(".attachment-card-size")?.textContent).toBe(
      "1.2 KB",
    );
    expect(container.querySelector(".attachment-card-caption")?.textContent).toBe(
      "hear this",
    );
    expect(fetchBlob).not.toHaveBeenCalled();
  });

  it("loads_audio_on_play_and_revokes_its_object_url", async () => {
    // Arrange
    fetchBlob.mockResolvedValue(audioBlob());
    const { container, unmount } = render(
      <AttachmentCard attachment={AUDIO} authToken="token" />,
    );

    // Act
    fireEvent.click(
      container.querySelector(".attachment-card-action") as HTMLButtonElement,
    );

    // Assert
    await waitFor(() => expect(container.querySelector("audio")).toBeTruthy());
    expect(fetchBlob).toHaveBeenCalledWith(
      "/api/attachments/00000000-0000-4000-8000-000000000000.mp3",
      "token",
    );
    expect(container.querySelector("audio")?.getAttribute("src")).toBe(
      "blob:attachment",
    );
    expect(container.querySelector("audio")?.hasAttribute("autoplay")).toBe(true);

    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:attachment");
  });

  it("download_does_not_autoplay_audio", async () => {
    // Arrange
    fetchBlob.mockResolvedValue(audioBlob());
    const { container } = render(
      <AttachmentCard attachment={AUDIO} authToken="token" />,
    );
    const actions = container.querySelectorAll(".attachment-card-action");
    expect(actions).toHaveLength(2);

    // Act: the second action is Download.
    fireEvent.click(actions[1] as HTMLButtonElement);

    // Assert: the player appears for later use but does not start.
    await waitFor(() => expect(container.querySelector("audio")).toBeTruthy());
    expect(container.querySelector("audio")?.hasAttribute("autoplay")).toBe(false);
  });

  it("shows_an_error_and_retries", async () => {
    // Arrange
    fetchBlob
      .mockRejectedValueOnce(new Error("network error"))
      .mockResolvedValueOnce(audioBlob());
    const { container } = render(
      <AttachmentCard attachment={AUDIO} authToken="token" />,
    );

    // Act: first attempt fails.
    fireEvent.click(
      container.querySelector(".attachment-card-action") as HTMLButtonElement,
    );
    await waitFor(() =>
      expect(container.querySelector(".attachment-card-error")).toBeTruthy(),
    );
    expect(container.querySelector(".attachment-card-error")?.textContent).toContain(
      "network error",
    );

    // Act: retry succeeds.
    fireEvent.click(
      container.querySelector(".attachment-card-retry") as HTMLButtonElement,
    );

    // Assert
    await waitFor(() => expect(container.querySelector("audio")).toBeTruthy());
    expect(fetchBlob).toHaveBeenCalledTimes(2);
  });

  it("offers_download_only_for_non_audio_attachments", async () => {
    // Arrange
    fetchBlob.mockResolvedValue(new Blob(["pdf"], { type: "application/octet-stream" }));
    const attachment: AttachmentInfo = {
      id: "00000000-0000-4000-8000-000000000001.bin",
      filename: "notes.pdf",
      content_type: "application/octet-stream",
      size_bytes: 10,
    };
    const { container } = render(
      <AttachmentCard attachment={attachment} authToken="token" />,
    );

    // Assert: no play control for a download-only attachment.
    expect(container.querySelector("audio")).toBeNull();
    const actions = container.querySelectorAll(".attachment-card-action");
    expect(actions).toHaveLength(1);
    expect(actions[0].textContent).toBe("Download");

    // Act
    fireEvent.click(actions[0] as HTMLButtonElement);

    // Assert
    await waitFor(() => expect(fetchBlob).toHaveBeenCalledTimes(1));
    expect(anchorClick).toHaveBeenCalled();
  });
});
