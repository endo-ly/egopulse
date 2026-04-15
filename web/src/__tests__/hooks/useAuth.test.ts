import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { AUTH_TOKEN_STORAGE_KEY } from "../../api";
import { useAuth } from "../../hooks/useAuth";

describe("useAuth", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("initializes from storage", () => {
    // Arrange
    localStorage.setItem(AUTH_TOKEN_STORAGE_KEY, "stored-token");

    // Act
    const { result } = renderHook(() => useAuth());

    // Assert
    expect(result.current.authToken).toBe("stored-token");
    expect(result.current.authDraft).toBe("stored-token");
  });

  it("save updates token", async () => {
    // Arrange
    const onSuccess = async () => {};
    const { result } = renderHook(() => useAuth());

    // Act
    await act(async () => {
      result.current.setAuthDraft("  new-token  ");
    });
    await act(async () => {
      await result.current.saveAuth(onSuccess);
    });

    // Assert
    await waitFor(() => {
      expect(result.current.authToken).toBe("new-token");
    });
    expect(localStorage.getItem(AUTH_TOKEN_STORAGE_KEY)).toBe("new-token");
    expect(result.current.showAuth).toBe(false);
  });
});
