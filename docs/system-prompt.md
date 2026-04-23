# EgoPulse System Prompt 構築仕様

LLM に送信される system prompt がどのように構築されるかを定義する。SOUL.md（人格定義）・AGENTS.md（ルール）・スキルカタログの読み込み、注入フォーマット、セクション順序を対象とする。

## 目次

1. [スコープ](#1-スコープ)
2. [System Prompt セクション構成](#2-system-prompt-セクション構成)
3. [Soul 選択フォールバックチェーン](#3-soul-選択フォールバックチェーン)
4. [AGENTS.md 読み込み](#4-agentsmd-読み込み)
5. [設定連携](#5-設定連携)
6. [デフォルト SOUL.md プロビジョニング](#6-デフォルト-soulmd-プロビジョニング)

---

## 1. スコープ

### 含むもの

- system prompt のセクション構築順序
- SOUL.md の3層フォールバックチェーン（account → channel → global → chat-specific）
- AGENTS.md の2層読み込み（global + per-chat）
- `ChannelConfig.soul_path` によるチャネル別人格紐付け
- デフォルト SOUL.md の初回プロビジョニング

### 含まないもの

- 会話履歴の保存・復元（→ `session-lifecycle.md`）
- スキルの発見・読み込み（→ `tools.md`）
- ツール定義の LLM への渡し方
- compaction 処理

---

## 2. System Prompt セクション構成

`build_system_prompt()` が構築する system prompt は、常に以下の順序でセクションを並べる。

```
┌─────────────────────────────────────────────┐
│ 1. <soul> セクション      （SOUL.md が存在する場合のみ）      │
│ 2. Identity + Capabilities （固定テキスト）                   │
│ 3. # Memories セクション   （AGENTS.md が存在する場合のみ）    │
│ 4. # Agent Skills セクション（スキルが存在する場合のみ）       │
└─────────────────────────────────────────────┘
```

各セクションの詳細:

### 2.1 Soul セクション

SOUL.md が存在する場合、system prompt の先頭に配置される。

```
<soul>
{SOUL.md の内容}
</soul>

Your name is EgoPulse. Current channel: {channel}.
```

- SOUL.md が存在しない場合、このセクションは出力されない
- `<soul>` タグによるラップ
- identity line は SOUL セクションの一部として付与される

### 2.2 Identity + Capabilities セクション

常に出力される固定テキスト。LLM の基本身份、利用可能ツール、実行ルールを定義する。

主要な内容:
- エージェント名とチャネル情報
- Identity rules（名前の宣言、否定禁止）
- セッション情報（session ID、type）
- 利用可能ツール一覧（bash, read, write, edit, find, grep, ls, activate_skill）
- ツール呼び出しフォーマットの説明
- 実行プレイブック（プロアクティブなツール使用、ワークスペースパスの扱い）
- 実行信頼性要件（副作用の完了確認、エラー報告）

### 2.3 Memories セクション

グローバル AGENTS.md またはチャット別 AGENTS.md が存在する場合、Identity セクションの直後（Skills の直前）に配置される。

```
# Memories

<agents>
{グローバル AGENTS.md の内容}
</agents>

<chat-agents>
{チャット別 AGENTS.md の内容}
</chat-agents>
```

- グローバル・チャット別いずれかが存在する場合のみ出力
- 両方存在する場合は両方を出力
- いずれも存在しない場合はセクション全体が省略される

### 2.4 Skills セクション

`SkillManager` が発見したスキルがある場合、最後に配置される。

```
# Agent Skills

The following skills are available. When a task matches a skill, use the `activate_skill` tool to load its full instructions before proceeding.

<available_skills>
- skill_name: Description
- another_skill: Description
</available_skills>
```

- スキル数が閾値を超えると compact mode（名前のみ表示）に切り替わる
- スキルが0件の場合はセクション全体が省略される

---

## 3. Soul 選択フォールバックチェーン

SOUL.md の読み込みは4段階のフォールバックチェーンで行う。**最初に見つかったもの**を使用する。

```
優先度:
  1 (最高)  account_id 固有 soul_path   ← 将来用（現状はスキップ）
  2         チャネル別 soul_path         ← ChannelConfig.soul_path
  3         state_root/SOUL.md           ← デフォルト人格
  4 (上書き) チャット別 SOUL.md           ← 完全上書き
```

### 3.1 優先度 1: アカウント別（将来用）

`account_id` パラメータが `Some` の場合に参照される。現在の EgoPulse には multi-account 機構がないため、`build_system_prompt()` からは常に `None` が渡される。

将来 `ChannelConfig` に `accounts` サブ構造と `SurfaceContext` に `account_id` を追加すれば自動的に有効になる。インターフェースは既に3層対応済み。

### 3.2 優先度 2: チャネル別 soul_path

`ChannelConfig.soul_path` に設定されたパスから読み込む。

```yaml
channels:
  discord:
    enabled: true
    bot_token: "..."
    soul_path: work        # → souls/work.md を探す
```

パス解決ルール:

| パスの種類 | 解決方法 |
|---|---|
| 絶対パス（`/`で始まる） | そのまま使用 |
| 相対パス | 以下の候補順に探索 |

相対パスの候補リスト:

1. `state_root/souls/{path}.md`
2. `state_root/souls/{path}`
3. `state_root/{path}.md`
4. `state_root/{path}`

最初に存在したファイルを使用。いずれも存在しなければ次の優先度へフォールバック。

### 3.3 優先度 3: デフォルト SOUL.md

`state_root/SOUL.md`（通常は `~/.egopulse/SOUL.md`）から読み込む。

### 3.4 優先度 4: チャット別 SOUL.md（完全上書き）

`runtime/groups/{channel}/{thread}/SOUL.md` が存在する場合、優先度 1〜3 で決定した内容を**完全に上書き**する。累積（マージ）ではなく置き換え。

この仕様により、特定のチャット（例: Discord の特定チャンネル）だけ別の人格を使用できる。

### 3.5 ファイル内容の判定

- ファイルが存在しない → `None`（次の候補へ）
- ファイルが存在し、trim 後が空文字 → `None`（次の候補へ）
- ファイルが存在し、trim 後が非空 → その内容を使用

---

## 4. AGENTS.md 読み込み

AGENTS.md は SOUL とは異なり、フォールバックチェーンではなく**2層の累積構造**で読み込む。

| 層 | パス | 性質 |
|---|---|---|
| グローバル | `state_root/AGENTS.md` | 全チャットで共有 |
| チャット別 | `runtime/groups/{channel}/{thread}/AGENTS.md` | そのチャット固有 |

両方存在する場合は両方を `<agents>` / `<chat-agents>` タグで区別して出力する。チャット別がグローバルを上書きするのではなく、**追加**される点が SOUL との違い。

---

## 5. 設定連携

### 5.1 ChannelConfig.soul_path

`egopulse.config.yaml` のチャネル設定に `soul_path` を追加することで、チャネルごとに人格を紐付けられる。

```yaml
channels:
  discord:
    enabled: true
    bot_token: "..."
    soul_path: friendly     # souls/friendly.md を使用
  telegram:
    enabled: true
    bot_token: "..."
    soul_path: professional  # souls/professional.md を使用
  web:
    enabled: true
    auth_token: "..."
    # soul_path 未設定 → デフォルト SOUL.md を使用
```

### 5.2 souls/ ディレクトリ

複数人格ファイルを配置するディレクトリ。パスは `state_root/souls/` で固定。

```
~/.egopulse/souls/
├── friendly.md       # channels.web.soul_path: friendly
├── professional.md   # channels.telegram.soul_path: professional
└── work.md           # channels.discord.soul_path: work
```

- ファイル名から `.md` 拡張子は省略可能（`soul_path: work` → `souls/work.md`）
- ユーザーが自由に追加・編集可能
- Config に `souls_dir` フィールドはなく、パスは固定

### 5.3 現在の動作まとめ

| シナリオ | 使用される SOUL |
|---|---|
| `soul_path` 未設定、SOUL.md なし | セクションなし（Identity のみ） |
| `soul_path` 未設定、SOUL.md あり | `~/.egopulse/SOUL.md` |
| `soul_path: work`、`souls/work.md` あり | `~/.egopulse/souls/work.md` |
| `soul_path: work`、`souls/work.md` なし | `~/.egopulse/SOUL.md`（フォールバック） |
| チャット別 SOUL.md が存在 | チャット別が常に勝つ（完全上書き） |

---

## 6. デフォルト SOUL.md プロビジョニング

初回起動時、`~/.egopulse/SOUL.md` が存在しない場合、バイナリに埋め込まれたデフォルト内容を自動書き出しする。

- タイミング: `build_app_state_with_path()` 内で `SoulAgentsLoader` 初期化直後
- 既存ファイルがある場合は上書きしない
- 書き出しに失敗した場合は warning ログを出力し、起動は継続する
