# EgoPulse

> Stricter than OpenClaw. Freer than Hermes Agent.

エージェントが記憶し、気づき、並び立つランタイム。Web UI / Discord / Telegram / TUI / CLI に対応。

## 特徴

### Agent-First

Session / Memory / Tool / PULSE のすべてが `agent_id` を支配的識別子とする。同じ runtime に複数のエージェントが独立した記憶を持って並立し、互いに委譲し合う。チャネルもツールも、すべてエージェントに紐づく。

### Sleep batch & Long time memory

会話履歴を episodic（エピソード記憶）/ semantic（意味記憶）/ prospective（展望記憶）の 3 層に蒸留する。エージェントはSleepバッチで過去を整理し、記憶を長期保持する。

### PULSE

時間・記憶・外界からの signal を受け取り、いま意識へ上げるべきものを選び、短く活性化する。必要なときだけ普段の会話場所で声を出す。「何時に何を実行する」ではなく「何時に何へ注意を向ける」。

---

## Getting Started

```bash
curl -fsSL https://raw.githubusercontent.com/endo-ly/egopulse/main/scripts/install.sh | bash
egopulse setup
egopulse gateway install   # systemd サービス登録 + 起動
```

起動後、ブラウザで http://127.0.0.1:10961 にアクセスすると WebUI が利用できる。

| モード | コマンド | 説明 |
|---|---|---|
| Gateway | `egopulse gateway install` | Web / Discord / Telegram をサービスとして起動 |
| Gateway 停止 | `egopulse gateway stop` | systemd サービス停止（登録は残す） |
| CLI chat | `egopulse chat` | ターミナルから直接会話 |
| TUI | `egopulse` | セッションブラウザ + チャット（対話型では `q` で終了） |

Discord / Telegram の設定は [channels.md](./docs/channels.md) を参照。

---

## Tech Stack

| レイヤー | 技術 |
|---|---|
| Runtime | Rust (Tokio) |
| 永続化 | SQLite (WAL モード) |
| Web Server | Axum |
| Web UI | React, Vite |
| LLM | OpenAI 互換 API |

---

## 設定

設定は `~/.egopulse/egopulse.config.yaml` に YAML で記述する。
プロバイダ、モデル、チャネル、Sleep スケジュール、PULSE 間隔などを設定できる。
詳細は [config.md](./docs/config.md) を参照。

---

## ドキュメント

| トピック | ドキュメント |
|---|---|
| アーキテクチャ概要 | [architecture.md](./docs/architecture.md) |
| コマンド仕様 | [commands.md](./docs/commands.md) |
| 設定仕様 | [config.md](./docs/config.md) |
| チャネル (Web/Discord/Telegram/TUI/CLI) | [channels.md](./docs/channels.md) |
| セッションライフサイクル | [session-lifecycle.md](./docs/session-lifecycle.md) |
| Built-in Tools | [tools.md](./docs/tools.md) |
| MCP 統合 | [mcp.md](./docs/mcp.md) |
| System Prompt 構築 | [system-prompt.md](./docs/system-prompt.md) |
| セキュリティ | [security.md](./docs/security.md) |
| デプロイ | [deploy.md](./docs/deploy.md) |
| DB スキーマ | [db.md](./docs/db.md) |
| WebUI API | [api.md](./docs/api.md) |
