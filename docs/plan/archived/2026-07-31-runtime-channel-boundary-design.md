# Runtime Channel 境界整理 設計・実装計画

## 1. 目的

runtime core が Discord / Telegram の ID 型、設定型、channel 固有の文字列を直接解釈しない構造へ整理する。

対象は次の2箇所に限定する。

- `ChannelLogKey::Discord(u64)` / `ChannelLogKey::Telegram(i64)`
- runtime にある `tool_progress_enabled`

Channel adapter の受信・送信ロジック全体を作り直したり、全 channel を新しい trait 階層へ移したりはしない。

## 2. 結論と実現可能性

実装可能であり、DB migration は不要である。最も単純な方針は、platform 固有値を入口で文字列へ正規化し、設定スキーマに依存する判定を `config` 側へ移すことである。

tool progress の判定を adapter trait へ移す案も成立するが、設定 hot reload と adapter のライフサイクルまで同時に考える必要があり、変更範囲が広がる。現段階では、`Config::tool_progress_enabled(...)` を追加して config resolver に閉じ込める案を採用する。

## 3. 現状の課題

### 3.1 `ChannelLogKey`

[`src/runtime/channel_input.rs`](../../../src/runtime/channel_input.rs) に次の enum がある。

```rust
enum ChannelLogKey {
    Discord(u64),
    Telegram(i64),
}
```

同じ module の保存処理がこの enum を match し、Discord と Telegram の storage method を呼び分けている。

このため、新しい channel を追加するたびに次の変更が必要になる。

- runtime core の enum 追加
- runtime core の match 追加
- storage の専用 resolve method 追加
- runtime の re-export 変更
- core 側テスト変更

Channel Log が必要なのは platform API の型ではなく、channel 名と安定した conversation identifier である。DB の `external_chat_id` も文字列なので、`u64` と `i64` を core まで運ぶ理由はない。

### 3.2 `tool_progress_enabled`

[`src/runtime/mod.rs`](../../../src/runtime/mod.rs) の関数は、runtime core で次を行っている。

- `"discord"` / `"telegram"` の文字列分岐
- `surface_thread` の `u64` / `i64` parse
- Discord / Telegram 専用 config map の参照
- `tool_progress` の判定

一方、channel adapter はすでに `ToolProgressSink` を提供している。runtime core が platform 設定の schema まで知ることで、channel の責務が二重化している。

この関数が新しい channel に対応しない場合、未知 channel は silently `false` になる。新しい channel を追加しても、コンパイルエラーではなく機能無効として現れるため、発見が遅い。

## 4. 設計方針

### 4.1 中立的な Channel Log address

`ChannelLogKey` を次の型へ置き換える。

```rust
pub(crate) struct ChannelLogAddress {
    pub(crate) channel: String,
    pub(crate) conversation_id: String,
}
```

Discord adapter は `channel = "discord"`、`conversation_id = channel_id.to_string()` を渡す。Telegram adapter は `channel = "telegram"`、`conversation_id = raw_chat_id.to_string()` を渡す。

runtime core は ID の parse や platform enum の match をしない。

### 4.2 storage の resolve を共通化する

Discord 専用と Telegram 専用の2メソッドを、次の共通契約へ統合する。

```rust
resolve_channel_log_chat_id(
    channel: &str,
    conversation_id: &str,
) -> Result<i64, StorageError>
```

既存の外部 ID 形式は変えない。

```text
{channel}:{conversation_id}:multi-room-log
```

これにより既存の Discord / Telegram Channel Log 行を同じ chat として解決できる。新しい channel は文字列 identifier を渡すだけでよい。

### 4.3 tool progress の config schema 依存を `config` に移す

`runtime::tool_progress_enabled` を削除し、`Config` に次の resolver を追加する。

```rust
pub(crate) fn tool_progress_enabled(
    &self,
    channel: &str,
    conversation_id: &str,
) -> bool
```

Discord / Telegram の map lookup と数値 parse は `src/config/resolve.rs` に置く。未知 channel、空 ID、parse 失敗、未登録 channel は `false` とする既存動作を維持する。

