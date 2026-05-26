# Plan: Pulse 配送先の明示的設定（PULSE.md delivery override）

PULSE.md front matter に `delivery` / `default_delivery` を追加し、Pulse の通知先（Home Surface）を intention 単位・agent 単位で明示的に指定できるようにする。現在は Home Surface の自動解決のみだが、ユーザーが意図的に配送先を制御したいユースケースに対応する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **PULSE.md に配送先を定義する**: Config はインフラ（チャネルの有無・認証）を管理し、PULSE.md は方針（何を・いつ・どこへ）を管理する。配送先は方針に属する（pulse.md §3.2 に基づく）
- **intention 単位 → agent 単位 → 自動解決の 3 段フォールバック**: `intention.delivery` → `default_delivery` → 従来の Home Surface 自動解決 → skipped
- **後方互換の維持**: delivery / default_delivery ともに省略可能。既存 PULSE.md は変更なしで動作する
- **バリデーションはパース時点で**: 不正な channel や空 external_chat_id はパースエラーとして即座に検出する

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | カテゴリ |
|---|---|
| `src/pulse/definition.rs` | Rust — パーサー拡張 |
| `src/pulse/capsule.rs` | Rust — Home Surface 解決拡張 |
| `src/pulse/scheduler.rs` | Rust — 解決チェーン統合 |
| `docs/pulse.md` | Docs — 仕様書更新 |

---

## Step 0: Worktree 作成

`worktree-create` skill を使用して worktree を作成する。

---

## Step 1: DeliverySpec の定義と PULSE.md パーサー拡張 (TDD)

前提: なし

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_intention_delivery` | intention に delivery（channel + external_chat_id）指定ありで正しくパースされる |
| `parse_default_delivery` | トップレベルの default_delivery が正しくパースされる |
| `parse_delivery_optional_on_intention` | intention の delivery 省略時は `None` になる |
| `parse_default_delivery_optional` | default_delivery 省略時は `None` になる |
| `parse_rejects_invalid_channel` | 不正 channel（`web`, `cli`, `foo`）で PulseParseError |
| `parse_rejects_empty_external_chat_id` | 空の external_chat_id で PulseParseError |
| `parse_both_delivery_sources` | default_delivery + intention delivery 両方ありで意図通り解決される |
| `parse_delivery_without_front_matter` | delivery なしの従来形式は変更なくパースされる（後方互換） |

### GREEN: 実装

- `DeliverySpec` struct を定義（channel, external_chat_id）
- `PulseDefinition` に `default_delivery: Option<DeliverySpec>` を追加
- `TemporalIntention` に `delivery: Option<DeliverySpec>` を追加
- `PulseFrontMatter` / `IntentionRaw` / `ScheduleRaw` に対応する serde フィールドを追加
- バリデーション: channel は discord/telegram のみ、external_chat_id は空不可
- 既存の parse テストがすべて通ることを確認

### コミット

`feat(pulse): add DeliverySpec and extend PULSE.md parser`

---

## Step 2: Home Surface 解決の拡張 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `explicit_delivery_resolves_from_db` | 明示 delivery 指定時、DB から該当 chat を解決して HomeSurface を返す |
| `explicit_delivery_returns_none_when_chat_not_found` | DB に該当 chat がない場合 None を返す |
| `explicit_delivery_returns_none_when_adapter_missing` | 指定 channel の adapter が存在しない場合 None を返す |
| `no_delivery_falls_back_to_auto_resolve` | delivery 指定なし → 従来の自動解決が動作する（既存テストで確認済み） |

### GREEN: 実装

- `resolve_home_surface` シグネチャに `Option<DeliverySpec>` を追加
- 明示指定時は `resolve_chat_id` で DB ルックアップ → `get_chat_by_id` で ChatInfo 取得 → HomeSurface 構築
- adapter 存在チェックは `available_channels` で検証
- None 時は従来の自動解決ロジックに委譲

### コミット

`feat(pulse): resolve explicit delivery in Home Surface resolution`

---

## Step 3: Scheduler の解決チェーン統合 (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `intention_delivery_overrides_default` | intention delivery あり → default を無視して intention の方を使う |
| `default_delivery_used_when_intention_omits` | intention delivery なし → default_delivery を使う |
| `auto_resolve_when_no_delivery` | 両方なし → 従来の自動解決 |
| `skipped_when_explicit_chat_not_found` | 明示指定だが DB に該当 chat なし → skipped になる |
| `mixed_agents_with_and_without_delivery` | 複数 agent で delivery の有無が混在していても正しく動作する |

### GREEN: 実装

- `process_intention` で解決チェーンを組み立て:
  1. `intention.delivery` を取得
  2. なければ `definition.default_delivery` を使用
  3. `resolve_home_surface` に delivery を渡す
  4. None フォールバックで自動解決

### コミット

`feat(pulse): wire delivery resolution chain in scheduler`

---

## Step 4: ドキュメント更新

前提: Step 3

### 実装

- `docs/pulse.md` §2.2 に delivery override の説明と解決順序を追記
- `docs/pulse.md` §3.1 に `delivery` / `default_delivery` フィールド仕様を追記
- YAML 例を追加

### コミット

`docs(pulse): document delivery override in PULSE.md spec`

---

## Step 5: 動作確認

- `cargo test` 全テスト通過
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`

