import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ConfigSectionNav } from "../ConfigSectionNav";
import { configSectionElementId } from "../configSections";
import { stubConfigApi } from "./configTestUtils";

let scrollIntoView: ReturnType<typeof vi.fn>;

beforeEach(() => {
  scrollIntoView = vi.fn();
  Element.prototype.scrollIntoView = scrollIntoView;
});

afterEach(() => {
  document.getElementById(configSectionElementId("sleep"))?.remove();
  cleanup();
  vi.unstubAllGlobals();
});

describe("ConfigSectionNav", () => {
  it("marks_the_section_currently_under_the_sticky_bar", async () => {
    // Arrange
    stubConfigApi();

    // Act
    render(<ConfigSectionNav authToken="t-nav-active" activeSection="providers" onSelectSection={vi.fn()} />);

    // Assert
    await waitFor(() => expect(screen.getByText("1 provider")).toBeTruthy());
    expect(screen.getByRole("button", { name: /Providers/ }).getAttribute("aria-current")).toBe("location");
    expect(screen.getByRole("button", { name: /General/ }).getAttribute("aria-current")).toBeNull();
  });

  it("scrolls_to_the_section_and_reports_the_selection", async () => {
    // Arrange
    stubConfigApi();
    const onSelectSection = vi.fn();
    const section = document.createElement("section");
    section.id = configSectionElementId("sleep");
    document.body.appendChild(section);
    render(<ConfigSectionNav authToken="t-nav-click" activeSection="general" onSelectSection={onSelectSection} />);
    await waitFor(() => expect(screen.getByText("1 provider")).toBeTruthy());

    // Act
    fireEvent.click(screen.getByRole("button", { name: /Sleep/ }));

    // Assert
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(onSelectSection).toHaveBeenCalledWith("sleep");
    section.remove();
  });
});
