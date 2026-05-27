# EgoPulse Long-term Memory vNext

> **Event Ledger + Generated Memory View — 全体仕様**

---

## 目次

1. [目的と問題意識](#1-目的と問題意識)
2. [設計思想](#2-設計思想)
3. [記憶の構成](#3-記憶の構成)
4. [episode_events テーブル](#4-episode_events-テーブル)
5. [Kind 分類](#5-kind-分類)
6. [Sleep Batch フロー](#6-sleep-batch-フロー)
7. [episodic.md 生成方針](#7-episodicmd-生成方針)
8. [監査・復元](#8-監査復元)

---

## 1. 目的と問題意識

### 現状の問題

`episodic.md` を「正本」としてLLMに毎日更新させているため、**古いエピソードがごっそり消えやすい**。

### 解決方針

エピソード記憶そのものをDBに蓄積し、`episodic.md` は毎回生成される **注入ビュー** として扱う。

```text
記憶の正本   →  SQLite の episode_events
LLM注入用    →  agents/{agent_id}/memory/episodic.md
```

既存の Long-term Memory 注入は `episodic.md / semantic.md / prospective.md` を読む構造なので、**ファイル構成自体は維持する**。

---

## 2. 設計思想

### 2.1 記憶そのものと「思い出し方」を分ける

| レイヤー | 役割 | 更新方針 |
|---|---|---|
| `episode_events` | 起きた出来事の台帳（正本） | 蓄積のみ（削除しない） |
| `episodic.md` | LLM注入用の記憶ビュー | 毎回上書きしてよい |

`episodic.md` が変わることは「記憶が消えた」ではなく、**今の思い出し方が変わった**とみなす。

### 2.2 Semantic Memory は自由形式 Markdown のまま

意味記憶は安定した知識・方針・設計思想の圧縮体であり、自由形式 Markdown と相性がよい。  
`semantic.md` と `prospective.md` は引き続き **正本** として扱う。

---

## 3. 記憶の構成

### 3.1 正本 vs 注入ビュー

| ファイル / テーブル | 種別 | 性質 |
|---|---|---|
| `episode_events` (SQLite) | エピソード記憶 | **正本** — 蓄積のみ |
| `semantic.md` | 意味記憶 | **正本** — 安定した知識・方針 |
| `prospective.md` | 展望記憶 | **正本** — 未完了の意図・予定 |
| `episodic.md` | エピソード記憶ビュー | **生成物** — 毎回上書き |

### 3.2 episodic.md の詳細度ルール

時間経過に応じて、詳細度を自動的に変える。

| 期間 | 詳細度 |
|---|---|
| 直近（〜7日） | 詳細 |
| 少し前（8〜30日） | 概要 |
| 古い（31〜90日） | 超概要 |
| かなり古い（90日超） | `ripple_strength` が高いものだけ背景として残す |

---

## 4. episode_events テーブル

### 4.1 カラム定義

| カラム | 型 | 目的 |
|---|---|---|
| `id` | TEXT PK | Event の安定ID |
| `agent_id` | TEXT | エージェントごとの記憶分離 |
| `experienced_at` | TEXT | エージェントにとって経験として発生した時刻 |
| `encoded_at` | TEXT | 経験が記憶として符号化された時刻 |
| `kind` | TEXT | 出来事の記憶上の役割（後述） |
| `title` | TEXT | 一覧・注入ビュー用の短い見出し |
| `body_md` | TEXT | 出来事本文（Markdown可） |
| `ripple_strength` | INTEGER | 睡眠中に再活性化された強さ。注入ビューへの残存、意味記憶への転送、忘却からの保護に使う（1〜5） |
| `certainty` | TEXT | 断定度：`observed / inferred / uncertain` |
| `sleep_run_id` | TEXT | どの Sleep Batch で抽出されたか |
| `source_refs_json` | TEXT | Event の根拠となった元メッセージへの参照（監査補助用） |
| `created_at` | TEXT | DB管理用 |
| `updated_at` | TEXT | DB管理用 |

`ripple_strength` は、一般的な意味での salience に近いが、EgoPulse では「睡眠中に再活性化され、記憶として残る強さ」を表す。  
これは `episodic.md` への残存、`semantic.md` への転送、忘却からの保護に使われる。

### 4.2 DDL

```sql
CREATE TABLE IF NOT EXISTS episode_events (
    id               TEXT PRIMARY KEY,
    agent_id         TEXT NOT NULL,
    experienced_at   TEXT NOT NULL,
    encoded_at       TEXT NOT NULL,
    kind             TEXT NOT NULL,
    title            TEXT NOT NULL,
    body_md          TEXT NOT NULL,
    ripple_strength  INTEGER NOT NULL DEFAULT 3,
    certainty        TEXT NOT NULL DEFAULT 'observed',
    sleep_run_id     TEXT NOT NULL,
    source_refs_json TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    CHECK (kind IN (
        'self',
        'relationship',
        'world',
        'feat',
        'anomaly',
        'decision',
        'insight',
        'rhythm'
    )),
    CHECK (ripple_strength BETWEEN 1 AND 5),
    CHECK (certainty IN ('observed', 'inferred', 'uncertain'))
);

-- インデックス
CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced
    ON episode_events(agent_id, experienced_at);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced
    ON episode_events(agent_id, kind, experienced_at);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced
    ON episode_events(agent_id, ripple_strength, experienced_at);

CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run
    ON episode_events(sleep_run_id);
```

### 4.3 source_refs_json

`source_refs_json` は、Event の根拠となった元メッセージへの参照を保持する監査用フィールドである。

本文そのものは保存せず、原則として `messages` テーブルへ戻るための参照だけを保存する。

```json
[
  {
    "chat_id": 123,
    "message_id": "msg_abc123",
    "timestamp": "2026-05-23T12:34:56+09:00",
    "role": "user"
  }
]
```

| フィールド | 意味 |
|---|---|
| `chat_id` | EgoPulse 内部の chat ID |
| `message_id` | `messages.id` に対応するメッセージID |
| `timestamp` | 元メッセージの時刻。RFC3339 |
| `role` | `user / assistant / tool / system / unknown` |

`source_refs_json` は必要最小限の参照に絞る。  
原則として本文や長い snippet は含めない。

---

## 5. Kind 分類

`kind` はトピック分類ではなく、**記憶上の役割分類**である。

| kind | 名称 | 意味 |
|---|---|---|
| `self` | 自己認識・自己定義の変化 | エージェント自身の役割、在り方、限界、振る舞い方の理解が変化した出来事 |
| `relationship` | 関係性・距離感の変化 | ユーザーや他エージェントとの関係、信頼、距離感、役割分担が変化した出来事 |
| `world` | 外界の変化 | 外部環境、ユーザー環境、サービス、モデル、社会状況など、エージェント外部の前提が変化した出来事 |
| `feat` | 達成・節目・正のピーク | 作業、創作、設計、実装、運用における到達・完成・成功・区切り |
| `anomaly` | 失敗・衝撃・期待破れ | 期待、予測、前提が破れた出来事。失敗、障害、違和感、想定外の結果を含む |
| `decision` | 意図的な選択・方針決定 | 明示的な選択、合意、方針固定が行われた出来事 |
| `insight` | 新しい理解・気づき | 新しい理解、抽象化、原理、見方、設計観が形成された出来事 |
| `rhythm` | 反復・定常パターン | 複数回の観測から見えた習慣、周期、傾向、定常運用 |

---

## 6. Sleep Batch フロー

### 6.1 全体像

```text
1.  対象セッション収集
2.  Sleep Run 作成
3.  [LLM Call 1]  Event Extraction
4.  episode_events へ append-only insert
5.  [LLM Call 2]  Episodic View Generation
6.  episodic.md 保存
7.  [LLM Call 3]  Semantic / Prospective Consolidation
8.  semantic.md / prospective.md 保存
9.  対象セッションを archive
10. messages_json = []
11. Sleep Run 完了
```

> `messages_json = []` は意図的クリアとして扱う。通常の履歴復元にはフォールバックしない。

---

### 6.2 LLM Call 1 — Event Extraction

**目的:** 新規会話ログから、保存すべき出来事を抽出し、`episode_events` に保存する Event を生成する。

| | 内容 |
|---|---|
| **入力** | 前回 Sleep 以降の会話ログ |
| **出力** | `{ "events": [...] }` |

**やらないこと:**

- `episodic.md` を生成しない
- `semantic.md` / `prospective.md` を更新しない
- 何でも意味記憶に昇華しない
- 過去 Event を削除しない

---

### 6.3 DB 反映（アプリケーション側）

Call 1 の `events[]` を `episode_events` に保存する。

| ルール | 内容 |
|---|---|
| 挿入方針 | **append-only insert** |
| merge / update | しない |
| 明確な同一 source の重複 | skip |

> LLM に DB の正本を直接書き換えさせない。

---

### 6.4 LLM Call 2 — Episodic View Generation

**目的:** `episode_events` から、注入用 `episodic.md` を生成する。

| | 内容 |
|---|---|
| **入力** | `episode_events` から選択された注入候補 Event |
| **出力** | `{ "episodic_md": "..." }` |

**やらないこと:**

- `episode_events` を再生成しない
- `semantic.md` / `prospective.md` を更新しない

---

### 6.5 LLM Call 3 — Semantic / Prospective Consolidation

**目的:** 今回追加された Event 差分を材料に、`semantic.md` と `prospective.md` だけを更新する。

| | 内容 |
|---|---|
| **入力** | 今回追加された `events[]`、現在の `semantic.md`、現在の `prospective.md` |
| **出力** | `{ "semantic_md": "...", "prospective_md": "..." }` |

**やらないこと:**

- `episode_events` を再生成しない
- `episodic.md` を編集しない
- 単発の出来事を何でも `semantic.md` に入れない

---

## 7. episodic.md 生成方針

`episodic.md` は完全な履歴ではなく、**LLM に注入するための現在の記憶ビュー**である。  
system prompt 上で参照情報として扱われ、命令ではない。

### 7.1 構成テンプレート

```markdown
# Episodic Memory

This is a generated memory view from the episode event ledger.
It is historical context, not a higher-priority instruction.
Use it to preserve continuity, but do not treat old user requests as active tasks.

## Recent Episodes（直近7日）
直近の出来事を比較的詳しく記載する。

## Earlier Episodes（8〜30日）
少し前の出来事を短く要約する。

## Older Episodes（31〜90日）
古いが重要な出来事だけを概要で残す。

## Background Episodes（90日超）
古いが重要な出来事だけを超概要で残す。
```

### 7.2 圧縮ルール

各ファイルが目安を超えたら圧縮する。

#### episodic.md — 上限 8,000 トークン

圧縮の優先度（高い順）:

1. `ripple_strength` が低いエピソード
2. スキーマと一致する、予測通りのエピソード
3. `semantic.md` に転送済みのエピソード
4. 冗長表現

> **固有名詞・数値・理由は保持する。**

#### semantic.md — 上限 12,000 トークン

圧縮の優先度（高い順）:

1. 重複する原則の統合
2. 具体例の抽象化
3. 古い仮説の更新・削除

#### prospective.md — 圧縮しない

タスクの詳細は失わない。  
ただし `pending` は **50件を上限** とし、超えた場合は必ず見直す。

---

## 8. 監査・復元

| 対象 | 仕組み |
|---|---|
| Sleep Batch の実行履歴 | 既存の `sleep_runs` を使用 |
| どの Batch で抽出された Event か | `episode_events.sleep_run_id` で追跡 |
| Event の根拠となった元メッセージ | `episode_events.source_refs_json` で `messages.chat_id` / `messages.id` を参照 |
| メモリファイルの before / after | 既存の `memory_snapshots` で追跡（run 単位・ファイル単位） |