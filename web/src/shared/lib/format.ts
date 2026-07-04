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
