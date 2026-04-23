# EgoPulse コマンド仕様書

EgoPulse の全コマンドインターフェースを統合した仕様書。CLI サブコマンドとチャットスラッシュコマンドは独立した別概念として定義する。

## 目次

1. [CLI サブコマンド](#1-cli-サブコマンド)
2. [チャットスラッシュコマンド](#2-チャットスラッシュコマンド)
3. [コマンド共通ルール](#3-コマンド共通ルール)
4. [インターフェース間比較マトリクス](#4-インターフェース間比較マトリクス)

---

## 1. CLI サブコマンド

ターミナルから `egopulse` コマンドとして実行する。プロセスの起動・設定・管理を担当する。

### 1.1 コマンド一覧

| コマンド | 引数 | 説明 | 設定必須 |
|---|---|---|:---:|
| `egopulse` | なし | ローカル TUI（セッションブラウザ + チャット） | 必須 |
| `egopulse setup` | なし | 対話型設定ウィザード（TUI） | 不要 |
| `egopulse ask <PROMPT>` | `[--session <SESSION>]` | 単発プロンプト、結果を stdout に出力 | 必須 |
| `egopulse chat` | `[--session <SESSION>]` | 永続化 CLI チャットセッション | 必須 |
| `egopulse run` | なし | 有効チャネルを一括起動（前景実行） | 必須 |
| `egopulse status` | `[--json]` | 起動時のシステム状態を表示（起動時スナップショットを参照） | 不要 |
| `egopulse gateway <ACTION>` | 下記参照 | systemd サービス管理 | 必須 |
| `egopulse update` | なし | 最新リリースに更新 | 不要 |

### 1.2 グローバルオプション

| オプション | 説明 |
|---|---|
| `--config <PATH>` | 設定ファイルのパス（絶対/相対） |
| `--version` | バージョン表示 |
| `--help` | ヘルプ表示 |

### 1.3 `egopulse gateway` アクション

| アクション | 説明 |
|---|---|
| `install` | systemd ユニットを作成・有効化・起動 |
| `start` | インストール済みサービスを起動 |
| `stop` | サービスを停止 |
| `restart` | サービスを再起動 |
| `uninstall` | サービスの無効化・停止・削除 |
| `status` | `systemctl status` の出力を表示 |

### 1.4 `egopulse status` の出力内容

**起動時**のシステム状態を表示する。プロセス起動時に書き出された `status.json`（スナップショット）を読み取るため、起動時点の情報がそのまま反映される。プロセス停止後でも参照可能。実行中のセッション情報は含まれない。

```
EgoPulse v0.1.0  PID 293918  started 2026-04-12 14:03:58
Config: /root/.egopulse/egopulse.config.yaml

Provider: openrouter / gpt-5

Channels
  web      enabled (127.0.0.1:10961)
  discord  enabled
  telegram enabled

MCP Servers (1/2 connected)
  ✓ context7  stdio   2 tools
  ✗ github    connection timed out after 30s
```

---

## 2. チャットスラッシュコマンド

Telegram / Discord / CLI チャット / TUI / Web チャット のいずれかで `/` から始まるメッセージとして送信する。セッション管理・モデル切替・システム操作を担当する。

### 2.1 設計原則

- **非エージェントループ**: スラッシュコマンドはエージェントループに入らず、即座に処理されて応答を返す
- **チャネル非依存**: 同じコマンドセットが全チャネルで動作する
- **YAML 永続化**: provider/model の変更は即座に YAML 設定ファイルに永続化される

### 2.2 コマンド一覧

#### セッション管理

| コマンド | 引数 | 説明 |
|---|---|---|
| `/new` | なし | 新規セッションを開始。現在のセッション snapshot とメッセージ履歴をクリア |
| `/compact` | なし | 手動 compaction をトリガー。閾値に関わらず現在のセッションを要約 |

#### プロバイダー・モデル操作

`/provider` と `/model` は `--scope` オプションで操作対象を指定できる。省略時はメッセージの送信元チャネルから自動的に推論される（Telegram → `telegram`、Discord → `discord`、CLI/TUI → `global`）。スコープの値は `global` | `web` | `discord` | `telegram`。

| コマンド | 引数 | 説明 |
|---|---|---|
| `/providers` | なし | 全プロバイダー一覧を表示（アクティブマーク付き） |
| `/provider` | なし | 現在のスコープのプロバイダー/モデルを表示 |
| `/provider <name>` | `[--scope <SCOPE>]` | プロバイダーを切り替え。モデルは新しいプロバイダーのデフォルトにリセット |
| `/provider reset` | `[--scope <SCOPE>]` | プロバイダー設定をデフォルトにリセット |
| `/models` | `[--scope <SCOPE>]` | 現在のプロバイダーで利用可能なモデル一覧を表示 |
| `/model` | なし | 現在のスコープのプロバイダー/モデルを表示 |
| `/model <name>` | `[--scope <SCOPE>]` | モデルを切り替え |
| `/model reset` | `[--scope <SCOPE>]` | モデル設定をプロバイダーデフォルトにリセット |

**スコープと設定の対応:**

| スコープ | `/provider <name>` の更新先 | `/model <name>` の更新先 |
|---|---|---|
| `global` | `config.default_provider` | `config.default_model` |
| `web` | `channels.web.provider` | `channels.web.model` |
| `discord` | `channels.discord.provider` | `channels.discord.model` |
| `telegram` | `channels.telegram.provider` | `channels.telegram.model` |

**モデル解決チェーン**（実際に使用されるモデルの決定順序）:

```
channel.model（チャネル固有モデル指定）
    ↓ null の場合
config.default_model（グローバルモデル上書き）
    ↓ null の場合
provider.default_model（プロバイダーのデフォルトモデル）
```

#### スキル

| コマンド | 引数 | 説明 |
|---|---|---|
| `/skills` | なし | ロード済みスキル一覧を表示 |

#### システム操作

| コマンド | 引数 | 説明 |
|---|---|---|
| `/status` | なし | 現在のセッション・チャネル状態を表示（provider/model/session/skills） |
| `/restart` | なし | プロセスを再起動 |

### 2.3 各コマンドの動作詳細

#### `/new`

1. 現在のセッションの session snapshot を削除
2. メッセージ履歴をクリア
3. 確認メッセージを返信: `"Session cleared. Starting fresh."`
4. 次のメッセージから新しいセッションとして扱われる（同じ `SurfaceContext` で snapshot が空の状態から再構築）

#### `/compact`

1. 現在のセッションの全メッセージを読み込み
2. compaction の閾値チェックをバイパス（メッセージ数に関わらず実行）
3. 古いメッセージを LLM で要約し、recent メッセージはそのまま保持
4. アーカイブファイル（Markdown）を `data_dir/groups/<channel>/<chat_id>/conversations/` に出力
5. 確認メッセージを返信: `"Compacted N messages."`

#### `/status`

**現在**のセッション・チャネル状態を表示する。`egopulse status` が起動時のスナップショットを参照するのに対し、`/status` は実行時に動的に情報を取得する。

```
Status
Channel: telegram
Provider: openrouter
Model: gpt-5
Session: active (12 messages)
```

#### `/restart`

| 実行環境 | 動作 |
|---|---|
| systemd サービス (`egopulse gateway`) | `systemctl restart egopulse` を実行 |
| フォアグラウンド (`egopulse run`) | `std::process::exit(0)` で終了（supervisor が再起動） |
| TUI / CLI チャット | `std::process::exit(0)` で終了 |

再起動後、既存のセッションは DB に永続化されているため自動的に復元される。

#### `/skills`

`skills_dir` から読み込まれたスキルの一覧を表示。各スキルの name と description をリスト形式で返す。

```
Available skills:
- pdf (PDF document processing)
- docx (Word document processing)
- weather (Weather lookup via wttr.in)
```

### 2.4 コマンドハンドリングルール

- `/` で始まる入力はすべてスラッシュコマンドとして処理される
- Telegram グループでは `@botname /model` のようにメンションを前置してもコマンドとして認識される
- Discord ギルドでは `@EgoPulse /model` のようにメンションを前置してもコマンドとして認識される
- スラッシュコマンドはエージェントループに入らず、セッション履歴にも記録されない
- 未定義のコマンドは `"Unknown command."` を返す

---

## 3. コマンド共通ルール

### 3.1 設定変更の即時性

| 変更対象 | 即時反映 | 再起動要 |
|---|:---:|:---:|
| `default_provider` / `default_model` | ✓ | |
| `channels.*.provider` / `channels.*.model` | ✓ | |
| `providers` の内容 | ✓ | |
| `compaction_*` / `max_*` | ✓ | |
| `channels.*.enabled` | | ✓ |
| `channels.*.bot_token` | | ✓ |
| `channels.web.host` / `channels.web.port` | | ✓ |
| `log_level` | | ✓ |

### 3.2 秘匿情報の取り扱い

スラッシュコマンドの応答では、API キーや Bot トークンの値を表示しない。

---

## 4. インターフェース間比較マトリクス

### 4.1 操作のカバレッジ

| 操作 | CLI サブコマンド | スラッシュコマンド | WebUI | YAML（手動） |
|---|:---:|:---:|:---:|:---:|
| プロセス起動 | ✓ (`run`) | | | |
| セッションクリア | | ✓ (`/new`) | | |
| 手動 compaction | | ✓ (`/compact`) | | |
| プロバイダー切替 | | ✓ (`/provider`) | ✓ | R/W |
| モデル切替 | | ✓ (`/model`) | ✓ | R/W |
| プロバイダー一覧 | | ✓ (`/providers`) | ✓ | R |
| モデル一覧 | | ✓ (`/models`) | ✓ | R |
| スキル一覧 | | ✓ (`/skills`) | | |
| 現在の状態 | | ✓ (`/status`) | | |
| 起動時の状態 | ✓ (`status`) | | ✓ (`/api/health`) | |
| プロセス再起動 | ✓ (`gateway restart`) | ✓ (`/restart`) | | |
| 初回セットアップ | ✓ (`setup`) | | | R/W |
| systemd 管理 | ✓ (`gateway`) | | | |

### 4.2 実行コンテキスト別の利用可能コマンド

| コマンド | ターミナル | TUI | CLI チャット | Web チャット | Telegram | Discord |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| `egopulse run` | ✓ | | | | | |
| `egopulse status` | ✓ | | | | | |
| `egopulse setup` | ✓ | | | | | |
| `/new` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/compact` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/providers` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/provider` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/models` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/model` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/skills` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/status` | | ✓ | ✓ | ✓ | ✓ | ✓ |
| `/restart` | | ✓ | ✓ | ✓ | ✓ | ✓ |

### 4.3 コマンド UI の実装詳細

#### CommandDef レジストリ

全チャネルのコマンド定義は `slash_commands.rs` の `CommandDef` 配列（`all_commands()`）に一元化されている。各チャネルはこのレジストリを通じてコマンドメタデータ（名前・説明・使用法）を参照する。コマンド定義の重複を排除し、単一ソースオブトゥルース（Single Source of Truth）を実現している。

#### Discord: Application Commands (ネイティブ UI)

Discord はテキストベースの `/` 入力に加え、Discord Application Commands（Interactions API）を登録している。Bot 起動時（`ready` イベント）に `Command::set_global_commands` で一括登録し、`interaction_create` イベントで応答する。これにより Discord ネイティブのオートコンプリート UI が提供される。

- **登録タイミング**: Bot 起動時の `ready` ハンドラ
- **応答方式**: `CreateInteractionResponse::Message` で即座に応答
- **フォールバック**: テキストベースの `/` 入力も引き続きサポート

#### WebUI: コマンドサジェスト

Web チャットでは `/` 入力時にクライアントサイドでコマンド候補をポップアップ表示する。`Composer` コンポーネントが `/` で始まる入力を検知し、`filterCommands()` で候補をフィルタして `CommandSuggest` コンポーネントに渡す。

- **キーボード操作**: `↑↓` で候補選択、`Tab` / `Enter` で確定、`Escape` で閉じる
- **マウス操作**: クリックで選択
- **コマンド定義**: `commands.ts` にハードコード（コマンド数が少なく変更頻度も低いため）
