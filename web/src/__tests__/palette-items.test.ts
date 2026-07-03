import { describe, expect, it, vi } from "vitest";
import { buildPaletteItems } from "../app/command-palette/buildPaletteItems";

describe("buildPaletteItems", () => {
  it("keeps_sleep_navigation_enabled", () => {
    const items = buildPaletteItems({
      agents: [],
      sessions: [],
      selectedAgent: "",
      actions: {
        close: vi.fn(),
        navigate: vi.fn(),
        selectAgent: vi.fn(),
        selectSession: vi.fn(),
        newSession: vi.fn(),
        refresh: vi.fn(),
      },
    });

    const sleepItem = items.find((item) => item.id === "nav-sleep");
    expect(sleepItem?.disabled).not.toBe(true);
  });
});
