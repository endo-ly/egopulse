import { describe, expect, it, vi, afterEach } from "vitest";
import { randomId } from "../random";

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

describe("randomId", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("uses crypto.randomUUID when the context is secure", () => {
    // Arrange
    const randomUUID = vi.fn(() => "11111111-1111-4111-8111-111111111111");
    vi.stubGlobal("crypto", { randomUUID, getRandomValues: vi.fn() });

    // Act
    const id = randomId();

    // Assert
    expect(id).toBe("11111111-1111-4111-8111-111111111111");
    expect(randomUUID).toHaveBeenCalledTimes(1);
  });

  it("falls back to getRandomValues when randomUUID is unavailable", () => {
    // Arrange: an insecure context exposes getRandomValues but not randomUUID.
    const getRandomValues = vi.fn((bytes: Uint8Array) => {
      bytes.fill(0xab);
      return bytes;
    });
    vi.stubGlobal("crypto", { getRandomValues });

    // Act
    const id = randomId();

    // Assert
    expect(getRandomValues).toHaveBeenCalledTimes(1);
    expect(id).toMatch(UUID_V4);
  });
});
