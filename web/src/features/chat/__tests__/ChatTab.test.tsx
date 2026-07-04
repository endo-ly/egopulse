import { describe, it, expect } from "vitest";
import { render, fireEvent } from "@testing-library/react";
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
  it("chat_tab_renders_header_timeline_composer_structure", () => {
    const { container } = render(
      <ChatTab
        sessionLabel="Web Chat"
        channel="web"
        readOnly={false}
      />,
    );

    const tab = container.querySelector(".chat-tab");
    expect(tab).toBeTruthy();

    const header = tab?.querySelector(".chat-header");
    expect(header).toBeTruthy();

    const timeline = tab?.querySelector(".timeline");
    expect(timeline).toBeTruthy();

    const composer = tab?.querySelector(".composer");
    expect(composer).toBeTruthy();

    const label = header?.querySelector(".chat-header-label");
    expect(label?.textContent).toBe("Web Chat");

    const badge = header?.querySelector(".badge-channel");
    expect(badge).toBeTruthy();

    // Message count is intentionally not shown; no meta for writable sessions.
    expect(header?.querySelector(".chat-header-meta")).toBeNull();
  });

  it("chat_tab_header_shows_read_only_for_non_web_channel", () => {
    const { container } = render(
      <ChatTab
        sessionLabel="Dev Chat"
        channel="discord"
        readOnly={true}
      />,
    );

    const meta = container.querySelector(".chat-header-meta");
    expect(meta?.textContent).toContain("Read-only");
  });

  it("chat_tab_search_opens_counts_and_closes", () => {
    const { container } = render(
      <ChatTab
        sessionLabel="Web Chat"
        channel="web"
        readOnly={false}
        messages={messages}
      />,
    );

    const btn = container.querySelector(".chat-search-btn") as HTMLButtonElement;
    fireEvent.click(btn);

    const input = container.querySelector(".chat-search-input") as HTMLInputElement;
    expect(input).toBeTruthy();
    fireEvent.change(input, { target: { value: "world" } });

    expect(container.querySelector(".chat-search-count")?.textContent).toBe("1 / 2");

    fireEvent.keyDown(input, { key: "Escape" });
    expect(container.querySelector(".chat-search-input")).toBeFalsy();
    expect(container.querySelector(".chat-search-btn")).toBeTruthy();
  });
});
