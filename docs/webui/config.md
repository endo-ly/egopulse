# EgoPulse WebUI — Config Tab

ランタイム設定を WebUI から表示・編集するためのタブ。

設定は `~/.egopulse/egopulse.config.yaml` に永続化され、API 経由で取得・更新する。Config Tab は設定項目をタブ内に常時表示し、Save / Discard のフローを明示する。

> **注記**: 設定ファイル `~/.egopulse/egopulse.config.yaml` および `.env` 系ファイルは WebUI を含むアプリ外からは読み取り不可。Config Tab が扱うのは `/api/config` 経由で公開された情報のみ。

## 1. 構成

```
┌─ Config Tab ─────────────────────────────────────────┐
│ ┌─ Config Header ──────────────────────────────┐    │
│ │  Runtime Configuration                          │    │
│ │  /home/u/.egopulse/egopulse.config.yaml         │    │
│ │  Last saved 14:32   [Discard] [Save changes]    │    │
│ └────────────────────────────────────────────────┘    │
│                                                       │
│ Default LLM                                           │
│ ┌─────────────────────────────────────────────────┐  │
│ │ Provider   [OpenRouter ▼]                       │  │
│ │ Model      [anthropic/claude-sonnet-4    ▼]     │  │
│ │ API Key    [········  (configured)] [Clear]     │  │
│ └─────────────────────────────────────────────────┘  │
│                                                       │
│ Web Server                                            │
│ ┌─────────────────────────────────────────────────┐  │
│ │ Enabled    [✓]                                  │  │
│ │ Host       [127.0.0.1]                          │  │
│ │ Port       [10961]                              │  │
│ └─────────────────────────────────────────────────┘  │
│                                                       │
│ Providers                                             │
│ ┌─────────────────────────────────────────────────┐  │
│ │ OpenRouter    base_url: https://openrouter.ai   │  │
│ │               default: claude-sonnet-4          │  │
│ │               api_key: ✓ configured             │  │
│ │ ─────                                           │  │
│ │ OpenAI        base_url: https://api.openai.com  │  │
│ │               default: gpt-4o                   │  │
│ │               api_key: ✗ not set                │  │
│ └─────────────────────────────────────────────────┘  │
│                                                       │
│ Channel Overrides                                     │
│ ┌─────────────────────────────────────────────────┐  │
│ │ discord   Provider [OpenRouter ▼]  Model [---]  │  │
│ │ telegram  Provider [--- ▼]         Model [---]  │  │
│ │ voice     Provider [--- ▼]         Model [---]  │  │
│ └─────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────┘
```

---

## 2. Config Header

- タイトル（"Runtime Configuration"）
- 現在の config ファイルパス（読み取り専用・等幅表示）
- 最終保存時刻
- 操作：Discard（変更破棄）・Save changes

### 2.1 Dirty Tracking

- 編集操作（input 変更・select 変更等）のたびに dirty flag を立てる
- dirty state に応じて Discard / Save の enabled を切替
- 保存成功時は dirty を false に戻す
- 編集中のページ离开防止は `beforeunload` イベントのみ対応

### 2.2 Load Failure Handling

`/api/config` の取得に失敗した場合、EmptyState（"Failed to load configuration" + error message + Retry button）を表示。

`/api/config` が 401 を返す場合（認証切れ）は AuthModal を表示。

---

## 3. セクション構成

Config Tab の本体は以下の順序でセクションを並べる：

1. **Default LLM** — グローバルプロバイダー・モデル・API Key
2. **Web Server** — Web チャネル自体のホスト・ポート・有効状態
3. **Providers** — 登録済みプロバイダーの一覧（読み取り専用メタデータ）
4. **Channel Overrides** — Discord / Telegram のプロバイダー・モデル上書き

各セクションは fieldset + legend でセマンティクスを保つ。

---

## 4. Default LLM

### 4.1 Provider

- select ドロップダウン
- `config.providers` から選択肢を生成

### 4.2 Model

- `<datalist>` を使った combobox（自由入力 + サジェスト）
- 入力値は `default_model`（空文字可、その場合 provider の `default_model` が有効になる）
- サジェストは選択 provider の `models` 配列

### 4.3 API Key

