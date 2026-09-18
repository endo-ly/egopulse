import { describe, it, expect } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { MessageBubble } from "../MessageBubble";
import type { ChatMessage } from "../../../shared/api/types";

function msg(overrides: Partial<ChatMessage>): ChatMessage {
  return {
    id: "m1",
    sender_id: "lyre",
    sender_kind: "assistant",
    content: "hello",
    timestamp: "2026-01-01T12:00:00Z",
    message_kind: "message",
    ...overrides,
  };
}

describe("MessageBubble", () => {
  it("message_bubble_renders_per_sender_kind", () => {
    const kinds: Array<{ kind: string; cls: string }> = [
      { kind: "user", cls: "bubble-user" },
      { kind: "assistant", cls: "bubble-assistant" },
      { kind: "system", cls: "bubble-system" },
      { kind: "tool", cls: "bubble-tool" },
    ];

    for (const { kind, cls } of kinds) {
      const { container } = render(
        <MessageBubble message={msg({ sender_kind: kind as ChatMessage["sender_kind"] })} />,
      );
      const row = container.querySelector(`.${cls}`);
      expect(row).toBeTruthy();
      const header = row?.querySelector(".message-header");
      expect(header).toBeTruthy();
      const avatar = header?.querySelector(".message-avatar");
      // User messages render no avatar; all others do.
      expect(avatar !== null).toBe(kind !== "user");
      const time = header?.querySelector(".message-time");
      expect(time).toBeTruthy();
    }
  });

  it("assistant_message_renders_content_without_stream_markers", () => {
    // Message identity is stable, so bubbles carry no draft/cursor state:
    // progress is a separate indicator row owned by ChatTab.
    const { container } = render(
      <MessageBubble message={msg({ id: "turn:t1:assistant:1", content: "partial" })} />,
    );
    expect(container.querySelector(".streaming-cursor")).toBeNull();
    expect(container.querySelector(".thinking-dots")).toBeNull();
  });

  it("pulse_notification_renders_pulse_badge", () => {
    const { container } = render(
      <MessageBubble message={msg({ message_kind: "pulse_notification" })} />,
    );
    const badge = container.querySelector(".pulse-badge");
    expect(badge).toBeTruthy();
  });

  it("assistant_avatar_uses_image_and_letter_fallback", () => {
    const withImage = render(
      <MessageBubble
        message={msg({ sender_id: "lyre" })}
        agentAvatars={{ lyre: "blob:lyre-avatar" }}
      />,
    );
    const imageAvatar = withImage.container.querySelector(".message-avatar");
    expect(imageAvatar?.querySelector("img")?.getAttribute("src")).toBe(
      "blob:lyre-avatar",
    );
    expect(imageAvatar?.textContent).toBe("");
    cleanup();

    const withoutImage = render(
      <MessageBubble message={msg({ sender_id: "lyre" })} agentAvatars={{}} />,
    );
    const letterAvatar = withoutImage.container.querySelector(".message-avatar");
    expect(letterAvatar?.querySelector("img")).toBeNull();
    expect(letterAvatar?.textContent).toBe("L");
  });
});
