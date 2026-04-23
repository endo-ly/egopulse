# EgoPulse ディレクトリ構成

`~/.egopulse/` 以下のディレクトリ・ファイル配置仕様。

## 目次

1. [全体構成](#1-全体構成)
2. [各ディレクトリの責務](#2-各ディレクトリの責務)
3. [バイナリとsystemdユニットの配置](#3-バイナリとsystemdユニットの配置)
4. [パス解決ルール](#4-パス解決ルール)
5. [チャット粒度とパスマッピング](#5-チャット粒度とパスマッピング)

---

## 1. 全体構成

```
~/.egopulse/
├── egopulse.config.yaml
├── egopulse.config.backups/
│
├── SOUL.md
├── souls/
│   └── friendly.md
│
├── AGENTS.md
│
├── skills/
│   └── pdf/SKILL.md
│
├── mcp.json
├── mcp.d/
│
├── runtime/
│   ├── egopulse.db
│   ├── assets/
│   ├── groups/
│   │   ├── telegram/
│   │   │   └── {chat_id}/
│   │   │       ├── AGENTS.md
│   │   │       └── conversations/
│   │   └── discord/
│   │       └── {chat_id}/
│   │           ├── AGENTS.md
│   │           └── conversations/
│   └── status.json
│
└── workspace/
    ├── skills/
    │   └── my-custom-skill/SKILL.md
    └── .tmp/
```

### レイヤー分類

| レイヤー | 配置 | 内容 |
|---|---|---|
| 設定 | 直下 | config.yaml, config.backups/ |
| 人格 | 直下 | SOUL.md, souls/ |
| ルール | 直下 | AGENTS.md (グローバル) |
| 組み込みスキル | skills/ | EgoPulse に同梱されるスキル |
| MCP | 直下 | mcp.json, mcp.d/ |
| 永続状態 | runtime/ | DB, assets, チャット別ルール, アーカイブ |
| ワークスペース | workspace/ | エージェントの作業領域, ユーザースキル |

---

## 2. 各ディレクトリの責務

### 2.1 直下 — 設定・人格・ルール・MCP

| パス | 責務 |
|---|---|
| `egopulse.config.yaml` | ランタイム設定（プロバイダー、チャネル、モデル等） |
| `egopulse.config.backups/` | セットアップウィザードが生成する設定バックアップ |
| `SOUL.md` | デフォルト人格定義。system prompt の先頭に注入される |
| `souls/` | 複数人格定義。チャネルやチャットに人格を紐付ける場合に使用 |
| `AGENTS.md` | グローバルルール。全チャットで共有 |
| `mcp.json` | MCP サーバー定義 |
| `mcp.d/` | MCP 追加設定ファイル群 |

### 2.2 skills/ — 組み込みスキル

EgoPulse に同梱されるスキル。バイナリのアップデートで上書きされる。

```
skills/
└── pdf/SKILL.md
```

### 2.3 runtime/ — 永続状態

| パス | 責務 |
|---|---|
| `egopulse.db` | SQLite。会話履歴、セッション、ツール呼び出し記録 |
| `assets/` | 会話中に生成・参照される画像等のバイナリアセット |
| `status.json` | ランタイムステータス（起動時刻、接続状態等） |
| `groups/` | チャット別永続データのルート |

### 2.4 runtime/groups/ — チャット別ルールとアーカイブ

チャット毎に独立したディレクトリを持ち、チャット別ルール(AGENTS.md)と会話アーカイブを配置する。

```
groups/
├── telegram/{chat_id}/AGENTS.md        ← チャット別ルール
├── telegram/{chat_id}/conversations/   ← compaction アーカイブ
├── discord/{chat_id}/AGENTS.md
└── discord/{chat_id}/conversations/
```

| パス | 責務 |
|---|---|
| `{channel}/{chat_id}/AGENTS.md` | チャット別ルール。そのチャット固有の行動ルール・制約 |
| `{channel}/{chat_id}/conversations/` | compaction によって生成される過去会話のアーカイブファイル |

ルールは2層構造:
- **グローバル**: `~/.egopulse/AGENTS.md`（直下）。全チャットで共有される行動ルール
- **チャット毎**: `runtime/groups/{channel}/{chat_id}/AGENTS.md`。そのチャット固有のルール

### 2.5 workspace/ — エージェント作業領域

全チャットで共有されるエージェントの作業領域。

| パス | 責務 |
|---|---|
| `skills/` | ユーザーが追加するスキル定義ファイル（SKILL.md） |
| `.tmp/` | bash ツールのスクリプトキャッシュ、一時ファイル |

エージェントが read / write / edit / grep / find ツールで相対パスを指定した場合、この `workspace/` を基準に解決される。bash ツールの `current_dir` もここになる。

### 2.6 スキルの2層構造

| パス | 種別 | 管理者 |
|---|---|---|
| `~/.egopulse/skills/` | 組み込みスキル | バイナリのアップデートで上書き |
| `~/.egopulse/workspace/skills/` | ユーザースキル | ユーザーが自由に追加・編集 |

SkillManager は両方をスキャンし、同名スキルがある場合はユーザースキルを優先する。

---

## 3. バイナリとsystemdユニット

`~/.egopulse/` 外にあるが、EgoPulseの実行に関わるファイル。

| ファイル | パス | 備考 |
|---|---|---|
| バイナリ | `/usr/local/bin/egopulse` | `install-egopulse.sh` / `egopulse update` で配置 |
| systemdユニット | `~/.config/systemd/user/egopulse.service` | `gateway install` が自動生成。`/etc/systemd/system/` には置かない |

---

## 4. パス解決ルール

エージェントのファイルアクセスは jail なし。相対パスは `workspace/` 基準、絶対パスはそのまま通る。

機密パス（`.ssh`, `.aws`, `.gnupg`, `.env`, `/proc/self/*` 等）は `path_guard` がブロックする。このチェックは read / write / edit / grep / find / bash の全ツールで適用される。

---

## 5. チャット粒度とパスマッピング

チャット ID はチャネルごとに以下の粒度で決定される:

| チャネル | チャット粒度 | chat_id の例 |
|---|---|---|
| Discord | テキストチャンネル毎 | `1234567890` |
| Telegram DM | ユーザー毎 | `987654321` |
| Telegram グループ | グループ毎 | `-1001234567890` |
| Web | セッション毎 | UUID ベース |
