# EgoPulse WebUI — Command Palette

`Cmd+K` / `Ctrl+K` で起動するグローバルコマンドパレット。キーボード操作でナビゲーション・操作・検索を完結する。

## 1. 起動と終了

### 1.1 起動トリガ

| トリガ | 動作 |
|---|---|
| `Cmd+K` / `Ctrl+K` | パレット開く（フォーカスが入力フィールド以外でも有効） |
| Top Bar の `[⌘K] Search or jump…` button click | 同上 |

### 1.2 終了トリガ

| トリガ | 動作 |
|---|---|
| `Esc` | パレット閉じる |
| パレット外 click | パレット閉じる |
| コマンド実行 | 実行後に自動的に閉じる |

---

## 2. レイアウト

```
┌─ Command Palette ────────────────────────┐
│  🔍  Search commands, sessions, agents…   │  ← Input
│ ───────────────────────────────────────  │
│  Recent                                   │  ← Section
│  ▸ Chat with lyre · Web Chat             │
│  ▸ Sleep run 2026-06-29 03:00            │
│ ───────────────────────────────────────  │
│  Quick Actions                            │
│  ⌘N  New Session (with lyre)              │
│  ⌘,  Open Config                          │
│  ↻   Refresh current tab                  │
│ ───────────────────────────────────────  │
│  Navigation                               │
│  →  Go to Chat tab                        │
│  →  Go to Sleep tab                       │
│  →  Go to Pulse tab                       │
│  →  Go to Metrics tab                     │
│ ───────────────────────────────────────  │
│  Agents                                   │
│  ▸ Switch to agent: ace                   │
│  ▸ Switch to agent: vega                  │
│ ───────────────────────────────────────  │
│  Sessions                                 │
│  ▸ Web Chat        [web]                  │
│  ▸ Dev #general   [discord]              │
│  ▸ Yesterday notes [cli]                 │
└───────────────────────────────────────────┘
   ↑↓ navigate · Enter select · Esc close
```

### 2.1 Modal 仕様の上書き

