import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Sidebar } from "../app/shell/Sidebar";

describe("Sidebar", () => {
  it("sidebar_renders_brand_new_session_and_runtime_status", () => {
    const onNewSession = vi.fn();
    render(
      <Sidebar onNewSession={onNewSession} healthStatus="ok" activeTurns={2} />,
    );

    expect(screen.getByText("EgoPulse")).toBeTruthy();
    expect(screen.getByText(/v0\.1\.0/)).toBeTruthy();

    expect(screen.queryByText("New Session")).not.toBeNull();
    fireEvent.click(screen.getByText("New Session"));
    expect(onNewSession).toHaveBeenCalledTimes(1);

    const runtime = screen.getByText(/2 turns live/);
    expect(runtime).toBeTruthy();
    expect(runtime.closest(".sidebar-runtime-status")?.querySelector(".dot-live")).not.toBeNull();
    cleanup();
  });

  it("sidebar_renders_runtime_status_idle_when_degraded", () => {
    render(
      <Sidebar onNewSession={() => {}} healthStatus="degraded" activeTurns={1} />,
    );
    const runtime = screen.getByText(/degraded/);
    expect(runtime.closest(".sidebar-runtime-status")?.querySelector(".dot-idle")).not.toBeNull();
  });
});
