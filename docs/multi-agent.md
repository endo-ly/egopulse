# EgoPulse Multi-Agent

マルチエージェント機能の運用ガイド。

## 概要

EgoPulse は複数の AI エージェントを 1 プロセスで同時に運用できる。各エージェントは個別のアイデンティティ（label）、LLM 設定（provider / model）、Discord Bot Token を持つ。

## Agent 定義

`agents` マップでエージェントを定義する。キーはエージェント ID。

```yaml
agents:
  default:
    label: Default Agent
    model: gpt-4o-mini
    provider: null
```

`default_agent` でエージェント未指定時のフォールバックを指定する。

## LLM 解決チェーン

Agent ごとに異なる LLM 設定が可能。解決優先度:

1. `agent.provider` / `agent.model`
2. `channel.provider` / `channel.model`
3. `config.default_provider` / `config.default_model`
4. `provider.default_model`

## Discord Multi-Bot

Agent ごとに個別の Discord Bot Token を設定できる（1 Bot = 1 Agent）。

### 設定例

```yaml
agents:
  developer:
    label: Developer Bot
    discord:
      bot_token:
        source: env
        id: DISCORD_AGENT_BOT_TOKEN_DEVELOPER
      allowed_channels:
        - 1234567890123456789
  reviewer:
    label: Reviewer Bot
    discord:
      bot_token:
        source: env
        id: DISCORD_AGENT_BOT_TOKEN_REVIEWER
      allowed_channels:
        - 9876543210987654321
```

### 起動時の動作

- `discord.bot_token` を持つ Agent の数だけ Discord Bot クライアントが起動する
- 各 Bot は Agent 固有の `allowed_channels` で応答可否を判定する
- DM は常に許可される
- 各 Bot のメッセージは Agent ID でタグ付けされたセッションに記録される

### `channels.discord.bot_token` について

`channels.discord.bot_token` は読み込まれない。Discord Bot を起動するには、各 Agent に `agents.<id>.discord.bot_token` を設定する必要がある。`channels.discord` には `enabled: true` のみを指定する。

## 内部セッション管理

マルチエージェント Discord Bot では、セッションスレッド ID が `{channel_id}:agent:{agent_id}` 形式になり、同一チャネル内でもエージェントごとに独立した会話履歴が保持される。

## 関連ドキュメント

- 設定仕様: [config.md](./config.md)（`agents` セクション）