import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";
import { AgentsSection } from "../AgentsSection";
import type { AgentEntry } from "../../../shared/api/types";

const { putAgentAvatar, deleteAgentAvatar } = vi.hoisted(() => ({
  putAgentAvatar: vi.fn(),
  deleteAgentAvatar: vi.fn(),
}));

vi.mock("../../../shared/api/agents", () => ({
  putAgentAvatar,
  deleteAgentAvatar,
}));

vi.mock("../AvatarCropModal", () => ({
  AvatarCropModal: ({ onApply }: { onApply: (blob: Blob) => void }) => (
    <button
      type="button"
      onClick={() => onApply(new Blob(["cropped"], { type: "image/webp" }))}
    >
      crop-apply-stub
    </button>
  ),
}));

const AGENTS: AgentEntry[] = [
  {
    id: "lyre",
    label: "Lyre",
    is_default: true,
    active: true,
    avatar_url: "/api/agents/lyre/avatar?v=1",
  },
  { id: "ace", label: "Ace", is_default: false, active: false },
];

describe("AgentsSection", () => {
  beforeEach(() => {
    putAgentAvatar.mockReset().mockResolvedValue({ ok: true, avatar_url: "/v=2" });
    deleteAgentAvatar.mockReset().mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  it("agents_section_renders_list_and_active_state", () => {
    const onSelectAgent = vi.fn();
    render(
      <AgentsSection
        agents={AGENTS}
        selectedAgent="lyre"
        onSelectAgent={onSelectAgent}
      />,
    );

    const lyreRow = screen.getByText("Lyre").closest(".agent-row");
    const aceRow = screen.getByText("Ace").closest(".agent-row");

    expect(lyreRow).not.toBeNull();
    expect(aceRow).not.toBeNull();
    expect(lyreRow?.className).toContain("active");
    expect(aceRow?.className).not.toContain("active");

    expect(lyreRow?.querySelector(".dot-live")).not.toBeNull();
    expect(aceRow?.querySelector(".dot-idle")).not.toBeNull();

    expect(lyreRow?.querySelector(".agent-default-tag")?.textContent).toBe(
      "default",
    );
    expect(aceRow?.querySelector(".agent-default-tag")).toBeNull();

    fireEvent.click(aceRow as HTMLElement);
    expect(onSelectAgent).toHaveBeenCalledWith("ace");
  });

  it("agents_section_renders_avatar_image_with_status_dot_overlay", () => {
    const { container } = render(
      <AgentsSection agents={AGENTS} selectedAgent="lyre" onSelectAgent={vi.fn()} />,
    );

    const avatar = container.querySelector(".agent-avatar");
    expect(avatar).not.toBeNull();
    const img = avatar?.querySelector("img");
    expect(img?.getAttribute("src")).toBe("/api/agents/lyre/avatar?v=1");
    expect(avatar?.querySelector(".agent-avatar-dot.dot-live")).not.toBeNull();

    // Agents without an avatar keep the plain status dot.
    const aceRow = screen.getByText("Ace").closest(".agent-row");
    expect(aceRow?.querySelector(".agent-avatar")).toBeNull();
    expect(aceRow?.querySelector(".dot-idle")).not.toBeNull();
  });

  it("agents_section_opens_crop_modal_on_file_selection", async () => {
    render(
      <AgentsSection
        agents={AGENTS}
        selectedAgent="lyre"
        onSelectAgent={vi.fn()}
        authToken="token"
        onAvatarChanged={vi.fn()}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: /change lyre icon/i }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: /upload image/i }));

    expect(
      screen.queryByRole("menuitem", { name: /upload image/i }),
    ).toBeNull();

    const input = document.querySelector(
      '.agents-section input[type="file"]',
    ) as HTMLInputElement;
    const file = new File(["png"], "icon.png", { type: "image/png" });
    Object.defineProperty(input, "files", { value: [file] });
    fireEvent.change(input);

    await waitFor(() =>
      expect(screen.getByText("crop-apply-stub")).not.toBeNull(),
    );
    expect(putAgentAvatar).not.toHaveBeenCalled();
  });

  it("agents_section_uploads_cropped_blob_on_apply", async () => {
    const onAvatarChanged = vi.fn();
    render(
      <AgentsSection
        agents={AGENTS}
        selectedAgent="lyre"
        onSelectAgent={vi.fn()}
        authToken="token"
        onAvatarChanged={onAvatarChanged}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: /change lyre icon/i }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: /upload image/i }));
    const input = document.querySelector(
      '.agents-section input[type="file"]',
    ) as HTMLInputElement;
    const file = new File(["png"], "icon.png", { type: "image/png" });
    Object.defineProperty(input, "files", { value: [file] });
    fireEvent.change(input);

    fireEvent.click(await screen.findByText("crop-apply-stub"));

    await waitFor(() => expect(putAgentAvatar).toHaveBeenCalledTimes(1));
    const [agentId, image, authToken] = putAgentAvatar.mock.calls[0];
    expect(agentId).toBe("lyre");
    expect(image).toBeInstanceOf(Blob);
    expect(authToken).toBe("token");
    expect(onAvatarChanged).toHaveBeenCalledTimes(1);
    // The crop modal closed after a successful upload.
    expect(screen.queryByText("crop-apply-stub")).toBeNull();
  });

  it("agents_section_removes_avatar_via_menu", async () => {
    const onAvatarChanged = vi.fn();
    render(
      <AgentsSection
        agents={AGENTS}
        selectedAgent="lyre"
        onSelectAgent={vi.fn()}
        authToken="token"
        onAvatarChanged={onAvatarChanged}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: /change lyre icon/i }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: /remove image/i }));

    await waitFor(() => expect(deleteAgentAvatar).toHaveBeenCalledTimes(1));
    expect(deleteAgentAvatar).toHaveBeenCalledWith("lyre", "token");
    expect(onAvatarChanged).toHaveBeenCalledTimes(1);
  });

  it("agents_section_keeps_edit_button_enabled_without_auth_token", () => {
    render(
      <AgentsSection agents={AGENTS} selectedAgent="lyre" onSelectAgent={vi.fn()} />,
    );

    const edit = screen.getByRole("button", { name: /change lyre icon/i });
    expect((edit as HTMLButtonElement).disabled).toBe(false);
  });

  it("agents_section_closes_menu_on_outside_click", () => {
    render(
      <AgentsSection agents={AGENTS} selectedAgent="lyre" onSelectAgent={vi.fn()} />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: /change lyre icon/i }),
    );
    expect(screen.getByRole("menuitem", { name: /upload image/i })).not.toBeNull();

    fireEvent.mouseDown(document.body);
    expect(
      screen.queryByRole("menuitem", { name: /upload image/i }),
    ).toBeNull();
  });
});
