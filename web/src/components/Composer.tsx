import { useState, useRef, useEffect, type KeyboardEvent } from "react";

export interface ComposerProps {
  onSubmit: (text: string) => void;
  disabled?: boolean;
  storageKey?: string;
}

const SLASH_COMMANDS = [
  { name: "/reset", description: "Clear conversation history" },
  { name: "/compact", description: "Compact messages" },
  { name: "/sleep", description: "Run sleep batch" },
  { name: "/help", description: "Show available commands" },
];

export function Composer({ onSubmit, disabled, storageKey }: ComposerProps) {
  const [text, setText] = useState(() => {
    if (!storageKey) return "";
    try {
      return localStorage.getItem(`egopulse.draft.${storageKey}`) ?? "";
    } catch {
      return "";
    }
  });
  const [suggestIndex, setSuggestIndex] = useState(-1);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const showSuggest = text.startsWith("/") && text.length > 0;
  const matches = showSuggest
    ? SLASH_COMMANDS.filter((c) => c.name.startsWith(text))
    : [];

  useEffect(() => {
    if (!storageKey) {
      setText("");
      return;
    }
    try {
      setText(localStorage.getItem(`egopulse.draft.${storageKey}`) ?? "");
    } catch {
      setText("");
    }
  }, [storageKey]);

  useEffect(() => {
    if (!storageKey) return;
    try {
      if (text) {
        localStorage.setItem(`egopulse.draft.${storageKey}`, text);
      } else {
        localStorage.removeItem(`egopulse.draft.${storageKey}`);
      }
    } catch {
      return;
    }
  }, [text, storageKey]);

  const submit = () => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSubmit(trimmed);
    setText("");
    setSuggestIndex(-1);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (showSuggest && matches.length > 0) {
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
        setText(matches[suggestIndex >= 0 ? suggestIndex : 0].name + " ");
        setSuggestIndex(-1);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setSuggestIndex(-1);
        setText("");
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
      {showSuggest && matches.length > 0 && (
        <ul className="command-suggest" role="listbox">
          {matches.map((cmd, i) => (
            <li
              key={cmd.name}
              className={`suggest-item ${i === suggestIndex ? "selected" : ""}`}
              role="option"
              aria-selected={i === suggestIndex}
              onClick={() => {
                setText(cmd.name + " ");
                setSuggestIndex(-1);
                textareaRef.current?.focus();
              }}
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
        placeholder="Type a message…  (/ for commands)"
        disabled={disabled}
        onChange={(e) => {
          setText(e.target.value);
          setSuggestIndex(-1);
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
