export interface SlashCommand {
  name: string;
  description: string;
  usage: string;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: "new", description: "Clear current session", usage: "/new" },
  { name: "compact", description: "Force compact session", usage: "/compact" },
  { name: "status", description: "Show current status", usage: "/status" },
  { name: "skills", description: "List available skills", usage: "/skills" },
  { name: "restart", description: "Restart the bot", usage: "/restart" },
  { name: "providers", description: "List LLM providers", usage: "/providers" },
  { name: "provider", description: "Show/switch provider", usage: "/provider [name]" },
  { name: "models", description: "List models", usage: "/models" },
  { name: "model", description: "Show/switch model", usage: "/model [name]" },
];

/** 入力テキストからコマンド候補をフィルタする。"/" は入力済み前提。 */
export function filterCommands(query: string): SlashCommand[] {
  const prefix = query.toLowerCase();
  return SLASH_COMMANDS.filter((c) => c.name.startsWith(prefix));
}
