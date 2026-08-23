# EgoPulse コマンド仕様

CLI サブコマンドとチャットスラッシュコマンドの完全仕様。

## 目次

1. [CLI サブコマンド](#1-cli-サブコマンド)
2. [チャットスラッシュコマンド](#2-チャットスラッシュコマンド)
3. [コマンド共通ルール](#3-コマンド共通ルール)

---

## 1. CLI サブコマンド

ターミナルから `egopulse` コマンドとして実行する。プロセスの起動・設定・管理を担当する。

### 1.1 コマンド一覧

| コマンド | 引数 | 説明 | 設定必須 |
|---|---|---|:---:|
| `egopulse` | なし | ローカル TUI（セッションブラウザ + チャット） | 必須 |
| `egopulse setup` | なし | 対話型設定ウィザード（非シークレット入力は表示、API key / Bot Token は hidden） | 不要 |
| `egopulse -p [PROMPT]` | `[--session <SESSION>]` / `[--continue]` | 非対話プロンプト、応答だけを stdout に出力 | 必須 |
| `egopulse run` | なし | 有効チャネルを一括起動（前景実行） | 必須 |
| `egopulse gateway <ACTION>` | 下記参照 | systemd サービス管理 | 必須 |
| `egopulse sleep` | `[--agent <AGENT>]` | 手動 sleep batch を実行（長期記憶の処理） | 必須 |
| `egopulse events extract` | `[--agent] [--from] [--to]` | 過去セッションからエピソードイベントを再抽出（バックフィル） | 必須 |
| `egopulse update` | なし | 最新リリースに更新 | 不要 |

`-p` / `--print` は非対話モードを起動する。`PROMPT` を省略した場合は、TTY でない stdin から読み込む。`PROMPT` と stdin の両方がある場合は、両者を空行 2 つで連結する。`--session` は指定セッションを再開し、`--continue` は最終更新セッションを再開する。成功時は応答本文だけを stdout、ログとエラーは stderr に出力する。プロンプトがない場合は終了コード 2、それ以外のエラーは 1、Ctrl-C は 0 で終了する。引数なしの `egopulse` は TUI を起動する。

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
| `status` | サービスの稼働状態・メトリクス・ターン履歴・エラー詳細を表示（`--json` で JSON 出力） |

### 1.4 `egopulse sleep`

指定したエージェントに対して手動 sleep batch を実行する。sleep batch は長期記憶の処理（入力収集 → 監査記録 → 成功/失敗）を一括で行う。

| 項目 | 内容 |
|---|---|
| `--agent <AGENT>` | 対象エージェント ID。省略時は `config.default_agent` を使用 |
| 必須設定 | `egopulse.config.yaml` が存在し、有効な provider 設定があること |
| 終了コード | 成功時 `0`、AlreadyRunning 時 `1`（stderr にメッセージ出力） |
| 動作 | 新規メッセージ数が閾値（≤ 64）以下の場合はスキップして終了 |

### 1.5 `egopulse events extract`

過去セッションの会話履歴からエピソードイベントを抽出する。イベントテーブルが不完全な場合のバックフィルに使用する。

```bash
egopulse events extract [--agent <AGENT>] [--from <DATE>] [--to <DATE>]
```

| オプション | 説明 |
|---|---|
| `--agent` | 対象エージェント ID。省略時は `config.default_agent` を使用 |
| `--from` | 抽出開始日（RFC3339 または `YYYY-MM-DD`）。省略時は制限なし |
| `--to` | 抽出終了日（RFC3339 または `YYYY-MM-DD`）。省略時は制限なし |

`--to YYYY-MM-DD` はその日を含む（境界 = 翌日 00:00 exclusive）。同一期間の再実行は、以前の backfill 由来イベントを同一トランザクション内で置換する。通常の sleep 由来イベントは保持される。

```bash
egopulse events extract --from 2025-01-01 --to 2025-06-01
egopulse events extract --agent lyre --from 2025-03-01
```

### 1.6 `egopulse gateway status`

サービスの稼働状態を表示する。サービスが実行中の場合は認証なしの `/health` で liveness を確認し、Bearer 認証付きの `/api/status` と `/telemetry` から詳細情報を取得して表示する。認証失敗、timeout、JSON parse error の場合は `systemctl --user status` の出力にフォールバックする。`--json` フラグで JSON 形式の出力が得られる。

```bash
egopulse gateway status        # 人間可読形式
egopulse gateway status --json # JSON 形式
```

---

## 2. チャットスラッシュコマンド

Telegram / Discord / TUI / Web チャット のいずれかで `/` から始まるメッセージとして送信する。セッション管理・モデル切替・システム操作を担当する。

### 2.1 設計原則

- **非エージェントループ**: スラッシュコマンドはエージェントループに入らず、即座に処理されて応答を返す
- **チャネル非依存**: 同じコマンドセットが全チャネルで動作する
- **YAML 永続化**: provider/model の変更は即座に YAML 設定ファイルに永続化される

### 2.2 コマンド一覧

#### セッション管理

| コマンド | 引数 | 説明 |
|---|---|---|
| `/new` | なし | 新規セッションを開始。現在のセッション snapshot とメッセージ履歴をクリア |
| `/compact` | なし | 手動 Safety Compaction をトリガー。閾値に関わらず現在のセッションを要約 |

#### プロバイダー・モデル操作

`/provider` と `/model` は `--scope` オプションで操作対象を指定できる。省略時は現在のエージェントに適用される。スコープの値は `global` | `agent:<id>`。

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
| `agent:<id>`（または省略時） | `agents.<id>.provider` | `agents.<id>.model` |

#### スキル

| コマンド | 引数 | 説明 |
|---|---|---|
| `/skills` | なし | ロード済みスキル一覧を表示 |

#### システム操作

| コマンド | 引数 | 説明 |
|---|---|---|
| `/status` | なし | 現在のセッション・チャネル状態を表示（provider/model/session/skills） |
| `/restart` | なし | プロセスを再起動（未実装。`Not implemented yet.` を返す） |

### 2.3 各コマンドの動作詳細

#### `/new`

1. 現在のセッションの `messages_json` を `[]` にリセット（楽観排他）
2. メッセージ履歴は保持（`messages` / `tool_calls` レコードは削除しない）
3. 確認メッセージを返信: `"Session cleared."`
4. 次のメッセージから新しいセッションとして扱われる（同じ `SurfaceContext` で snapshot が空の状態から再構築）

#### `/compact`

1. 現在のセッションの全メッセージを読み込み
2. Safety Compaction の閾値チェックをバイパス（メッセージ数や推定 token に関わらず実行）
3. old / recent の 2 領域に分割し、old を reference-only summary へ畳み込み（recent はそのまま保持。詳細は [session-lifecycle.md §5](./session-lifecycle.md#5-safety-compaction) 参照）
4. アーカイブファイル（Markdown）を `<state_root>/runtime/groups/<channel>/<chat_id>/conversations/` に出力
5. 確認メッセージを返信: `"Compacted N messages to M."`

#### `/status`

**現在**のセッション・チャネル状態を表示する。`egopulse gateway status` がライブランタイム情報を参照するのに対し、`/status` はセッションコンテキスト内の動的な情報を表示する。

```
Status
Channel: telegram
Provider: openrouter
Model: gpt-5
Session: active (12 messages)
```

#### `/restart`

未実装。`"Not implemented yet."` を返すだけで、プロセス再起動・systemd 操作は行わない。

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

### 3.1 秘匿情報の取り扱い

スラッシュコマンドの応答では、API キーや Bot トークンの値を表示しない。

---
