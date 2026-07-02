import { Badge } from "./Badge";

export interface ReadOnlyBannerProps {
  channel: string;
}

function channelLabel(channel: string): string {
  const map: Record<string, string> = {
    discord: "Discord",
    telegram: "Telegram",
    cli: "CLI",
    tui: "TUI",
    voice: "Voice",
  };
  return map[channel] ?? channel;
}

export function ReadOnlyBanner({ channel }: ReadOnlyBannerProps) {
  return (
    <div className="readonly-banner">
      <Badge kind="channel">{channelLabel(channel)}</Badge>
      <span className="readonly-text">
        This is a {channelLabel(channel)} session. To reply, use{" "}
        {channelLabel(channel)} directly.
      </span>
    </div>
  );
}
