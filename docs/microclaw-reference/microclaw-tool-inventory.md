# Pi / MicroClaw Tool Inventory

`egopulse` の phase2 以降で何を built-in tool として取り込むか判断するための棚卸し。`microclaw` と `pi` の両方を見て、最小セットと拡張セットを比較できるようにしている。

## 方針メモ

- `microclaw` には `ping` / `time` / `runtime_status` のような簡易ツールは存在しない。
- built-in は「作業」「探索」「外部アクセス」「記憶」「実行 orchestration」を一通り揃える思想になっている。
- `pi` は built-in をかなり絞っており、coding harness としての最小セットを重視している。
- 一部は常時 built-in ではなく、実行環境や設定に依存する。
  - `mcp_*`: 接続された MCP サーバーから動的生成
  - `clawhub_*`: feature flag 有効時のみ追加

## Pi Built-in Tools

`pi` の built-in tool 実装は `packages/agent` ではなく `packages/coding-agent/src/core/tools/` にある。README によると、デフォルトの built-in は `read`, `write`, `edit`, `bash` で、追加で `grep`, `find`, `ls` を有効化できる。

### `read`

- 役割: ファイル内容を読む。
- 位置づけ: デフォルト built-in。

### `write`

- 役割: ファイルを書き込む。
- 位置づけ: デフォルト built-in。

### `edit`

- 役割: ファイルを編集する。
- 位置づけ: デフォルト built-in。

### `bash`

- 役割: シェルコマンドを実行する。
- 位置づけ: デフォルト built-in。

### `grep`

- 役割: 内容検索をする。
- 位置づけ: オプション built-in。read-only mode に含まれる。

### `find`

- 役割: ファイル探索をする。
- 位置づけ: オプション built-in。read-only mode に含まれる。

### `ls`

- 役割: ディレクトリ一覧を返す。
- 位置づけ: オプション built-in。read-only mode に含まれる。

## Pi と MicroClaw の差

- `pi` は coding agent 向けの最小セットに絞っている。
- `microclaw` は coding tool に加えて、skills、web、memory、schedule、subagents、A2A まで built-in に含む。
- `egopulse` の phase1/phase2 を `pi` 基準で切るなら、まずは `read` / `write` / `edit` / `bash` を基本線とし、必要に応じて `grep` / `find` / `ls` を追加するのが自然。

## 1. MicroClaw Built-in Tools

ここから下は `microclaw` の built-in tool 一覧。`microclaw/src/tools/` の実装を読んで整理している。

### 1.1 Workspace / File Editing

### `read_file`

- 役割: ファイル内容を行番号付きで読む。
- 主入力: `path`, `offset`, `limit`
- 挙動: 作業ディレクトリ基準。path guard を通し、外部パスを拒否する。
- 備考: すでに `egopulse` phase1 実装済み。`microclaw` は isolation 設定つき。

### `write_file`

- 役割: ファイルを書き込む。親ディレクトリも自動作成し、既存内容は上書き。
- 主入力: `path`, `content`
- 挙動: 作業ディレクトリ基準。path guard あり。
- 特記事項: `SKILL.md` は専用 skills ディレクトリ以外への書き込みを拒否し、`sync_skills` 利用を促す。

### `edit_file`

- 役割: ファイル内の文字列を exact match で 1 箇所置換する。
- 主入力: `path`, `old_string`, `new_string`
- 挙動: `old_string` は 0 回なら失敗、複数回でも失敗。1 回だけ一致する必要がある。
- 備考: 破壊的な全文書き換えより安全寄り。

### `glob`

- 役割: glob パターンでファイルを探す。
- 主入力: `pattern`, `path`
- 挙動: 結果はソートして返す。500 件超は省略表示。

### `grep`

- 役割: regex でファイル内容を検索する。
- 主入力: `pattern`, `path`, `glob`
- 挙動: ファイルパス・行番号つきで返す。path guard あり。
- 備考: `read_file` だけだと探索が弱いので、実用性が高い。

### 1.2 Skill / Runtime Integration

### `activate_skill`

- 役割: 発見済み skill の完全な本文をロードする。
- 主入力: `skill_name`
- 備考: すでに `egopulse` phase1 実装済み。

### `sync_skills`

- 役割: GitHub 上の skill を取得してローカル skills ディレクトリへ同期する。
- 主入力: `source_repo`, `skill_name`, `git_ref`, `target_name`
- 挙動: raw GitHub URL 候補を複数試し、frontmatter を MicroClaw 向けに正規化して保存する。
- 備考: skill 配布・更新まで built-in に含める設計。

### `browser`

- 役割: headless browser を CLI 経由で操作する。
- 主入力: `command`, `timeout_secs`
- 挙動: chat ごとに browser profile を永続化し、cookie や localStorage を持続させる。
- 備考: `open`, `snapshot`, `click`, `fill`, `get`, `screenshot`, `pdf` など多機能。

### `bash`

- 役割: シェルコマンド実行。
- 主入力: `command`, `timeout_secs`
- 挙動: sandbox router・working_dir isolation・timeout を伴う。
- 備考: `microclaw` の中でも最も強い権限を持つ部類で、`egopulse` に入れるなら policy 設計が先。

### `mcp_{server}_{tool}`

- 役割: MCP server が公開するツールを namespaced 名で公開する動的 tool。
- 主入力: 各 MCP tool の input schema に従う。
- 挙動: 内部キー `__microclaw_*` は除去してからサーバーへ送る。
- 備考: 静的な built-in ではなく、接続中サーバー次第で変化する。

### 1.3 Web / Time / Utility

### `web_fetch`

- 役割: URL を取得し、HTML から本文テキストを抽出して返す。
- 主入力: `url`, `timeout_secs`
- 挙動: script/style 除去済み。本文は最大 20KB。URL validation と content validation が入る。

