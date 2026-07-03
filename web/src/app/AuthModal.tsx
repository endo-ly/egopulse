import { Button } from "../shared/ui/Button";
import { Modal } from "../shared/ui/Modal";

export interface AuthModalProps {
  message: string;
  token: string;
  onTokenChange: (value: string) => void;
  onSubmit: () => void;
}

export function AuthModal({
  message,
  token,
  onTokenChange,
  onSubmit,
}: AuthModalProps) {
  return (
    <Modal open={true} onClose={() => undefined} labelledBy="auth-modal-title">
      <form
        className="auth-form"
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit();
        }}
      >
        <div className="modal-header">
          <h2 id="auth-modal-title">Web Access Token</h2>
          <p>{message}</p>
        </div>
        <label className="auth-token-field">
          <span>Auth Token</span>
          <input
            type="password"
            value={token}
            autoFocus
            onChange={(event) => onTokenChange(event.target.value)}
          />
        </label>
        <div className="modal-footer">
          <span>Stored locally in this browser only.</span>
          <Button variant="primary" type="submit">
            Unlock
          </Button>
        </div>
      </form>
    </Modal>
  );
}