---

## Step 6: PR 作成

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/pulse/definition.rs` | 変更 | DeliverySpec struct 追加、PulseDefinition / TemporalIntention 拡張、パーサー拡張 |
| `src/pulse/capsule.rs` | 変更 | resolve_home_surface シグネチャ変更、明示配送ルート追加 |
| `src/pulse/scheduler.rs` | 変更 | 解決チェーン（intention → default → auto）の組み立て |
| `docs/pulse.md` | 変更 | delivery override 仕様追記 |

---

## コミット分割

1. `feat(pulse): add DeliverySpec and extend PULSE.md parser`
2. `feat(pulse): resolve explicit delivery in Home Surface resolution`
3. `feat(pulse): wire delivery resolution chain in scheduler`
4. `docs(pulse): document delivery override in PULSE.md spec`

---

## テストケース一覧（全 17 件）

### definition.rs — パーサー (8)
1. `parse_intention_delivery` — intention delivery ありで正しくパース
2. `parse_default_delivery` — default_delivery が正しくパース
3. `parse_delivery_optional_on_intention` — intention delivery 省略時は None
4. `parse_default_delivery_optional` — default_delivery 省略時は None
5. `parse_rejects_invalid_channel` — 不正 channel でエラー
6. `parse_rejects_empty_external_chat_id` — 空 external_chat_id でエラー
7. `parse_both_delivery_sources` — default + intention 両方ありで正しく解決
8. `parse_delivery_without_front_matter` — 従来形式の後方互換

### capsule.rs — Home Surface 解決 (4)
9. `explicit_delivery_resolves_from_db` — 明示指定で DB から解決
10. `explicit_delivery_returns_none_when_chat_not_found` — DB に chat なしで None
11. `explicit_delivery_returns_none_when_adapter_missing` — adapter なしで None
12. `no_delivery_falls_back_to_auto_resolve` — 省略時は従来ロジック

### scheduler.rs — 統合 (5)
13. `intention_delivery_overrides_default` — intention が default より優先
14. `default_delivery_used_when_intention_omits` — intention 省略 → default 使用
15. `auto_resolve_when_no_delivery` — 両方なし → 自動解決
16. `skipped_when_explicit_chat_not_found` — 明示指定で chat なし → skipped
17. `mixed_agents_with_and_without_delivery` — 複数 agent 混在動作

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | Worktree 作成 | ~10 行 |
| Step 1 | DeliverySpec + パーサー拡張 | ~200 行 |
| Step 2 | Home Surface 解決拡張 | ~120 行 |
| Step 3 | Scheduler 統合 | ~100 行 |
| Step 4 | ドキュメント更新 | ~80 行 |
| Step 5 | 動作確認 | ~0 行 |
| Step 6 | PR 作成 | ~0 行 |
| **合計** | | **~510 行** |
