import type {
  PulseDelivery,
  PulseIntention,
  PulseNextDue,
  PulseRun,
  PulseSchedule,
  SessionEntry,
} from "../../shared/api/types";
import { channelLabel } from "../../shared/lib/format";
import { sessionTitle } from "../../shared/lib/session";
import { statusLabel, triggerLabel } from "./runStatus";

const DAY_NAMES: Record<string, string> = {
  mon: "Monday",
  tue: "Tuesday",
  wed: "Wednesday",
  thu: "Thursday",
  fri: "Friday",
  sat: "Saturday",
  sun: "Sunday",
};

/** A schedule as people say it: "Every day 09:00", "Every Sunday 21:00". */
export function describeSchedule(schedule: PulseSchedule): string {
  switch (schedule.kind) {
    case "daily":
      return `Every day ${schedule.at}`;
    case "weekly":
      return `Every ${DAY_NAMES[schedule.day] ?? schedule.day} ${schedule.at}`;
    case "interval":
      return `Every ${schedule.interval_days} days ${schedule.at}`;
    case "once":
      return `Once on ${schedule.date} ${schedule.at}`;
  }
}

function startOfDay(time: number): number {
  const date = new Date(time);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/** A local day relative to today: "Today", "Yesterday", "Tomorrow", "Mon, Nov 3". */
export function formatDay(iso: string, now: number = Date.now()): string {
  const date = new Date(iso);
  const days = Math.round((startOfDay(date.getTime()) - startOfDay(now)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === -1) return "Yesterday";
  if (days === 1) return "Tomorrow";
  return date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
}

/** The local start of the day an instant falls on, for grouping by day. */
export function dayKey(iso: string): number {
  return startOfDay(Date.parse(iso));
}

/** A local time of day: "09:00". */
export function formatClock(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}

/** An upcoming instant relative to today: "Today 21:00", "Tomorrow 09:00", "Mon, Nov 3 10:00". */
export function formatUpcomingTime(iso: string, now: number = Date.now()): string {
  return `${formatDay(iso, now)} ${formatClock(iso)}`;
}

/** A run's start as a compact local timestamp: "Oct 31, 09:00". */
export function formatRunTime(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

/** When an intention fires next; `null` means it is disabled. */
export function describeNextDue(next: PulseNextDue | null, now: number = Date.now()): string {
  if (next === null) return "Off";
  switch (next.state) {
    case "due_now":
      return "Due now";
    case "scheduled":
      return next.at ? formatUpcomingTime(next.at, now) : "Scheduled";
    case "attempted_this_period":
      return "Done for this period";
    case "attempted_once":
      return "Already fired";
    case "no_upcoming":
      return "No upcoming";
  }
}

/**
 * A Pulse run's outcome: "Notified" / "Silent" for successes, "Not delivered"
 * for a firing skipped because its destination did not resolve, else the status.
 */
export function pulseOutcomeLabel(run: Pick<PulseRun, "status" | "output_kind">): string {
  if (run.status === "success") return run.output_kind === "notify" ? "Notified" : "Silent";
  if (run.status === "skipped") return "Not delivered";
  return statusLabel(run.status);
}

/** The first line of a Markdown message as plain text, for one-line previews. */
export function markdownPreview(markdown: string | null): string | null {
  const line = markdown?.split("\n").find((candidate) => candidate.trim() !== "");
  if (line === undefined) return null;
  return line
    .trim()
    .replace(/^(#{1,6}\s+|>\s*|[-*+]\s+|\d+\.\s+)/, "")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/(\*\*|__|`)/g, "");
}

/** What an intention is about, as a title: the first line of its attention, else its id. */
export function intentionTitle(intention: Pick<PulseIntention, "id" | "attention">): string {
  return markdownPreview(intention.attention) || intention.id;
}

/** Where an intention's firings go, as shown to people. */
interface DeliveryDescription {
  /** "Discord · Team chat", or the configured destination when it does not resolve. */
  label: string;
  /** How the destination was chosen. */
  origin: "intention" | "default" | "latest";
  /** The conversation firings go to; null when none resolves and firings are skipped. */
  sessionKey: string | null;
}

/**
 * Describes where an intention delivers: its own destination, else the
 * agent's default, else the agent's latest Discord / Telegram conversation.
 * Resolved conversations are named as in the session list.
 */
export function describeDelivery(
  intention: Pick<PulseIntention, "delivery" | "target">,
  defaultDelivery: PulseDelivery | null,
  sessions: readonly SessionEntry[],
): DeliveryDescription {
  const configured = intention.delivery ?? defaultDelivery;
  const origin = intention.delivery ? "intention" : defaultDelivery ? "default" : "latest";
  const { target } = intention;
  if (target !== null) {
    const session = sessions.find((candidate) => candidate.session_key === target.session_key);
    const channel = channelLabel(target.channel);
    return {
      label: session ? `${channel} · ${sessionTitle(session)}` : channel,
      origin,
      sessionKey: target.session_key,
    };
  }
  return {
    label: configured
      ? `${channelLabel(configured.channel)} · ${configured.external_chat_id}`
      : "Latest conversation",
    origin,
    sessionKey: null,
  };
}

/** A sleep run's title: "Sleep" for the scheduled one, else "Manual sleep" / "Backfill sleep". */
export function sleepTitle(trigger: string): string {
  return trigger === "scheduled" ? "Sleep" : `${triggerLabel(trigger)} sleep`;
}
