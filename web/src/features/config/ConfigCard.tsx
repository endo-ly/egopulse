import { useEffect, useRef, type ReactNode } from "react";

import type { RuntimeConfig, SecretStatus } from "../../shared/api/config";
import { Button } from "../../shared/ui/Button";
import { StatusDot } from "../../shared/ui/StatusDot";
import type { EditTarget } from "./configForms";
import { EditForm, type FormValues } from "./EditForm";
import type { McpStatusSummary } from "./mcpStatus";

/** State the section views share with the page shell. */
export interface ConfigSectionContext {
  config: RuntimeConfig;
  authToken: string;
  mcp: McpStatusSummary | null;
  editing: string | null;
  onEdit: (key: string | null) => void;
  onSave: (edit: EditTarget, changes: FormValues) => Promise<void>;
  onReload: () => void;
  onOpenSchedule: (agentId: string) => void;
}

export function Card({
  title,
  note,
  action,
  children,
}: {
  title: ReactNode;
  note?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="config-card">
      <header className="config-card-head">
        <h3>{title}</h3>
        {action}
      </header>
      {note && <p className="config-card-note">{note}</p>}
      {children}
    </section>
  );
}

export function EditableCard({
  cardKey,
  edit,
  title,
  ctx,
  extra,
  children,
}: {
  cardKey: string;
  edit: EditTarget | null;
  title: ReactNode;
  ctx: ConfigSectionContext;
  extra?: ReactNode;
  children: ReactNode;
}) {
  const isEditing = ctx.editing === cardKey;
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    if (isEditing) ref.current?.querySelector<HTMLElement>("input, select, button")?.focus();
  }, [isEditing]);

  return (
    <section ref={ref} className={`config-card ${isEditing ? "editing" : ""}`}>
      <header className="config-card-head">
        <h3>{title}</h3>
        {edit && !isEditing && (
          <Button
            variant="secondary"
            type="button"
            onClick={() => ctx.onEdit(cardKey)}
            disabled={ctx.editing !== null}
            title={ctx.editing !== null ? "Finish the other edit first" : undefined}
          >
            Edit
          </Button>
        )}
      </header>
      {isEditing && edit ? (
        <EditForm
          formId={cardKey}
          specs={edit.specs}
          values={edit.values}
          onSave={(changes) => ctx.onSave(edit, changes)}
          onCancel={() => ctx.onEdit(null)}
          onReload={ctx.onReload}
        />
      ) : (
        <>
          {children}
          {extra}
        </>
      )}
    </section>
  );
}

export function Rows({ children }: { children: ReactNode }) {
  return <dl className="config-rows">{children}</dl>;
}

export function Row({
  label,
  value,
  sub,
  env,
  isDefault,
}: {
  label: string;
  value: ReactNode;
  /** A second line: where an inherited value comes from, etc. */
  sub?: ReactNode;
  env?: string;
  isDefault?: boolean;
}) {
  return (
    <div className="config-row">
      <dt>{label}</dt>
      <dd>
        <span className="config-value">
          {value}
          {isDefault && <span className="value-tag">default</span>}
          {env && (
            <span className="value-tag env" title={`Set by the ${env} environment variable, which overrides the YAML file`}>
              env {env}
            </span>
          )}
        </span>
        {sub && <span className="config-sub">{sub}</span>}
      </dd>
    </div>
  );
}

export function OnOff({ on }: { on: boolean }) {
  return <span className={`on-off ${on ? "on" : "off"}`}>{on ? "On" : "Off"}</span>;
}

export function Secret({
  secret,
  optional = false,
  envOverride,
}: {
  secret: SecretStatus;
  optional?: boolean;
  /** Environment variable that replaces the YAML value at startup. */
  envOverride?: string;
}) {
  if (envOverride) {
    return (
      <span className="secret">
        <StatusDot tone="success" />
        Set <span className="secret-source">from the <code>{envOverride}</code> environment variable</span>
      </span>
    );
  }
  if (!secret.configured) {
    return (
      <span className={`secret ${optional ? "" : "missing"}`}>
        <StatusDot tone={optional ? "idle" : "warning"} />
        {optional ? "Not needed" : "Not set"}
      </span>
    );
  }
  const source =
    secret.kind === "env" ? (
      <>
        from <code>{secret.ref}</code>
      </>
    ) : secret.kind === "exec" ? (
      "from a command"
    ) : (
      "written in the YAML file"
    );
  return (
    <span className="secret">
      <StatusDot tone="success" />
      Set <span className="secret-source">{source}</span>
    </span>
  );
}

export function None({ children = "None" }: { children?: ReactNode }) {
  return <span className="muted">{children}</span>;
}

export function Chips({ items, empty = "None" }: { items: string[]; empty?: string }) {
  if (items.length === 0) return <None>{empty}</None>;
  return (
    <span className="chips">
      {items.map((item) => (
        <span key={item} className="chip">
          {item}
        </span>
      ))}
    </span>
  );
}

export function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
