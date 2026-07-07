# Plan: Safety Compaction のトークン推定を実測 usage フィードバックで校正する

Safety Compaction の発火判定が依存するトークン推定を、文字数ベースの概算（`bytes/3`）単体から、**実測 usage で継続的に校正するハイブリッド方式**へ置き換える。プロバイダが返す `usage.input_tokens` で補正係数を学習し、chars/3 の日本語過小評価を吸収する。ローカル tokenizer のダウンロードや新規 DB API の追加は行わない。

> **Note**: 振る舞い（What）は「推定トークンが閾値に達したら compaction 発火」。これは不変。変更するのは推定の精度（HOW）のみ。

## 目次

1. [動機](#1-動機)
2. [設計方針](#2-設計方針)
3. [アーキテクチャ概要](#3-アーキテクチャ概要)
4. [コンポーネント設計](#4-コンポーネント設計)
5. [設定スキーマ](#5-設定スキーマ)
6. [データフロー](#6-データフロー)
7. [対象一覧](#7-対象一覧)
8. [TDD テストリスト](#8-tdd-テストリスト)
9. [実装スコープと限界](#9-実装スコープと限界)
10. [セキュリティと運用](#10-セキュリティと運用)
11. [未解決 / 保留](#11-未解決--保留)
12. [動作確認](#12-動作確認)
13. [Plan・仕様書との自己チェック](#13-plan仕様書との自己チェック)
14. [PR 作成](#14-pr-作成)
15. [初回レビューバック](#15-初回レビューバック)
16. [レビュー対応後の再レビューバック](#16-レビュー対応後の再レビューバック)

---

## 1. 動機

### 現状の問題

`estimate_prompt_tokens`（`src/agent_loop/compaction.rs`）は次の概算式を採っている。

```rust
(total_chars / 3).max(1)   // total_chars は String::len() = バイト長
```

コメント上は「chars-based 近似で実際より多めに見積もる」ことを意図しているが、`String::len()` は**バイト長**であるため、UTF-8 で3バイト表現の日本語テキストでは `bytes/3 = 文字数` となり、BPE 実トークン（日本語1文字 ≈ 1.3〜2 token）に対して**過小評価**になる。

### 実被害

`chat_id=18`（Discord / agent=lyre / model=glm-5.2）で発生した Context7 MCP 無限ループ事故：

- ループ中の実 `input_tokens` が 74K → 100K へ単調増加
- 一方 `llm_usage_logs` の `request_kind='compaction'` は **0件**（summary LLM 呼び出しが一度も発火していない）
- つまり推定値が閾値 `usable × 0.80` に一度も到達せず、安全装置が作動しなかった
- 結果、コンテキスト肥大で推論品質が劣化し、EgoGraph/Context7 のツール誤選択ループに陥った

### 決定的な点

egopulse は**すでにプロバイダから実測 `usage.input_tokens` を受け取り、`llm_usage_logs` に記録している**。この実測値を推定の校正に使えば、chars/3 の過小評価を吸収できる。外部リソースのダウンロード（ローカル tokenizer 等）は不要。`usage` を返すプロバイダでは実測で収束し、返さないプロバイダでも未計測時の保守的な既定係数で過小評価リスクを下げる。

### 目標

実測 usage で校正された補正係数を推定に掛け合わせ、Safety Compaction の発火判定を信頼させる。日本語中心の会話でも閾値到達しやすい保守的な推定にする。

---

## 2. 設計方針

1. **実測 usage フィードバックが主軸**: プロバイダが返す `usage.input_tokens` で補正係数を学習し、chars/3 推定に掛ける。外部ダウンロード不要。
2. **chars/3 は推定のベース**: `estimate_prompt_tokens` の中身（bytes/3）は維持。過小評価のズレは補正係数で吸収する。
3. **学習は送信 payload 基準**: 補正係数の学習は、compaction 判定時の推定ではなく、**LLM へ実際に送信した payload** の推定と、その response の `usage.input_tokens` を対応付けて行う（compaction 前後で payload が変わるため）。
4. **未計測時も保守側に倒す**: 起動直後（実測蓄積なし）は factor=1.0 ではなく、固定の `DEFAULT_FACTOR` を返して過小評価を抑える。
5. **新仕様へ一直線**: `maybe_compact_messages` の推定フローを差し替え。旧仕様維持のための分岐は作らない。
6. **公開範囲は最小**: 新 API は `pub(crate)` から始める。

---

## 3. アーキテクチャ概要

### Before（現状）

```
maybe_compact_messages
  ├─ resolve_context_window_tokens(provider, model)   // 設定値
  ├─ estimate_prompt_tokens(system, messages, tools)   // bytes/3
  └─ should_compact(estimated, usable, 0.80)?
        └─ safety_compact → summarize_old_messages (LLM) → build_compaction_result
```

### After（本プラン）

```
maybe_compact_messages
  ├─ raw_est  = estimate_prompt_tokens(system, messages, tools)  // bytes/3（維持）
  ├─ key      = CalibrationKey { provider, model, "agent_loop", has_tools }
  ├─ factor   = state.calibrator.factor(&key)        // 実測で校正された補正係数（未計測時 DEFAULT_FACTOR）
  ├─ estimated = calibrated_estimate(raw_est, factor)
  ├─ usable   = resolve_context_window_tokens(provider, model) - 8192
  └─ should_compact(estimated, usable, 0.80)?
        └─ safety_compact → ... (内部の target 判定も同じ calibrated_estimate を使用)

LLM 送信経路ごと（送信前 keep → response 後 record）:
  ├─ agent_loop:       record(CalibrationKey{.., "agent_loop", has_tools}, raw_est, usage)
  └─ compaction sum.:  record(CalibrationKey{.., "compaction", false}, raw_est, usage)
```

コンポーネント構成：

```
src/agent_loop/
├── compaction.rs         (変更) maybe_compact_messages で factor 適用
└── tool_phase.rs         (変更) 学習パス（送信前 estimate 保持 → response 後 record）

src/runtime/
└── mod.rs                (変更) AppState に UsageCalibrator

src/llm/
└── calibration.rs        (新規) UsageCalibrator + CalibrationKey
```

---

## 4. コンポーネント設計

### 4.1 `UsageCalibrator`（`src/llm/calibration.rs` 新規）

`(provider, model, request_kind, has_tools)` 単位の補正係数をメモリ内で保持する。観測は `llm_usage_logs` に永続化し、起動時に最近 N 件から EMA で再構築する（プロセス再起動後も学習状態が維持される）。

```rust
#[derive(Hash, Eq, PartialEq, Clone)]
pub(crate) struct CalibrationKey {
    pub provider: String,
    pub model: String,
    pub request_kind: String,   // "agent_loop" / "compaction"
    pub has_tools: bool,         // tool schema を含む payload か
}

pub(crate) struct UsageCalibrator {
    factors: tokio::sync::RwLock<HashMap<CalibrationKey, f64>>,
    // EMA の重み（例: α=0.3）
}

impl UsageCalibrator {
    /// 補正係数を取得。未計測時は DEFAULT_FACTOR。
    pub(crate) async fn factor(&self, key: &CalibrationKey) -> f64;

    /// estimated に対する actual の比で係数を EMA 更新。
    /// estimated が 0 以下、actual が異常値（0 以下）は無視。
    /// 係数は [0.5, 3.0] にクリップ（異常スパイク防止）。
    pub(crate) async fn record(
        &self, key: CalibrationKey, estimated: usize, actual: i64,
    );
}
```

設計判断：

- **キー粒度**: `(provider, model, request_kind, has_tools)`。tool-heavy な agent_loop と tool 無しの compaction summarizer は overhead 構造が異なるため別係数。これより細かい（tool schema hash 等）とサンプルが足りず収束しない。`has_tools` の大区分が実用的妥協点。
- **EMA（指数移動平均）**: 単発のスパイク（ツール結果が異常に大きい等）で係数が振れるのを防ぐ。
- **クリップ [0.5, 3.0]**: 係数が極端になると推定が暴走する。下限 0.5（過大評価時の修正）、上限 3.0（過小評価時の修正、今回の日本語ケース相当）。
- **estimated == 0 を除外**: 推定が 0 の場合は記録しない（ゼロ除算・異常係数防止）。
- **永続化**: 観測（生推定値と実測 `input_tokens` のペア）は `llm_usage_logs` に保存し、起動時に最近 N 件から EMA で再構築する。これにより再起動後も学習状態が維持され、コールドスタート時の過大 `DEFAULT_FACTOR` による誤発火を防ぐ。

### 4.2 `estimate_prompt_tokens`（既存、維持）

`src/agent_loop/compaction.rs` の既存関数。`bytes/3` 推定の中身は変更しない。過小評価のズレは `UsageCalibrator` の factor で吸収するため、推定関数自体の改良は不要。

呼び出し元の `maybe_compact_messages` と compaction 後の target 判定で、推定結果に同じ factor を掛ける。

### 4.3 `DEFAULT_FACTOR`

起動直後（実測蓄積なし）の過小評価をカバーするため、未計測 key では 1.0 ではなく固定の既定係数を返す。

```rust
/// chars/3 の日本語過小評価を未計測時から補う保守係数。
const DEFAULT_FACTOR: f64 = 1.3;

fn calibrated_estimate(raw_estimate: usize, factor: f64) -> usize {
    ((raw_estimate as f64) * factor).ceil().max(1.0) as usize
}
```

新しい設定は追加しない。既存の `compaction_threshold_ratio` は維持し、発火ロジックの調整は補正係数に閉じる。

### 4.4 学習パス（送信 payload 基準・2経路）

学習は LLM 送信を行う2経路で実施。いずれも**送信前に estimate と key を計算してローカル変数に保持**する（payload は `send_message` に move されるため、response 後には再計算できない）。response の `usage` 受信後に record する。

#### 経路1: agent_loop（`send_tool_phase_request`）

```text
send_tool_phase_request (src/agent_loop/tool_phase.rs:116)
  │  ※ 送信前（move 前）に ToolPhaseRequest から以下を計算して保持:
  │    raw_est = estimate_prompt_tokens(request.system_prompt, &request.messages, tools_json)
  │    key     = CalibrationKey { provider, model, request_kind: "agent_loop",
  │                               has_tools: request.tools.as_ref().map_or(false, |t| !t.is_empty()) }
  ├─ response = llm.send_message(request.system_prompt, request.messages, request.tools).await  // move 発生
  └─ response.usage が存在すれば:
       state.calibrator.record(key, raw_est, usage.input_tokens)
```

#### 経路2: compaction summarizer（`send_summary_request`）

```text
send_summary_request (src/agent_loop/compaction.rs)
  │  ※ 送信前に summary_input から以下を計算して保持:
  │    raw_est = estimate_prompt_tokens(SUMMARIZER_SYSTEM_PROMPT,
  │                                    &[Message::text("user", &summary_input)], None)
  │    key     = CalibrationKey { provider, model, request_kind: "compaction", has_tools: false }
  ├─ response = llm.send_message(...).await  // move 発生
  └─ summarize_old_messages 側で response.usage が存在すれば:
       state.calibrator.record(key, raw_est, usage.input_tokens)
```

`TurnLoopState` には推定値を持たせない。agent_loop は `send_tool_phase_request` のローカル変数で完結させる。compaction summarizer は `send_summary_request` の戻り値に `raw_est` と `key` を添えるか、呼び出し元 `summarize_old_messages` で送信前に保持して response 後に record する。`usage` が `None` のプロバイダでは record しない。

compaction 判定（§6）の estimate は record に使わない。判定は「推定 × factor（agent_loop 係数）」のみで行い、学習は上記2経路のみ。

---

## 5. 設定スキーマ

**設定追加なし。**

- `UsageCalibrator` の factor は実測から自動学習するため、ユーザー設定不要。
- `DEFAULT_FACTOR` はコード内定数。
- 既存の `compaction_threshold_ratio`（デフォルト 0.80）は維持する。

---

## 6. データフロー

### compaction 判定

```text
persist_user_turn_with_compaction / tool 後 maybe_compact_messages
  │
  ├─ raw_est = estimate_prompt_tokens(system, messages, tools)   // bytes/3（維持）
  ├─ next_tools が存在するか確認（has_tools 判定）
  ├─ key     = CalibrationKey { provider, model, "agent_loop", has_tools }
  │            ※ 判定は「次に送る agent_loop payload」を予測するため agent_loop 係数を使用
  ├─ factor  = state.calibrator.factor(&key).await                // 未計測時 DEFAULT_FACTOR
  ├─ estimated = calibrated_estimate(raw_est, factor)
  ├─ usable  = resolve_context_window_tokens(provider, model) - 8192
  └─ should_compact(estimated, usable, 0.80)?
        └─ YES → safety_compact（target 判定も同じ calibrated_estimate を使用）
```

### 補正係数学習

§4.4 の2経路（agent_loop / compaction summarizer）を参照。

---

## 7. 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/llm/calibration.rs` | **新規** | — | `UsageCalibrator`, `CalibrationKey` |
| `src/llm/mod.rs` | 変更 | 既存モジュール公開 | `pub(crate) mod calibration;` |
| `src/runtime/mod.rs` | 変更 | `AppState` | `UsageCalibrator` フィールド、`build_app_state` での初期化 |
| `src/agent_loop/compaction.rs` | 変更 | `maybe_compact_messages` | 推定に factor を適用。compaction 後 target 判定にも同じ補正式を使用。summary 送信の学習パス追加 |
| `src/agent_loop/tool_phase.rs` | 変更 | `send_tool_phase_request`（tool_phase.rs:116） | 学習パス追加。送信前（move 前）に `raw_est`/`key` を保持し、response 後に `calibrator.record`。`AppState` アクセス追加 |
| `docs/session-lifecycle.md` | 変更 | §5 Safety Compaction | 推定方式（chars/3 + 実測補正）の記述へ更新 |

---

## 8. TDD テストリスト

バックエンド（Red→Green→Refactor サイクルごとに1件ずつ追加）：

- **T1**: `UsageCalibrator.record` で `estimated < actual` のとき factor > 1.0 に更新される
- **T2**: `UsageCalibrator` の EMA が単発スパイクで急激に変動しない（連続 record で漸増）
- **T3**: `UsageCalibrator` の係数が `[0.5, 3.0]` にクリップされる（極端値入力で上下限）
- **T4**: `UsageCalibrator.record` が `estimated == 0` または `actual <= 0` を無視する
- **T5**: `UsageCalibrator.factor` が未計測 key で `DEFAULT_FACTOR` を返す
- **T6**: `CalibrationKey` が `(request_kind, has_tools)` で異なる係数を保持する（agent_loop と compaction の汚染防止）
- **T7**: `maybe_compact_messages` が factor 適用で `should_compact` を判定（GLM-5.2 日本語ケースで、factor=2.0 相当で閾値到達することを模擬データで検証）
- **T8**: compaction 後 target 判定（post check / summary truncation）にも同じ補正式が使われる
- **T9**: `send_tool_phase_request` で送信前（move 前）に `raw_est`/`key` を計算して保持し、response 受信後に `calibrator.record`（move 後に再計算しない）
- **T10**: compaction summarizer 経路でも送信前に estimate 保持 → response 後に record（key の `request_kind="compaction"`, `has_tools=false`）
- **T11**: `usage` が `None` のプロバイダでは record しない（未計測係数 `DEFAULT_FACTOR` のまま）
- **T12**: `has_tools` が空 tool list で `false` になる（`request.tools.is_some()` ではなく中身の有無で判定）

### 実データによる検証（手動）

- **V1**: `chat_id=18` の実際の `llm_usage_logs.input_tokens` に対し、chars/3 推定値が過小評価（実トークンの半分以下）であることを確認。その上で、factor を適用した推定値が実測の **±30% 以内**に収まることを確認
- **V2**: factor 収束後（数ターン後）、推定値が実測値に ±20% 以内で追従することを確認

---

## 9. 実装スコープと限界

### 実装する内容

- `UsageCalibrator`（メモリ内、EMA、クリップ [0.5, 3.0]、非永続化）
- `CalibrationKey { provider, model, request_kind, has_tools }`
- `maybe_compact_messages` と compaction 後 target 判定で factor 適用
- 学習パス（`send_tool_phase_request`, `send_summary_request` の2経路、送信前 keep → response 後 record）

### 今回の事故に対する効果と限界

今回の GLM ループ事故の**発生確率を大幅に低下**させる。実測 usage で校正された factor が chars/3 の過小評価を吸収するため、推定が実トークンに近付く。

ただし完全保証ではない。以下の制約がある:

- 補正係数は起動直後 `DEFAULT_FACTOR`（実測蓄積なし）。実測がある provider/model は数ターンで収束する
- `usage` を返さないプロバイダでは学習が働かず、`DEFAULT_FACTOR` のまま

境界対策: 未計測時から `DEFAULT_FACTOR` を適用し、設定・DB・外部依存を増やさず保守側に倒す。

---

## 10. セキュリティと運用

### セキュリティ

- **外部通信なし**: ローカル tokenizer のダウンロード等は行わない。実測 usage は既存の LLM API レスポンスから取得するもので、追加の通信は発生しない。
- **秘密情報の不入力**: `CalibrationKey` は provider/model/request_kind/has_tools のみで、プロンプト内容や認証情報は含まない。factor も比率のみで内容は保持しない。

### 運用

- **依存追加なし**: 新規クレートなし。`Cargo.toml` の変更不要。
- **ビルド時間影響なし**: ローカル tokenizer 等の重い依存を入れないため、ビルド時間・バイナリサイズへの影響はない。
- **オフライン環境**: 外部リソースに依存しないため、オフラインでも全力で動作する。
- **プロバイダ互換**: `usage.input_tokens` を返すプロバイダでは実測補正が働く。返さないプロバイダでは `DEFAULT_FACTOR` の保守推定で動作する。

---

## 11. 未解決 / 保留

- **`DEFAULT_FACTOR` の値**: 1.3 に調整。永続化により DEFAULT に頼るのは未知の provider/model の初回のみとなったため、過大だった 1.6 から引き下げた。
- **EMA の重み α**: 0.3 で妥当か。収束速度とスパイク耐性のトレードオフを実データで調整。
- **クリップ範囲 `[0.5, 3.0]`**: 実データで不適切（広すぎる/狭すぎる）なら調整。
- **`sleep/orchestrator.rs` の `context_tokens`**: `resolve_context_window_tokens` の戻り値（設定値そのもの）を使用している場合は本プランの影響なし。実装修正時に確認。
- **永続化**: プロセス再起動で factor は未計測状態に戻る。永続 warm start は追加しない。

---

## 12. 動作確認

- 全テスト通過: `cargo test --all-features`
- Lint / フォーマット: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`
- 型チェック: `cargo check --all-features`
- 実データ検証: V1（chars/3 推定の過小評価確認 + factor 適用で ±30% 以内）、V2（収束後 ±20% 以内）
- デッドコード確認: 新規補助関数が本流から使われていることを `rg` で検証
- 失敗時に戻る Step: 該当する TDD Cycle（§8）へ戻り修正

---

## 13. Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書（`docs/session-lifecycle.md` §5 Safety Compaction）を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリスト（§8 T1-T12）と各 Cycle が完了条件を満たしている
- `docs/session-lifecycle.md` §5 の Safety Compaction 振る舞い（What: 推定トークンが閾値に達したら発火）と実装結果が一致している
- 実装中に変更した設計判断（推定方式の chars/3 + 実測補正、`DEFAULT_FACTOR` の導入）が `docs/session-lifecycle.md` §5 へ反映されている
- 変更ファイル一覧（§7）、自動テスト一覧（§8）が実際の変更と一致している

---

## 14. PR 作成

- PR タイトル: `feat: calibrate token estimation with observed usage for safety compaction`
- PR description:
  - 概要: Safety Compaction のトークン推定（chars/3）に、実測 usage で校正する補正係数を導入。chars/3 の日本語過小評価を吸収し、GLM-5.2 等での compaction 不発火を抑制。外部ダウンロード・設定追加・DB API 追加なし。
  - テスト: UT（T1-T12）+ 実データ検証（V1, V2）
  - Close #<issue-number>（該当する場合）

---

## 15. 初回レビューバック

PR 作成後、レビュー生成を待ってから `pr-review-back-workflow` Skill を実行し、未対応のレビューコメントがあれば修正・検証・コミット・push まで完了する。

- 初回待機: `sleep 15m`
- レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだレビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- レビューコメントが無い、または最大待機後もレビューが無い場合は、その結果を PR に記録して完了扱いにする
- レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## 16. レビュー対応後の再レビューバック

レビュー対応を push した後、追加レビュー生成を待ってから `pr-review-back-workflow` Skill を再実行し、残った指摘や新規指摘があれば同じ品質基準で対応する。

- 対象: §15 でレビュー対応の変更を push した場合
- 初回待機: `sleep 15m`
- 再レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだ追加レビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- 追加レビューコメントが無い、または最大待機後も追加レビューが無い場合は、その結果を PR に記録して完了扱いにする
- 再レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する
