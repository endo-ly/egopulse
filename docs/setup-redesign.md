# Setup Wizard Refresh (設計メモ)

> **Status**: 設計段階 (未実装)
> **Date**: 2026-06-22
> **関連**: [commands.md §1.1](./commands.md) `egopulse setup` / [config.md §7](./config.md#7-セットアップウィザード)

## 目次

1. [背景と目的](#1-背景と目的)
2. [設計方針](#2-設計方針)
3. [スコープ](#3-スコープ)
4. [確定フロー](#4-確定フロー)
5. [入力項目仕様](#5-入力項目仕様)
6. [完了メッセージ仕様](#6-完了メッセージ仕様)
7. [コード観点の影響](#7-コード観点の影響)
8. [関連課題 (本メモのスコープ外)](#8-関連課題-本メモのスコープ外)

---

## 1. 背景と目的

現状の `egopulse setup` は ratatui + crossterm を用いたフル TUI ウィザードとして実装されている。機能追加・保守性・学習コストの観点で以下の問題を抱えているため、チャットライクな順次プロンプト方式へ全面刷新する。

### 1.1 現状の問題点

| 分類 | 問題 |
|---|---|
| アーキテクチャ | `src/setup/` 4ファイル 計 2213 行のフル TUI 実装。ratatui 描画・イベントループ・状態遷移を内包し、`src/channels/tui.rs` (ローカル TUI) とスタック重複しながらコンポーネント再利用ゼロ |
| UX | 縦に並んだ 9 フィールドを Navigate / Edit / Selector の 3 モードで埋めるフォーム形式。「ウィザード」のステージ分割なし、戻る/進むなし |
| カバレッジ | `web.host/port/allowed_origins`、`channels.voice.*`、Discord/Telegram の channel access control 等、初回設定で設定不可の項目が多い ([config.md §7](./config.md#7-セットアップウィザード) 参照) |
| 強制設定 | Web チャネルが常に `enabled: true` で host/port 固定。Web を使わない選択肢がない |
| 検証 | モデル名が preset の `models` リストと照合されない。ブール入力が無効値を黙って `false` 扱い。既存 Config のパースエラーを黙殺 |
| エージェント扱い | `agents.default` が永続化時に暗黙生成される。Agent-First アーキテクチャと乖離 |

### 1.2 目的

- 初回セットアップを「**対話型の順次プロンプト**」で行い、ユーザーの学習コストを下げる
- **動かすために最低限必要な設定**のみを問い、詳細項目は全てデフォルト運用にする
- Agent-First 設計に合わせて、**エージェント定義を明示ステップ**にする
- ratatui への依存をセットアップ側から排除し、実装を数百行規模に圧縮する

---

## 2. 設計方針

### 2.1 基本方針

- **Agent-First**: 最初の質問はエージェントの名前。プロバイダー選択等は「そのエージェントが使う LLM」として位置づける
- **Minimum Viable Setup**: LLM と対話するために必要な項目のみ。ホスト・ポート・タイムゾーン等は全てデフォルト
- **Chat-like Sequential Prompts**: ステージ分割された順次プロンプト。フォーム形式ではなく、1質問1回答の対話
- **明示的選択**: Web の強制有効化を廃止。Discord / Telegram も含め、ユーザーが明示的に選ぶ

### 2.2 採用ライブラリ

- **`dialoguer`** を採用。プロンプト primitives (text / select / confirm / password / multi_select) を標準提供し、依存が少なく `cargo`, `rustup` 等のツール実績もある。AGENTS.md 「既存ライブラリ優先」原則に合致

---

## 3. スコープ

### 3.1 対象 (やること)

- `src/setup/` のチャットライクな順次プロンプト方式への全面刷新
- エージェント定義ステップの追加 (label 入力)
- Web チャネル有効化の明示的 yes/no 化 (強制有効化の廃止)
- 入力バリデーションの導入 (URL / モデル名 / 必須項目)
- 既存 Config パースエラーの warn 表示 (Y/N 確認付き、黙殺は廃止)
- Additional Options ステップの追加 (設定しなかった項目の案内)

### 3.2 対象外 (やらないこと)

- **TUI チャネル (`src/channels/tui.rs`) の刷新** — 別課題 ([§9](#9-関連課題-本メモのスコープ外)) で扱う
- 複数プロバイダー / 複数エージェントの設定 (1つだけ生成、残りは手動 YAML or WebUI)
- Discord / Telegram の channel access control (channels マップ)
- Voice / Sleep Batch / Pulse / DB backup / Web Fetch 等の高度設定
- `web.host` / `web.port` / `web.allowed_origins` / `timezone` / `log_level` / `compaction_*` / `max_*` 等の詳細項目
- 人格 (`SOUL.md`) の設定
- WebUI での設定編集機能

### 3.3 過渡期の扱い

- 本リフレッシュ完了後も `ratatui` / `crossterm` 依存は `src/channels/tui.rs` が残る限り `Cargo.toml` に残置される。依存削除は TUI 廃止と同タイミングで行う

---

## 4. 確定フロー

### 4.1 ステップ構成

```
[Welcome]
   ↓
[Q1: Agent Label]  ── Agent-First のため最初に聞く
   ↓
[Q2: Provider]  ── 25 presets から選択
   ↓ (Custom 選択時のみ base_url 追加質問)
[Q3: Model]  ── preset から選択 / Custom 時は手入力
   ↓
[Q4: API Key]  ── 空欄可。非 localhost 系で空欄時は Y/N 確認
   ↓
[Q5: Web Channel]  ── デフォルト yes、host/port は固定
   ↓
[Q6: Discord]  ── デフォルト no
   ↓ (yes のみ bot token 入力)
[Q7: Telegram]  ── デフォルト no
   ↓ (yes のみ bot token 入力)
[Review]  ── 生成 YAML の内容を表示、保存確認 (no の場合は 戻る/中断/保存 の3択)
   ↓
[Save]  ── 設定ファイル永続化
   ↓
[Additional Options]  ── 設定しなかった項目の案内 (情報表示のみ)
   ↓
[Done]  ── 保存先パスと次ステップの案内
```

### 4.2 各ステップの疑似プロンプト

#### [Welcome]

```
Welcome to EgoPulse setup.
Answer a few questions to configure the minimum settings to run your AI agent.
```

#### [Q1: Agent Label]

```
Name your agent (e.g. Partner, Companion, Assistant):
> _
```

- 入力値を **自前 slugify** (lowercase + 英数字以外をハイフン置換 + 連続ハイフン圧縮) で agent id を自動生成
  - 例: `"Lyre"` → `"lyre"`、`"My Agent"` → `"my-agent"`、`"Vega 2"` → `"vega-2"`
  - slugify 結果が空になった場合 (label が記号のみ等) は `"default"` にフォールバック
- 以降のプロンプトではこの label を用いて `Choose the LLM provider for {Agent Label}` のように表示

#### [Q2: Provider]

```
Choose the LLM provider for {Agent Label} (arrow keys to move, Enter to confirm):
> OpenAI
  OpenRouter
  DeepSeek
  Ollama (local)
  ... (25 presets)
  Custom
```

- 選択肢は `PROVIDER_PRESETS` (`src/setup/provider.rs:12-234`) を流用
- **Custom 選択時のみ Q2 の直後で `base_url` 入力を追加**:

```
Enter the base_url (e.g. https://api.example.com/v1):
> _
```

  - `url::Url::parse` で検証、不正なら再入力

#### [Q3: Model]

preset 選択時 (選択式):

```
Choose the model to use:
> {preset.default_model}
  {preset.models[*]}
```

Custom 選択時 (手入力テキスト):

```
Enter the model name (e.g. gpt-4o, claude-3-opus):
> _
```

- **preset 選択時は手入力不可** (validation で preset 外を拒否、再選択)
- **Custom 選択時は Q3 も表示するが手入力テキストモードに切り替え**。空文字拒否、再入力

#### [Q4: API Key]

```
Enter the API key for {Provider} (input is hidden).
For local endpoints (Ollama/LMStudio), leave it empty and press Enter:
********
```

- **ステップ自体は常に表示**。空欄 Enter で進める。プロバイダーごとの分岐なし
- **非 localhost 系プロバイダで空欄入力時は Y/N で確認**:

```
WARNING: {Provider} usually requires an API key. Proceed with an empty key? (y/N)
```

  - `no` の場合は Q4 に戻り再入力
  - `yes` の場合は警告付きでそのまま進める (ローカルプロキシ等の例外ケースを想定)

#### [Q5: Web Channel]

```
Enable the Web UI? (Y/n)
You can access it at http://127.0.0.1:10961 from your browser.
> Y
```

- デフォルト `yes`
- `auth_token` は `generate_auth_token()` で自動生成、ユーザーには聞かない
- トークン実値は Review / Done いずれでも**表示しない** (`.env` 参照を案内するのみ)

#### [Q6: Discord]

```
Configure a Discord bot? (y/N)
> N
```

- デフォルト `no`
- `yes` の場合のみ追加で bot token 入力:

```
Enter the Discord bot token (input is hidden):
********
```

#### [Q7: Telegram]

```
Configure a Telegram bot? (y/N)
> N
```

- デフォルト `no`
- `yes` の場合のみ追加で bot token 入力 (Discord と同様)

#### [Review]

```
About to save the configuration file with the following values:

  Agent:    Partner (id: partner)
  Provider: openai (https://api.openai.com/v1)
  Model:    gpt-5.2
  API Key:  sk-...xxxx
  Web:      enabled (auth_token: auto-generated, saved to .env)
  Discord:  disabled
  Telegram: disabled

Save? (Y/n)
```

- API Key は末尾 4 文字のみ表示、それ以外は `...` でマスク
- API Key 空欄時は `(empty)` と明示
- **`no` の場合は `dialoguer::Select` で 3択を提示**:

```
What would you like to do?
> Start over (back to Agent Label)
  Abort (exit without saving)
  Save anyway
```

  - 「Start over」→ Q1 に戻る
  - 「Abort」→ 保存せずに終了 (exit code 1)
  - 「Save anyway」→ 保存処理へ進む

#### [Additional Options]

保存完了後、セットアップで設定しなかったが YAML 編集で設定可能な項目を案内する。入力は受け付けず、Enter で次へ進む。

```
The configuration has been saved. The following options were not configured in
this setup, but can be set by editing ~/.egopulse/egopulse.config.yaml:

System:
  - timezone (default: UTC)
  - log_level (default: info)
  - default_context_window_tokens (default: 32768)
  - compaction_threshold_ratio / compaction_target_ratio / compact_keep_recent
  - max_history_messages

Web UI:
  - channels.web.host (default: 127.0.0.1)
  - channels.web.port (default: 10961)
  - channels.web.allowed_origins (default: [])

Channels:
  - Additional providers and agents (add entries under "providers" / "agents")
  - Discord/Telegram channel access control (see docs/channels.md)
  - Voice channel (channels.voice.*)
  - Per-agent persona (SOUL.md)

Subsystems:
  - sleep_batch (long-term memory processing)
  - pulse (attention activation)
  - db.backup (SQLite backup settings)
  - web_fetch (built-in tool settings)

See docs/config.md for the full reference.

Press Enter to continue.
```

- 情報表示のみ。入力フィールドなし、Enter のみで次へ
- カテゴリ分けして概要を提示、詳細は `docs/config.md` へ誘導
- セットアップで終わりではなく、YAML 編集で拡張できることを教育する役割

#### [Done]

```
Configuration saved: ~/.egopulse/egopulse.config.yaml
Backup: (shown only if an existing config was backed up)

Next steps:
  - Start chatting now:          egopulse chat
  - Install as a systemd service: egopulse gateway install
  - Edit configuration:          ~/.egopulse/egopulse.config.yaml
  - Add more agents:             edit the "agents" section in the YAML

If Web UI is enabled:
  - URL:    http://127.0.0.1:10961
  - Token:  see WEB_AUTH_TOKEN in ~/.egopulse/.env

If Discord or Telegram is enabled:
  - The bot responds to DMs out of the box.
  - To enable server/group responses, add channel/chat IDs to the YAML.
    See docs/channels.md for details.
```

---

## 5. 入力項目仕様

### 5.1 一覧

| Q | 項目 | 必須 | デフォルト | バリデーション |
|---|---|:---:|---|---|
| 1 | Agent Label | ○ | なし | 空文字拒否、表示名として妥当な長さ (1〜64 文字程度) |
| 1' | Agent ID | (自動) | label を自前 slugify | 英数字・ハイフンのみ、連続ハイフン圧縮、空結果拒否 (フォールバックで `"default"`) |
| 2 | Provider | ○ | なし | `PROVIDER_PRESETS` いずれか、または `Custom` |
| 2' | base_url | 条件付き | なし | Custom 選択時のみ Q2 の直後に聞く。`url::Url::parse` で検証、再入力 |
| 3 | Model | ○ | preset の `default_model` | preset 選択時: `models` リスト内であること。Custom 選択時: 手入力テキスト、空文字拒否 |
| 4 | API Key | △ | 空文字 | 常に入力ステップ表示。localhost 系は空欄でそのまま通す。非 localhost 系で空欄時は Y/N 確認 (no で再入力、yes で警告付きで進行) |
| 5 | Web Channel enabled | — | `yes` | 真偽値。無効時は `channels.web` エントリ自体を YAML に含めない (Discord/Telegram と一貫) |
| 5' | Web auth_token | (自動) | `generate_auth_token()` | ユーザー入力なし、実値は Review/Done で非表示 |
| 6 | Discord enabled | — | `no` | 真偽値 |
| 6' | Discord bot token | 条件付き | なし | `yes` 時は必須、空拒否 |
| 7 | Telegram enabled | — | `no` | 真偽値 |
| 7' | Telegram bot token | 条件付き | なし | `yes` 時は必須、空拒否 |

### 5.2 生成される YAML の構造

- `default_agent`: Q1 で生成した agent id
- `default_provider`: Q2 で選んだ provider id
- `agents.<id>.label`: Q1 の入力値
- `providers.<id>`: Q2/Q3/Q4 の値 (label, base_url, api_key, default_model, models)
- `channels.web`: Q5 の結果。`yes` の場合は `enabled: true, host=127.0.0.1, port=10961, auth_token` を保存。`no` の場合はエントリ自体を含めない (Discord/Telegram と一貫)
- `channels.discord`: Q6 の結果 (enabled, bots.default.token)
- `channels.telegram`: Q7 の結果 (enabled, bots.default.token)
- 秘匿値は `.env` に書き出し、YAML には `SecretRef` で参照 (現状仕様を維持)

### 5.3 既存設定の再編集時の挙動

`egopulse setup` を**既存設定が存在する状態**で実行した場合、各 Q のプロンプトは既存値を**デフォルトとして事前入力**する (現行仕様を維持)。

| Q | デフォルトとして事前入力される値 |
|---|---|
| Q1 Agent Label | 既存 `agents.<default_agent>.label` (無ければ空) |
| Q2 Provider | 既存 `default_provider` (preset に一致しない場合は `Custom` 扱いで `base_url` も事前入力) |
| Q3 Model | 既存 `providers.<id>.default_model` またはグローバル `default_model` |
| Q4 API Key | 既存 `.env` から解決した `providers.<id>.api_key` (解決不能なら空) |
| Q5 Web | 既存 `channels.web.enabled` (無ければ `yes`) |
| Q6 Discord | 既存 `channels.discord.enabled` と `bots.default.token` |
| Q7 Telegram | 既存 `channels.telegram.enabled` と `bots.default.token` |

- 既存 YAML のパースエラー時は Q1 の前に warn 表示 + Y/N 確認 (§3.1)
- ユーザーが Enter でそのまま進めば既存値を維持、入力し直せば上書き
- `WEB_AUTH_TOKEN` と `state_root` は事前入力の対象外だが、上書きされない (Plan テストリスト T20/T21 で保証)

---

## 6. 完了メッセージ仕様

### 6.1 表示項目

- 設定ファイル保存先 (`~/.egopulse/egopulse.config.yaml`)
- 既存設定があった場合、バックアップファイルパス
- 次ステップの案内:
  - `egopulse chat` — すぐチャット開始
  - `egopulse gateway install` — systemd サービス登録
  - `~/.egopulse/egopulse.config.yaml` 編集 — 詳細設定
  - `agents` セクション編集 — エージェント追加
- Web UI 有効化時:
  - アクセス URL (`http://127.0.0.1:10961`)
  - 認証トークンの参照先 (`~/.egopulse/.env` の `WEB_AUTH_TOKEN`)
- Discord / Telegram 有効化時:
  - DM は即利用可能
  - サーバー / グループで応答させるには YAML にチャンネル/チャット ID を追加が必要
  - 詳細は `docs/channels.md` を参照

### 6.2 明示しない項目

- API Key / トークン類の**実値** (セキュリティ)。Review でのマスク表示、Done での .env 参照案内のみ

---

## 7. コード観点の影響

### 7.1 削除対象

| 対象 | 場所 | 備考 |
|---|---|---|
| `SetupApp` 構造体と関連メソッド | `src/setup/mod.rs:86-338` | Navigate/Edit/Selector モード含む全 TUI 状態管理 |
| `init_terminal()` / `restore_terminal()` | `src/setup/mod.rs:711-778` | ratatui `Terminal` 初期化・イベントループ |
| `draw_*()` 系描画関数 | `src/setup/mod.rs:362-708` | `draw_fields`, `draw_selector_popup` 等 |
| `handle_*_key()` 系キーハンドラ | `src/setup/mod.rs:779-956` | `handle_navigate_key`, `handle_edit_key`, `handle_selector_key` |
| `SetupMode` enum | `src/setup/mod.rs` | Navigate / Edit / Selector の 3 モード |
| `read_setup_key()` | `src/setup/mod.rs` | crossterm event poll |
| `load_existing_config()` の TUI 的扱い | `src/setup/mod.rs:207-276` | パースエラー黙殺を廃止し warn 表示へ (下記残置で改修) |

推定削減: 約 900 行。

### 7.2 残置対象 (情報資産として流用)

| 対象 | 場所 | 用途 |
|---|---|---|
| `PROVIDER_PRESETS` 配列 | `src/setup/provider.rs:12-234` | 25 preset のデータ。そのまま参照 |
| `build_channel_configs()` | `src/setup/channels.rs:88-130` | ChannelConfig 生成ロジック。Web 強制有効化を廃止して `enabled: Some(user_choice)` へ |
| `generate_auth_token()` | `src/setup/channels.rs:135-139` | 32 bytes ランダム base64。そのまま流用 |
| `validate_fields()` | `src/setup/summary.rs:31-90` | バリデーションロジック。新しい入力項目に合わせて拡張 |
| `save_config()` | `src/setup/summary.rs:92-350` | YAML + `.env` 永続化。そのまま流用 |
| `backup_config()` | `src/setup/summary.rs:395-419` | 上書き前バックアップ。そのまま流用 |
| `MAX_CONFIG_BACKUPS = 50` | `src/setup/summary.rs:29` | バックアップ世代数。そのまま流用 |

### 7.3 新規追加

| 対象 | 場所 | 内容 |
|---|---|---|
| prompts 層 | `src/setup/prompts.rs` (新設) | `dialoguer` を用いたラッパー。provider 選択、model 選択、api key 入力、バリデーション等をカプセル化 |
| wizard フロー | `src/setup/wizard.rs` (新設) | Welcome → Q1〜Q7 → Review → Save → Additional Options → Done の順次制御。`run_setup_wizard()` の新本体 |
| slugify ユーティリティ | `src/setup/mod.rs` or 共通ユーティリティ | Agent Label から agent id を生成 |

### 7.4 既存エントリポイントの互換性

- `src/main.rs:97-101` の `setup::run_setup_wizard()` 呼び出しはそのまま維持 (シグネチャ互換)
- `docs/commands.md §1.1` の `egopulse setup` 行は説明更新のみ (「対話型設定ウィザード (TUI)」→「対話型設定プロンプト」)
- `docs/config.md §7` は刷新後に全面書き換え

---

## 8. 関連課題 (本メモのスコープ外)

以下は本リフレッシュとは独立に扱う。別途メモ / Plan を起す予定。

### 8.1 ローカル TUI チャネルの廃止と再構築

- `src/channels/tui.rs` (961 行) はアーキテクチャ上の限界 (ストリーミング非対応 / ツールコール不可視 / マークダウン非対応 / 画像破棄 / 単行入力 / セッション管理貧弱) があり、刷新ではなく**一度廃止して別ライブラリで再構築**する方針
- ratatui 以外の候補 (cursive / tui-realm / crossterm 直叩き / 他) を比較検討する必要あり
- 完了後に `ratatui` 依存を `Cargo.toml` から削除可能

### 8.2 docs 整備

本リフレッシュ実装完了後に以下を更新する:

- `docs/commands.md §1.1` — `egopulse setup` 行の説明を「対話型設定プロンプト」へ
- `docs/config.md §7` — 「セットアップウィザード」節を新仕様へ全面書き換え
- `README.md` — Getting Started の `egopulse setup` 記載があれば整合性確認