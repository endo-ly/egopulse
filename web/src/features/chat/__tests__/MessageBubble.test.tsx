import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
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
      expect(avatar).toBeTruthy();
      const time = header?.querySelector(".message-time");
      expect(time).toBeTruthy();
    }
  });

  it("streaming_indicator_renders_for_draft_message", () => {
    const { container } = render(
      <MessageBubble message={msg({ id: "draft:abc", content: "partial" })} />,
    );
    const cursor = container.querySelector(".streaming-cursor");
    expect(cursor).toBeTruthy();
  });

  it("streaming_indicator_removed_on_done", () => {
    const { container } = render(
      <MessageBubble message={msg({ id: "draft:abc:done", content: "final" })} />,
    );
    const cursor = container.querySelector(".streaming-cursor");
    expect(cursor).toBeFalsy();
  });

  it("normal_assistant_message_has_no_pulse_badge", () => {
    const { container } = render(
      <MessageBubble message={msg({ message_kind: "message" })} />,
    );
    const badge = container.querySelector(".pulse-badge");
    expect(badge).toBeFalsy();
  });

  it("pulse_notification_renders_pulse_badge", () => {
    const { container } = render(
      <MessageBubble message={msg({ message_kind: "pulse_notification" })} />,
    );
    const badge = container.querySelector(".pulse-badge");
    expect(badge).toBeTruthy();
  });
});
