# EgoPulse WebUI — Agent Tab

選択中 agent の「何者で、何を覚えていて、何ができるか」を一か所で確認・編集するタブ。人格（SOUL.md）、規約（AGENTS.md）、秘密モード指示（SECRET.md）、長期記憶、スキル、実効モデルを扱う。

## 1. 構成

```
┌ Sidebar ─────────────┬ Overview ───────────────────────────────────────────┐
│ AGENTS               │ (avatar) Lyre  DEFAULT                               │
│  ● Lyre   DEFAULT    │          lyre  sakura / Kimi-K2.6  effort medium     │
│ SECTIONS             │          [Discord lyre]                              │
│ [▦] Overview         │ ┌SESSIONS─┐┌LAST ACTIVE┐┌MEMORY──┐┌SKILLS─┐          │
│     Kimi-K2.6        │ │ 13      ││ 25m       ││ 8.4k   ││ 6     │          │
│ [♡] Soul             │ └─────────┘└───────────┘└────────┘└───────┘          │
│     agents/lyre/…    │ ┌ Model ──────────────┐┌ System prompt ─────────────┐│
│ [≡] Rules            │ │ Channel Model Set by││ ███▓▓▓▓░░░░░░░░░░░░░░░     ││
│ [🔒] Secret          │ │ Other   Kimi  agents.lyre ││ Soul / Rules / Memory  ││
│ [☾] Memory           │ │ voice   gpt…  …profiles.voice│                      ││
│ [▤] Skills           │ └─────────────────────┘└────────────────────────────┘│
│                      │ ┌ Recent activity ─────────────────────────────────┐ │
│                      │ │ ● Sleep · Scheduled  Success · 3 sessions   3h   │ │
│                      │ │ ● Memory · semantic  +12 −3                 3h   │ │
│                      │ └──────────────────────────────────────────────────┘ │
└──────────────────────┴──────────────────────────────────────────────────────┘
```