`Modal` コンポーネント（[design-system.md §9.4](./design-system.md#94-modal)）をベースにしつつ、以下を上書き：

- パレット専用の最大幅（640px 程度）
- 上寄せ表示（中央ではなく）。上部に 12vh 程度の margin
- backdrop は通常 modal より暗め
- z-index は通常 modal より前面（[design-system.md §8](./design-system.md#8-z-index)）

---

## 3. セクション構成

検索クエリが空の場合、以下のセクションを順に表示する。各セクションは最大5件まで表示。

| 順序 | セクション | 内容 |
|---|---|---|
| 1 | Recent | 最近使ったコマンド・最近開いたセッション（最大5件） |
| 2 | Quick Actions | 高頻度アクション |
| 3 | Navigation | タブ遷移 |
| 4 | Agents | agent 切替 |
| 5 | Sessions | セッション検索結果 |
| 6 | Sleep Runs | Sleep run 検索結果 |
| 7 | Pulse Runs | Pulse run 検索結果 |

クエリ入力時は、全セクションからマッチする項目を抽出し、マッチしないセクションは非表示にする。マッチが0件の場合は `"No results for '{query}'"` を表示。

---

## 4. コマンド一覧

### 4.1 Quick Actions

| label | shortcut | 動作 |
|---|---|---|
| New Session | `Cmd+N` | 選択 agent で新規 web セッション作成 → Chat タブへ |
| Refresh current tab | (なし) | 現在タブのデータを再取得 |
| Show shortcuts | (なし) | ショートカット一覧モーダルを表示 |
| Toggle sidebar | (なし) | Sidebar 開閉（mobile/tablet のみ有効） |

> Config タブへの遷移は Navigation セクションから行う。`Cmd+,` は macOS のブラウザ設定と衝突するためショートカットには割り当てない。

### 4.2 Navigation

5タブへの遷移。URL 設計は別途確定（例：`/agents/{currentAgent}/chat`, `/metrics` 等）。

| label | 動作 |
|---|---|
| Go to Chat tab | Chat タブへ遷移 |
| Go to Sleep tab | Sleep タブへ遷移 |
| Go to Pulse tab | Pulse タブへ遷移 |
| Go to Metrics tab | Metrics タブへ遷移（agent 非依存） |
| Go to Config tab | Config タブへ遷移（agent 非依存） |

### 4.3 Agents

agent 一覧から選択して切り替え。

| label | 動作 |
|---|---|
| Switch to agent: {agentId} | 選択 agent を切り替え、現在のタブcontext を維持 |

選択中 agent は表示しない（既に選択済みのため）。

#### 4.4 Sessions

セッション検索。クエリで `session.label` と `session_key` の部分一致でフィルタ。

| label | 動作 |
|---|---|
| {sessionLabel} · {preview} | 当該セッションを Chat タブで開く |

検索対象は選択中 agent のセッションのみ。

### 4.5 Sleep Runs

Sleep run 検索。クエリが `"sleep"` を含むか、時刻フォーマット（`2026-06-29` 等）にマッチする場合に優先表示。

| label | 動作 |
|---|---|
| Sleep run {date} {time} · {status} | 当該 run の Detail を開く |

### 4.6 Pulse Runs

Pulse run 検索。同上。

| label | 動作 |
|---|---|
| Pulse run {date} {time} · {intention_id} | 当該 run の Detail を開く |

---

## 5. Recent セクション

直近に実行したコマンド・開いたセッションを最大5件記憶する。

### 5.1 記憶対象

- 実行した Quick Action / Navigation
- 開いた Session / Sleep run / Pulse run

### 5.2 保存先

- ブラウザの localStorage に保存
- 最大20件保持、5件のみ表示
- 各エントリ：`{ type, label, target, lastUsed }`
- localStorage が利用できない環境（プライベートブラウジングモード等）では Recent 機能を無効化し、他のパレット機能は通常動作する

### 5.3 並び順

`lastUsed` 降順。パレットを開くたびに最近使ったものが上位に出る。

---

## 6. キーボード操作

### 6.1 基本操作

| キー | 動作 |
|---|---|
| `↑` / `↓` | 項目間移動（セクション境界をまたぐ） |
| `Enter` | 選択項目を実行 |
| `Tab` | `Enter` と同じ（補完的） |
| `Esc` | パレット閉じる |
| `Cmd+K` | パレットが開いている場合は閉じる（トグル） |

`Home` / `End` は入力フィールド内でのカーソル移動（行頭・行末）に予約されているため、項目移動には割り当てない。先頭・末尾へのジャンプは `↑` / `↓` の長押しで代替する。

### 6.3 入力中

- 入力中（クエリが空でない）は絞り込み結果の最初の項目が自動的に `active`（プレビュー選択状態）
- `Enter` 押下で `active` 項目を実行
- 矢印キーで `active` を移動

---

## 7. アクセシビリティ

- Input：`role="combobox"` `aria-expanded="true"` `aria-controls="palette-results"` `aria-autocomplete="list"`
- Results container：`role="listbox"` `aria-label="Commands"`
- 各項目：`role="option"` `aria-selected` で active 状態を表現
- Section header：`role="presentation"` `aria-hidden="true"`
- focus trap：モーダル内にフォーカスを閉じ込め、Tab キーで項目間を循環
- パレット閉じたとき、開いた元要素へフォーカスを戻す
- `aria-live="polite"` で検索結果件数を読み上げ（"5 results"）

---

## 8. パフォーマンス

- セッション・Sleep run・Pulse run の検索対象はサーバー状態キャッシュから取得（再 fetch しない）
- 入力の debounce：50ms（体感ほぼリアルタイム）
- 検索は単純な部分一致ベース
- 結果表示上限：セクション毎 5件、全体で 30件まで
- 項目のレンダリング最適化（不要な再レンダリングをスキップ）

---

## 9. Out of Scope

- fuzzy match・全文検索エンジン（部分一致のみで確定）
- カスタムコマンド登録機能（`/clear` 等のスラッシュコマンドとの統合も含め未対応）
- LLM へのクエリ直接入力（`> {query}` 等のショートカット）
- MCP ツールの直接呼び出し
- コマンド履歴のサーバー同期（ブラウザ localStorage のみ）
