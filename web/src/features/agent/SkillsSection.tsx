import { useState } from "react";

import { fetchSkill, fetchSkillFile, fetchSkills } from "../../shared/api/agents";
import type { SkillDetail } from "../../shared/api/types";
import { EmptyState } from "../../shared/ui/EmptyState";
import { useAgentQuery } from "./agentQuery";
import { DocumentCard } from "./DocumentCard";
import { QueryState } from "../../shared/ui/QueryState";

const BOOK_ICON = (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
    <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20V3H6.5A2.5 2.5 0 0 0 4 5.5v14Z" strokeLinejoin="round" />
    <path d="M4 19.5A2.5 2.5 0 0 0 6.5 22H20v-5" strokeLinejoin="round" />
  </svg>
);

const SKILL_MD = "SKILL.md";

/** Readable notes for files the API declines to preview. */
const UNPREVIEWABLE: Record<string, string> = {
  skill_file_not_text: "Binary file — no text preview.",
  skill_file_too_large: "File is too large to preview.",
  skill_file_not_found: "File no longer exists.",
};

/** Skills available to every agent, with each skill's frontmatter and files. */
export function SkillsSection({ authToken }: { authToken: string }) {
  const [selected, setSelected] = useState<string | null>(null);
  const skillsState = useAgentQuery(authToken, ["skills"], () => fetchSkills(authToken));

  return (
    <QueryState state={skillsState}>
      {(skills) => {
        if (skills.length === 0) {
          return (
            <EmptyState
              icon={BOOK_ICON}
              title="No skills available"
              description="Skills placed in skills/ or workspace/skills/ appear here."
            />
          );
        }
        // A selection that vanished on reload falls back to the first skill.
        const active =
          selected !== null && skills.some((skill) => skill.name === selected)
            ? selected
            : skills[0].name;
        return (
          <div className="skills-layout">
            <ul className="skills-list" aria-label="Skills">
              {skills.map((skill) => (
                <li key={skill.name}>
                  <button
                    type="button"
                    className={`skill-row ${skill.name === active ? "active" : ""}`}
                    aria-current={skill.name === active ? "true" : undefined}
                    onClick={() => setSelected(skill.name)}
                  >
                    <span className="skill-row-name">
                      {skill.name}
                      <span className="skill-row-source">{skill.source}</span>
                    </span>
                    <span className="skill-row-description">{skill.description}</span>
                  </button>
                </li>
              ))}
            </ul>
            <SkillDetailView key={active} name={active} authToken={authToken} />
          </div>
        );
      }}
    </QueryState>
  );
}

function SkillDetailView({ name, authToken }: { name: string; authToken: string }) {
  const skillState = useAgentQuery(authToken, ["skill", name], () =>
    fetchSkill(name, authToken),
  );

  return (
    <section className="skill-detail" aria-label={name}>
      <QueryState state={skillState}>{(skill) => <SkillBody skill={skill} authToken={authToken} />}</QueryState>
    </section>
  );
}

function SkillBody({ skill, authToken }: { skill: SkillDetail; authToken: string }) {
  const [file, setFile] = useState(SKILL_MD);
  const frontmatter: [string, string[]][] = [
    ["Platforms", skill.platforms],
    ["Dependencies", skill.deps],
    ["Required env", skill.required_env],
  ];

  return (
    <>
      <dl className="agent-kv skill-meta">
        <div className="agent-kv-row">
          <dt>Location</dt>
          <dd>
            <code>{skill.path}</code>
          </dd>
        </div>
        <div className="agent-kv-row">
          <dt>Source</dt>
          <dd>{skill.source}</dd>
        </div>
        {frontmatter
          .filter(([, values]) => values.length > 0)
          .map(([label, values]) => (
            <div key={label} className="agent-kv-row">
              <dt>{label}</dt>
              <dd className="skill-meta-values">
                {values.map((value) => (
                  <code key={value}>{value}</code>
                ))}
              </dd>
            </div>
          ))}
      </dl>

      {skill.files.length > 0 && (
        <div className="skill-files" role="tablist" aria-label="Skill files">
          {[SKILL_MD, ...skill.files.map((entry) => entry.path)].map((path) => (
            <button
              key={path}
              type="button"
              role="tab"
              aria-selected={path === file}
              className={`skill-file ${path === file ? "active" : ""}`}
              onClick={() => setFile(path)}
            >
              {path}
            </button>
          ))}
        </div>
      )}

      {file === SKILL_MD ? (
        <DocumentCard path={`${skill.path}/${SKILL_MD}`} content={skill.instructions} />
      ) : (
        <SkillFileView key={file} skill={skill} path={file} authToken={authToken} />
      )}
    </>
  );
}

function SkillFileView({
  skill,
  path,
  authToken,
}: {
  skill: SkillDetail;
  path: string;
  authToken: string;
}) {
  const fileState = useAgentQuery(authToken, ["skill-file", skill.name, path], () =>
    fetchSkillFile(skill.name, path, authToken),
  );

  const note = fileState.error ? UNPREVIEWABLE[fileState.error.message] : undefined;
  if (note) {
    return (
      <article className="doc-card" data-tone="inactive">
        <header className="doc-card-header">
          <code className="doc-card-path">{`${skill.path}/${path}`}</code>
        </header>
        <p className="doc-card-empty">{note}</p>
      </article>
    );
  }

  return (
    <QueryState state={fileState}>
      {(content) => (
        <DocumentCard
          path={`${skill.path}/${path}`}
          content={content}
          markdown={path.endsWith(".md")}
        />
      )}
    </QueryState>
  );
}