- Sidebar の文脈領域にセクション一覧（Overview / Soul / Rules / Secret / Memory / Skills）を表示する（[layout.md §2.5](./layout.md#25-context-section)）
- 画面ヘッダーはセクション名と、そのファイルが system prompt にどう入るかの説明を示す
- ファイルはすべて共通のカード（パス・適用状態・Markdown 表示 / 原文表示の切替）で表示する
- 表示のたびにキャッシュを即時表示しつつ裏で再取得する。Sleep や手動編集など UI 外からの変更に追従するためで、手動の再読み込みボタンは置かない

---

## 2. セクション

### 2.1 Overview

agent の現在の姿を一望するページ。各要素から詳細セクション・他タブへ遷移できる。

| 要素 | 内容 | ソース |
|---|---|---|
| Hero | アバター、表示名、`default` タグ、agent id、既定の provider / model、reasoning effort、bot の紐づけ | プロフィール API・アバター |
| Stats | Sessions（アーカイブ以外の件数とチャネル内訳）、Last active、Memory（総文字数と最終更新）、Skills（数） | セッション一覧・プロフィール・記憶の変更履歴 |
| Model | チャネル別の実効 model / provider と、それを決めた設定キー（`agents.<id>`、`agents.<id>.profiles.<channel>`、`default_model` など）。profile のないチャネルは 1 行にまとめる | モデル解決チェーン（[config.md §4](../config.md#4-モデル解決チェーン)） |
| System prompt | agent 固有のコンテキスト（Soul / Rules / Memory）の構成比を帯で示し、各行から該当セクションへ移る。SECRET.md とスキルカタログが加わる条件を注記する | プロフィール API |
| Recent activity | 直近の Sleep run（スキップされた判定を除く）と記憶の変更を時系列にまとめる。Sleep run は Schedule タブの run 詳細へ、記憶の変更は Memory へ移る | Sleep run 一覧・記憶の変更履歴 |

モデル設定の編集は Config タブの Agents セクションで行い、Agent タブでは編集しない。

### 2.2 Soul

- agent 別 `agents/{agent_id}/SOUL.md` とグローバル `SOUL.md` を優先順に並べ、どちらが適用されているかを示す（[system-prompt.md §2](../system-prompt.md#2-soulmd-読み込み)）
- 各層の状態を `Applied` / `Not used`（内容はあるが適用されていない）/ `Empty`（空ファイル）/ `Not created` で示す

### 2.3 Rules

- グローバル `AGENTS.md` と agent 別 `AGENTS.md` を、system prompt への注入順に並べる（[system-prompt.md §3](../system-prompt.md#3-agentsmd-読み込み)）
- 存在しない層は「未作成」として表示する

### 2.4 Secret

- `agents/{agent_id}/SECRET.md` の本文は `Reveal` 操作を行ったときにのみ取得・表示する。画面を開いただけで秘密モードの指示が出ることはない

### 2.5 Memory

episodic / semantic の 2 種を 1 ページで扱う。

| 領域 | 内容 |
|---|---|
| Files | 2 種を順にカードで表示する。文字数と最終更新（直近の変更時刻）を添え、長いものは先頭のみ表示して全文表示に切り替えられる |
| Changes | 2 種をまたいだ公開済みの変更を新しい順に並べる。各行は層・時刻・増減行数を示し、開くと diff（[schedule.md §5.3](./schedule.md#53-diffviewer) の DiffViewer）を表示する。最新の 1 件は開いた状態で表示する。Sleep run 由来の行は Schedule タブの run 詳細へリンクする |

### 2.6 Skills

- 利用可能なスキル（組み込み `skills/` / ユーザー `workspace/`）を一覧表示し、名前・説明・配置を示す。開いた時点で先頭のスキルを選択する
- 選択したスキルについて、配置パスと frontmatter（platforms / deps / required_env）、SKILL.md 本文を表示する
- 同梱ファイル（`references/` など）はファイル名のタブで切り替えて閲覧する。Markdown は描画表示、その他はテキスト表示とし、テキストでないファイルや大きすぎるファイルはその旨のみ示す

---

## 3. URL 構造

| URL | 状態 |
|---|---|
| `/agents/:agentId` | Overview |
| `/agents/:agentId/{soul\|rules\|secret\|memory\|skills}` | 各セクション |

agent を切り替えた場合はセクションを維持する。

---

## 4. 編集

Soul / Rules / Secret（system prompt に注入されるファイル）を編集できる。Memory と Skills は閲覧のみ。流れは 3 セクションで共通。

### 4.1 流れ

1. `Edit` でエディタに切り替える
2. `Write` / `Preview` で本文を確認し、`Changes` で現在内容との差分（増減行数）を確認する
3. `Save` で書き込む
   - 取得時の内容ハッシュを `expected_hash` として送り、現在内容と不一致なら `409` で拒否して最新内容の再取得を促す。基準は常に編集開始時の内容で、編集中の再取得には左右されない
   - 編集中の draft は保持し、`Reload` で破棄して最新内容を表示する

### 4.2 ルール

- 保存する本文は trim 済み。空で保存すると空ファイルになり、その層は無効化される
- グローバル `SOUL.md` / `AGENTS.md` は複数 agent で共有される。編集は他の agent の system prompt にも影響するため、編集中にその旨を表示する
- `SECRET.md` は `Reveal` 後でのみ編集できる
- 未保存の編集がある状態でセクション・タブ・agent を切り替える、新規セッションを作成する、または画面を離れる場合は確認する
- 既存ファイルが読み取れない（権限・不正 UTF-8）場合や、保存先がシンボリックリンク（親ディレクトリを含む）の場合は、データ消失を避けるため保存を拒否する

---

## 5. バックエンド API

プロフィール・prompt ファイル（`soul` / `rules` / `secret`）・長期記憶とその変更履歴・Recent activity の Sleep run は、[api.md §2.12](../api.md#212-agent-画面)・[§2.9](../api.md#29-sleep-batch) の API で提供される。

- スキルは agent 非依存のため、スキル API は agent に紐づかないグローバルなパスに置く
- §4.1 の保存は `expected_hash` 必須の `PUT` で行う

---

## 6. Out of Scope

- 合成済み system prompt 全体のプレビュー
- Memory の編集
- スキルの追加・編集・削除
- `souls/` の複数人格定義の管理
- agent の新規作成・削除
