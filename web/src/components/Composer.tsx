import { FormEvent, useCallback, useRef, useState } from "react";
import { SLASH_COMMANDS, filterCommands } from "../commands";
import { CommandSuggest } from "./CommandSuggest";

type ComposerProps = {
  draft: string;
  setDraft: (value: string) => void;
  onSubmit: (event: FormEvent) => void;
};

export function Composer({ draft, setDraft, onSubmit }: ComposerProps) {
  const [showSuggest, setShowSuggest] = useState(() => draft.startsWith("/") && !draft.startsWith("//"));
  const [activeIndex, setActiveIndex] = useState(0);
  const committedRef = useRef(false);

  const isSlash = draft.startsWith("/") && !draft.startsWith("//");

  const filtered = isSlash ? filterCommands(draft.slice(1)) : [];
  const visible = showSuggest && isSlash && filtered.length > 0;

  const handleChange = useCallback(
    (value: string) => {
      setDraft(value);

      if (value.startsWith("/") && !value.startsWith("//")) {
        setShowSuggest(true);
        setActiveIndex(0);
      } else {
        setShowSuggest(false);
      }
    },
    [setDraft],
  );

  const selectCommand = useCallback(
    (command: (typeof SLASH_COMMANDS)[number]) => {
      setDraft(command.usage + " ");
      setShowSuggest(false);
    },
    [setDraft],
  );

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (!visible) {
        // サジェスト非表示時の既存ショートカット
        if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
          event.preventDefault();
          onSubmit(event as unknown as FormEvent);
        }
        return;
      }

      committedRef.current = false;

      switch (event.key) {
        case "ArrowDown": {
          event.preventDefault();
          setActiveIndex((i) => (i + 1) % filtered.length);
          break;
        }
        case "ArrowUp": {
          event.preventDefault();
          setActiveIndex((i) => (i - 1 + filtered.length) % filtered.length);
          break;
        }
        case "Tab":
        case "Enter": {
          event.preventDefault();
          committedRef.current = true;
          selectCommand(filtered[activeIndex]);
          break;
        }
        case "Escape": {
          event.preventDefault();
          setShowSuggest(false);
          break;
        }
      }
    },
    [visible, filtered, activeIndex, selectCommand, onSubmit],
  );

  return (
    <form className="composer" onSubmit={onSubmit}>
      <div className="composer-wrapper">
        <textarea
          value={draft}
          onChange={(event) => handleChange(event.target.value)}
          onKeyDown={handleKeyDown}
          onBlur={() => {
            // Tab/Enter による選択直後は閉じない
            if (committedRef.current) {
              committedRef.current = false;
              return;
            }
            setShowSuggest(false);
          }}
          placeholder="Type a message"
          rows={3}
        />
        {visible && (
          <CommandSuggest
            commands={filtered}
            activeIndex={activeIndex}
            onSelect={selectCommand}
          />
        )}
      </div>
      <button className="primary-button" type="submit">
        Send
      </button>
    </form>
  );
}
