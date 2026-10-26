import { describe, expect, it } from "vitest";
import { channelShortLabel } from "../format";

describe("channelShortLabel", () => {
  it("maps_known_channels_to_distinct_letters", () => {
    // Arrange
    const channels = ["web", "discord", "telegram", "cli", "tui", "voice"];

    // Act
    const labels = channels.map(channelShortLabel);

    // Assert
    expect(labels).toEqual(["W", "D", "T", "C", "U", "V"]);
  });

  it("falls_back_to_the_first_letter_of_unknown_channels", () => {
    // Arrange
    const channel = "matrix";

    // Act
    const label = channelShortLabel(channel);

    // Assert
    expect(label).toBe("M");
  });
});
