# EgoPulse WebUI — Config Tab

ランタイム設定（`~/.egopulse/egopulse.config.yaml`）を確認し、再起動なしで反映される設定をその場で編集するグローバルタブ。値は「実際に効く形」で見せる：未指定のモデルは解決後の値と解決元を、秘密は設定状態だけを表示する。

## 1. 構成

```
┌ Sidebar ──────────────┐ Agents                  ~/.egopulse/egopulse.config.yaml
│ SECTIONS              │ The model each agent…                      Revision 42
│ General               │ ┌ Lyre  lyre  default ─────────────────────── [Edit] ┐
│   Default agent Lyre  │ │ Model             Sakura AI  preview/Kimi-K2.6      │
│ Agents      4 agents  │ │ Reasoning effort  medium                            │
│ Providers             │ │ Discord bot       lyre                              │
│   1 missing API key   │ │ Channel profiles  voice  OpenRouter openai/gpt-4.1… │
│ Channels  Web, Discord│ └─────────────────────────────────────────────────────┘
│ …                     │ ┌ Ace  ace ─────────────────────────────────── [Edit] ┐
│ ───────────────────── │ │ Model             OpenRouter  anthropic/claude-…     │
│ YAML file  Revision 42│ │                   Provider inherited from the global │
└───────────────────────┘ └─────────────────────────────────────────────────────┘
```

- セクションは URL で表す（`/config` が General、`/config/:section`）
- Sidebar の文脈領域にセクション一覧と 1 行の要約を出す。注意が必要な状態（API key 未設定、MCP 接続失敗、Backup 無効）は warning / danger 色にする
- Header に設定ファイルのパスと revision を表示する

---

## 2. セクション

| セクション | 内容 | 編集 |
|---|---|---|
| General | 既定 agent / provider / model、timezone、履歴と compaction の値。Process（log level・state directory）は再起動必須 | Defaults カード |
| Agents | agent ごとのカード：実効モデルと解決元、reasoning effort、Bot 紐づけ、channel profiles | agent ごと（名前・provider・model・reasoning effort・Discord bot・Telegram bot） |
| Providers | provider ごとのカード：base URL、API key の状態、default model、models（context window・custom instructions の有無）、追加ヘッダ名、使っている agent | provider ごと（名前・base URL・models・default model・API key の Replace / Remove） |
| Channels | Web（listen address・auth token・allowed origins）/ Discord・Telegram（Bot と token 状態・紐づく agent、許可チャネルの表）/ Voice | — |
| Sleep | 実行時刻、対象 agent、実効モデル、reasoning effort、retry | Sleep batch カード |
| Pulse | tick interval。agent ごとの Pulse 定義は Schedule へ | Scheduler カード |
| Webhooks | receiver ごとの配送先・agent・token 状態 | — |
| Backup / Web Fetch | 値の一覧 | —（再起動必須） |
| MCP | サーバー定義（transport・command のプログラム名 / endpoint の origin・env と header のキー名・timeout）と接続状態、定義ファイルの探索順 | — |
| YAML file | マスク済み YAML（行番号・Copy） | — |

編集できない設定の変更方法は、セクションまたはカード単位で 1 回だけ示す（「YAML を編集して再起動」など）。フィールドごとのバッジは付けない。

### 2.1 値の表示

| 状況 | 表示 |
|---|---|
| 環境変数で上書きされている | 値に `env NAME` タグ（hover で説明） |
| 既定値と同じ（General の数値・timezone、Pulse の tick interval） | 値に `default` タグ |
| 未指定で継承している（agent / Sleep の provider・model） | 解決後の値と、2 行目に解決元（`Provider inherited from the global default` / `the provider's default model` など） |
| 秘密（API key・token） | `Set from OPENROUTER_API_KEY` / `Set from a command` / `Set written in the YAML file` / `Not set`。key が不要な provider は `Not needed` 。環境変数で上書きされた token は `Set from the WEB_AUTH_TOKEN environment variable` |

値そのもの、`exec` のコマンド、provider の追加ヘッダ値、MCP の args・env と header の値・endpoint のパスと query は返さない。

---

## 3. 編集

1. カードの `Edit` でそのカードがフォームになる。編集中は他のカードの `Edit` を無効にする
2. フォーム下に変更点（`Model: Not set (anthropic/claude-sonnet-4) → google/gemini-2.5-pro`）を常に表示する。変更がなければ Save は無効
3. `Save` は変更したフィールドだけを `expected_fingerprint` 付きで送る。成功するとカードは閲覧に戻り、トーストで知らせる

| 状況 | 振る舞い |
|---|---|
| 編集中に他所（YAML・スラッシュコマンド）で設定が変わった | 触っていないフィールドは最新値に追従する。触ったフィールドが他所でも変わっていれば、変更点に `Also changed elsewhere; saving replaces it` を出す |
| `409 config_conflict` | 最新を再取得し、上と同じ三者比較で下書きを残したまま再表示する |
| `400` | サーバーのメッセージをフォーム上部に表示する |
| 未保存の変更がある | 画面・セクション・agent の切替とブラウザの戻る / 進むで確認ダイアログ、ページ離脱で `beforeunload` |

- 更新は `ConfigManager` の単一の更新境界を通る（[config.md §10](../config.md#10-設定の変更インターフェース)）。YAML 全体を書き込む経路は設けない
- API key の新しい値は `.env` に保存され、YAML には SecretRef として記録される

---

## 4. データソース

| 用途 | API | ポーリング |
|---|---|---|
| 構造化した設定 | `GET /api/config` | 可視時 10 秒（外部変更への追従） |
| YAML ビュー | `GET /api/config/yaml` | revision が変わったとき |
| MCP の定義 | `GET /api/config/mcp` | MCP セクションを開いたとき |
| MCP の接続状態 | `GET /api/status` | 10 秒 |
| 更新 | `PATCH /api/config/{general,sleep,pulse}`、`PATCH /api/config/agents/{id}`、`PATCH /api/config/providers/{id}` | — |

詳細は [api.md §2.4](../api.md#24-設定)。

---

## 5. Out of Scope

- agent / provider の追加・削除
- channel profiles、model ごとのメタデータ（context window・instructions）、Channels・Webhooks の編集
- `.env` の閲覧
- 設定バックアップ（`egopulse.config.backups/`）の閲覧・復元
- 設定の import / export
- MCP サーバー定義の編集
