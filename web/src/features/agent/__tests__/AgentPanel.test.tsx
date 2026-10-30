import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { AgentPanel } from "../AgentPanel";
import type { AgentSection } from "../../../app/router";
import type {
  AgentProfile,
  MemoryChange,
  PromptFileLayer,
  SessionEntry,
  SleepRun,
} from "../../../shared/api/types";

const PROFILE: AgentProfile = {
  id: "lyre",
  label: "Lyre",
  is_default: true,
  reasoning_effort: "medium",
  llm: [
    {
      channel: null,
      provider: "sakura",
      model: "Kimi-K2.6",
      provider_source: "agent",
      model_source: "agent",
    },
    {
      channel: "voice",
      provider: "openrouter",
      model: "gpt-4.1-mini",
      provider_source: "profile",
      model_source: "profile",
    },
  ],
  bindings: { discord_bot: "lyre", telegram_bot: null },
  prompt_sources: {
    soul: { scope: "agent", path: "agents/lyre/SOUL.md", chars: 2100 },
    rules: [{ scope: "global", path: "AGENTS.md", chars: 300 }],
    secret: { path: "agents/lyre/SECRET.md", exists: true },
    memory: { episodic: 8400, semantic: 3200, prospective: 0 },
    skills: 6,
  },
};

type Routes = Record<string, unknown>;

/** Stubs fetch so each API path returns its own JSON body. */
function stubApi(routes: Routes) {
  const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
    const path = new URL(String(input), "http://localhost").pathname;
    const body = routes[path];
    if (body === undefined) {
      return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({}) });
    }
    return Promise.resolve({ ok: true, json: () => Promise.resolve({ ok: true, ...body }) });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function requestedPaths(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls.map(
    ([input]) => new URL(String(input), "http://localhost").pathname,
  );
}

let tokenSeq = 0;

const SESSIONS: SessionEntry[] = [
  {
    session_key: "chat:1",
    chat_id: 1,
    name: null,
    channel: "discord",
    agent_id: "lyre",
    last_message_preview: "hi",
    last_message_time: Date.now() - 5 * 60_000,
    archived: false,
    pinned: false,
  },
  {
    session_key: "chat:2",
    chat_id: 2,
    name: null,
    channel: "web",
    agent_id: "lyre",
    last_message_preview: "yo",
    last_message_time: Date.now() - 3 * 3600_000,
    archived: false,
    pinned: false,
  },
  {
    session_key: "chat:3",
    chat_id: 3,
    name: null,
    channel: "web",
    agent_id: "ace",
    last_message_preview: "other agent",
    last_message_time: Date.now(),
    archived: false,
    pinned: false,
  },
];

const CHANGE: MemoryChange = {
  run_id: "run-abcdef",
  file: "semantic",
  created_at: "2026-10-30T03:05:00+00:00",
  content_before: "a\nb",
  content_after: "a\nc",
};

