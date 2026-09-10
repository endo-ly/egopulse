import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  site: "https://endo-ly.github.io",
  base: "/egopulse",
  trailingSlash: "always",
  integrations: [
    starlight({
      title: "EgoPulse",
      components: {
        Head: "./src/components/Head.astro",
      },
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/endo-ly/egopulse",
        },
      ],
      defaultLocale: "root",
      locales: {
        root: { label: "日本語", lang: "ja" },
      },
      customCss: ["./src/styles/custom.css"],
      sidebar: [
        {
          label: "はじめに",
          items: [
            { label: "インストール", link: "install" },
            { label: "コマンド", link: "commands" },
          ],
        },
        {
          label: "設定",
          items: [
            { label: "設定仕様", link: "config" },
            { label: "チャネル仕様", link: "channels" },
            { label: "セキュリティ", link: "security" },
            { label: "ディレクトリ構成", link: "directory" },
          ],
        },
        {
          label: "機能",
          items: [
            { label: "Built-in Tools", link: "tools" },
            { label: "MCP", link: "mcp" },
            { label: "Sleep Batch", link: "sleep" },
            { label: "Pulse", link: "pulse" },
          ],
        },
        {
          label: "開発者向け",
          items: [
            { label: "全体設計", link: "architecture" },
            { label: "セッションライフサイクル", link: "session-lifecycle" },
            { label: "System Prompt", link: "system-prompt" },
            { label: "HTTP API", link: "api" },
            { label: "DB スキーマ", link: "db" },
            { label: "OpenAI Codex", link: "openai-codex" },
            { label: "デプロイ運用", link: "deploy" },
          ],
        },
      ],
    }),
  ],
});
