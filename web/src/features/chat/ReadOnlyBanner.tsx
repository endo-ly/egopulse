import { Badge } from "../../shared/ui/Badge";
import { channelLabel } from "../../shared/lib/format";

export interface ReadOnlyBannerProps {
  channel: string;
}

export function ReadOnlyBanner({ channel }: ReadOnlyBannerProps) {
  const label = channelLabel(channel);
  return (
    <div className="readonly-banner">
      <Badge kind="channel">{label}</Badge>
      <span className="readonly-text">
        This is a {label} session. To reply, use {label} directly.
      </span>
    </div>
  );
}
