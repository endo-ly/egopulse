import { useEffect, useRef, useState } from "react";

import {
  AuthRequiredError,
  loadAuthToken,
  persistAuthToken,
} from "../api";

type UseAuthResult = {
  authToken: string;
  authTokenRef: React.MutableRefObject<string>;
  showAuth: boolean;
  setShowAuth: React.Dispatch<React.SetStateAction<boolean>>;
  authDraft: string;
  setAuthDraft: React.Dispatch<React.SetStateAction<string>>;
  saveAuth: (onSuccess: () => Promise<void>) => Promise<void>;
  withAuthHandling: (action: () => Promise<void>) => Promise<void>;
};

export function useAuth(): UseAuthResult {
  const [authToken, setAuthToken] = useState(() => loadAuthToken());
  const [authDraft, setAuthDraft] = useState(() => loadAuthToken());
  const [showAuth, setShowAuth] = useState(false);
  const authTokenRef = useRef(authToken);

  useEffect(() => {
    authTokenRef.current = authToken;
  }, [authToken]);

  async function withAuthHandling(action: () => Promise<void>) {
    try {
      await action();
    } catch (error) {
      if (error instanceof AuthRequiredError) {
        setShowAuth(true);
        return;
      }
      throw error;
    }
  }

  async function saveAuth(onSuccess: () => Promise<void>) {
    persistAuthToken(authDraft);
    setAuthToken(authDraft.trim());
    setShowAuth(false);
    await onSuccess();
  }

  return {
    authToken,
    authTokenRef,
    showAuth,
    setShowAuth,
    authDraft,
    setAuthDraft,
    saveAuth,
    withAuthHandling,
  };
}
