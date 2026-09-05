import { describe, expect, it, vi } from "vitest";
import { buildPaletteItems } from "../buildPaletteItems";
import type { ChatMessage } from "../../../shared/api/types";

const noopActions = {
  close: vi.fn(),
  navigate: vi.fn(),
  selectAgent: vi.fn(),
  selectSession: vi.fn(),
  newSession: vi.fn(),
  refresh: vi.fn(),
  jumpToMessage: vi.fn(),
};

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
    content: "the deploy finished successfully",
    timestamp: "2026-07-04T00:00:01.000Z",
    message_kind: "tool_call",
  },
];

describe("buildPaletteItems", () => {
  it("keeps_sleep_navigation_enabled", () => {
    const items = buildPaletteItems({
      agents: [],
      sessions: [],
      selectedAgent: "",
      query: "",
      messages: [],
      actions: noopActions,
    });

    const sleepItem = items.find((item) => item.id === "nav-sleep");
    expect(sleepItem?.disabled).not.toBe(true);
  });

  it("no_message_items_without_query", () => {
    const items = buildPaletteItems({
      agents: [],
      sessions: [],
      selectedAgent: "",
      query: "",
      messages,
      actions: noopActions,
    });

    expect(items.filter((item) => item.section === "Messages")).toHaveLength(0);
  });

  it("message_items_match_query_and_jump_to_index", () => {
    const actions = { ...noopActions };
    const items = buildPaletteItems({
      agents: [],
      sessions: [],
      selectedAgent: "",
      query: "world",
      messages,
      actions,
    });

    const messageItems = items.filter((item) => item.section === "Messages");
    expect(messageItems).toHaveLength(1);
    expect(messageItems[0].label).toBe("hello world");

    messageItems[0].onSelect();
    expect(actions.jumpToMessage).toHaveBeenCalledWith(0);
    expect(actions.close).toHaveBeenCalled();
  });

  it("message_items_skip_tool_calls", () => {
    const items = buildPaletteItems({
      agents: [],
      sessions: [],
      selectedAgent: "",
      query: "deploy",
      messages,
      actions: noopActions,
    });

    expect(items.filter((item) => item.section === "Messages")).toHaveLength(0);
  });
});
