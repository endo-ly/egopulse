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

- Sidebar の文脈領域にセクション一覧（Overview / Soul / Rules / Secret / Memory / Skills）を表示する（[layout.md §2.5](./layout.md#25-context-section)）。各行は SESSIONS と同じ「アイコン + 名前 + 1 行の要約」で、要約はそのセクションの実効値（使われているパス、文字数など）
- 画面ヘッダーはセクション名と、そのファイルが system prompt にどう入るかの説明を示す
- ファイルはすべて共通のカード（パス・適用状態・Markdown 表示 / 原文表示の切替）で表示する
- 表示のたびにキャッシュを即時表示しつつ裏で再取得する（Sleep や手動編集で UI 外から変わるため）。手動の再読み込みボタンは置かない

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
- 各層の状態を `Applied` / `Not used`（内容はあるが適用されていない）/ `Not created` で示す

### 2.3 Rules

- グローバル `AGENTS.md` と agent 別 `AGENTS.md` を、system prompt への注入順に並べる（[system-prompt.md §3](../system-prompt.md#3-agentsmd-読み込み)）
- 存在しない層は「未作成」として表示する

### 2.4 Secret

- `agents/{agent_id}/SECRET.md` の本文は `Reveal` 操作を行ったときにのみ取得・表示する。画面を開いただけで秘密モードの指示が出ることはない

### 2.5 Memory

episodic / semantic / prospective の 3 層を 1 ページで扱う。

| 領域 | 内容 |
|---|---|
| Files | 3 層を順にカードで表示する。文字数と最終更新（直近の変更時刻）を添え、長いものは先頭のみ表示して全文表示に切り替えられる |
| Changes | 3 層をまたいだ公開済みの変更を新しい順に並べる。各行は層・時刻・増減行数を示し、開くと diff（[schedule.md §5.3](./schedule.md#53-diffviewer) の DiffViewer）を表示する。最新の 1 件は開いた状態で表示する。Sleep run 由来の行は Schedule タブの run 詳細へリンクする |

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

Soul / Rules / Secret / Memory は WebUI から編集できる。流れは共通：

1. `Edit` でエディタに切り替える
2. 保存前に現在内容との diff を確認する
3. `Save` で楽観的同時実行制御付きで書き込む

- 取得時に内容ハッシュを返し、更新時に `expected_hash` を送る。不一致は `409` とし、最新内容を再取得させる
- Memory の編集には以下の制約を設ける
  - **Sleep run 実行中の agent では編集を拒否する**。Sleep の公開処理は run 開始時の内容（`content_before`）を前提条件とするため、並行編集は公開・起動時復旧の失敗につながる
  - **手動編集も `memory_snapshots` に記録する**（run を持たない manual 種別）。Changes で Sleep 由来と手動編集を一本の時系列として監査できる
- 編集対象は system prompt に注入されるファイルのみ。Skills は閲覧のみとする

---

## 5. バックエンド API

API の詳細は [api.md §2.12](../api.md#212-agent-画面)。

| API | 用途 |
|---|---|
| `GET /api/agents/:agentId/profile` | 実効モデルと解決元、bindings、prompt に注入されるファイルのパス・サイズ |
| `GET /api/agents/:agentId/files/:kind` | `soul` / `rules` / `secret` の各層（パス・内容・適用有無） |
| `GET /api/agents/:agentId/memory` | 現在の長期記憶 3 ファイル |
| `GET /api/agents/:agentId/memory/history` | 3 層をまたいだ公開済みの変更履歴 |
| `GET /api/sleep/runs?agent_id=&status=` | Recent activity の Sleep run |
| `GET /api/skills` / `GET /api/skills/:name` / `GET /api/skills/:name/files/*` | スキル一覧・詳細・同梱ファイル。スキルは agent 非依存のためグローバルなパスに置く |
| `PUT /api/agents/:agentId/files/:kind` / `PUT /api/agents/:agentId/memory/:layer` | 編集（`expected_hash` 必須） |

---

## 6. Out of Scope

- 合成済み system prompt 全体のプレビュー
- スキルの追加・編集・削除
- `souls/` の複数人格定義の管理
- agent の新規作成・削除
