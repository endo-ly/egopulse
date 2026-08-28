import { describe, it, expect, vi, beforeAll, afterAll } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { ErrorBoundary } from "../ErrorBoundary";

function Bomb(): never {
  throw new Error("boom");
}

describe("ErrorBoundary", () => {
  beforeAll(() => {
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterAll(() => {
    vi.restoreAllMocks();
  });

  it("error_boundary_renders_children_when_no_error", () => {
    const { getByText } = render(
      <ErrorBoundary>
        <p>fine</p>
      </ErrorBoundary>,
    );
    expect(getByText("fine")).toBeTruthy();
    cleanup();
  });

  it("error_boundary_shows_fallback_with_reload_when_child_throws", () => {
    const reload = vi.fn();
    vi.stubGlobal("location", { reload } as unknown as Location);
    const { getByText, getByRole } = render(
      <ErrorBoundary>
        <Bomb />
      </ErrorBoundary>,
    );

    expect(getByText("Something went wrong")).toBeTruthy();
    expect(getByText("boom")).toBeTruthy();

    fireEvent.click(getByRole("button", { name: "Reload" }));
    expect(reload).toHaveBeenCalledTimes(1);
    vi.unstubAllGlobals();
    cleanup();
  });
});
