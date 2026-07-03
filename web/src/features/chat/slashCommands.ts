export interface SlashCommand {
  name: string;
  description: string;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: "/reset", description: "Clear conversation history" },
  { name: "/compact", description: "Compact messages" },
  { name: "/sleep", description: "Run sleep batch" },
  { name: "/help", description: "Show available commands" },
];

export function matchSlashCommands(text: string): SlashCommand[] {
  if (!text.startsWith("/") || text.length === 0) return [];
  return SLASH_COMMANDS.filter((command) => command.name.startsWith(text));
}
