export interface PaletteHistoryEntry {
  id: string;
  label: string;
  section: string;
}

const STORAGE_KEY = "egopulse.paletteHistory";
const HISTORY_LIMIT = 5;

export function loadPaletteHistory(): PaletteHistoryEntry[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as PaletteHistoryEntry[];
    return parsed.filter(isHistoryEntry).slice(0, HISTORY_LIMIT);
  } catch {
    return [];
  }
}

export function pushPaletteHistory(item: PaletteHistoryEntry): void {
  try {
    const entries = loadPaletteHistory()
      .filter((historyItem) => historyItem.id !== item.id)
      .slice(0, HISTORY_LIMIT - 1);
    localStorage.setItem(STORAGE_KEY, JSON.stringify([item, ...entries]));
  } catch {
    return;
  }
}

function isHistoryEntry(value: unknown): value is PaletteHistoryEntry {
  if (!value || typeof value !== "object") return false;
  const entry = value as PaletteHistoryEntry;
  return (
    typeof entry.id === "string" &&
    typeof entry.label === "string" &&
    typeof entry.section === "string"
  );
}
