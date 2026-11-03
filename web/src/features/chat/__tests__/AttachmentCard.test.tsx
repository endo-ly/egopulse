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

  it("loads_audio_on_click_without_autoplay_and_revokes_on_unmount", async () => {
    // Arrange
    fetchBlob.mockResolvedValue(audioBlob());
    const { container, unmount } = render(
      <AttachmentCard attachment={AUDIO} authToken="token" />,
    );
    const loadButton = container.querySelector(
      ".attachment-card-action",
    ) as HTMLButtonElement;
    expect(loadButton.textContent).toBe("Load audio");

    // Act
    fireEvent.click(loadButton);

    // Assert: the standard player is shown, but playback is not started
    // programmatically after the async fetch.
    await waitFor(() => expect(container.querySelector("audio")).toBeTruthy());
    expect(fetchBlob).toHaveBeenCalledWith(
      "/api/attachments/00000000-0000-4000-8000-000000000000.mp3",
      "token",
    );
    expect(container.querySelector("audio")?.getAttribute("src")).toBe(
      "blob:attachment",
    );
    expect(container.querySelector("audio")?.hasAttribute("autoplay")).toBe(false);

    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:attachment");
  });

  it("download_loads_the_player_without_starting_playback", async () => {
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
    expect(anchorClick).toHaveBeenCalled();
  });

  it("releases_the_object_url_when_playback_ends", async () => {
    // Arrange
    fetchBlob.mockResolvedValue(audioBlob());
    const { container } = render(
      <AttachmentCard attachment={AUDIO} authToken="token" />,
    );
    fireEvent.click(
      container.querySelector(".attachment-card-action") as HTMLButtonElement,
    );
    await waitFor(() => expect(container.querySelector("audio")).toBeTruthy());
    const audio = container.querySelector("audio") as HTMLAudioElement;

    // Act
    fireEvent(audio, new Event("ended"));

    // Assert
    await waitFor(() => expect(container.querySelector("audio")).toBeNull());
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:attachment");
    expect(
      container.querySelector(".attachment-card-action")?.textContent,
    ).toBe("Load audio");
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