- type=password の入力欄
- 既に設定済み（`has_api_key === true`）の場合、placeholder を `"Configured. Enter to replace."` に
- 設定済みの場合のみ "Clear" button を表示。クリックで API key の削除をマークし、Save 時に削除がサーバーへ伝わる

> API key の具体的な削除シグナル（sentinel 値等）は実装詳細であり、本仕様書では「Clear 操作で API key を削除できること」のみを規定する。

### 4.4 Provider 変更時の副作用

Provider を変更すると：

- `default_model` を新しい provider の `default_model` に更新
- API Key 入力欄をクリア
- `has_api_key` を新しい provider の状態に更新

---

## 5. Web Server

### 5.1 項目

| 項目 | 型 | 制約 |
|---|---|---|
| Enabled | toggle | なし |
| Host | text input | 自由入力（`127.0.0.1` / `0.0.0.0` / `localhost` 等） |
| Port | number input | 1-65535 の整数のみ受け付け。範囲外は無視 |

### 5.2 反映タイミング

- 設定保存は次回 agent turn から反映（実行中の turn には影響しない）
- Web Server の host / port / enabled を変更した場合、ランタイムの再起動が必要
- 再起動を促すトーストを Save 成功時に表示：`"Web server settings will apply after runtime restart."`

---

## 6. Providers

読み取り専用セクション。プロバイダーの追加・削除は WebUI から行わない（YAML を直接編集）。

各 provider を card 形式で表示し、以下のメタデータを並べる：

- `label` + `id`
- `base_url`（等幅表示）
- `default_model`
- `models`（カンマ区切り、空でなければ）
- `api_key` 状態（`configured` / `not set`、実際の key 値は表示しない）

---

## 7. Channel Overrides

Discord / Telegram チャネルごとに、default LLM とは別の provider / model を指定可能。空欄の場合は Default LLM 設定を使う。

### 7.1 対象チャネル

- `discord` / `telegram` / `voice` を対象とする
- チャネル毎に default LLM とは別の provider / model を指定可能

### 7.2 各チャネルのフィールド

- Provider（select、`---` を含む。`---` は default 使用を意味する）
- Model（text input、空欄で default 使用）

### 7.3 Provider 変更時の副作用

Override の Provider を変更すると：

- Model フィールドを新しい provider の default_model で上書き
- Provider を `---` にした場合は Model も空に

---

## 8. Save 処理

### 8.1 フロー

1. Save button click
2. draft から payload を構築
3. `PUT /api/config` で送信
4. 成功時：
   - response で config state を更新
   - apiKeyDraft をクリア
   - dirty = false
   - lastSavedAt を更新
   - success toast: `"Configuration saved. Changes take effect on next turn."`
5. 失敗時：
   - error toast を表示
   - dirty = true を維持（retry 可能）
   - 401 → AuthModal 表示

### 8.2 API Key の扱い

- 入力欄が空（未入力）：payload に `api_key` を含めない（変更なし）
- Clear 操作を実行：payload で API key 削除を示す（実装固有の表現はサーバー側契約に委ねる）
- その他：payload に `api_key` 実値を含める

API key 実体は `.env` に保存され、YAML には SecretRef として記録される（[config.md](../config.md) 参照）。

---

## 9. バックエンド API

### 9.1 `GET /api/config`

[api.md §2.3](../api.md#23-設定) に従う。変更なし。

### 9.2 `PUT /api/config`

[api.md §2.3](../api.md#23-設定) に従う。変更なし。

---

## 10. アクセシビリティ

- 各 field は `<label>` で input と関連付け
- Required 項目には `aria-required="true"`
- Save / Discard button は状態に応じて `aria-label` を切替（`"Save changes"` / `"Saving..."` 等）
- API Key input は `autocomplete="off"` `spellcheck="false"`
- 変更がある限り Config Header に `role="status"` で `"You have unsaved changes"` を読み上げ

---

## 11. Out of Scope

- Provider の追加・削除（YAML 直接編集のみ）
- Agent 毎の LLM 設定（`agents.<id>` 配下のプロバイダー指定）
- Sleep / Pulse スケジュール設定（`sleep:` / `pulse:` ブロック）
- MCP サーバー設定（`mcp_servers:` ブロック）
- チャネル自体の設定（Discord bots / Telegram bots 等）
- 設定の import / export
- 変更履歴（git 管理を推奨）
