import { describe, it, expect, vi, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invalidateQuery } from "../../../shared/hooks/useServerState";
import { agentQueryKey } from "../agentQuery";
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

type PutResult = { status: number; body: unknown };

/** Stubs fetch so each API path returns its own JSON body. */
function stubApi(routes: Routes, onPut?: (path: string, body: unknown) => PutResult) {
  const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
    const path = new URL(String(input), "http://localhost").pathname;
    if (init?.method === "PUT" && onPut) {
      const body = init.body ? JSON.parse(String(init.body)) : {};
      const result = onPut(path, body);
      return Promise.resolve({
        ok: result.status < 400,
        status: result.status,
        json: () => Promise.resolve(result.body),
      });
    }
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

/** Stable stand-in for the server hash; only equality matters in the UI. */
function promptHash(content: string): string {
  return `hash:${content}`;
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
    onOpenSleepRun: vi.fn(),
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
      changed_memory: [],
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
    expect(props.onOpenSleepRun).toHaveBeenCalledWith("run-sleep1");
  });

  it("soul_marks_the_applied_layer_and_the_fallback", async () => {
    // Arrange
    const layers: PromptFileLayer[] = [
      {
        scope: "agent",
        path: "agents/lyre/SOUL.md",
        content: null,
        applied: false,
        exists: false,
        hash: promptHash(""),
      },
      {
        scope: "global",
        path: "SOUL.md",
        content: "Global soul",
        applied: true,
        exists: true,
        hash: promptHash("Global soul"),
      },
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
          {
            scope: "agent",
            path: "agents/lyre/SECRET.md",
            content: "intimate",
            applied: true,
            exists: true,
            hash: promptHash("intimate"),
          },
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
    expect(screen.getByRole("button", { name: "Edit" })).toBeTruthy();
  });

  it("distinguishes_empty_files_from_missing_layers", async () => {
    // Arrange
    const layers: PromptFileLayer[] = [
      {
        scope: "global",
        path: "AGENTS.md",
        content: null,
        applied: false,
        exists: true,
        hash: "empty-hash",
      },
      {
        scope: "agent",
        path: "agents/lyre/AGENTS.md",
        content: null,
        applied: false,
        exists: false,
        hash: "empty-hash",
      },
    ];
    stubApi({ "/api/agents/lyre/files/rules": { layers } });

    // Act
    renderPanel("rules");

    // Assert
    await waitFor(() => expect(screen.getAllByRole("article")).toHaveLength(2));
    const cards = screen.getAllByRole("article");
    expect(cards[0].textContent).toContain("Empty");
    expect(cards[1].textContent).toContain("Not created");
  });

  it("prompt_file_edit_saves_content_with_expected_hash", async () => {
    // Arrange
    const before: PromptFileLayer[] = [
      {
        scope: "agent",
        path: "agents/lyre/SOUL.md",
        content: "old soul",
        applied: true,
        exists: true,
        hash: "hash-old",
      },
      {
        scope: "global",
        path: "SOUL.md",
        content: "global soul",
        applied: false,
        exists: true,
        hash: "hash-global",
      },
    ];
    const after: PromptFileLayer[] = [
      { ...before[0], content: "new soul", hash: "hash-new" },
      before[1],
    ];
    const put = vi
      .fn()
      .mockReturnValue({ status: 200, body: { ok: true, layers: after } });
    stubApi({ "/api/agents/lyre/files/soul": { layers: before } }, put);

    // Act
    renderPanel("soul");
    await waitFor(() => expect(screen.getByText("old soul")).toBeTruthy());
    fireEvent.click(screen.getAllByRole("button", { name: "Edit" })[0]);
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "new soul" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    // Assert
    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put).toHaveBeenCalledWith("/api/agents/lyre/files/soul", {
      scope: "agent",
      content: "new soul",
      expected_hash: "hash-old",
    });
    await waitFor(() => expect(screen.getByText("new soul")).toBeTruthy());
    expect(screen.queryByLabelText("Content")).toBeNull();
  });

  it("prompt_file_edit_keeps_the_starting_hash_when_the_layer_refetches", async () => {
    // Arrange
    const routes: Routes = {
      "/api/agents/lyre/files/soul": {
        layers: [
          {
            scope: "agent",
            path: "agents/lyre/SOUL.md",
            content: "old soul",
            applied: true,
            exists: true,
            hash: "hash-old",
          },
        ],
      },
    };
    const put = vi.fn().mockReturnValue({
      status: 200,
      body: {
        ok: true,
        layers: [
          {
            scope: "agent",
            path: "agents/lyre/SOUL.md",
            content: "my draft",
            applied: true,
            exists: true,
            hash: "hash-my-draft",
          },
        ],
      },
    });
    const fetchMock = stubApi(routes, put);
    const props = renderPanel("soul");
    await waitFor(() => expect(screen.getByText("old soul")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "my draft" } });

    // Act: a background invalidation replaces the layer while the editor is open.
    routes["/api/agents/lyre/files/soul"] = {
      layers: [
        {
          scope: "agent",
          path: "agents/lyre/SOUL.md",
          content: "remote soul",
          applied: true,
          exists: true,
          hash: "hash-remote",
        },
      ],
    };
    await act(async () => {
      invalidateQuery(agentQueryKey(props.authToken, ["file", "lyre", "soul"]));
    });
    await waitFor(() =>
      expect(
        requestedPaths(fetchMock).filter((path) => path === "/api/agents/lyre/files/soul"),
      ).toHaveLength(2),
    );

    // Act: saving must still use the hash captured when editing started.
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    // Assert
    await waitFor(() => expect(put).toHaveBeenCalledTimes(1));
    expect(put).toHaveBeenCalledWith("/api/agents/lyre/files/soul", {
      scope: "agent",
      content: "my draft",
      expected_hash: "hash-old",
    });
  });

  it("missing_layer_can_be_edited_and_global_layers_warn", async () => {
    // Arrange
    const layers: PromptFileLayer[] = [
      {
        scope: "agent",
        path: "agents/lyre/SOUL.md",
        content: null,
        applied: false,
        exists: false,
        hash: "empty-hash",
      },
      {
        scope: "global",
        path: "SOUL.md",
        content: "Global soul",
        applied: true,
        exists: true,
        hash: "global-hash",
      },
    ];
    stubApi({ "/api/agents/lyre/files/soul": { layers } });

    // Act
    renderPanel("soul");
    await waitFor(() => expect(screen.getByText("Global soul")).toBeTruthy());
    const cards = screen.getAllByRole("article");
    fireEvent.click(within(cards[0]).getByRole("button", { name: "Edit" }));

    // Assert: a missing layer opens an empty editor
    expect((screen.getByLabelText("Content") as HTMLTextAreaElement).value).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    // Act
    fireEvent.click(within(cards[1]).getByRole("button", { name: "Edit" }));

    // Assert: the shared scope is called out before editing
    await waitFor(() => expect(screen.getByText(/shared global file/i)).toBeTruthy());
  });

  it("prompt_file_editor_previews_markdown_and_guards_unsaved_edits", async () => {
    // Arrange
    const layers: PromptFileLayer[] = [
      {
        scope: "agent",
        path: "agents/lyre/SOUL.md",
        content: "old soul",
        applied: true,
        exists: true,
        hash: "hash-old",
      },
    ];
    stubApi({ "/api/agents/lyre/files/soul": { layers } });
    renderPanel("soul");
    await waitFor(() => expect(screen.getByText("old soul")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    // Act: edit and preview
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "**bold**" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    // Assert: Markdown is rendered from the draft
    await waitFor(() => expect(screen.getByText("bold").tagName).toBe("STRONG"));
    const changes = screen.getByRole("button", { name: "Changes" });
    expect(changes.getAttribute("aria-expanded")).toBe("false");
    expect(document.querySelector(".diff-container")).toBeNull();

    // Act: open the diff on demand
    fireEvent.click(changes);

    // Assert: the counts and the diff are computed once opened
    await waitFor(() => expect(changes.textContent).toContain("+1"));
    expect(changes.textContent).toContain("−1");
    expect(changes.getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelector(".diff-container")).toBeTruthy();

    // Assert: the draft guards unload until discarded
    const unload = new Event("beforeunload", { cancelable: true });
    globalThis.dispatchEvent(unload);
    expect(unload.defaultPrevented).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    const afterCancel = new Event("beforeunload", { cancelable: true });
    globalThis.dispatchEvent(afterCancel);
    expect(afterCancel.defaultPrevented).toBe(false);
  });

  it("prompt_file_conflict_keeps_draft_and_reloads_latest", async () => {
    // Arrange
    const routes: Routes = {
      "/api/agents/lyre/files/soul": {
        layers: [
          {
            scope: "agent",
            path: "agents/lyre/SOUL.md",
            content: "old soul",
            applied: true,
            exists: true,
            hash: "hash-old",
          },
        ],
      },
    };
    stubApi(routes, () => ({
      status: 409,
      body: { ok: false, error: "prompt_file_conflict" },
    }));
    vi.stubGlobal("confirm", vi.fn(() => true));
    renderPanel("soul");
    await waitFor(() => expect(screen.getByText("old soul")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText("Content"), { target: { value: "my draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    // Assert: the draft survives the conflict
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("changed elsewhere"));
    expect((screen.getByLabelText("Content") as HTMLTextAreaElement).value).toBe("my draft");

    // Act: the remote content is reloaded
    routes["/api/agents/lyre/files/soul"] = {
      layers: [
        {
          scope: "agent",
          path: "agents/lyre/SOUL.md",
          content: "remote soul",
          applied: true,
          exists: true,
          hash: "hash-remote",
        },
      ],
    };
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));

    // Assert
    await waitFor(() => expect(screen.getByText("remote soul")).toBeTruthy());
    expect(screen.queryByLabelText("Content")).toBeNull();
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
    expect(props.onOpenSleepRun).toHaveBeenCalledWith("run-abcdef");
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
