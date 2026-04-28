# EgoPulse System Prompt 構築仕様

LLM に送信される system prompt がどのように構築されるかを定義する。SOUL.md（人格定義）・AGENTS.md（ルール）・スキルカタログの読み込み、注入フォーマット、セクション順序を対象とする。

## 目次

1. [System Prompt セクション構成](#1-system-prompt-セクション構成)
2. [Soul 選択フォールバックチェーン](#2-soul-選択フォールバックチェーン)
3. [AGENTS.md 読み込み](#3-agentsmd-読み込み)
4. [設定連携](#4-設定連携)
5. [デフォルト SOUL.md プロビジョニング](#5-デフォルト-soulmd-プロビジョニング)

---

## 1. System Prompt セクション構成

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

### 1.1 Soul セクション

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

### 1.2 Identity + Capabilities セクション

常に出力される固定テキスト。LLM の基本身份、利用可能ツール、実行ルールを定義する。

主要な内容:
- エージェント名とチャネル情報
- Identity rules（名前の宣言、否定禁止）
- セッション情報（session ID、type）
- 利用可能ツール一覧（bash, read, write, edit, find, grep, ls, activate_skill）
- ツール呼び出しフォーマットの説明
- 実行プレイブック（プロアクティブなツール使用、ワークスペースパスの扱い）
- 実行信頼性要件（副作用の完了確認、エラー報告）

### 1.3 Memories セクション

グローバル AGENTS.md またはエージェント別 AGENTS.md が存在する場合、Identity セクションの直後（Skills の直前）に配置される。

```
# Memories

<agents>
{グローバル AGENTS.md の内容}
</agents>

<agents>
{エージェント別 AGENTS.md の内容}
</agents>
```

- グローバル・エージェント別いずれかが存在する場合のみ出力
- 両方存在する場合は両方を出力（いずれも `<agents>` タグでラップ）
- いずれも存在しない場合はセクション全体が省略される

### 1.4 Skills セクション

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

## 2. Soul 選択フォールバックチェーン

SOUL.md の読み込みは3段階のフォールバックチェーンで行う。**最初に見つかったもの**を使用する。

```
優先度:
  1 (最高)  Agent 固有 SOUL.md     ← agents/{agent_id}/SOUL.md
  2         チャネル別 soul_path    ← ChannelConfig.soul_path
  3         state_root/SOUL.md     ← デフォルト人格
```

### 2.1 優先度 1: エージェント別 SOUL.md

`agents/{agent_id}/SOUL.md` が存在する場合に参照される。エージェントごとに独立した人格定義を持ち、チャネルやグローバル設定よりも優先される。

### 2.2 優先度 2: チャネル別 soul_path

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

### 2.3 優先度 3: デフォルト SOUL.md

`state_root/SOUL.md`（通常は `~/.egopulse/SOUL.md`）から読み込む。

### 2.4 ファイル内容の判定

- ファイルが存在しない → `None`（次の候補へ）
- ファイルが存在し、trim 後が空文字 → `None`（次の候補へ）
- ファイルが存在し、trim 後が非空 → その内容を使用

---

## 3. AGENTS.md 読み込み

AGENTS.md は SOUL とは異なり、フォールバックチェーンではなく**2層の累積構造**で読み込む。

| 層 | パス | 性質 |
|---|---|---|
| グローバル | `state_root/AGENTS.md` | 全チャット・全エージェントで共有 |
| エージェント別 | `agents/{agent_id}/AGENTS.md` | そのエージェント固有 |

両方存在する場合は両方を `<agents>` タグで出力する。エージェント別がグローバルを上書きするのではなく、**追加**される点が SOUL との違い。

---

## 4. 設定連携

### 4.1 ChannelConfig.soul_path

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

### 4.2 souls/ ディレクトリ

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

---

## 5. デフォルト SOUL.md プロビジョニング

初回起動時、`~/.egopulse/SOUL.md` が存在しない場合、バイナリに埋め込まれたデフォルト内容を自動書き出しする。

- タイミング: `build_app_state_with_path()` 内で `SoulAgentsLoader` 初期化直後
- 既存ファイルがある場合は上書きしない
- 書き出しに失敗した場合は warning ログを出力し、起動は継続する
