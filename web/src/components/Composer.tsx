import { FormEvent } from "react";

type ComposerProps = {
  draft: string;
  setDraft: (value: string) => void;
  onSubmit: (event: FormEvent) => void;
};

export function Composer({ draft, setDraft, onSubmit }: ComposerProps) {
  return (
    <form className="composer" onSubmit={onSubmit}>
      <textarea
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            onSubmit(event as unknown as FormEvent);
          }
        }}
        placeholder="Type a message"
        rows={3}
      />
      <button className="primary-button" type="submit">
        Send
      </button>
    </form>
  );
}
