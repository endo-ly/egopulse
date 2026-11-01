import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ConfigPanel } from "../ConfigPanel";
import { configSectionElementId, type ConfigSection } from "../configSections";
import { CONFIG_YAML, runtimeConfig, stubConfigApi } from "./configTestUtils";

const SECTION_LABELS = [
  "General",
  "Agents",
  "Providers",
  "Channels",
  "Sleep",
  "Pulse",
  "Webhooks",
  "Backup",
  "Web Fetch",
  "MCP",
  "YAML file",
];

let scrollIntoView: ReturnType<typeof vi.fn>;

beforeEach(() => {
  scrollIntoView = vi.fn();
  Element.prototype.scrollIntoView = scrollIntoView;
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function renderPanel(authToken: string, section: ConfigSection = "general", activeSection: ConfigSection | null = null) {
  const onSelectSection = vi.fn();
  const onActiveSectionChange = vi.fn();
  const onOpenSchedule = vi.fn();
  const view = render(
    <ConfigPanel
      authToken={authToken}
      section={section}
      activeSection={activeSection}
      onSelectSection={onSelectSection}
      onActiveSectionChange={onActiveSectionChange}
      onOpenSchedule={onOpenSchedule}
    />,
  );
  return { ...view, onSelectSection, onActiveSectionChange, onOpenSchedule };
}

describe("ConfigPanel", () => {
  it("shows_every_section_on_one_page", async () => {
    // Arrange
    stubConfigApi();

    // Act
    renderPanel("t-all");

    // Assert
    await screen.findByRole("heading", { level: 2, name: "General" });
    for (const label of SECTION_LABELS) {
      expect(screen.getByRole("heading", { level: 2, name: label })).toBeTruthy();
    }
    expect(screen.getByRole("heading", { level: 3, name: /Lyre/ })).toBeTruthy();
    expect(screen.getByRole("heading", { level: 3, name: /OpenRouter/ })).toBeTruthy();
  });

  it("opens_at_the_section_named_by_the_route", async () => {
    // Arrange
    stubConfigApi();

    // Act
    renderPanel("t-deep-link", "mcp");

    // Assert
    await screen.findByRole("heading", { level: 2, name: "MCP" });
    const contexts = scrollIntoView.mock.contexts;
    expect(contexts[contexts.length - 1]).toBe(document.getElementById(configSectionElementId("mcp")));
  });

  it("highlights_the_section_reported_as_active", async () => {
    // Arrange
    stubConfigApi();

    // Act
    renderPanel("t-active", "general", "providers");

    // Assert
    const bar = await screen.findByRole("navigation", { name: "Config sections on this page" });
    expect(within(bar).getByRole("button", { name: "Providers" }).getAttribute("aria-current")).toBe("location");
    expect(within(bar).getByRole("button", { name: "General" }).getAttribute("aria-current")).toBeNull();
  });

  it("jumps_to_a_section_from_the_section_bar", async () => {
    // Arrange
    stubConfigApi();
    const { onSelectSection } = renderPanel("t-jump");
    const providers = await screen.findByRole("button", { name: "Providers" });
    scrollIntoView.mockClear();

    // Act
    fireEvent.click(providers);

    // Assert
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(onSelectSection).toHaveBeenCalledWith("providers");
  });

  it("shows_each_sections_headline_state", async () => {
    // Arrange
    stubConfigApi();

    // Act
    renderPanel("t-summary");

    // Assert
    const providers = await screen.findByRole("region", { name: "Providers" });
    expect(within(providers).getByText("1 provider")).toBeTruthy();
    const backup = screen.getByRole("region", { name: "Backup" });
    expect(within(backup).getByText("Every 7 days at 03:00")).toBeTruthy();
  });

  it("marks_a_section_that_needs_attention_in_the_section_bar", async () => {
    // Arrange
    const config = runtimeConfig();
    config.providers[0].api_key = { kind: null, ref: null, configured: false };
    stubConfigApi({ config });

    // Act
    renderPanel("t-warning");

    // Assert
    const providers = await screen.findByRole("region", { name: "Providers" });
    expect(within(providers).getByText("1 missing API key")).toBeTruthy();
    const bar = screen.getByRole("navigation", { name: "Config sections on this page" });
    const chip = within(bar).getByRole("button", { name: "Providers" });
    expect(chip.querySelector(".dot-warning")).toBeTruthy();
  });

  it("loads_the_yaml_only_after_it_is_opened", async () => {
    // Arrange
    const requested = stubConfigApi({ yaml: CONFIG_YAML });
    renderPanel("t-yaml");
    await screen.findByRole("heading", { level: 2, name: "YAML file" });
    expect(requested.some((url) => url.pathname === "/api/config/yaml")).toBe(false);

    // Act
    fireEvent.click(screen.getByRole("button", { name: "Show file" }));

    // Assert
    expect(await screen.findByText("default_agent: lyre")).toBeTruthy();
    expect(requested.some((url) => url.pathname === "/api/config/yaml")).toBe(true);
    expect(screen.getByRole("button", { name: "Hide file" })).toBeTruthy();
  });

  it("keeps_an_open_edit_when_the_route_moves_to_another_section", async () => {
    // Arrange
    stubConfigApi();
    const { rerender, onSelectSection, onActiveSectionChange, onOpenSchedule } = renderPanel("t-edit");
    const general = await screen.findByRole("region", { name: "General" });
    fireEvent.click(within(general).getByRole("button", { name: "Edit" }));
    expect(within(general).getByLabelText("Timezone")).toBeTruthy();

    // Act
    rerender(
      <ConfigPanel
        authToken="t-edit"
        section="providers"
        activeSection={null}
        onSelectSection={onSelectSection}
        onActiveSectionChange={onActiveSectionChange}
        onOpenSchedule={onOpenSchedule}
      />,
    );

    // Assert
    expect(within(general).getByLabelText("Timezone")).toBeTruthy();
  });
});