### `web_search`

- 役割: DuckDuckGo で検索する。
- 主入力: `query`, `timeout_secs`
- 挙動: タイトル・URL・snippet を返す。timeout は 1〜60 秒に clamp。

### `get_current_time`

- 役割: UTC と指定 timezone の現在時刻を返す。
- 主入力: `timezone`
- 挙動: `timezone` 未指定時は設定済み timezone を使う。

### `compare_time`

- 役割: 2 つの timestamp を比較し、前後関係と差分を返す。
- 主入力: `left`, `right`, `timezone`
- 挙動: RFC3339 だけでなく naive local datetime も扱う。

### `calculate`

- 役割: 算術式を評価する。
- 主入力: `expression`
- 挙動: 四則演算と括弧を扱う evaluator。

### 1.4 Memory / Planning

### `read_memory`

- 役割: 内部 AGENTS.md memory を読む。
- 主入力: `scope`, `chat_id`
- 挙動: `scope` は `global` / `bot` / `chat`。chat scope は認可チェックつき。
- 備考: raw をそのままユーザーへ出さず、要約して使う前提の説明が入っている。

### `write_memory`

- 役割: AGENTS.md memory に書き込む。
- 主入力: `scope`, `content`, `chat_id`
- 挙動: chat scope では sender ごとの person section を更新する実装。

### `todo_read`

- 役割: TODO リストを読む。
- 主入力: なし
- 備考: data 配下の task state を読む軽量 planner 補助。

### `todo_write`

- 役割: TODO リストを書き換える。
- 主入力: `todos`
- 備考: 長い作業の自己管理用途。

### `structured_memory_search`

- 役割: 構造化 memory を検索する。
- 主入力: 検索語やフィルタ条件
- 備考: DB / memory backend と接続。

### `structured_memory_delete`

- 役割: 構造化 memory を削除する。
- 主入力: memory identifier など

### `structured_memory_update`

- 役割: 構造化 memory を更新する。
- 主入力: 対象 identifier と更新内容

### 1.5 Messaging / Multi-Agent / Remote Agent

### `send_message`

- 役割: 現在の runtime から外部チャネルへ bot message を送信する。
- 主入力: 宛先 chat context と `message`
- 挙動: channel policy を通し、送信後は DB にも保存する。
- 備考: agent が自分から proactive に返答するための tool。

### `a2a_list_peers`

- 役割: 設定済み A2A peer を列挙する。
- 主入力: なし
- 挙動: `a2a.enabled` が false なら失敗。

### `a2a_send`

- 役割: remote MicroClaw peer に task / question を送る。
- 主入力: `peer`, `message`, `session_key`, `timeout_secs`
- 挙動: A2A HTTP protocol を使う。

### `sessions_spawn`

- 役割: 新しい session / subagent run を生成する。
- 主入力: session 生成条件、依頼本文など

### `subagents_list`

- 役割: subagent 一覧を返す。

### `subagents_info`

- 役割: 特定 subagent の状態や metadata を返す。

### `subagents_kill`

- 役割: subagent を停止する。

### `subagents_focus`

- 役割: 特定 subagent を focus 対象にする。

### `subagents_unfocus`

- 役割: focus を外す。

### `subagents_focused`

- 役割: 現在 focus 中の subagent を返す。

### `subagents_send`

- 役割: subagent にメッセージを送る。

### `subagents_orchestrate`

- 役割: 複数 subagent をまとめて起動・配分する orchestration tool。

### `subagents_log`

- 役割: subagent 実行ログを読む。

### `subagents_retry_announces`

- 役割: subagent announce / delivery の再試行。

### 1.6 Scheduling / Export

### `schedule_task`

- 役割: 将来実行する task を登録する。
- 主入力: `message` と schedule 条件
- スケジュール入力: absolute datetime または cron 系の指定
- 備考: human hint や validation を持っている。

### `list_scheduled_tasks`

- 役割: 登録済みタスク一覧を返す。

### `pause_scheduled_task`

- 役割: scheduled task を一時停止する。

### `resume_scheduled_task`

- 役割: paused task を再開する。

### `cancel_scheduled_task`

- 役割: scheduled task をキャンセルする。

### `get_task_history`

- 役割: task 実行履歴を返す。

### `list_scheduled_task_dlq`

- 役割: dead-letter queue に入った task を列挙する。

### `replay_scheduled_task_dlq`

- 役割: DLQ task を再実行キューへ戻す。

### `export_chat`

- 役割: chat history を markdown に export する。
- 主入力: `chat_id`, `path`

### 1.7 Optional / Feature-Flagged

### `clawhub_search`

- 役割: ClawHub 検索。
- 条件: `config.clawhub.agent_tools_enabled`

### `clawhub_install`

- 役割: ClawHub から skill / artifact を導入する。
- 条件: `config.clawhub.agent_tools_enabled`

## `egopulse` での採否を考えるための初期整理

### `pi` 最小思想に合わせる場合の phase1 完了ライン

- `read_file`
- `write_file`
- `edit_file`
- `bash`

### `pi` の read-only 補助まで含める場合

- `grep`
- `find`
- `ls`

### phase1 で残すべきだったもの

- `read_file`
- `activate_skill`

### phase2 の最有力候補

- `write_file`
- `edit_file`
- `glob`
- `grep`

### phase2 で入れるなら設計が先に必要なもの

- `bash`
- `browser`
- `web_fetch`
- `web_search`
- `sync_skills`

### 現時点では後回しが自然なもの

- `read_memory`
- `write_memory`
- `todo_read`
- `todo_write`
- `structured_memory_*`
- `send_message`
- `a2a_*`
- `schedule_*`
- `export_chat`
- `subagents_*`
- `mcp_*`
- `clawhub_*`