runtime core は次の形になる。

```text
current turn config
  -> Config::tool_progress_enabled(channel, surface_thread)
  -> adapter.tool_progress_sink()
  -> ToolProgressCoordinator
```

adapter trait を変更しないため、sink の投稿・編集責務と runtime の coordinator は既存のまま利用できる。

### 4.4 Config hot reload との接続

設定 hot reload が有効になった後は、runtime が `state.config` 固定値を使わず、Turn 開始時の `ConfigSnapshot` を resolver へ渡す。

この設計自体では hot reload の実装を再設計しない。ただし、`tool_progress_enabled` の resolver が `&Config` を受け取ることで、将来の snapshot 化を阻害しない。

## 5. 変更前後の責務

| 責務 | 変更前 | 変更後 |
|---|---|---|
| platform ID の parse | runtime / channel の両方 | channel adapter 入口 |
| Channel Log identity | `ChannelLogKey` enum | `ChannelLogAddress` |
| Channel Log chat 解決 | platform 別 storage method | 共通 storage method |
| tool progress config lookup | runtime core | config resolver |
| progress 投稿・編集 | channel adapter | 変更なし |
| progress timing / aggregation | runtime coordinator | 変更なし |

## 6. 非対象

- `SurfaceContext.channel` を enum 化すること
- Discord / Telegram の受信 routing 全体の再設計
- `ChannelAdapter` に channel ごとの巨大な policy trait を追加すること
- Channel Log の DB schema や既存 external ID 形式を変更すること
- tool progress の表示仕様、遅延、編集方式を変更すること
- 新しい channel adapter の追加

## 7. 実装計画

### Step 0: Worktree 作成

- ブランチ名: `refactor/neutralize-runtime-channel-boundary`
- 実装を開始する場合は専用 worktree を作成する。

### Step 1: Channel Log address を中立化する

TDD 項目: `T1`

RED:

- `store_human_channel_log_message_accepts_string_address` を追加する。
- Discord 相当の address が既存の Channel Log chat を解決し、メッセージを保存することを検証する。

GREEN:

- `ChannelLogKey` を `ChannelLogAddress` に置き換える。
- Discord / Telegram の呼び出し側で数値を文字列へ変換する。
- runtime の match を削除する。

REFACTOR:

- `conversation_id` と `external_chat_id` の命名を整理する。
- `ChannelLogAddress` の公開範囲を `pub(crate)` に限定する。
- ID の format を channel adapter や runtime に重複させない。

### Step 2: storage の Channel Log resolver を共通化する

TDD 項目: `T2`

RED:

- `resolve_channel_log_chat_id_preserves_existing_external_identity` を追加する。
- Discord と Telegram の既存 external ID 形式が共通 method で再現されることを検証する。

GREEN:

- `resolve_channel_log_chat_id(channel, conversation_id)` を追加する。
- 既存の platform 専用 method の本体を共通 method へ寄せる。
- 呼び出し側をすべて共通 method へ移行する。

REFACTOR:

- platform 専用 method を削除する。
- `format!("{channel}:{conversation_id}:multi-room-log")` を storage の一箇所だけに置く。
- DB migration を追加していないことを確認する。

### Step 3: tool progress resolver を config へ移す

TDD 項目: `T3`

RED:

- `config_tool_progress_enabled_resolves_discord_and_telegram` を config module のテストとして追加する。
- 有効、無効、未登録、未知 channel、数値 parse 失敗を検証する。

GREEN:

- `Config::tool_progress_enabled` を `src/config/resolve.rs` に追加する。
- 現在の分岐と同じ値を返す。

REFACTOR:

- `DiscordChannelConfig` / `TelegramChatConfig` の lookup は config module 内に限定する。
- runtime tests から platform config fixture を取り除く。

### Step 4: runtime の利用箇所を中立化する

TDD 項目: `T4`

RED:

- `turn_progress_uses_config_resolver` を追加する。
- runtime が resolver の結果に従って sink を coordinator へ渡すことを検証する。

GREEN:

- `execute_turn_with_progress` から `tool_progress_enabled` の local function を削除する。
- `Config` の resolver を呼ぶ。
- `ChannelLogKey` の re-export と関連 import を削除する。

REFACTOR:

- runtime core に Discord / Telegram config 型が残っていないことを確認する。
- `rg` による文字列検索を補助チェックとして実行する。
- provider/config snapshot を受け取る境界を壊していないことを確認する。

### Step 5: feature build と既存動作を確認する

TDD 項目: `T5`

実施内容:

- `cargo test --no-default-features`
- `cargo test --all-features`
- Discord / Telegram の既存 channel tests
- Channel Log の agent context injection tests
- tool progress coordinator tests

### Step 6: 文書と自己レビュー

- `docs/architecture.md` の Channel Input Boundary の説明を更新する。
- `docs/channels.md` の Channel Log と tool progress の責務を更新する。
- `docs/session-lifecycle.md` の `channel_log_chat_id` の説明を確認する。

自己レビューでは、runtime に platform type が残っていないことだけでなく、両 platform の実際の呼び出しが共通 address / resolver を通っていることを確認する。「型を移動しただけ」で片方の経路が旧 method を使っていないかを重点的に見る。

## 8. テストリスト

| ID | 期待する振る舞い | 優先 | 対応 Step |
|---|---|---:|---:|
| T1 | platform-neutral address で Channel Log を保存できる | High | Step 1 |
| T2 | 既存 Discord / Telegram の external identity が変わらない | High | Step 2 |
| T3 | tool progress の判定結果が既存動作と一致する | High | Step 3 |
| T4 | runtime が platform config 型を参照せず resolver を利用する | High | Step 4 |
| T5 | default features 有無の両方で build/test が通る | High | Step 5 |
| T6 | Channel Log の context injection が従来どおり動く | High | Step 5 |
| T7 | tool progress の sink / coordinator の挙動が変わらない | High | Step 5 |

## 9. 変更ファイル一覧

| ファイル | 変更内容 |
|---|---|
| `src/runtime/channel_input.rs` | `ChannelLogKey` の中立 address 化 |
| `src/runtime/mod.rs` | platform match と local resolver の削除 |
| `src/storage/chat.rs` | Channel Log resolver の共通化 |
| `src/config/resolve.rs` | tool progress policy resolver の追加 |
| `src/config/tests.rs` | channel config lookup のテスト |
| `src/channels/discord.rs` | address 生成の更新 |
| `src/channels/telegram.rs` | address 生成の更新 |
| `docs/architecture.md` | runtime / channel 境界の更新 |
| `docs/channels.md` | Channel Log / tool progress の更新 |
| `docs/session-lifecycle.md` | Channel Log ID の説明確認 |

## 10. コミット分割

1. `refactor: normalize channel log address`
2. `refactor: move tool progress config lookup to config resolver`
3. `docs: clarify runtime channel boundary`

## 11. 完了条件

- `ChannelLogKey::Discord` / `ChannelLogKey::Telegram` が存在しない。
- runtime core が Discord / Telegram の ID 型と config 型を直接参照しない。
- Channel Log の既存 external ID と保存内容が変わらない。
- `tool_progress_enabled` の platform-specific lookup が config module に閉じている。
- 新しい channel を追加するとき、今回の2箇所の runtime core を変更せずに済む。
- tool progress の表示挙動と Channel Log の context injection に回帰がない。

## 12. 見積もりとリスク

実装量は中程度だが、設計上の無理は少ない。最大のリスクは `channel_log_chat_id` を含む durable payload、テスト fixture、agent context injection の参照漏れである。DB schema と external ID 形式を変えないため、移行リスクは抑えられる。

adapter に設定 policy まで移す案は、hot reload と adapter の再構築を同時に要求するため、現段階では採用しない。将来 channel ごとに複雑な capability が増えた場合だけ再評価する。