function renderPanel(section: AgentSection, overrides: Partial<Parameters<typeof AgentPanel>[0]> = {}) {
  const props = {
    agentId: "lyre",
    // A distinct token per render keeps the module-level query cache isolated.
    authToken: `token-${++tokenSeq}`,
    section,
    avatarUrl: undefined,
    sessions: SESSIONS,
    onSelectSection: vi.fn(),
    onOpenRun: vi.fn(),
    ...overrides,
  };
  render(<AgentPanel {...props} />);
  return props;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("AgentPanel", () => {
  it("overview_summarises_identity_usage_model_and_prompt", async () => {
    // Arrange
    const run: SleepRun = {
      id: "run-sleep1",
      agent_id: "lyre",
      status: "success",
      trigger: "scheduled",
      started_at: "2026-10-30T03:00:00+00:00",
      finished_at: "2026-10-30T03:04:00+00:00",
      source_chats_json: "[]",
      source_digest_md: "",
      input_tokens: 0,
      output_tokens: 0,
      total_tokens: 0,
      error_message: null,
      session_count: 2,
    };
    stubApi({
      "/api/agents/lyre/profile": { profile: PROFILE },
      "/api/agents/lyre/memory/history": { changes: [CHANGE] },
      "/api/sleep/runs": { runs: [run] },
    });

    // Act
    const props = renderPanel("overview");

    // Assert: identity
    await waitFor(() => expect(screen.getByRole("heading", { name: "Lyre" })).toBeTruthy());
    const hero = screen.getByRole("region", { name: "Lyre" });
    expect(hero.textContent).toContain("sakura / Kimi-K2.6");
    expect(hero.textContent).toContain("Discord lyre");

    // Assert: usage counts only this agent's sessions
    const stats = document.querySelector(".agent-stats") as HTMLElement;
    expect(stats.textContent).toContain("Sessions2");
    expect(stats.textContent).toContain("Discord 1 · Web 1");
    expect(stats.textContent).toContain("Last active5m");

    // Assert: model rows name the config key that set them
    const model = screen.getByRole("region", { name: "Model" });
    expect(model.textContent).toContain("Other channels");
    expect(model.textContent).toContain("agents.lyre");
    expect(model.textContent).toContain("agents.lyre.profiles.voice");
    expect(model.textContent).not.toContain("←");

    // Assert: prompt makeup links to its sections
    const prompt = screen.getByRole("region", { name: "System prompt" });
    expect(prompt.textContent).toContain("agents/lyre/SOUL.md");
    fireEvent.click(screen.getByRole("button", { name: /Rules/ }));
    expect(props.onSelectSection).toHaveBeenCalledWith("rules");

    // Assert: recent activity merges sleep runs and memory changes
    await waitFor(() => expect(screen.getByText("Sleep · Scheduled")).toBeTruthy());
    expect(screen.getByText("Memory · semantic")).toBeTruthy();
    fireEvent.click(screen.getByText("Sleep · Scheduled"));
    expect(props.onOpenRun).toHaveBeenCalledWith("run-sleep1");
  });

  it("soul_marks_the_applied_layer_and_the_fallback", async () => {
    // Arrange
    const layers: PromptFileLayer[] = [
      { scope: "agent", path: "agents/lyre/SOUL.md", content: null, applied: false },
      { scope: "global", path: "SOUL.md", content: "Global soul", applied: true },
    ];
    stubApi({ "/api/agents/lyre/files/soul": { layers } });

    // Act
    renderPanel("soul");

    // Assert
    await waitFor(() => expect(screen.getByText("Global soul")).toBeTruthy());
    const cards = screen.getAllByRole("article");
    expect(cards[0].textContent).toContain("agents/lyre/SOUL.md");
    expect(cards[0].textContent).toContain("Not created");
    expect(cards[1].textContent).toContain("SOUL.md");
    expect(cards[1].textContent).toContain("Applied");
  });

  it("secret_is_not_fetched_until_revealed", async () => {
    // Arrange
    const fetchMock = stubApi({
      "/api/agents/lyre/files/secret": {
        layers: [
          { scope: "agent", path: "agents/lyre/SECRET.md", content: "intimate", applied: true },
        ],
      },
    });

    // Act
    renderPanel("secret");

    // Assert: hidden until the user asks
    expect(screen.queryByText("intimate")).toBeNull();
    expect(requestedPaths(fetchMock)).not.toContain("/api/agents/lyre/files/secret");

    // Act
    fireEvent.click(screen.getByRole("button", { name: "Reveal" }));

    // Assert
    await waitFor(() => expect(screen.getByText("intimate")).toBeTruthy());
  });

  it("memory_shows_all_layers_and_their_changes_on_one_page", async () => {
    // Arrange
    stubApi({
      "/api/agents/lyre/memory": {
        memory: { episodic: "", semantic: "# Facts\n- tea", prospective: "" },
      },
      "/api/agents/lyre/memory/history": { changes: [CHANGE] },
    });

    // Act
    const props = renderPanel("memory");

    // Assert: every layer is a card, rendered as Markdown
    await waitFor(() => expect(screen.getByRole("heading", { name: "Facts" })).toBeTruthy());
    const cards = screen.getAllByRole("article");
    expect(cards.map((card) => card.querySelector(".doc-card-path")?.textContent)).toEqual([
      "episodic.md",
      "semantic.md",
      "prospective.md",
    ]);
    expect(cards[1].textContent).toContain("updated");
    expect(screen.queryByRole("tab")).toBeNull();

    // Assert: the newest change is open with its diff
    await waitFor(() => expect(document.querySelector(".diff-container")).not.toBeNull());
    expect(document.querySelector(".memory-change")!.textContent).toContain("+1 −1");
    fireEvent.click(screen.getByRole("button", { name: /Sleep run #run-ab/ }));
    expect(props.onOpenRun).toHaveBeenCalledWith("run-abcdef");
  });

  it("memory_folds_long_files", async () => {
    // Arrange
    const long = Array.from({ length: 40 }, (_, i) => `- fact ${i}`).join("\n");
    stubApi({
      "/api/agents/lyre/memory": { memory: { episodic: long, semantic: "", prospective: "" } },
      "/api/agents/lyre/memory/history": { changes: [] },
    });

    // Act
    renderPanel("memory");
    await waitFor(() => expect(screen.getByRole("button", { name: "Show all" })).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Show all" }));

    // Assert
    expect(document.querySelector(".doc-card-content.folded")).toBeNull();
    expect(screen.getByRole("button", { name: "Show less" })).toBeTruthy();
  });

  it("skills_show_frontmatter_and_bundled_files", async () => {
    // Arrange
    const fetchMock = stubApi({
      "/api/skills": {
        skills: [
          { name: "egopulse", description: "Reference", source: "builtin" },
          { name: "notes", description: "Take notes", source: "workspace" },
        ],
      },
      "/api/skills/egopulse": {
        skill: {
          name: "egopulse",
          description: "Reference",
          source: "builtin",
          path: "skills/egopulse",
          platforms: [],
          deps: [],
          required_env: [],
          instructions: "Read the references.",
          files: [],
        },
      },
      "/api/skills/notes": {
        skill: {
          name: "notes",
          description: "Take notes",
          source: "workspace",
          path: "workspace/skills/notes",
          platforms: ["linux"],
          deps: ["git"],
          required_env: ["NOTES_TOKEN"],
          instructions: "Write things down.",
          files: [{ path: "references/style.md", bytes: 7 }],
        },
      },
      "/api/skills/notes/files/references/style.md": { content: "# Style guide" },
    });

    // Act: the first skill opens without a click
    renderPanel("skills");

    // Assert
    await waitFor(() => expect(screen.getByText("Read the references.")).toBeTruthy());

    // Act
    fireEvent.click(screen.getByRole("button", { name: /Take notes/ }));

    // Assert: frontmatter and SKILL.md
    await waitFor(() => expect(screen.getByText("Write things down.")).toBeTruthy());
    const detail = screen.getByRole("region", { name: "notes" });
    expect(detail.textContent).toContain("workspace/skills/notes");
    expect(detail.textContent).toContain("NOTES_TOKEN");
    expect(detail.textContent).toContain("git");

    // Act
    fireEvent.click(screen.getByRole("tab", { name: "references/style.md" }));

    // Assert: bundled files are fetched on demand
    await waitFor(() => expect(screen.getByRole("heading", { name: "Style guide" })).toBeTruthy());
    expect(requestedPaths(fetchMock)).toContain("/api/skills/notes/files/references/style.md");
  });
});
