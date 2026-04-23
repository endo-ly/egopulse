# MicroClaw ユーザー定義システムプロンプトファイル — 仕様リファレンス

MicroClaw がエージェント挙動の制御に用いるユーザー定義ファイル群の仕様をまとめたリファレンス。
EgoPulse への実装検討のベースラインとして利用する。

> ソース: `/root/workspace/microclaw/`

---

## 目次

1. [全体アーキテクチャ](#1-全体アーキテクチャ)
2. [System Prompt 組立順序](#2-system-prompt-組立順序)
3. [SOUL.md — 人格定義](#3-soulmd--人格定義)
4. [AGENTS.md — 永続メモリ](#4-agentsmd--永続メモリ)
5. [SKILL.md — スキルシステム](#5-skillmd--スキルシステム)
6. [HOOK.md — イベントフック](#6-hookmd--イベントフック)
7. [CLAUDE.md / AGENTS.md (プロジェクトルート)](#7-claudemd--agentsmd-プロジェクトルート)
8. [ファイル配置の全体像](#8-ファイル配置の全体像)
9. [Config フィールド一覧](#9-config-フィールド一覧)
10. [設計上の特徴](#10-設計上の特徴)

---

## 1. 全体アーキテクチャ

MicroClaw は以下の 5 種類のユーザー定義ファイルでエージェントの挙動を制御する。

| # | ファイル | 目的 | 読込タイミング | スコープ |
|---|---|---|---|---|
| 1 | **SOUL.md** | Bot の人格・価値観・口調の定義 | 毎ターン (system prompt 先頭) | グローバル / チャネル / チャット毎 |
| 2 | **AGENTS.md** (永続メモリ) | 会話を跨いだ永続的な記憶 | 毎ターン (system prompt 内) | グローバル / ボット / チャット毎 |
| 3 | **SKILL.md** | 特化機能の指示書 (Anthropic Skills 互換) | オンデマンド (`activate_skill` 時) | グローバル |
| 4 | **HOOK.md** | イベント駆動の挙動横断 (LLM/ツール呼び出し前後) | イベント時 | グローバル |
| 5 | **CLAUDE.md** | 開発コンテキスト (ランタイムでは不使用) | 不使用 | プロジェクトルート |

エージェントループの実行フロー (`process_with_agent_with_events_guarded`, `src/agent_engine.rs`):

```
1. チャット毎のターンロックを取得
2. SOUL.md をロード (load_soul_content)
3. メモリコンテキストを構築 (build_db_memory_context)
4. スキルカタログを構築 (skills.build_skills_catalog)
5. System prompt を組立 (build_system_prompt)
6. Hook: BeforeLLMCall を実行 → block/modify 判定
7. LLM API を呼び出し (ツールスキーマ付き)
8. tool_use レスポンスの場合:
   a. Hook: BeforeToolCall を実行
   b. ツールを実行
   c. Hook: AfterToolCall を実行
   d. 結果をメッセージに追加 → 6 に戻る
9. end_turn の場合:
   a. セッションを永続化
   b. レスポンスを返却
```

---

## 2. System Prompt 組立順序

`build_system_prompt()` (`src/agent_engine.rs:1669-1830`) で以下の順序で構築される。

```
┌─────────────────────────────────────────────────────┐
│ 1. <soul> SOUL.md 内容 </soul>                      │  ← 人格定義
│    "Your name is {bot_username}. Current channel:    │
│     {caller_channel}."                               │
│                                                     │
│ 2. Identity rules + Capabilities                    │  ← 固定文字列
│    - bash, read_file, write_file, edit_file         │
│    - glob, grep, memory tools, web tools            │
│    - schedule tools, subagents, todo                │
│                                                     │
│ 3. Channel-specific extension                       │  ← channels::system_prompt_extension()
│                                                     │
│ 4. # Memories                                       │  ← AGENTS.md + 構造化メモリ
│    <structured_memories>                            │     (3層インジェクション)
│      L0 Identity / L1 Essential / L2 Relevant       │
│    </structured_memories>                           │
│                                                     │
│ 5. # Agent Skills                                   │  ← SKILL.md カタログ
│    <available_skills>                               │     (名前一覧のみ、本文は遅延ロード)
│    </available_skills>                              │
│                                                     │
│ 6. Plugin context injections (任意)                 │  ← プラグインからの追加セクション
│                                                     │
│ 7. Execution playbook + reliability requirements    │  ← 固定文字列
└─────────────────────────────────────────────────────┘
```

---

## 3. SOUL.md — 人格定義

> ソース: `src/agent_engine.rs:1577-1667` (`load_soul_content`)

### 役割

Bot の性格、価値観、作業スタイルを Markdown で定義する。system prompt の先頭に `<soul>` タグで注入され、エージェントの「個性」を決定する最も優先度の高い指示。

### ファイルの内容例 (プロジェクトルートの `SOUL.md`)

```markdown
# Soul

I am a capable, action-oriented AI assistant that lives inside chat channels.

## Personality

- I prefer doing over discussing. When asked to do something, I reach for tools first and explain after.
- I am direct and concise. I don't pad responses with filler or caveats.
- I have a calm confidence. I don't overqualify my abilities, but I'm honest when I hit a wall.
- I adapt my language to match the user — casual when they're casual, precise when they need precision.
- I have a dry sense of humor. A well-placed quip makes the work lighter, but I never let jokes get in the way of getting things done.
- I'm optimistic by default. Problems are puzzles, errors are clues, and setbacks are just plot twists.

## Values

- **Reliability over impressiveness.** I'd rather do a simple thing correctly than attempt something flashy and fail.
- **Transparency.** If a tool fails or I'm uncertain, I say so plainly — but with a smile, not a shrug.
- **Respect for context.** I remember what matters to the user and use that knowledge thoughtfully.
- **Efficiency.** I don't waste the user's time with unnecessary back-and-forth.

## Working style

- For complex tasks, I break them into steps and track progress.
- I execute tools to verify rather than guess.
- I report outcomes, not intentions — "done" beats "I'll try".
- When something fails, I report the failure and propose a next step.
```

### 読込フォールバックチェーン

`load_soul_content()` は以下の順序でファイルを探索し、**最初に見つかったものを使用**する:

| 優先度 | 探索パス | 設定方法 | 備考 |
|---|---|---|---|
| 1 (最高) | `channels.<channel>.accounts.<id>.soul_path` | Config (アカウント固有) | マルチボット環境でボット毎に人格を分ける場合 |
| 2 | `channels.<channel>.soul_path` | Config (チャネル固有) | チャネルレベルのフォールバック |
| 3 | `soul_path` | Config (グローバル) | YAML トップレベル |
| 4 | `<data_dir>/SOUL.md` | ファイル配置 | データディレクトリ直下 |
| 5 | `./SOUL.md` | ファイル配置 | カレントディレクトリ (プロジェクトルート) |
| 6 (チャット固有) | `<data_dir>/runtime/groups/{chat_id}/SOUL.md` | ファイル配置 | **グローバル SOUL を完全に上書き** |

### 注入方式

SOUL.md が見つかった場合:

```xml
<soul>
{SOUL.md の内容}
</soul>

Your name is {bot_username}. Current channel: {caller_channel}.
```

SOUL.md が見つからない場合 (デフォルトフォールバック):

```
You are {bot_username}, a helpful AI assistant across chat channels.
You can execute tools to help users with tasks.

Current channel: {caller_channel}.
```

### `souls_dir` — 複数人格ディレクトリ

Config の `souls_dir` (デフォルト: `<data_dir>/souls`) に複数の人格ファイルを配置可能。

```
<data_dir>/souls/
    work.md
    casual.md
    telegram-bot.md
```

チャネル/アカウントの `soul_path` にファイル名のみ指定した場合、`souls_dir` から解決される。
例: `soul_path: "work"` → `<data_dir>/souls/work.md` をロード。

---

## 4. AGENTS.md — 永続メモリ

> ソース: `crates/microclaw-storage/src/memory.rs`, `src/memory_service.rs`, `src/tools/memory.rs`

### 役割

エージェントがユーザーの好みや文脈をセッションを跨いで記憶する仕組み。ファイルベース (AGENTS.md) と構造化 (SQLite) の 2 層で構成される。

### ファイルベースメモリ (AGENTS.md)

エージェントが `read_memory` / `write_memory` ツールで直接読み書きする Markdown ファイル。

#### メモリパス階層

```
<data_dir>/runtime/groups/
    AGENTS.md                              # グローバル (全チャット共通)
    {channel}/
        AGENTS.md                          # チャネル/ボット固有
        {chat_id}/
            AGENTS.md                      # チャット固有
```

| スコープ | パス | ツールでの指定 | 利用場面 |
|---|---|---|---|
| グローバル | `runtime/groups/AGENTS.md` | `scope: "global"` | 全チャットで共有する記憶 |
| ボット | `runtime/groups/{channel}/AGENTS.md` | `scope: "bot"` | チャネル固有の記憶 |
| チャット | `runtime/groups/{channel}/{chat_id}/AGENTS.md` | `scope: "chat"` | チャット固有の記憶 |

#### ツール

- **`read_memory`**: 指定スコープの AGENTS.md の内容を返却
- **`write_memory`**: 指定スコープの AGENTS.md に内容を書き込み。同時に SQLite 構造化メモリにも upsert

### 構造化メモリ (SQLite `memories` テーブル)

バックグラウンドの **Reflector** が会話履歴から事実を自動抽出し、SQLite に永続化する。

#### レコード構造

| フィールド | 説明 |
|---|---|
| `id` | 主キー |
| `chat_id` | チャットID (NULL = グローバル) |
| `chat_channel` | チャネル |
| `external_chat_id` | 外部チャットID |
| `category` | カテゴリ (`PROFILE`, `PREFERENCE`, `FACT` 等) |
| `content` | メモリ内容 |
| `confidence` | 信頼度 (0.0 ~ 1.0) |
| `source` | 出典 |
| `last_seen` | 最終確認日時 |
| `is_archived` | ソフトアーカイブフラグ |
| `embedding_model` | 埋め込みモデル (sqlite-vec 有効時) |

#### 特記事項

- **明示的記憶**: ユーザーが "remember ..." / "记住..." と指示した場合、Reflector をバイパスして直接 upsert (高速パス)
- **品質ゲート**: 低品質・ノイズの多いメモリは信頼度スコアでフィルタリング
- **スーパーセッド**: 新しい記憶が古い記憶を置き換える関係を `memory_supersede_edges` テーブルで管理
- **監査ログ**: 全書き込みは `<data_dir>/runtime/wal/memory_writes.jsonl` に記録

### 3層インジェクション

`build_db_memory_context()` (`src/memory_service.rs:369-`) がメモリを system prompt に注入する際、トークンバジェットを 3 層に分割する:

| レイヤ | 予算割合 (デフォルト) | 内容 | 選択基準 |
|---|---|---|---|
| **L0 Identity** | 20% (`memory_l0_identity_pct`) | PROFILE カテゴリ | 最高信頼度のプロファイル情報 |
| **L1 Essential** | 30% (`memory_l1_essential_pct`) | 高信頼度メモリ | 信頼度降順 |
| **L2 Relevance** | 残り | クエリマッチメモリ | キーワード一致 or セマンティック KNN |

#### 出力フォーマット

```xml
<structured_memories>
# Identity
[PROFILE] [global] ユーザーはPythonを好む
# Essential
[PREFERENCE] [chat] テストにはpytestを使用
# Relevant
[FACT] [chat] プロジェクトはRustで構築されている
(+15 memories available via structured_memory_search tool)
</structured_memories>
```

#### トークンバジェット設定

| Config フィールド | デフォルト | 説明 |
|---|---|---|
| `memory_token_budget` | `1500` | L0+L1+L2 の合計予算 (トークン換算) |
| `memory_l0_identity_pct` | `20` | L0 に割り当てる予算の割合 (%) |
| `memory_l1_essential_pct` | `30` | L1 に割り当てる予算の割合 (%) |
| `memory_max_entries_per_chat` | `200` | チャット毎の最大アクティブメモリ数 (0 = 無制限) |
| `memory_max_global_entries` | `500` | グローバルの最大アクティブメモリ数 (0 = 無制限) |

### ナレッジグラフ (任意)

`knowledge_graph` テーブルに subject-predicate-object のトリプルを保存。
Reflector が会話から関係性を抽出し、`knowledge_graph_query` / `knowledge_graph_add` ツールでアクセス可能。

---

## 5. SKILL.md — スキルシステム

> ソース: `src/skills.rs`

### 役割

Anthropic Agent Skills 互換のモジュール形式指示書。エージェントに特化機能を追加する。

### 配置と発見

```
<data_dir>/skills/
    pdf/
        SKILL.md                  # 必須: name, description + 指示内容
    docx/
        SKILL.md
    my-custom-skill/
        SKILL.md
```

- デフォルトディレクトリ: `<data_dir>/skills`
- Config 上書き: `skills_dir` フィールド
- 組み込みスキル: `skills/built-in/*/SKILL.md` (コードに同梱)

### ファイルフォーマット

YAML frontmatter + Markdown body:

```markdown
---
name: pdf
description: Create, edit, and analyze PDF documents
platforms: [darwin, linux, windows]
deps: [python3]
---

# PDF Skill

When asked to work with PDFs, follow these instructions:
...
```

#### Frontmatter フィールド

| フィールド | 必須 | 型 | 説明 |
|---|---|---|---|
| `name` | 任意 | `string` | スキルID。未指定時はディレクトリ名 |
| `description` | 必須 | `string` | スキルの短い説明 (カタログ表示用) |
| `platforms` | 任意 | `[string]` | 対応プラットフォーム (`darwin`, `linux`, `windows`) |
| `deps` | 任意 | `[string]` | 必須コマンド (PATH 内を確認) |
| `compatibility.os` | 任意 | `[string]` | `platforms` のエイリアス |
| `compatibility.deps` | 任意 | `[string]` | `deps` のエイリアス |
| `source` | 任意 | `string` | スキルの出典 (ClawHub 等) |
| `version` | 任意 | `string` | バージョン |
| `env_file` | 任意 | `string` | 環境変数ファイル |

### カタログ生成

`build_skills_catalog()` (`src/skills.rs:280-317`) が system prompt 用のカタログを生成する。

**通常モード** (スキル数 ≤ 20):

```xml
<available_skills>
- pdf: Create, edit, and analyze PDF documents
- docx: Create and modify Word documents
- weather: Quick weather lookup
</available_skills>
```

**コンパクトモード** (スキル数 > 20):

```xml
<available_skills>
- pdf
- docx
- weather
- (compact mode: use activate_skill to load full instructions)
- ... (12 additional skills omitted for prompt budget)
</available_skills>
```

- 最大表示件数: 40 (`MAX_SKILLS_CATALOG_ITEMS`)
- コンパクトモード閾値: 21件以上 (`COMPACT_SKILLS_MODE_THRESHOLD`)
- 説明文の最大長: 120文字 (`MAX_SKILL_DESCRIPTION_CHARS`)

### ライフサイクル

1. **発見**: `discover_skills()` が `skills/*/SKILL.md` をスキャン
2. **フィルタリング**: プラットフォーム (OS) と依存コマンド (deps) で自動フィルタ
3. **カタログ注入**: system prompt に名前一覧を注入 (~100 トークン/スキル)
4. **遅延ロード**: ユーザーの要求に合わせて `activate_skill` ツールで本文をロード
5. **有効/無効**: `skills_state.json` で個別に制御。`/reload-skills` で再スキャン

---

## 6. HOOK.md — イベントフック

> ソース: `src/hooks.rs`, `docs/hooks/HOOK.md`

### 役割

エージェントの LLM 呼び出しやツール実行の前後に介入し、allow / block / modify の判定を行う仕組み。外部スクリプトとして実装するため、Rust コードの変更なしに挙動をカスタマイズできる。

### 配置と発見

```
hooks/
    block-bash/
        HOOK.md                      # フック定義
        hook.sh                      # フックスクリプト
    redact-tool-output/
        HOOK.md
        hook.sh
    filter-global-structured-memory/
        HOOK.md
        hook.sh
```

探索ディレクトリ (優先順):
1. `<data_dir>/hooks/`
2. `./hooks/` (カレントディレクトリ)

### HOOK.md フォーマット

```markdown
---
name: block-bash
description: Block bash tool usage
events: [BeforeToolCall]
command: "sh hook.sh"
enabled: true
timeout_ms: 1500
priority: 100
---

Free-form notes for maintainers.
```

#### Frontmatter フィールド

| フィールド | 必須 | デフォルト | 説明 |
|---|---|---|---|
| `name` | 任意 | ディレクトリ名 | フックID |
| `description` | 任意 | `""` | 人間可読な説明 |
| `events` | **必須** | — | 対応イベントの配列 |
| `command` | **必須** | — | フックディレクトリ内で実行するシェルコマンド |
| `enabled` | 任意 | `true` | デフォルトの有効状態 |
| `timeout_ms` | 任意 | `1500` | 実行タイムアウト (10 ~ 120,000ms) |
| `priority` | 任意 | `100` | 実行優先度 (低い値ほど先に実行) |

### イベント種別

| イベント | タイミング | stdin ペイロードの主なフィールド |
|---|---|---|
| `BeforeLLMCall` | LLM API 呼び出しの直前 | `system_prompt`, `iteration`, `messages_len`, `tools_len`, `chat_id`, `caller_channel` |
| `BeforeToolCall` | ツール実行の直前 | `tool_name`, `tool_input`, `iteration`, `chat_id`, `caller_channel` |
| `AfterToolCall` | ツール実行の直後 | `tool_name`, `tool_input`, `result` (content, is_error, status_code, duration_ms, ...), `chat_id`, `caller_channel` |

### I/O 契約

**Input**: フックランタイムが JSON オブジェクトを stdin に書き込む。

**Output**: フックコマンドは stdout に JSON を出力する必要がある。

#### Allow (許可)

```json
{"action": "allow"}
```

#### Block (ブロック)

```json
{"action": "block", "reason": "policy blocked"}
```

ブロックされた場合、エージェントループは当該操作を中止し、`reason` をエージェントに通知する。

#### Modify (変更)

```json
{"action": "modify", "patch": {"system_prompt": "..."}}
```

パッチフィールドはイベント種別に応じて異なる:

| イベント | 変更可能フィールド |
|---|---|
| `BeforeLLMCall` | `system_prompt` (string) |
| `BeforeToolCall` | `tool_input` (object) |
| `AfterToolCall` | `content` (string), `is_error` (bool), `error_type` (string), `status_code` (number) |

### 実行フロー

```
HookManager.run(event, payload)
  │
  ├── 1. イベントにマッチするフックを収集
  ├── 2. priority 昇順でソート (低い値が先)
  ├── 3. 有効なフックのみ実行
  │     ├── サブプロセスとして起動 (sh -lc "{command}")
  │     ├── 環境変数: MICROCLAW_HOOK_EVENT, MICROCLAW_HOOK_NAME
  │     ├── stdin に JSON ペイロードを書き込み
  │     ├── timeout_ms 内に stdout から JSON レスポンスを読み取り
  │     └── アクション判定:
  │         ├── "allow" → 次のフックへ
  │         ├── "block" → 即座に Block 結果を返却
  │         └── "modify" → パッチを収集して次のフックへ
  └── 4. 全フック完了後、Allow { patches } を返却
```

### 実行制約

| パラメータ | デフォルト | 上限 | 説明 |
|---|---|---|---|
| `max_input_bytes` | 128 KB | 4 MB | stdin に書き込むペイロードの最大サイズ |
| `max_output_bytes` | 64 KB | 2 MB | stdout から読み取るレスポンスの最大サイズ |
| `timeout_ms` | 1500 ms | 120,000 ms | フックコマンドの実行タイムアウト |

### 状態管理

- 有効/無効状態: `<data_dir>/runtime/hooks_state.json` (`{"hook-name": false}` の形式)
- 監査ログ: SQLite `audit_logs` テーブルに `actor=hook, action={event}, status={allow|block|modify|error}` を記録
- CLI: `microclaw hooks list`, `microclaw hooks info <name>`, `microclaw hooks enable/disable <name>`

### 組み込みフック例

| フック | イベント | 動作 |
|---|---|---|
| `block-bash` | `BeforeToolCall` | `bash` ツールの実行をブロック |
| `block-global-memory` | `BeforeToolCall` | グローバルスコープのメモリ書き込みをブロック |
| `filter-global-structured-memory` | `BeforeToolCall` | グローバル構造化メモリの内容をフィルタリング |
| `redact-tool-output` | `AfterToolCall` | ツール出力に含まれる機密情報をマスク |

---

## 7. CLAUDE.md / AGENTS.md (プロジェクトルート)

### 役割

プロジェクトルートに配置する AI コーディングアシスタント向けのコンテキストファイル。**ランタイム (エージェントループ) では読み込まれない**。

| ファイル | 対象 | 内容 |
|---|---|---|
| `CLAUDE.md` | Claude Code 等 | プロジェクト固有の開発コンテキスト |
| `AGENTS.md` | 汎用 AI エージェント | プロジェクト概要、技術スタック、ソース索引 |

MicroClaw のプロジェクトルートの `AGENTS.md` には以下が含まれる:
- プロジェクト概要
- 技術スタック (Rust, Tokio, SQLite 等)
- ソース索引 (`src/` + `crates/` のファイル一覧と役割)
- ツールシステムの説明
- メモリアーキテクチャの概要
- ビルド・テストコマンド

---

## 8. ファイル配置の全体像

```
プロジェクトルート/
    SOUL.md                              # デフォルトの人格ファイル
    AGENTS.md                            # プロジェクト概要 (開発補助用)
    CLAUDE.md                            # 開発コンテキスト (開発補助用)
    microclaw.config.yaml                # メイン設定ファイル

    hooks/                               # イベントフック定義
        block-bash/
            HOOK.md                      # フック定義 (frontmatter + 自由記述)
            hook.sh                      # フックスクリプト
        redact-tool-output/
            HOOK.md
            hook.sh
        filter-global-structured-memory/
            HOOK.md
            hook.sh
        block-global-memory/
            HOOK.md
            hook.sh

    skills/                              # 組み込みスキル
        built-in/
            pdf/SKILL.md
            docx/SKILL.md
            xlsx/SKILL.md
            pptx/SKILL.md
            skill-creator/SKILL.md
            apple-notes/SKILL.md
            apple-reminders/SKILL.md
            apple-calendar/SKILL.md
            weather/SKILL.md
            find-skills/SKILL.md
            github/SKILL.md

<data_dir>/                              # デフォルト: ~/.microclaw
    SOUL.md                              # グローバル SOUL 上書き (任意)
    souls/                               # 複数人格ファイル (任意)
        work.md
        casual.md
        telegram-bot.md

    skills/                              # ユーザー追加スキル
        my-skill/SKILL.md

    hooks/                               # ユーザー追加フック (data_dir 内も探索対象)

    runtime/
        skills_state.json                # スキル有効/無効状態
        hooks_state.json                 # フック有効/無効状態
        microclaw.db                     # SQLite (セッション・メモリ・タスク等)
        logs/                            # ランタイムログ

        groups/
            AGENTS.md                    # グローバルメモリ (全チャット共通)
            {channel}/
                AGENTS.md                # チャネル/ボット固有メモリ
                {chat_id}/
                    AGENTS.md            # チャット固有メモリ
                    SOUL.md              # チャット固有人格 (任意)
                    TODO.json            # Todo リスト

        wal/
            memory_writes.jsonl          # メモリ書き込み監査ログ
```

---

## 9. Config フィールド一覧

システムプロンプト関連の `microclaw.config.yaml` フィールド。

### SOUL.md 関連

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `soul_path` | `string` | `null` | グローバル SOUL ファイルパス |
| `souls_dir` | `string` | `<data_dir>/souls` | 複数人格ファイルのディレクトリ |
| `channels.<channel>.soul_path` | `string` | `null` | チャネルレベル SOUL ファイルパス |
| `channels.<channel>.accounts.<id>.soul_path` | `string` | `null` | アカウント固有 SOUL ファイルパス |

### メモリ関連

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `memory_token_budget` | `usize` | `1500` | 3層メモリ注入の合計トークン予算 |
| `memory_l0_identity_pct` | `usize` | `20` | L0 Identity に割り当てる予算の割合 (%) |
| `memory_l1_essential_pct` | `usize` | `30` | L1 Essential に割り当てる予算の割合 (%) |
| `memory_max_entries_per_chat` | `usize` | `200` | チャット毎の最大アクティブメモリ数 |
| `memory_max_global_entries` | `usize` | `500` | グローバルの最大アクティブメモリ数 |
| `kg_max_triples_per_chat` | `usize` | `1000` | チャット毎の最大ナレッジグラフトリプル数 |

### セマンティックメモリ (sqlite-vec feature 必須)

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `embedding_provider` | `string` | — | 埋め込みプロバイダー (`openai` / `ollama`) |
| `embedding_api_key` | `string` | — | 埋め込み API キー |
| `embedding_base_url` | `string` | プロバイダーデフォルト | 埋め込み API ベース URL |
| `embedding_model` | `string` | プロバイダーデフォルト | 埋め込みモデル ID |
| `embedding_dim` | `usize` | プロバイダーデフォルト | 埋め込みベクトル次元数 |

### スキル関連

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `skills_dir` | `string` | `<data_dir>/skills` | スキルディレクトリ |
| `skill_review_min_tool_calls` | `usize` | `0` | 0 より大きい場合、ツール呼び出し数が閾値を超えたら Reflector 後に自動スキルレビュー |

### フック関連

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `channels.hooks.enabled` | `bool` | `true` | フックシステム全体の有効/無効 |
| `channels.hooks.max_input_bytes` | `usize` | `131072` | stdin ペイロードの最大バイト数 |
| `channels.hooks.max_output_bytes` | `usize` | `65536` | stdout レスポンスの最大バイト数 |

---

## 10. 設計上の特徴

| 特徴 | 説明 |
|---|---|
| **遅延ロード** | SKILL.md の本文は `activate_skill` 時にのみロード。カタログは名前+説明のみでトークンを節約 |
| **フォールバックチェーン** | SOUL.md は config → data_dir → カレントDir → チャット固有の順で探索。最初に見つかったものを使用 |
| **トークンバジェット** | メモリは3層 (L0/L1/L2) に割合制限あり。スキルカタログは最大40件、20件超でコンパクトモード |
| **プラットフォームフィルタ** | スキル・フックは OS (`platforms`) や依存コマンド (`deps`) で自動フィルタリング |
| **イベント駆動** | Hook は LLM 呼び出し前後・ツール呼び出し前後に起動。外部スクリプトとして allow/block/modify を返却 |
| **ファイル + DB の2層メモリ** | AGENTS.md (ファイル) は即時読み書き。SQLite (構造化) はバックグラウンド Reflector が事実を抽出・重複排除 |
| **サブプロセス実行** | フックはサブプロセスとして実行。Rust コードの変更なしに任意の言語で拡張可能 |
| **状態の永続化** | スキル・フックの有効/無効状態は JSON ファイルに永続化。再起動後も維持 |
| **監査ログ** | メモリ書き込み・フック実行結果は監査ログに記録。デバッグ・不正検知に利用 |
