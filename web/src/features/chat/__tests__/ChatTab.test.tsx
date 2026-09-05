import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { ChatTab } from "../ChatTab";
import type { ChatMessage } from "../../../shared/api/types";

const messages: ChatMessage[] = [
  {
    id: "m1",
    sender_id: "user",
    sender_kind: "user",
    content: "hello world",
    timestamp: "2026-07-04T00:00:00.000Z",
    message_kind: "text",
  },
  {
    id: "m2",
    sender_id: "lyre",
    sender_kind: "assistant",
    content: "world peace",
    timestamp: "2026-07-04T00:00:01.000Z",
    message_kind: "text",
  },
];

describe("ChatTab", () => {
  it("chat_tab_renders_timeline_and_composer_without_header", () => {
    const { container } = render(
      <ChatTab channel="web" readOnly={false} />,
    );

    const tab = container.querySelector(".chat-tab");
    expect(tab).toBeTruthy();

    const timeline = tab?.querySelector(".timeline");
    expect(timeline).toBeTruthy();

    const composer = tab?.querySelector(".composer");
    expect(composer).toBeTruthy();

    // The session header was removed; the chat occupies the full height.
    expect(tab?.querySelector(".chat-header")).toBeNull();
  });

  it("chat_tab_shows_read_only_banner_for_non_web_channel", () => {
    const { container } = render(
      <ChatTab channel="discord" readOnly={true} />,
    );

    expect(container.querySelector(".readonly-banner")).toBeTruthy();
    expect(container.querySelector(".composer-form")).toBeNull();
  });

  it("jump_request_flashes_the_target_message_row", () => {
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        messages={messages}
        jumpRequest={{ index: 1, seq: 1 }}
      />,
    );

    const rows = container.querySelectorAll(".timeline-messages > *");
    expect(rows.length).toBe(2);
    expect(rows[1].className).toContain("search-highlight");
    expect(rows[0].className).not.toContain("search-highlight");
  });
});
