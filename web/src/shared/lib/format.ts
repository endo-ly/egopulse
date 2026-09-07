const CHANNEL_LABELS: Record<string, string> = {
  web: "Web",
  discord: "Discord",
  telegram: "Telegram",
  cli: "CLI",
  tui: "TUI",
  voice: "Voice",
};

/** Returns a human-friendly label for a session channel id. */
export function channelLabel(channel: string): string {
  return CHANNEL_LABELS[channel] ?? channel;
}

export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(1)}k`;
}

/** Formats an ISO timestamp as a compact relative age ("now", "5m", "3h", "2d"). */
export function formatRelativeTime(iso: string, now: number = Date.now()): string {
  const elapsedMs = now - new Date(iso).getTime();
  if (elapsedMs < 60_000) return "now";
  const minutes = Math.floor(elapsedMs / 60_000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d`;
  return new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Formats an elapsed duration as a compact human string ("33s", "4m 12s", "1h 03m"). */
export function formatDuration(ms: number): string {
  if (ms < 0) return "0s";
  const totalSeconds = Math.floor(ms / 1000);
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `${minutes}m ${seconds}s`;
  const hours = Math.floor(minutes / 60);
  const restMinutes = String(minutes % 60).padStart(2, "0");
  return `${hours}h ${restMinutes}m`;
}
