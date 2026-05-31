# Plan: PULSE.md に配送先を指定

PULSE.md front matter に `default_delivery` と intention 単位の `delivery` を追加し、配送先を明示的に指定できるようにする。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **PULSE.md に配送先を定義する**: Config はインフラを管理し、PULSE.md は方針を管理する
- **3 段解決順序**: `intention.delivery` → `default_delivery` → 従来の自動解決 → skipped
- **現行仕様のみ許可**: delivery / default_delivery は省略可能だが、PULSE.md に内容がある場合は front matter を必須にする
- **エラーは握り潰さない**: 指定先が不正ならその intention のみ skipped + 警告ログ
- **バリデーションはパース時点で**: 不正な channel や空 external_chat_id はパースエラー

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 変更種別 |
|---|---|
| `src/pulse/definition.rs` | 変更 — DeliverySpec 定義、パーサー拡張 |
| `src/pulse/capsule.rs` | 変更 — resolve_home_surface シグネチャ変更、明示配送ルート追加 |
| `src/pulse/scheduler.rs` | 変更 — resolve_home_surface 呼び出しに delivery 伝播 |
| `docs/pulse.md` | 変更 — 仕様書に delivery 機能を追記 |

---

## Step 0: Worktree 作成

`worktree-create` skill を使用して worktree を作成する。

---

## Step 1: DeliverySpec 定義とパーサー拡張 (TDD)

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
| `parse_delivery_without_front_matter_is_rejected` | front matter なしの PULSE.md はパースエラー |

### GREEN: 実装

- `DeliverySpec` struct を定義（channel, external_chat_id）
- `PulseDefinition` に `default_delivery: Option<DeliverySpec>` を追加
- `TemporalIntention` に `delivery: Option<DeliverySpec>` を追加
- `PulseFrontMatter` / `IntentionRaw` に対応する serde フィールドを追加
- バリデーション: channel は discord/telegram のみ、external_chat_id は空不可
- 既存のパーステストがすべて通ることを確認

### コミット

`feat(pulse): add DeliverySpec and extend PULSE.md parser`

---

## Step 2: resolve_home_surface 拡張 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `explicit_delivery_resolves_from_db` | 明示 delivery 指定時、DB から該当 chat を解決して HomeSurface を返す |
| `explicit_delivery_returns_none_when_chat_not_found` | DB に該当 chat がない場合 None を返す |
| `explicit_delivery_returns_none_when_adapter_missing` | 指定 channel の adapter が存在しない場合 None を返す |
| `no_delivery_falls_back_to_auto_resolve` | delivery 指定なし → 従来の自動解決が動作する |

### GREEN: 実装

- `resolve_home_surface` シグネチャに `Option<&DeliverySpec>` を追加
- 明示指定時は `resolve_or_create_chat_id` → `get_chat_by_channel_external` で DB ルックアップ
- adapter 存在チェックは `available_channels` で検証
- None 時は従来の自動解決ロジックに委譲

### コミット

`feat(pulse): resolve explicit delivery in Home Surface resolution`

---

## Step 3: Scheduler に delivery 伝播 (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `scheduler_passes_intention_delivery` | intention に delivery 指定ありで、その配送先が使われる |
| `scheduler_passes_default_delivery` | intention の delivery なし、default_delivery ありで、その配送先が使われる |
| `scheduler_falls_back_to_auto_when_no_delivery` | 両方なしで従来の自動解決が使われる |

### GREEN: 実装

- `process_intention` 内で intention.delivery / default_delivery を解決し `resolve_home_surface` に渡す
- delivery 解決ロジックは `capsule.rs` に helper 関数として追加
- skipped 時のログに配送先解決の詳細を含める

### コミット

`feat(pulse): wire delivery resolution chain in scheduler`

---

## Step 4: docs/pulse.md 更新

前提: Step 3

### GREEN: 実装

- §2.2 Home Surface 解決順序に intention.delivery / default_delivery を追記
- §3.1 PULSE.md 仕様に delivery / default_delivery フィールドを追記
- §14 Phase 1 のスコープ「やらない」から「複雑な delivery override」を削除
- §14 実装判断で `default_delivery` 未実装を撤回

### コミット

`docs(pulse): document delivery override in PULSE.md spec`

---

## Step 5: 動作確認

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- パースエラーテストの動作確認

---

## Step 6: PR 作成

PR description は日本語。該当 Issue がある場合は `Close #XX` 明記。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/pulse/definition.rs` | **変更** | DeliverySpec struct、PulseDefinition/TemporalIntention にフィールド追加、パース拡張 |
| `src/pulse/capsule.rs` | **変更** | resolve_home_surface シグネチャ変更、明示配送ルート追加 |
| `src/pulse/scheduler.rs` | **変更** | resolve_home_surface 呼び出しに delivery 伝播 |
| `docs/pulse.md` | **変更** | 仕様書に delivery 機能を追記 |

---

## コミット分割

1. `feat(pulse): add DeliverySpec and extend PULSE.md parser` — definition.rs
2. `feat(pulse): resolve explicit delivery in Home Surface resolution` — capsule.rs
3. `feat(pulse): wire delivery resolution chain in scheduler` — scheduler.rs
4. `docs(pulse): document delivery override in PULSE.md spec` — docs/pulse.md

---

## テストケース一覧（全 15 件）

### definition.rs パーサー (8)

1. `parse_intention_delivery` — intention に delivery 指定ありで正しくパースされる
2. `parse_default_delivery` — トップレベルの default_delivery が正しくパースされる
3. `parse_delivery_optional_on_intention` — intention の delivery 省略時は None
4. `parse_default_delivery_optional` — default_delivery 省略時は None
5. `parse_rejects_invalid_channel` — 不正 channel で PulseParseError
6. `parse_rejects_empty_external_chat_id` — 空 external_chat_id で PulseParseError
7. `parse_both_delivery_sources` — default_delivery + intention delivery 両方あり
8. `parse_delivery_without_front_matter_is_rejected` — front matter なしはパースエラー

### capsule.rs Home Surface (4)

9. `explicit_delivery_resolves_from_db` — 明示配送先を DB から解決
10. `explicit_delivery_returns_none_when_chat_not_found` — chat 未存在で None
11. `explicit_delivery_returns_none_when_adapter_missing` — adapter 無効で None
12. `no_delivery_falls_back_to_auto_resolve` — delivery なしで自動解決

### scheduler.rs 連携 (3)

13. `scheduler_passes_intention_delivery` — intention delivery が伝播される
14. `scheduler_passes_default_delivery` — default_delivery が伝播される
15. `scheduler_falls_back_to_auto_when_no_delivery` — 両方なしで自動解決

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | Worktree 作成 | ~10 行 |
| Step 1 | DeliverySpec + パーサー | ~120 行 |
| Step 2 | resolve_home_surface 拡張 | ~80 行 |
| Step 3 | Scheduler に delivery 伝播 | ~40 行 |
| Step 4 | docs/pulse.md 更新 | ~50 行 |
| Step 5 | 動作確認 | — |
| Step 6 | PR 作成 | — |
| **合計** | | **~300 行** |
