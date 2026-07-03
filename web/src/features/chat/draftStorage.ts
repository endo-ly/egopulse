const DRAFT_PREFIX = "egopulse.draft.";

export function loadDraft(storageKey?: string): string {
  if (!storageKey) return "";
  try {
    return localStorage.getItem(draftKey(storageKey)) ?? "";
  } catch {
    return "";
  }
}

export function saveDraft(storageKey: string | undefined, text: string): void {
  if (!storageKey) return;
  try {
    const key = draftKey(storageKey);
    if (text) {
      localStorage.setItem(key, text);
    } else {
      localStorage.removeItem(key);
    }
  } catch {
    return;
  }
}

function draftKey(storageKey: string): string {
  return `${DRAFT_PREFIX}${storageKey}`;
}
