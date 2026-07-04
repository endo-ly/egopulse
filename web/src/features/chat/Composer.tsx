import { useState, useRef, useEffect, type KeyboardEvent } from "react";
import { loadDraft, saveDraft } from "./draftStorage";
import { matchSlashCommands } from "./slashCommands";

export interface ComposerProps {
  onSubmit: (text: string) => void;
  disabled?: boolean;
  storageKey?: string;
}

export function Composer({ onSubmit, disabled, storageKey }: ComposerProps) {
  const [text, setText] = useState(() => loadDraft(storageKey));
  const [suggestIndex, setSuggestIndex] = useState(-1);
  const [suggestHidden, setSuggestHidden] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const matches = matchSlashCommands(text);
  const showSuggestPopup =
    text.startsWith("/") && matches.length > 0 && !suggestHidden;

  useEffect(() => {
    setText(loadDraft(storageKey));
  }, [storageKey]);

  useEffect(() => {
    saveDraft(storageKey, text);
  }, [text, storageKey]);

  const submit = () => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSubmit(trimmed);
    setText("");
    setSuggestIndex(-1);
    setSuggestHidden(false);
  };

  const acceptSuggestion = (index: number) => {
    setText(matches[index].name + " ");
    setSuggestIndex(-1);
    setSuggestHidden(false);
    textareaRef.current?.focus();
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (showSuggestPopup) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSuggestIndex((i) => (i + 1) % matches.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSuggestIndex((i) => (i <= 0 ? matches.length - 1 : i - 1));
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && suggestIndex >= 0)) {
        e.preventDefault();
        acceptSuggestion(suggestIndex >= 0 ? suggestIndex : 0);
        return;
      }
      // Escape dismisses the suggestion popup but keeps the typed text.
      if (e.key === "Escape") {
        e.preventDefault();
        setSuggestIndex(-1);
        setSuggestHidden(true);
        return;
      }
    }

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <div className="composer-input-wrapper">
      {showSuggestPopup && (
        <ul className="command-suggest" role="listbox">
          {matches.map((cmd, i) => (
            <li
              key={cmd.name}
              className={`suggest-item ${i === suggestIndex ? "selected" : ""}`}
              role="option"
              aria-selected={i === suggestIndex}
              onClick={() => acceptSuggestion(i)}
            >
              <span className="suggest-name">{cmd.name}</span>
              <span className="suggest-desc">{cmd.description}</span>
            </li>
          ))}
        </ul>
      )}
      <textarea
        ref={textareaRef}
        className="composer-textarea"
        value={text}
        placeholder="Type a message…"
        disabled={disabled}
        onChange={(e) => {
          setText(e.target.value);
          setSuggestIndex(-1);
          setSuggestHidden(false);
        }}
        onKeyDown={handleKeyDown}
        rows={1}
      />
      <button
        type="button"
        className="btn-primary composer-send"
        disabled={disabled || !text.trim()}
        onClick={submit}
      >
        Send
      </button>
    </div>
  );
}
