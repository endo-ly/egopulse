export const AUTH_TOKEN_STORAGE_KEY = "egopulse.webAuthToken";

export class AuthRequiredError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AuthRequiredError";
  }
}

export function loadAuthToken(): string {
  try {
    return window.localStorage.getItem(AUTH_TOKEN_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

export function persistAuthToken(token: string): void {
  const trimmed = token.trim();
  try {
    if (trimmed) {
      window.localStorage.setItem(AUTH_TOKEN_STORAGE_KEY, trimmed);
    } else {
      window.localStorage.removeItem(AUTH_TOKEN_STORAGE_KEY);
    }
  } catch {
    return;
  }
}
