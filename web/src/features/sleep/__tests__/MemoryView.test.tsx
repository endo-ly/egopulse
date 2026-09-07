import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryView } from "../MemoryView";

function stubMemory(memory: Record<string, string>) {
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation(() =>
      Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true, memory }) }),
    ),
  );
}

describe("MemoryView", () => {
  it("renders_the_selected_memory_file_content", async () => {
    stubMemory({
      episodic: "# Episodic\n- entry\n",
      semantic: "",
      prospective: "",
    });
    const { container } = render(<MemoryView agentId="lyre" authToken="token" />);

    await waitFor(() => {
      expect(container.querySelector(".memory-content")).toBeTruthy();
    });
    expect(container.querySelector(".memory-content")!.textContent).toContain("# Episodic");
  });

  it("switches_files_and_reports_empty_files", async () => {
    stubMemory({
      episodic: "# Episodic\n",
      semantic: "",
      prospective: "# Prospective\n",
    });
    const { container } = render(<MemoryView agentId="switch" authToken="token" />);

    await waitFor(() => {
      expect(container.querySelectorAll(".tab-switch-item")).toHaveLength(3);
    });

    fireEvent.click(container.querySelectorAll(".tab-switch-item")[1]);
    await waitFor(() => {
      expect(screen.getByText("The semantic memory file is empty.")).toBeTruthy();
    });

    fireEvent.click(container.querySelectorAll(".tab-switch-item")[2]);
    await waitFor(() => {
      expect(container.querySelector(".memory-content")!.textContent).toContain("# Prospective");
    });
  });

  it("shows_empty_state_when_no_memory_exists", async () => {
    stubMemory({ episodic: "", semantic: "", prospective: "" });
    render(<MemoryView agentId="fresh" authToken="token" />);

    await waitFor(() => {
      expect(screen.getByText("No memory yet")).toBeTruthy();
    });
  });

  it("shows_error_state_on_fetch_failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(() =>
        Promise.resolve({ ok: false, status: 500, json: () => Promise.resolve({ ok: false, error: "boom" }) }),
      ),
    );
    const { container } = render(<MemoryView agentId="err-agent" authToken="token" />);

    await waitFor(() => {
      expect(container.querySelector(".run-error")).toBeTruthy();
    });
  });
});
