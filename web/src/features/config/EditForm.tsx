import { useEffect, useId, useMemo, useRef, useState, type FormEvent } from "react";

import { ConfigConflictError } from "../../shared/api/config";
import { setUnsavedChanges } from "../../shared/lib/unsaved";
import { Button } from "../../shared/ui/Button";

type SecretChange = { action: "keep" } | { action: "replace"; value: string } | { action: "remove" };
type FieldValue = string | number | boolean | string[] | null | SecretChange;
export type FormValues = Record<string, FieldValue>;

export interface FieldOption {
  value: string;
  label: string;
}

export interface FieldSpec {
  key: string;
  label: string;
  kind: "text" | "number" | "select" | "toggle" | "list" | "secret" | "choices";
  /** Choices for `select` / `choices`, or suggestions for `text`. */
  options?: (draft: FormValues) => FieldOption[];
  /** Empty input means "not set" (null) and the value is inherited. */
  nullable?: boolean;
  /** Shown in an empty nullable field: what applies when unset. */
  placeholder?: (draft: FormValues) => string;
  hint?: string;
  step?: number;
  min?: number;
  max?: number;
  /** Shown instead of the input when the field cannot be changed here. */
  readOnlyReason?: string;
  /** Shown when a secret can be replaced but not removed. */
  removeDisabledReason?: string;
}

export interface EditFormProps {
  /** Unique per edited target, for the unsaved-changes guard. */
  formId: string;
  specs: FieldSpec[];
  /** The latest values from the server; they move while the form is open. */
  values: FormValues;
  onSave: (changes: FormValues) => Promise<void>;
  onCancel: () => void;
  /** Reloads the config after a conflict. */
  onReload: () => void;
}

function equal(a: FieldValue, b: FieldValue): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function isChanged(spec: FieldSpec, draft: FieldValue, base: FieldValue): boolean {
  if (spec.kind === "secret") return (draft as SecretChange).action !== "keep";
  return !equal(draft, base);
}

/** How a value reads in the change list. */
function displayValue(spec: FieldSpec, value: FieldValue, draft: FormValues): string {
  if (spec.kind === "secret") {
    const change = value as SecretChange;
    return change.action === "remove" ? "Removed" : change.action === "replace" ? "New key" : "Unchanged";
  }
  if (value === null || value === "") return spec.placeholder ? `Not set (${spec.placeholder(draft)})` : "Not set";
  if (typeof value === "boolean") return value ? "On" : "Off";
  if (Array.isArray(value)) return value.length > 0 ? value.join(", ") : "None";
  const option = spec.options?.(draft).find((o) => o.value === String(value));
  return option && spec.kind !== "text" ? option.label : String(value);
}

function validate(spec: FieldSpec, value: FieldValue): string | null {
  if (spec.kind === "number") {
    if (typeof value !== "number" || Number.isNaN(value)) return "Enter a number";
    if (spec.min !== undefined && value < spec.min) return `At least ${spec.min}`;
    if (spec.max !== undefined && value > spec.max) return `At most ${spec.max}`;
  }
  if (spec.kind === "text" && !spec.nullable && (value === null || String(value).trim() === "")) return "Required";
  if (spec.kind === "secret") {
    const change = value as SecretChange;
    if (change.action === "replace" && change.value.trim() === "") return "Enter the new key";
  }
  return null;
}

/**
 * An inline edit form that only ever sends what the user changed.
 *
 * It compares three states: the values the edit started from, the draft, and
 * the latest server values. When the server moves on while the form is open,
 * untouched fields follow it; a field the user also changed is flagged so the
 * save knowingly overwrites the newer value.
 */
