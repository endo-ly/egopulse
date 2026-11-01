import { describe, expect, it } from "vitest";

import { pickActiveSection, type SectionPosition } from "../configSections";

const POSITIONS: SectionPosition[] = [
  { id: "general", top: 120 },
  { id: "agents", top: 480 },
  { id: "providers", top: 900 },
];

describe("pickActiveSection", () => {
  it("keeps_the_first_section_before_any_heading_reaches_the_line", () => {
    // Arrange / Act / Assert
    expect(pickActiveSection(POSITIONS, 40, false)).toBe("general");
    expect(pickActiveSection(POSITIONS, 120, false)).toBe("general");
  });

  it("follows_the_last_heading_above_the_line", () => {
    // Arrange / Act / Assert
    expect(pickActiveSection(POSITIONS, 479, false)).toBe("general");
    expect(pickActiveSection(POSITIONS, 480, false)).toBe("agents");
    expect(pickActiveSection(POSITIONS, 1_400, false)).toBe("providers");
  });

  it("selects_the_last_section_at_the_bottom_even_when_it_is_short", () => {
    // Arrange / Act / Assert
    expect(pickActiveSection(POSITIONS, 120, true)).toBe("providers");
  });

  it("falls_back_to_the_first_section_when_no_section_is_mounted", () => {
    // Arrange / Act / Assert
    expect(pickActiveSection([], 120, false)).toBe("general");
  });
});
