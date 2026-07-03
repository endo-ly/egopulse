import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { ChatTab } from "../features/chat/ChatTab";

describe("ChatTab", () => {
  it("chat_tab_renders_header_timeline_composer_structure", () => {
    const { container } = render(
      <ChatTab
        sessionLabel="Web Chat"
        channel="web"
        messageCount={5}
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

    const meta = header?.querySelector(".chat-header-meta");
    expect(meta?.textContent).toContain("5 messages");
  });

  it("chat_tab_header_shows_read_only_for_non_web_channel", () => {
    const { container } = render(
      <ChatTab
        sessionLabel="Dev Chat"
        channel="discord"
        messageCount={12}
        readOnly={true}
      />,
    );

    const meta = container.querySelector(".chat-header-meta");
    expect(meta?.textContent).toContain("read-only");
  });
});
