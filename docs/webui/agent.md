# EgoPulse WebUI — Agent Tab

選択中 agent の「何者で、何を覚えていて、何ができるか」を一か所で確認・編集するタブ。人格（SOUL.md）、規約（AGENTS.md）、秘密モード指示（SECRET.md）、長期記憶、スキル、実効モデルを扱う。

## 1. 構成

```
┌ Agent · lyre ───────────────────────────────── [Chat で開く] ┐
│ Overview                                                     │
│ ┌ Model ─────────────────────────────────────────────────┐   │
│ │ web      sakura / Kimi-K2.6          ← agent.model      │   │
│ │ discord  sakura / Kimi-K2.6          ← agent.model      │   │
│ │ voice    openrouter / gpt-4.1-mini   ← profiles.voice   │   │
│ │ reasoning_effort  medium                                │   │
│ └─────────────────────────────────────────────────────────┘   │
│ ┌ Bindings ───────────┐ ┌ Autonomy ────────────────────────┐ │
│ │ discord_bot  lyre   │ │ Pulse  ON · 4 intentions          │ │
│ │ telegram_bot —      │ │ 次回 09:00 morning_review         │ │
│ └─────────────────────┘ │ Sleep  最終 03:00 success         │ │
│                         └───────────────────────────────────┘ │
│ ┌ Prompt Sources ────────────────────────────────────────┐   │
│ │ Soul    agents/lyre/SOUL.md                 2.1k chars  │   │
│ │ Rules   global AGENTS.md + agent AGENTS.md              │   │
│ │ Secret  agents/lyre/SECRET.md  🔒                       │   │
│ │ Memory  episodic 8.4k / semantic 3.2k / prospective 1k  │   │
│ │ Skills  6 available                                     │   │
│ └─────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────┘
```

- Sidebar の文脈領域にセクション一覧（Overview / Soul / Rules / Secret / Memory ×3 / Skills）を表示し、各行に更新時刻を添える（[layout.md §2.5](./layout.md#25-context-section)）
- 各セクションは URL で表現する（§3）

---

## 2. セクション

### 2.1 Overview

agent の実効状態の要約。各カードから詳細セクション・他タブへ遷移できる。

| カード | 内容 | ソース |
|---|---|---|
| Model | チャネル別の実効 provider / model と、その解決元（`profiles.<channel>` / `agents.<id>` / `default_*`）。`reasoning_effort` | モデル解決チェーン（[config.md §4](../config.md#4-モデル解決チェーン)） |
| Bindings | `discord_bot` / `telegram_bot` の紐づけ | `agents.<id>` |
| Autonomy | Pulse の有効状態・intention 数・次回発火、Sleep の最終結果。クリックで Schedule タブへ | Schedule と同じ API |
| Prompt Sources | system prompt に注入される各ソースのパスとサイズ。クリックで各セクションへ | §2.2〜§2.6 |

モデル設定の編集は Config タブの Agents セクションへリンクし、Agent タブでは編集しない。

### 2.2 Soul

- 実効の SOUL.md を表示し、そのパスを明示する（`agents/{agent_id}/SOUL.md` か、グローバル `SOUL.md` へのフォールバックか。[system-prompt.md §2](../system-prompt.md#2-soulmd-読み込み)）
- Markdown 表示 / 原文表示を切り替えられる

### 2.3 Rules

- グローバル `AGENTS.md` と agent 別 `AGENTS.md` を、system prompt への注入順に2段で表示する（[system-prompt.md §3](../system-prompt.md#3-agentsmd-読み込み)）
- 存在しない層は「未作成」として表示する

### 2.4 Secret

- `agents/{agent_id}/SECRET.md` の有無のみを表示する
- 本文は `Reveal` 操作を行ったときにのみ取得・表示する。タブ表示だけで秘密モードの指示が画面に出ることはない

### 2.5 Memory

episodic / semantic / prospective の3層を扱う。

| ビュー | 内容 |
|---|---|
| Current | 公開済みの現在内容とサイズ |
| History | `memory_snapshots` から、ファイル単位の変更を時系列に並べる。各項目は変更元（Sleep run / 手動編集）、増減行数、diff（[schedule.md §4.4](./schedule.md#44-diffviewer) の DiffViewer）を持ち、Sleep run 由来の項目は Schedule タブの run 詳細へリンクする |

### 2.6 Skills

- 利用可能なスキル（組み込み `skills/` / ユーザー `workspace/skills/`）を一覧表示し、名前・説明・配置を示す
- 選択で SKILL.md を表示する

---

## 3. URL 構造

| URL | 状態 |
|---|---|
| `/agents/:agentId` | Overview |
| `/agents/:agentId/{soul\|rules\|secret\|skills}` | 各セクション |
| `/agents/:agentId/memory/:layer` | Memory Current（`layer` = `episodic` / `semantic` / `prospective`） |
| `/agents/:agentId/memory/:layer?view=history` | Memory History |

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
  - **手動編集も `memory_snapshots` に記録する**（run を持たない manual 種別）。History で Sleep 由来と手動編集を一本の時系列として監査できる
- 編集は Prompt Sources の対象ファイルのみ。Skills は閲覧のみとする

---

## 5. バックエンド API

| API | 用途 |
|---|---|
| `GET /api/agents/:agentId/profile` | Overview（実効モデルと解決元、bindings、prompt sources のパス・サイズ） |
| `GET /api/agents/:agentId/files/:kind` | `soul` / `rules` / `secret` の内容・実効パス・内容ハッシュ |
| `GET /api/agents/:agentId/memory` | 現在の長期記憶3ファイル（既存） |
| `GET /api/agents/:agentId/memory/:layer/history` | Memory History |
| `GET /api/agents/:agentId/skills` | スキル一覧と SKILL.md |
| `PUT /api/agents/:agentId/files/:kind` / `PUT /api/agents/:agentId/memory/:layer` | 編集（`expected_hash` 必須） |

---

## 6. Out of Scope

- 合成済み system prompt 全体のプレビュー
- スキルの追加・編集・削除
- `souls/` の複数人格定義の管理
- agent の新規作成・削除
