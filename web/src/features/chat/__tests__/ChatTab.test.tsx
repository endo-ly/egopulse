import { describe, it, expect, vi } from "vitest";
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
const entries = messages.map((message) => ({ message }));

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
        entries={entries}
        jumpRequest={{ index: 1, seq: 1 }}
      />,
    );

    const rows = container.querySelectorAll(".timeline-messages > *");
    expect(rows.length).toBe(2);
    expect(rows[1].className).toContain("search-highlight");
    expect(rows[0].className).not.toContain("search-highlight");
  });

  it("waiting_indicator_renders_outside_the_message_list", () => {
    const withWaiting = render(
      <ChatTab channel="web" readOnly={false} entries={entries} waitingForAssistant />,
    );
    expect(withWaiting.container.querySelector(".thinking-dots")).toBeTruthy();
    // The indicator is progress UI, not a message: it renders one extra row
    // without touching the message list itself.
    expect(
      withWaiting.container.querySelectorAll(".timeline-messages > *"),
    ).toHaveLength(3);

    const withoutWaiting = render(
      <ChatTab channel="web" readOnly={false} entries={entries} />,
    );
    expect(withoutWaiting.container.querySelector(".thinking-dots")).toBeNull();
  });

  it("waiting_indicator_shows_the_agent_avatar", () => {
    // Arrange / Act
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        agentId="lyre"
        agentAvatars={{ lyre: "blob:lyre-avatar" }}
        waitingForAssistant
      />,
    );

    // Assert
    const row = container.querySelector(".timeline-messages > *") as HTMLElement;
    expect(row.querySelector(".thinking-dots")).toBeTruthy();
    expect(row.querySelector(".message-avatar img")?.getAttribute("src")).toBe(
      "blob:lyre-avatar",
    );
  });

  it("waiting_indicator_after_an_assistant_row_does_not_repeat_the_header", () => {
    // Arrange / Act
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        entries={entries}
        agentId="lyre"
        agentAvatars={{ lyre: "blob:lyre-avatar" }}
        waitingForAssistant
      />,
    );

    // Assert: the assistant row above already carries the turn's header.
    const rows = container.querySelectorAll(".timeline-messages > *");
    const waitingRow = rows[rows.length - 1] as HTMLElement;
    expect(waitingRow.querySelector(".thinking-dots")).toBeTruthy();
    expect(waitingRow.querySelector(".message-header")).toBeNull();
  });

  it("tool_row_that_starts_a_turn_shows_the_agent_avatar", () => {
    // Arrange
    const tool: ChatMessage = {
      id: "tool:call-1",
      sender_id: "assistant",
      sender_kind: "assistant",
      content: JSON.stringify({ tool: "read", status: "success", result: "ok" }),
      timestamp: "2026-07-04T00:00:02.000Z",
      message_kind: "tool_call",
    };

    // Act
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        agentId="lyre"
        agentAvatars={{ lyre: "blob:lyre-avatar" }}
        entries={[{ message: messages[0] }, { message: tool }]}
      />,
    );

    // Assert
    const toolRow = container.querySelectorAll(".timeline-messages > *")[1] as HTMLElement;
    expect(toolRow.querySelector(".tool-card")).toBeTruthy();
    expect(toolRow.querySelector(".message-avatar img")?.getAttribute("src")).toBe(
      "blob:lyre-avatar",
    );
  });

  it("tool_row_under_a_narration_does_not_repeat_the_header", () => {
    // Arrange
    const tool: ChatMessage = {
      id: "tool:call-1",
      sender_id: "assistant",
      sender_kind: "assistant",
      content: JSON.stringify({ tool: "read", status: "success", result: "ok" }),
      timestamp: "2026-07-04T00:00:02.000Z",
      message_kind: "tool_call",
    };

    // Act
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        agentId="lyre"
        agentAvatars={{ lyre: "blob:lyre-avatar" }}
        entries={[
          { message: messages[0] },
          { message: messages[1] },
          { message: tool },
        ]}
      />,
    );

    // Assert: the assistant narration already carries the turn's header.
    const toolRow = container.querySelectorAll(".timeline-messages > *")[2] as HTMLElement;
    expect(toolRow.querySelector(".tool-card")).toBeTruthy();
    expect(toolRow.querySelector(".message-header")).toBeNull();
  });

  it("renders_older_history_progress_and_start_marker", () => {
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        entries={entries}
        loadingOlder
        atHistoryStart
      />,
    );

    expect(container.querySelector(".timeline-older-loading")).toBeTruthy();
    expect(container.querySelector(".timeline-history-start")?.textContent).toContain(
      "Beginning of conversation",
    );
  });

  it("retries_a_failed_older_page", () => {
    // Arrange
    const retry = vi.fn();
    const { container } = render(
      <ChatTab
        channel="web"
        readOnly={false}
        entries={entries}
        olderError="request failed"
        onRetryOlder={retry}
      />,
    );

    // Act
    const retryButton = container.querySelector(
      ".timeline-older-error button",
    ) as HTMLButtonElement;
    expect(retryButton).toBeTruthy();
    retryButton.click();

    // Assert
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("pulse_output_row_carries_its_folded_trace_line", () => {
    // Arrange
    const output = {
      ...messages[1],
      id: "pulse:run-1:output",
      pulse: { run_id: "run-1", role: "output" as const, intention_id: "morning_review" },
    };

    // Act
    const { container } = render(
      <ChatTab
        channel="discord"
        readOnly
        entries={[
          { message: messages[0] },
          { message: output, pulseTrace: { intentionId: "morning_review", entries: [] } },
        ]}
      />,
    );

    // Assert
    const rows = container.querySelectorAll(".timeline-messages > *");
    expect(rows).toHaveLength(2);
    expect(rows[0].querySelector(".pulse-trace")).toBeNull();
    expect(rows[1].querySelector(".pulse-trace-name")?.textContent).toBe("morning_review");
  });
});
