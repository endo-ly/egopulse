# システムプロンプト設計メモ

このドキュメントは Clawdbot のシステムプロンプト設計に関するメモをまとめています。

## Clwdbot SystemPrompt 参考構成

参考: https://raw.githubusercontent.com/clawdbot/clawdbot/main/docs/concepts/system-prompt.md

### セクション構成

| セクション | 説明 |
|-----------|------|
| **Tooling** | ツール一覧＋短い説明 |
| **Skills** | 利用可能なスキル一覧。必要時に読みに行く指示 |
| **Clawdbot Self-Update** | config.apply / update.run の実行指示 |
| **Workspace** | 作業ディレクトリ |
| **Documentation** | ローカルDocsの場所、参照指示 |
| **Workspace Files (injected)** | Bootstrapファイルが下に続く旨 |
| **Sandbox** | サンドボックス有無・制約 |
| **Current Date & Time** | ユーザーのローカル時間／TZ／フォーマット |
| **Reply Tags** | 必要時の返答タグ |
| **Heartbeats** | 定期応答・ACKの扱い |
| **Runtime** | ホスト/OS/Node/モデル/Repo root 等の一行 |
| **Reasoning** | 可視性レベルと /reasoning トグル |

### promptMode オプション

- **full**: 上記すべて
- **minimal**: Skills/Memory/Self-Update/Model Aliases/User Identity/Reply Tags/Messaging/Silent Replies/Heartbeatsを省略
- **none**: ベースのアイデンティティ行のみ

## EgoGraph での実装方針

現在の EgoGraph では以下のセクションを実装しています:

1. **Tooling**: ツール使用指針
2. **Workspace Files (injected)**: `build_bootstrap_context()` で注入
3. **Current Date & Time**: JST タイムゾーンでの日時情報

将来的な拡張として、Skills や Heartbeats の導入を検討できます。
