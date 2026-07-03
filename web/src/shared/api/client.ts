import { AuthRequiredError } from "./auth";

export async function apiFetch<T>(
  path: string,
  authToken: string,
  options?: RequestInit,
): Promise<T> {
  const trimmedToken = authToken.trim();
  let response: Response;

  try {
    response = await fetch(path, {
      ...options,
      headers: {
        "Content-Type": "application/json",
        ...(trimmedToken ? { Authorization: `Bearer ${trimmedToken}` } : {}),
        ...options?.headers,
      },
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`network error: ${message}`);
  }

  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    const payload = data as { error?: string; message?: string };
    const message = String(payload.message || payload.error || `HTTP ${response.status}`);
    if (response.status === 401) {
      throw new AuthRequiredError(message);
    }
    throw new Error(message);
  }

  return data as T;
}
