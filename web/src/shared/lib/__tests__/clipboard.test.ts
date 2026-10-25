import { afterEach, describe, expect, it, vi } from "vitest";
import { copyText } from "../clipboard";

afterEach(() => {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: undefined,
  });
  Object.defineProperty(document, "execCommand", {
    configurable: true,
    value: undefined,
  });
});

describe("copyText", () => {
  it("uses_the_async_clipboard_api_when_available", async () => {
    // Arrange
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    // Act
    const copied = await copyText("chat:1");

    // Assert
    expect(copied).toBe(true);
    expect(writeText).toHaveBeenCalledWith("chat:1");
  });

  it("falls_back_to_exec_command_without_the_clipboard_api", async () => {
    // Arrange
    const execCommand = vi.fn().mockReturnValue(true);
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: execCommand,
    });

    // Act
    const copied = await copyText("chat:2");

    // Assert
    expect(copied).toBe(true);
    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("falls_back_when_the_clipboard_api_rejects", async () => {
    // Arrange
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const execCommand = vi.fn().mockReturnValue(true);
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: execCommand,
    });

    // Act
    const copied = await copyText("chat:3");

    // Assert
    expect(copied).toBe(true);
    expect(execCommand).toHaveBeenCalledWith("copy");
  });

  it("reports_failure_when_both_copy_paths_fail", async () => {
    // Arrange
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: vi.fn().mockReturnValue(false),
    });

    // Act
    const copied = await copyText("chat:4");

    // Assert
    expect(copied).toBe(false);
  });
});