export function EditForm({ formId, specs, values, onSave, onCancel, onReload }: EditFormProps) {
  const [base, setBase] = useState<FormValues>(values);
  const [draft, setDraft] = useState<FormValues>(values);
  const [conflicts, setConflicts] = useState<Record<string, FieldValue>>({});
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idPrefix = useId();
  const latestSeen = useRef(values);

  useEffect(() => {
    if (JSON.stringify(latestSeen.current) === JSON.stringify(values)) return;
    latestSeen.current = values;
    const nextDraft = { ...draft };
    const nextConflicts = { ...conflicts };
    for (const spec of specs) {
      const key = spec.key;
      if (!isChanged(spec, draft[key], base[key])) {
        nextDraft[key] = values[key];
      } else if (!equal(values[key], base[key])) {
        nextConflicts[key] = values[key];
      }
    }
    setDraft(nextDraft);
    setBase(values);
    setConflicts(nextConflicts);
    // Only a server update rebases the form; draft edits must not.
  }, [values]);

  const changed = useMemo(
    () => specs.filter((spec) => !spec.readOnlyReason && isChanged(spec, draft[spec.key], base[spec.key])),
    [specs, draft, base],
  );
  const errors = useMemo(() => {
    const result: Record<string, string> = {};
    for (const spec of changed) {
      const message = validate(spec, draft[spec.key]);
      if (message) result[spec.key] = message;
    }
    return result;
  }, [changed, draft]);

  useEffect(() => {
    setUnsavedChanges(formId, changed.length > 0);
    return () => setUnsavedChanges(formId, false);
  }, [formId, changed.length]);

  const update = (key: string, value: FieldValue) => {
    setDraft((prev) => ({ ...prev, [key]: value }));
    setError(null);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (changed.length === 0 || Object.keys(errors).length > 0) return;
    const payload: FormValues = {};
    for (const spec of changed) payload[spec.key] = draft[spec.key];
    setSaving(true);
    setError(null);
    try {
      await onSave(payload);
      setUnsavedChanges(formId, false);
    } catch (caught) {
      if (caught instanceof ConfigConflictError) {
        setError("The config changed elsewhere. The form now shows the latest values; review your changes and save again.");
        onReload();
      } else {
        setError(caught instanceof Error ? caught.message : String(caught));
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <form className="config-form" onSubmit={submit}>
      {error && (
        <div className="run-error" role="alert">
          {error}
        </div>
      )}
      <div className="config-form-fields">
        {specs.map((spec) => (
          <Field
            key={spec.key}
            id={`${idPrefix}-${spec.key}`}
            spec={spec}
            value={draft[spec.key]}
            draft={draft}
            error={errors[spec.key]}
            onChange={(value) => update(spec.key, value)}
          />
        ))}
      </div>
      <div className="config-form-footer">
        {changed.length > 0 ? (
          <div className="change-list" aria-live="polite">
            <h4>{changed.length === 1 ? "1 change" : `${changed.length} changes`}</h4>
            <ul>
              {changed.map((spec) => (
                <li key={spec.key}>
                  <span className="change-field">{spec.label}</span>
                  <span className="change-from">{displayValue(spec, base[spec.key], base)}</span>
                  <span className="change-arrow" aria-label="to">→</span>
                  <span className="change-to">{displayValue(spec, draft[spec.key], draft)}</span>
                  {spec.key in conflicts && (
                    <span className="change-conflict">Also changed elsewhere; saving replaces it</span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        ) : (
          <p className="change-none">No changes yet</p>
        )}
        <div className="config-form-actions">
          <Button variant="secondary" type="button" onClick={onCancel} disabled={saving}>
            Cancel
          </Button>
          <Button
            variant="primary"
            type="submit"
            busy={saving}
            disabled={changed.length === 0 || Object.keys(errors).length > 0}
          >
            Save
          </Button>
        </div>
      </div>
    </form>
  );
}

function Field({
  id,
  spec,
  value,
  draft,
  error,
  onChange,
}: {
  id: string;
  spec: FieldSpec;
  value: FieldValue;
  draft: FormValues;
  error?: string;
  onChange: (value: FieldValue) => void;
}) {
  const hintId = `${id}-hint`;
  const describedBy = spec.hint || error ? hintId : undefined;
  const options = spec.options?.(draft) ?? [];

  let control;
  if (spec.readOnlyReason) {
    control = (
      <div className="config-readonly">
        <span>{displayValue(spec, value, draft)}</span>
        <span className="config-readonly-reason">{spec.readOnlyReason}</span>
      </div>
    );
  } else if (spec.kind === "toggle") {
    control = (
      <label className="toggle">
        <input id={id} type="checkbox" checked={Boolean(value)} onChange={(e) => onChange(e.target.checked)} />
        <span>{value ? "On" : "Off"}</span>
      </label>
    );
  } else if (spec.kind === "select") {
    control = (
      <select
        id={id}
        value={value === null ? "" : String(value)}
        aria-describedby={describedBy}
        onChange={(e) => onChange(e.target.value === "" && spec.nullable ? null : e.target.value)}
      >
        {spec.nullable && <option value="">{spec.placeholder ? `Not set (${spec.placeholder(draft)})` : "Not set"}</option>}
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    );
  } else if (spec.kind === "number") {
    control = (
      <input
        id={id}
        type="number"
        inputMode="decimal"
        value={typeof value === "number" && !Number.isNaN(value) ? value : ""}
        step={spec.step ?? 1}
        min={spec.min}
        max={spec.max}
        aria-describedby={describedBy}
        aria-invalid={error ? true : undefined}
        onChange={(e) => onChange(e.target.value === "" ? Number.NaN : Number(e.target.value))}
      />
    );
  } else if (spec.kind === "list") {
    control = <ListInput id={id} values={(value as string[]) ?? []} onChange={onChange} />;
  } else if (spec.kind === "choices") {
    const selected = value as string[] | null;
    control = (
      <div className="choice-group" role="group" aria-labelledby={`${id}-label`}>
        <label className="choice">
          <input type="radio" name={id} checked={selected === null} onChange={() => onChange(null)} />
          All agents
        </label>
        <label className="choice">
          <input type="radio" name={id} checked={selected !== null} onChange={() => onChange(selected ?? [])} />
          Only these
        </label>
        {selected !== null && (
          <div className="choice-chips">
            {options.map((option) => {
              const on = selected.includes(option.value);
              return (
                <button
                  key={option.value}
                  type="button"
                  className={`chip ${on ? "on" : ""}`}
                  aria-pressed={on}
                  onClick={() =>
                    onChange(on ? selected.filter((v) => v !== option.value) : [...selected, option.value])
                  }
                >
                  {option.label}
                </button>
              );
            })}
          </div>
        )}
      </div>
    );
  } else if (spec.kind === "secret") {
    control = (
      <SecretInput
        id={id}
        value={value as SecretChange}
        onChange={onChange}
        removeDisabledReason={spec.removeDisabledReason}
      />
    );
  } else {
    const listId = options.length > 0 ? `${id}-options` : undefined;
    control = (
      <>
        <input
          id={id}
          type="text"
          value={value === null ? "" : String(value)}
          placeholder={spec.placeholder?.(draft)}
          list={listId}
          aria-describedby={describedBy}
          aria-invalid={error ? true : undefined}
          onChange={(e) => onChange(e.target.value === "" && spec.nullable ? null : e.target.value)}
        />
        {listId && (
          <datalist id={listId}>
            {options.map((option) => (
              <option key={option.value} value={option.value} />
            ))}
          </datalist>
        )}
      </>
    );
  }

  return (
    <div className={`config-field ${error ? "has-error" : ""}`}>
      <label id={`${id}-label`} htmlFor={id} className="config-field-label">
        {spec.label}
      </label>
      <div className="config-field-control">
        {control}
        {(error || spec.hint) && (
          <span id={hintId} className={error ? "config-field-error" : "config-field-hint"}>
            {error ?? spec.hint}
          </span>
        )}
      </div>
    </div>
  );
}

function ListInput({ id, values, onChange }: { id: string; values: string[]; onChange: (value: string[]) => void }) {
  const [entry, setEntry] = useState("");
  const add = () => {
    const name = entry.trim();
    if (name && !values.includes(name)) onChange([...values, name]);
    setEntry("");
  };
  return (
    <div className="list-input">
      <ul>
        {values.map((value) => (
          <li key={value} className="chip on">
            {value}
            <button type="button" aria-label={`Remove ${value}`} onClick={() => onChange(values.filter((v) => v !== value))}>
              ×
            </button>
          </li>
        ))}
      </ul>
      <div className="list-input-add">
        <input
          id={id}
          type="text"
          value={entry}
          placeholder="Add a model"
          onChange={(e) => setEntry(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
        />
        <Button variant="secondary" type="button" onClick={add} disabled={entry.trim() === ""}>
          Add
        </Button>
      </div>
    </div>
  );
}

function SecretInput({
  id,
  value,
  onChange,
  removeDisabledReason,
}: {
  id: string;
  value: SecretChange;
  onChange: (value: SecretChange) => void;
  removeDisabledReason?: string;
}) {
  if (value.action === "replace") {
    return (
      <div className="secret-input">
        <input
          id={id}
          type="password"
          autoComplete="off"
          value={value.value}
          placeholder="Paste the new key"
          onChange={(e) => onChange({ action: "replace", value: e.target.value })}
        />
        <Button variant="secondary" type="button" onClick={() => onChange({ action: "keep" })}>
          Keep current
        </Button>
      </div>
    );
  }
  return (
    <div className="secret-input">
      <span className="secret-state">{value.action === "remove" ? "Will be removed" : "Unchanged"}</span>
      <Button variant="secondary" type="button" onClick={() => onChange({ action: "replace", value: "" })}>
        Replace
      </Button>
      {value.action === "remove" ? (
        <Button variant="secondary" type="button" onClick={() => onChange({ action: "keep" })}>
          Undo
        </Button>
      ) : (
        <Button
          variant="danger"
          type="button"
          onClick={() => onChange({ action: "remove" })}
          disabled={Boolean(removeDisabledReason)}
          title={removeDisabledReason}
        >
          Remove
        </Button>
      )}
    </div>
  );
}
