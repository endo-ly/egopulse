type AuthModalProps = {
  authDraft: string;
  setAuthDraft: (value: string) => void;
  onSubmit: () => void;
};

export function AuthModal({ authDraft, setAuthDraft, onSubmit }: AuthModalProps) {
  return (
    <div
      className="fixed inset-0 grid place-items-center bg-[rgba(3,5,12,0.75)] p-5"
      role="presentation"
    >
      <div
        className="w-full max-w-3xl max-h-[90vh] flex flex-col border border-border rounded-[28px] bg-gradient-to-b from-[rgba(10,16,30,0.98)] to-[rgba(6,10,20,0.98)] shadow-[0_28px_60px_rgba(0,0,0,0.3)]"
        role="dialog"
        aria-modal="true"
        aria-labelledby="auth-modal-title"
      >
        <div className="flex justify-between gap-3 px-6 pt-6 pb-2 shrink-0">
          <div>
            <h3 id="auth-modal-title" className="m-0 text-lg">
              Web Access Token
            </h3>
            <p className="mt-1 text-sm text-muted">
              Enter channels.web.auth_token to access EgoPulse APIs.
            </p>
          </div>
        </div>

        <form className="config-form" onSubmit={onSubmit}>
          <label>
            <span>Auth Token</span>
            <input
              type="password"
              value={authDraft}
              autoFocus
              onChange={(event) => setAuthDraft(event.target.value)}
            />
          </label>
          <div className="config-footer">
            <span>Stored locally in this browser only.</span>
            <button className="primary-button" type="submit">
              Unlock
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
