import type { SlashCommand } from "../commands";

type CommandSuggestProps = {
  commands: SlashCommand[];
  activeIndex: number;
  onSelect: (command: SlashCommand) => void;
};

export function CommandSuggest({
  commands,
  activeIndex,
  onSelect,
}: CommandSuggestProps) {
  if (commands.length === 0) return null;

  return (
    <ul className="command-suggest" role="listbox">
      {commands.map((cmd, i) => (
        <li
          key={cmd.name}
          className={`command-suggest-item${i === activeIndex ? " active" : ""}`}
          role="option"
          aria-selected={i === activeIndex}
          onClick={() => onSelect(cmd)}
          onMouseDown={(e) => e.preventDefault()}
        >
          <span className="cmd-name">{cmd.usage}</span>
          <span className="cmd-desc">{cmd.description}</span>
        </li>
      ))}
    </ul>
  );
}
