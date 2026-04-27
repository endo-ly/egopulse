# EgoPulse Multi-Agent

マルチエージェント機能の運用ガイド。

## 概要

EgoPulse は複数の AI エージェントを 1 プロセスで同時に運用できる。各エージェントは個別のアイデンティティ（label）、LLM 設定（provider / model）を持つ。

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

Bot 単位で Discord 接続を定義し、Bot ごとに複数のエージェントをチャンネル別に割り当てられる。

### 設定例

```yaml
channels:
  discord:
    enabled: true
    bots:
      main:
        token:
          source: env
          id: DISCORD_BOT_TOKEN
        default_agent: assistant
        allowed_channels:
          - 1234567890
        channel_agents:
          "9876543210": reviewer
```

### 起動時の動作

- `bots` に定義された Bot の数だけ Discord クライアントが起動する
- 各メッセージは `select_agent(channel_id, is_dm)` によりエージェントを決定
- ルーティング優先順位: `channel_agents[channel_id]` → `default_agent`
- `allowed_channels` に含まれないギルドチャンネルは拒否。DM は常に `default_agent` で許可
- 各 Bot+Agent ペアのメッセージは独立したセッションに記録される

## 内部セッション管理

マルチエージェント Discord Bot では、セッションスレッド ID が `{channel_id}:agent:{agent_id}` 形式になり、同一チャネル内でもエージェントごとに独立した会話履歴が保持される。

## 関連ドキュメント

- 設定仕様: [config.md](./config.md)（`agents` セクション）