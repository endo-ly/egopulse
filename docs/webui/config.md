# EgoPulse WebUI — Config Tab

ランタイム設定（`~/.egopulse/egopulse.config.yaml`）を WebUI から確認・編集するグローバルタブ。全フィールドを構造化して表示し、各値の出どころ・反映タイミング・秘密情報の設定状態を明示する。

## 1. 構成

```
┌ Config ────────────────── ~/.egopulse/egopulse.config.yaml · rev 42 ┐
│ [Structured | YAML]                                                │
│ ┌───────────┐  Agents › lyre                                       │
│ │ General   │  label            Lyre                               │
│ │ Agents    │  provider         sakura              ⚡ 即時反映     │
│ │ Providers │  model            preview/Kimi-K2.6   ⚡ 即時反映     │
│ │ Channels  │  reasoning_effort medium                             │
│ │ Sleep     │  discord_bot      lyre                               │
│ │ Pulse     │  profiles.voice   openrouter / gpt-4.1-mini          │
│ │ Webhooks  │                                                      │
│ │ Backup    │  Providers › openrouter                              │
│ │ Web Fetch │  api_key   env:OPENROUTER_API_KEY   ✓ configured     │
│ │ MCP       │  base_url  https://openrouter.ai/…  ENV override     │
│ └───────────┘                                                      │
└────────────────────────────────────────────────────────────────────┘
```

- Header に config ファイルパスと revision を表示する
- 左にセクションナビ、右に選択セクションの内容。セクションは URL で表現する（`/config/:section`）
- Sidebar の文脈領域にもセクション一覧を表示する

---

## 2. 表示

### 2.1 セクション

[config.md §3](../config.md#3-完全フィールドリファレンス) の構成に従う。

| セクション | 対象 |
|---|---|
| General | `default_agent` / `default_provider` / `default_model` / `timezone` / compaction 系 など |
| Agents | `agents.<id>`（provider / model / reasoning_effort / bot 紐づけ / profiles） |
| Providers | `providers.<id>`（base_url / models / api_key） |
| Channels | Web / Discord / Telegram / Voice |
| Sleep | `sleep_batch` |
| Pulse | `pulse` |
| Webhooks | `webhooks` |
| Backup | `db.backup` |
| Web Fetch | `web_fetch` |
| MCP | `mcp.json` / `mcp.d/` のサーバー定義（読み取り専用。接続状態は Metrics タブ） |

### 2.2 フィールド表示

各フィールドに以下を添える。

| 属性 | 表示 |
|---|---|
| 値の出どころ | YAML / 環境変数オーバーライド / デフォルト値 |
| 反映タイミング | 即時反映 / 要再起動（[config.md §11](../config.md#11-再起動要否と秘匿フィールド) の分類） |
| 秘密情報 | SecretRef（`env:XXX` 等）と設定有無（`configured` / `not set`）のみ。実値は表示しない |

### 2.3 YAML ビュー

- 設定ファイルの原文を表示する。秘密情報の値はサーバー側でマスクして返す
- `.env` の内容は扱わない

### 2.4 外部変更の検知

設定ファイルは外部から編集され得る。取得した revision と現在の revision が異なる場合は「外部で変更されました」と表示し、再読み込みを促す。

---

## 3. 編集

セクション単位で編集する。

1. セクションの `Edit` でフォームに切り替える
2. 保存前に YAML の diff を確認する（[schedule.md §4.4](./schedule.md#44-diffviewer) の DiffViewer）
3. `Save` で `expected_fingerprint` 付きで更新する。`409` の場合は最新を再取得させる

- 更新は `ConfigManager` の単一の更新境界を通る（[config.md §10](../config.md#10-設定の変更インターフェース)）。YAML 全体を書き込む経路は設けない
- 要再起動フィールドは編集不可とし、バッジで理由を示す
- 秘密情報は新しい値の入力と削除のみ受け付ける。実値は `.env` に保存され、YAML には SecretRef として記録される
- 未保存の変更がある間は `beforeunload` で離脱を確認する

---

## 4. バックエンド API

| API | 用途 |
|---|---|
| `GET /api/config` | 全フィールド・値の出どころ・反映区分・秘密情報の設定有無・revision・fingerprint を返すよう拡張 |
| `GET /api/config/yaml` | マスク済み YAML 原文 |
| `PUT /api/config` | 更新（既存）。編集可能なフィールドをセクション単位で拡張していく |

---

## 5. Out of Scope

- `.env` の閲覧
- 設定バックアップ（`egopulse.config.backups/`）の閲覧・復元
- 設定の import / export
- MCP サーバー定義の編集
