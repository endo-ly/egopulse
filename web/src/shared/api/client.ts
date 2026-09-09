import { AuthRequiredError } from "./auth";

async function request(path: string, authToken: string, options?: RequestInit): Promise<Response> {
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

  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    const payload = data as { error?: string; message?: string };
    const message = String(payload.message || payload.error || `HTTP ${response.status}`);
    if (response.status === 401) {
      throw new AuthRequiredError(message);
    }
    throw new Error(message);
  }

  return response;
}

export async function apiFetch<T>(
  path: string,
  authToken: string,
  options?: RequestInit,
): Promise<T> {
  const response = await request(path, authToken, options);
  const data = await response.json().catch(() => ({}));
  return data as T;
}

/** Fetches an authorized binary response (e.g. agent avatars for <img> tags). */
export async function apiFetchBlob(path: string, authToken: string): Promise<Blob> {
  const response = await request(path, authToken);
  return response.blob();
}
