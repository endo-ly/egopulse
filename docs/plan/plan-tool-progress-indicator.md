# Plan: Discord / Telegram 向けツール進捗インジケータ（A3 遅延型 × B2 編集式・累積ログ）

遅延型（5s 未満は表示せず）× 編集式累積ログで、Discord / Telegram の長時間ターン中のツール実行状況を可視化する。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **既存イベント土台を活用**: `agent_loop/turn.rs` の `ToolExecutionHooks` が既に `AgentEvent::ToolStart` / `ToolResult` を emit 済み。Discord/Telegram 経路をイベント購読に切替えるだけで、生成ロジックは新規不要
- **Channel Adapter 拡張**: `ChannelAdapter` trait に進捗表示 API（`ToolProgressSink` / `Handle`）を追加。デフォルト `None` で既存チャネル（Voice/CLI/TUI/Web）非影響
- **責務分離**: coordinator（runtime 層）が状態機械・遅延タイマー・間引きを担い、sink（チャネル層）は「投稿/編集/残置」のみ。残置ポリシーは固定で設定化しない（KISS/YAGNI）
- **既存の `Arc<dyn ChannelAdapter>` / 429 リトライ（3 回 / Retry-After）を再利用**
- **関連 docs**: [architecture.md](../../docs/architecture.md) §7 設計パターン、[channels.md](../../docs/channels.md) §3 Discord / §4 Telegram、[config.md](../../docs/config.md) §3.4 / §3.5

## TDD 方針

テストリスト項目（T1, T2…）と自動テスト（`test_name`）を区別する。1 回の Red では自動テスト 1 件だけを追加し、Green はその最小実装、Refactor は全テスト green 状態で整理する。Green/Refactor 中に別ケースや新規不安があれば実装に混ぜずテストリストへ戻し、次の Cycle で扱う。1 項目に必要な自動テスト総数は 1 件とは限らず、境界値・状態遷移ごとに Cycle を分ける。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成 → レビュー待機・レビューバック

## 対象一覧

| 対象 | 種别 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/agent_loop/event.rs`（新規） | 新規 | `web/sse.rs` の `AgentEvent` 定義 | レイヤー逆依存の解消。SSE シリアライズ形状は維持 |
| `src/agent_loop/turn.rs` | 変更 | `EventEmitter` / `process_turn_with_events` | import 切替のみ。イベント生成ロジックは既存 |
| `src/channels/adapter.rs` | 変更 | `ChannelAdapter` trait | `ToolProgressSink` / `Handle` trait とデフォルト `None` 追加 |
| `src/channels/discord.rs` | 変更 | `DiscordAdapter` / `send_text` / 429 リトライ | `DiscordToolProgressSink`/`Handle`・`PATCH /messages/{mid}` |
| `src/channels/telegram.rs` | 変更 | `TelegramAdapter` / `send_text` | `TelegramToolProgressSink`/`Handle`・`editMessageText` |
| `src/config/types.rs` | 変更 | `DiscordChannelConfig` / `TelegramChatConfig` | runtime 用 `tool_progress: bool` 追加（enable 1件のみ） |
| `src/config/loader.rs` | 変更 | `FileDiscordChannelConfig`（`deny_unknown_fields`） | serde 用 `tool_progress: Option<bool>` + normalize |
| `src/config/persist.rs` | 変更 | `SerializableDiscordChannel` / `SerializableTelegramChannel` | 設定保存時に `tool_progress` を保持 |
| `src/runtime/tool_progress.rs`（新規） | 新規 | `AgentEvent` / `tokio::select!` | `ToolProgressCoordinator`。状態機械・遅延タイマー・間引き |
| `src/runtime/mod.rs` | 変更 | `execute_turn_with_retry` / `execute_scheduled_turn` | `process_turn_with_events` 切替・coordinator 配線 |
| `docs/channels.md` | 変更 | §3 / §4 | ツール進捗表示セクション追加 |
| `docs/config.md` | 変更 | §3.4 / §3.5 | `tool_progress` フィールド表追加 |
| `docs/architecture.md` | 変更 | §7 設計パターン表 | Tool Progress Coordinator 追加 |
| `docs/directory.md` | 変更 | モジュール一覧 | `agent_loop/event.rs`, `runtime/tool_progress.rs` 追加 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 回帰 | `AgentEvent` 引越し後も既存 Web stream テストが green のまま | High | Step 1 | 未着手 |
| T2 | 正常系 | デフォルトで `ChannelAdapter::tool_progress_sink()` が `None` を返す | High | Step 2 | 未着手 |
| T3 | 境界値 | `tool_progress` 省略時は `false` になり、保存 round-trip でも保持される | High | Step 3 | 未着手 |
| T4 | 正常系 | Discord sink の begin → update → close シーケンスが機能する | High | Step 4 | 未着手 |
| T5 | 境界値 | Discord 本文 2000 字超過時は打ち切って編集される | High | Step 4 | 未着手 |
| T6 | 異常系 | Discord edit 失敗時は warn のみでターン継続し、進捗のために追加投稿しない | Medium | Step 4 | 未着手 |
| T7 | 正常系 | Telegram sink の begin → update → close シーケンスが機能する | High | Step 5 | 未着手 |
| T8 | 境界値 | coordinator 遅延閾値未満のターンは進捗投稿しない | High | Step 6 | 未着手 |
| T9 | 境界値 | 単一 long-running tool が delay を跨いだ時点で begin される | High | Step 6 | 未着手 |
| T10 | 正常系 | ToolStart/ToolResult で累積ログが正しく構築される | High | Step 6 | 未着手 |
| T11 | 異常系 | イベントストリーム EOF（`evt_tx` drop）で確実に close される | High | Step 6 | 未着手 |
| T12 | 正常系 | sink=None（非対応/設定OFF）で coordinator は no-op | High | Step 6 | 未着手 |
| T13 | 境界値 | 間引き: 高頻度イベントでも min_edit_interval 内は 1 回だけ update | Medium | Step 6 | 未着手 |
| T14 | セキュリティ | Discord/Telegram 進捗本文に tool input / result preview を含めない | High | Step 6 | 未着手 |
| T15 | 正常系 | 経路切替後、設定OFF時は進捗投稿されず最終応答のみ送信 | High | Step 7 | 未着手 |
| T16 | 異常系 | ターン失敗時（最終 Err）に進捗が close される | High | Step 7 | 未着手 |
| T17 | 正常系 | `drop(evt_tx)` で coordinator が正常終了する（ハングしない） | High | Step 7 | 未着手 |
| T18 | 正常系 | リトライ発生時も進捗メッセージが1つに保たれる | Medium | Step 7 | 未着手 |
| T19 | 異常系 | close / final update が失敗・遅延しても最終応答送信を長時間ブロックしない | Medium | Step 7 | 未着手 |

---

## 1. 背景・目的

Discord / Telegram では、現在エージェントターン中の最終応答しかチャネルに投稿されない。
長時間ターン（`web_fetch` 連打、重い `bash`、多数のツール呼び出し等）の際に「何をしているか」が一切見えず、ユーザーが不安になる課題がある。

本番 `tool_calls` テーブルでも `bash`(2475) / `web_fetch`(961) / `read`(735) など、1 ターンあたり複数ツール・数秒〜数十秒かかるターンが日常的に発生している。

**目的**: 閾値以上に時間のかかるターンについてのみ、1 メッセージを編集で更新する累積ログ形式でツール実行状況を可視化する。速いターンは現状通り最終応答のみ（ノイズゼロ）。

## 2. 方針（確定）

| 軸 | 選択 | 意味 |
|---|---|---|
| タイミング | **A3 遅延型** | ターン開始から N 秒（デフォルト 5s）経過、もしくはツール実行が継続している場合のみ進捗表示を開始 |
| 表現 | **B2 編集式・累積ログ** | 1 メッセージを投稿し、ツールイベントごとに編集で本文を更新。完了後も累積ログを残す |

### しないこと（スコープ外）

- Voice / CLI / TUI / Web の変更（Web は既存 SSE/WS イベントで別経路対応済み）
- 進捗メッセージへのリッチ装飾（Embed / Markdown 太字等の過剰な整形）。最小限のプレーンテキスト形式
- リアルタイム性の極限追求（編集は間引いて API 負荷を抑える）
- 事後サマリー（A1）の併用。累積ログが完成形で事後ログも兼ねる
- `tool_progress` 以外の設定追加（delay / interval / keep / delete 等は設定化しない）
- ツール引数・ツール結果 preview の Discord / Telegram 表示（公開チャネルに出さない）

## 3. 現状の課題（コードベース調査結果）

### 3.1 イベント生成土台は完成済み

- `agent_loop/turn.rs:786-800`: `execute_tool_calls` が `ToolExecutionHooks` 経由で `AgentEvent::ToolStart { name, input }` / `ToolResult { name, is_error, preview, duration_ms }` を emit
- `EventEmitter`（`turn.rs:43`）: 購読の口。`process_turn`（イベントなし）と `process_turn_with_events`（イベントあり）の 2 エントリポイント
- **Web は `process_turn_with_events` で購読**（`web/stream.rs:377`）
- `tool_calls` テーブルに input/output 付きで全件永続化済み

### 3.2 ギャップ

1. **Discord/Telegram 経路がイベントなし版を使っている**: `execute_scheduled_turn`（`runtime/mod.rs:676` 付近）→ `execute_turn_with_retry` → `process_turn`（イベント購読なし）
2. **`ChannelAdapter` trait に進捗 API がない**: `begin_turn_activity` / `send_text` / `send_attachment` のみ
3. **レイヤー逆依存**: `AgentEvent` が `channels/web/sse.rs` に定義されているのに `agent_loop/turn.rs:22` が依存。エージェントループの概念が web チャネル下に居座っている

## 4. 設計

### 4.1 `AgentEvent` の引越し（レイヤー修正）

`channels/web/sse.rs` → `agent_loop/` へ移動。

- 新ファイル: `src/agent_loop/event.rs` に `AgentEvent` enum を定義
- `agent_loop/mod.rs` から再公開
- `web/sse.rs` は `agent_loop` から import する alias に縮小（SSE シリアライズ形状は維持し、Web の既存ペイロード形状・イベント名は一切変更しない）
- `turn.rs:22` の `use crate::channels::web::sse::AgentEvent` を `use crate::agent_loop::event::AgentEvent` へ

**非互換なし**: `AgentEvent` は `pub(crate)` で外部公開なし。Web の SSE イベント名（`tool_start` / `tool_result` 等）は serde attribute で維持。

### 4.2 `ChannelAdapter` trait 拡張

`channels/adapter.rs` に進捗 API を追加。デフォルト実装で「非対応」を返す。

**設計の制約**: coordinator は `tokio::spawn` で別タスク駆動するため、sink は `'static` でなければならない。よって sink は `Arc<dyn ToolProgressSink>` として渡す（`&dyn` 借用は不可）。adapter は内部に `Arc<dyn ToolProgressSink>` を保持し、それをクローンして返す。

```rust
/// ツール進捗の表示器。ターン単位で生成・更新・完了する。
/// `Arc` で共有され、`tokio::spawn` された coordinator タスクへ渡されるため `Send + Sync + 'static`。
#[async_trait]
pub(crate) trait ToolProgressSink: Send + Sync {
    /// 進捗メッセージを初回投稿。返り値ハンドルを以降の update/close で使用。
    async fn begin(&self, external_chat_id: &str, body: &str)
        -> Result<Box<dyn ToolProgressHandle>, String>;
}

/// begin で返るハンドル。編集で更新し、close で終了。
#[async_trait]
pub(crate) trait ToolProgressHandle: Send {
    /// 本文を置換（編集）。文字数超過時は打ち切り編集。
    async fn update(&mut self, body: &str) -> Result<(), String>;
    /// 進捗表示を閉じる（常に残置）。
    async fn close(self: Box<Self>) -> Result<(), String>;
}
```

`ChannelAdapter` に進捗ファクトリを追加（デフォルト `None` = 非対応）:

```rust
#[async_trait]
pub(crate) trait ChannelAdapter: Send + Sync {
    // ... 既存 ...

    /// ツール進捗表示器を返す（`Arc` クローン）。非対応チャネルは None。
    fn tool_progress_sink(&self) -> Option<Arc<dyn ToolProgressSink>> { None }
}
```

Voice / CLI / TUI / Web adapter はデフォルトのまま（オーバーライドしない → None）。Discord/Telegram adapter はコンストラクタで `Arc<dyn ToolProgressSink>` を受け取り保持、`tool_progress_sink()` でクローンして返す。

### 4.3 Discord / Telegram の編集式進捗実装

#### Discord（`channels/discord.rs`）

- `DiscordToolProgressSink`: `begin` でメッセージ投稿、メッセージ ID を保持
- `DiscordToolProgressHandle`: `update` で `PATCH /channels/{cid}/messages/{mid}`。2000 字超過時は本文を打ち切って編集
- `close`: 残置固定（no-op で完了ログを残す）
- トークン解決は既存 `select_token` を再利用
- 編集失敗時は warn ログのみ。進捗メッセージのために追加投稿せず、最終応答送信を妨げない

#### Telegram（`channels/telegram.rs`）

- `TelegramToolProgressSink` / `TelegramToolProgressHandle`
- `update` で `editMessageText`。4096 字超過時は打ち切り編集
- `close`: 残置固定（no-op）
- Telegram edit は 48 時間以内のみ。失敗時は warn ログのみで、最終応答送信を妨げない

### 4.4 `ToolProgressCoordinator`（runtime 層・新設）

イベントストリームと閾値タイマーを監視し、進捗表示を駆動する。`runtime/tool_progress.rs` に新設。

```rust
pub(crate) struct ToolProgressCoordinator {
    sink: Option<Arc<dyn ToolProgressSink>>,  // None = チャネル非対応 or 設定 OFF（`'static`）
    external_chat_id: String,
    state: ProgressState,                      // 未開始 / 表示中(message_id, ログ)
}
```

#### 駆動ライフサイクル（A3 遅延型）

**重要**: 単一の long-running tool が delay 閾値を跨くケース（本機能の主用途）に対応するため、coordinator のメインループは `tokio::select!` で「イベント受信」と「delay タイマー発火」の両方を待ち受ける。タイマーは独立分岐で、`pending` 中のツールがある状態で delay に達したら `begin` を発火させる。

```
turn 開始
  ├─ coordinator 生成（sink 解決済み、状態=未開始、開始時刻記録）
  ├─ on_event コールバックで coordinator の mpsc チャネルへイベント転送
  │
  ├─ メインループ (tokio::select! でイベント受信 vs delay タイマー):
  │
  ├─ ToolStart 受信:
  │    ├─ 状態=未開始 & 経過 < delay → pending ログに追加（投稿せず）。delay タイマー稼働中
  │    ├─ 状態=未開始 & 経過 >= delay → begin 投稿、状態=表示中、pending を含む本文で初期化
  │    └─ 状態=表示中 → ログ追記、間引き間隔経過していれば update
  │
  ├─ ToolResult 受信:
  │    └─ ログの該当行を ✓ 完了マーク＋ duration に更新、間引き update
  │
  ├─ ★ delay タイマー発火（イベントなしで経過）:
  │    └─ 状態=未開始 & pending あり → begin 投稿、状態=表示中
  │       （単一 long-running tool が delay を跨いだケース。本機能の核心）
  │
  └─ close 契約（下記 4.4.1 に準拠）:
       └─ 状態=表示中なら close（常に残置）。最終応答は別途 send_text で新規投稿
```

**タイマー設計**: delay は `turn 開始時刻` からの経過で判定（初回 ToolStart からではない）。`tokio::time::sleep` を `select!` で待ち、発火後に狽態遷移。発火済みなら以降はタイマー再利用しない。

##### 4.4.1 close 契約（確実な終了保証）

coordinator は次のいずれかで **必ず** close する。これが崩れると進捗メッセージが残りっぱなしになる。

**重要**: `ToolProgressHandle::close` は async であり Rust の `Drop` からは await できない。したがって close 保証は **`run()` 内の通常終了経路で完結** させ、Drop には頼らない。Drop はせいぜい warn ログ出力（状態破棄）まで。

| close トリガ | 検出方法 | 動作 |
|---|---|---|
| FinalResponse イベント受信 | `AgentEvent::FinalResponse` | `run()` 内で close（残置）して return |
| イベントストリーム EOF | `evt_rx.recv() = None`（`evt_tx` の全クローン drop） | `run()` 内で close して return。**これが最強の安全網** |

両経路とも `run()` のループ内で検出され、async close を await できる。close は常に残置（完了ログとして残す）。coordinator タスクが panic などで異常終了した場合は進捗メッセージが残る可能性があるが、これは(best-effort で) adapter 側に TTL-based な自己掃除を持たせる余地はある（本 Plan のスコープ外・将来課題）。

**リトライ中の挙動**（`execute_turn_with_retry`）: 1 ターン = 1 coordinator ではなく、**リトライ含むターン全体で 1 coordinator** を使う。各 `process_turn_with_events` 呼出のイベントを同一 coordinator のチャネルへ転送し、最終成功 or 最終失敗のどちらかで EOF が来て close される。リトライ間で進捗メッセージを使い回す（状態維持）。

**最終応答をブロックしない制約**: 進捗表示は補助情報であり、最終応答送信より優先しない。`coordinator_handle.await` は短い timeout（例: `Duration::from_secs(2)`）で待ち、timeout / join error / close error は warn のみで握りつぶして `execute_turn_with_retry` の結果を返す。これにより Edit API の遅延や一時失敗でユーザーへの最終応答が長時間止まらない。

#### 本文フォーマット（累積ログ）

```
tools running...
✓ web_fetch (1.8s)
✓ bash (0.3s)
... read
```

- 完了ツール: `✓ <name> (<duration>s)`。エラー時は `✗ <name> (<duration>s) エラー`
- 実行中ツール: `... <name>`
- 表示する情報は tool name / 実行状態 / duration / error 有無のみ。`AgentEvent::ToolStart.input` と `AgentEvent::ToolResult.preview` は Discord / Telegram 進捗本文に絶対に含めない
- 文字数超過時は古い行から折りたたんで省略（最新 N 行を保持）

#### 間引き（スロットリング）

- 編集 API のレート制限回避。更新イベントが来ても即時編集せず、最後の編集から `MIN_EDIT_INTERVAL`（定数 800ms）経過後に最新状態を反映
- ただし完了時（close）は即時反映

#### 定数（ハードコード・設定化しない）

| パラメータ | 値 | 根拠 |
|---|---|---|
| 遅延開始閾値 `DELAY_SECS` | 5s | 「長いと不安」に効く閾値の実用値。ユーザー調整価値低 |
| 最小編集間隔 `MIN_EDIT_INTERVAL` | 800ms | Discord 編集レート制限回避の実装チューニング |
| 残置ポリシー | 常に残置 | 完了ログが事後記録としても有用。設定不要 |

### 4.5 Discord/Telegram 経路をイベント購読に切替

`runtime/mod.rs` の `execute_turn_with_retry` 呼出を `process_turn_with_events` に変更。

```rust
// execute_turn_with_retry 内（リトアループの外で coordinator を1つ生成）
let sink = adapter.and_then(|a| a.tool_progress_sink());  // Option<Arc<dyn ToolProgressSink>>
let (evt_tx, evt_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
let coordinator = ToolProgressCoordinator::new(sink, external_chat_id.clone());
let coordinator_handle = tokio::spawn(async move { coordinator.run(evt_rx).await });

// リトライループ内で process_turn_with_events を呼び、イベントを evt_tx へ転送
let mut result: Result<String, EgoPulseError>;
let evt_tx_loop = evt_tx.clone();
loop {
    let attempt = ...;
    let evt_tx_clone = evt_tx_loop.clone();
    let one = process_turn_with_events(state, context, input, move |event| {
        let _ = evt_tx_clone.send(event);
    }).await;
    match one {
        Ok(response) => { result = Ok(response); break; }
        Err(e) if retryable && attempt < MAX_TURN_RETRIES => { /* continue; coordinator 維持 */ }
        Err(e) => { result = Err(e); break; }
    }
}
// ★ここが重要: ループ退出後、evt_tx の全クローンを明示的に drop してから await する
//  （さもないと recv() が None を返さず coordinator がハングする）
drop(evt_tx);
drop(evt_tx_loop);
match tokio::time::timeout(Duration::from_secs(2), coordinator_handle).await {
    Ok(Ok(())) => {}
    Ok(Err(error)) => tracing::warn!(error = %error, "tool progress coordinator failed"),
    Err(_) => tracing::warn!("tool progress coordinator did not finish before timeout"),
}
result
```

**ポイント**:
- `execute_turn_with_retry` 内で `let adapter = state.channels.get(&context.channel);` と `let external_chat_id = context.session_key();` を作り、sink と設定フラグを解決する
- coordinator は1つの `evt_rx` を持ち、すべてのリトライ attempt のイベントを転送先とする
- **`drop(evt_tx)` を `coordinator_handle.await` の直前で明示的に呼ぶ**（Web 側 `stream.rs` と同じパターン）。これで `recv() = None` が発生し、`run()` 内で close → return する。これを忘れると coordinator が永遠に待機してハングする
- `coordinator_handle.await` は短い timeout 付きで待ち、失敗時は warn のみ。進捗 close の失敗を turn 結果に混ぜない
- 進捗非対応チャネル / 設定 OFF の場合は sink=None で coordinator は実質 no-op（イベント受信しても何もしない）
- sink 生成可否は `adapter.tool_progress_sink()` のみで判定。チャネル個別の ON/OFF は §4.6 の `tool_progress: bool` で制御し、OFF なら sink を `None` 扱いにする（§4.7）

### 4.6 設定追加

チャネル個別の **bool 1フラグのみ** 追加（既存 `require_mention` / `secret` と同じ粒度）。delay / interval / keep / delete は設定化せず、ハードコード定数と固定ポリシーにする（§4.4 参照）。

```yaml
channels:
  discord:
    channels:
      "123456789":
        require_mention: true
        agents: [lyre]
        tool_progress: true     # 新設。デフォルト false
```

- `tool_progress: false`（デフォルト）なら本機能無効 → 既存挙動完全互換
- `tool_progress: true` で当該チャネルの遅いターンに進捗表示
- Telegram も `telegram_channels.<id>.tool_progress: bool` 同様

#### 4.6.1 loader の File 構造体への追加（重要）

このコードベースでは runtime config 型を直接 `Deserialize` しておらず、`config/loader.rs` の `FileDiscordChannelConfig` / `FileTelegramChatConfig`（共に `#[serde(deny_unknown_fields)]`）が YAML を受ける。**runtime 側の `DiscordChannelConfig` / `TelegramChatConfig` に足すだけでは `tool_progress` が unknown field でパースエラーになる。** よって loader / runtime / persist の 3 経路に追加が必要:

1. **`config/loader.rs`**: `FileDiscordChannelConfig` / `FileTelegramChatConfig` に `#[serde(default)] tool_progress: Option<bool>` を追加。
2. **`config/types.rs`**: `DiscordChannelConfig` / `TelegramChatConfig` に `tool_progress: bool` フィールドを追加。
3. **`config/loader.rs` の normalize**: `normalize_discord_channel` / `normalize_telegram_chat` で `Option<bool>` → `bool`（`None` は `false`）の変換を追加。
4. **`config/persist.rs`**: `SerializableDiscordChannel` / `SerializableTelegramChannel` と保存マッピングに `tool_progress` を追加。`false` は serialize しない。

バリデーション不要（bool のみで範囲制約なし）。

ホットリロード対象: 既存 `channels.<x>.channels` と同様（設定変更で即時反映、次回ターンから適用）。

### 4.7 設定の伝播経路

`SurfaceContext` には進捗設定を載せない（コンテキスト肥大化を避ける）。
`execute_turn_with_retry` 側で、`context.channel` / `context.surface_thread` から Discord channel_id / Telegram chat_id を抽出し、`AppState.config` から当該チャネルの `tool_progress` フラグをルックアップ。`false` なら sink を `None` 扱いにして coordinator を no-op 化。

```
execute_turn_with_retry
  ├─ adapter.tool_progress_sink() で sink 取得（チャネル種で決定）
  ├─ config から当該チャネル ID の tool_progress フラグを取得
  ├─ sink = sink.filter(|_| tool_progress_enabled)  // false なら None
  └─ coordinator へ注入
```

## Step 0: Worktree 作成

- ブランチ名: `feat/tool-progress-indicator`
- 作業ディレクトリ: `/root/workspace/egopulse/wt-tool-progress`
- 作成コマンド: `git worktree add ./wt-tool-progress -b feat/tool-progress-indicator origin/main`
- 状態: 作成済み

---

## 5. 実装ステップ（TDD Cycle）

各ステップで `cargo fmt && cargo check && cargo clippy --all-targets --all-features -- -D warnings && cargo test` を必須。Worktree は Step 0 で作成済み。

**TDD 進め方**: 各 Step はモジュール単位の実装 Step であり、その中で「テストリストID 1 件 → Red（自動テスト 1 件）→ Green（最小実装）→ Refactor」のサイクルを項目数分回す。1 Step 内で複数 Cycle を回す場合は、各 Cycle 完了ごとにコミットはせず Step 完了時に 1 コミット（意味単位）とする。ただし Step が大きい場合はテストリストID ごとにコミットを分けてもよい（§コミット分割参照）。

### Step 1: `AgentEvent` 引越し（リファクタリング・振る舞い不改変）
- [ ] `src/agent_loop/event.rs` 新設、`AgentEvent` を移動
- [ ] `agent_loop/mod.rs` で再公開
- [ ] `web/sse.rs` を alias 化（import 元変更のみ、形状維持）
- [ ] `turn.rs` の import 更新
- [ ] **UT**: 既存 web stream テストが green のまま通ること（回帰確認）
- [ ] **検証**: `cargo test -p egopulse` 全 green

### Step 2: `ChannelAdapter` trait 拡張
- [ ] `channels/adapter.rs` に `ToolProgressSink` / `ToolProgressHandle` trait 追加
- [ ] `ChannelAdapter::tool_progress_sink()` デフォルト実装（`None`）追加
- [ ] **UT**: デフォルトで `None` が返ること。モック adapter で `Some` を返せること

### Step 3: `tool_progress` フラグ追加
- [ ] `config/types.rs` の `DiscordChannelConfig` / `TelegramChatConfig` に `tool_progress: bool` フィールド追加
- [ ] `config/loader.rs` の `FileDiscordChannelConfig` / `FileTelegramChatConfig`（`deny_unknown_fields`）に `#[serde(default)] tool_progress: Option<bool>` 追加
- [ ] `config/loader.rs` の `normalize_discord_channel` / `normalize_telegram_chat` で `Option<bool>` → `bool`（`None` は `false`）変換追加
- [ ] `config/persist.rs` の `SerializableDiscordChannel` / `SerializableTelegramChannel` と保存マッピングに `tool_progress` 追加（`false` は省略）
- [ ] **UT**: 
  - デフォルト値（`tool_progress` 省略で `false`）
  - `tool_progress: true` 指定時のパース（Discord / Telegram それぞれ）
  - save/load round-trip で `tool_progress: true` が保持されること
  - YAML パースの AAA テスト

### Step 4: Discord 編集式進捗実装
- [ ] `DiscordToolProgressSink`（`Arc` で保持可能・http_client と token 解決情報のみ）実装
- [ ] `DiscordToolProgressHandle`（message_id 保持）実装
- [ ] `begin(external_chat_id, body)`: メッセージ投稿 → message_id 保持
- [ ] `update`: `PATCH /messages/{mid}`。2000 字超過時の打ち切り
- [ ] `close`: 残置固定（no-op で完了ログを残す）
- [ ] edit 失敗時は warn のみで追加投稿しない
- [ ] `DiscordAdapter` コンストラクタで `Arc<dyn ToolProgressSink>` を受け取り保持
- [ ] `DiscordAdapter::tool_progress_sink()` オーバーライド（保持している `Arc` をクローンして返す）
- [ ] **UT**: 
  - begin → update → close のシーケンス（モック HTTP）
  - 2000 字超過時の打ち切り
  - edit 失敗時に追加投稿せず `Ok(())` 扱いでターンを継続
- [ ] **検証**: 既存 Discord adapter テスト全 green

### Step 5: Telegram 編集式進捗実装
- [ ] `TelegramToolProgressSink`（`Arc` 保持可能） / `TelegramToolProgressHandle` 実装。Step 4 と同構造
- [ ] `TelegramAdapter` コンストラクタで `Arc<dyn ToolProgressSink>` 受け取り保持
- [ ] `TelegramAdapter::tool_progress_sink()` オーバーライド（`Arc` クローン返却）
- [ ] **UT**: Step 4 と同様（4096 字制約・editMessageText モック）

### Step 6: `ToolProgressCoordinator` 実装
- [ ] `runtime/tool_progress.rs` 新設
- [ ] coordinator は `Option<Arc<dyn ToolProgressSink>>` を保持（`'static`）
- [ ] イベント受信（`mpsc::UnboundedReceiver<AgentEvent>`）・状態機械（未開始/表示中）
- [ ] 閾値タイマー（定数 `DELAY_SECS=5`）・間引き（定数 `MIN_EDIT_INTERVAL=800ms`）ロジック
- [ ] 累積ログ本文ビルダー
- [ ] Discord / Telegram 向け本文には tool input / result preview を含めず、tool name / 状態 / duration / error 有無だけを使う
- [ ] **close 契約の実装**: `run()` 内ループで FinalResponse 受信時 close、`recv() = None`（EOF）時 close の 2 層保証。**Drop には頼らない**（async close は Drop から await できないため）。close は常に残置
- [ ] `sink.begin(external_chat_id, body)` で投稿
- [ ] **UT**（AAA・重点）:
  - 遅延閾値未満のターンは進捗投稿しない（sink.begin 呼ばれない）
  - 閾値到達で begin → 以降 update
  - **単一 long-running tool が delay を跨いだ時点で begin されること**（イベント来なくても delay タイマーで begin）
  - ToolStart/ToolResult でログが正しく構築される
  - **tool input / result preview が本文に含まれないこと**
  - **EOF で確実に close されること**（`evt_tx` drop シナリオ）
  - sink=None（非対応チャネル / 設定 OFF）で no-op
  - 間引き: 高頻度イベントでも min_edit_interval 内は 1 回だけ update

### Step 7: Discord/Telegram 経路をイベント購読に切替
- [ ] `runtime/mod.rs` `execute_turn_with_retry` を `process_turn_with_events` へ変更
- [ ] `execute_turn_with_retry` 内で `state.channels.get(&context.channel)` / `context.session_key()` から adapter と external_chat_id を解決
- [ ] **リトライループの外で coordinator を1つ生成**（リトライ間で進捗メッセージを使い回す）
- [ ] coordinator 起動・`evt_tx`/`evt_rx` 配線。**`drop(evt_tx)` を `coordinator_handle.await` 直前に明示**（ハング防止。Web `stream.rs` と同じパターン）
- [ ] `coordinator_handle.await` は短い timeout 付きにし、timeout / join error / close error は warn のみにして turn 結果へ伝播しない
- [ ] `AppState.config` からチャネル ID で `tool_progress` フラグをルックアップ。`false` なら sink を `None` 扱い（coordinator no-op 化）
- [ ] **UT**:
  - coordinator がイベントを受信すること
  - 設定 OFF 時は進捗投稿されず最終応答のみ送信
  - **リトライ発生時も進捗メッセージが1つに保たれること**
  - **ターン失敗時（最終 Err）に進捗が close されること**
  - **`drop(evt_tx)` により coordinator がハングせず正常終了すること**
  - close / final update の遅延・失敗で最終応答が長時間ブロックされないこと
  - 既存 `execute_scheduled_turn` のテストが green

### Step 8: ドキュメント更新
- [ ] `docs/channels.md`: §3 Discord / §4 Telegram に「ツール進捗表示」セクション追加（A3×B2 累積ログの挙動・ライフサイクル・UI イメージ）
- [ ] `docs/config.md`: §3.4 / §3.5 に `tool_progress` フィールド表追加
- [ ] `docs/architecture.md`: §7 設計パターン表に「Tool Progress Coordinator」追加、§8 オブザーバビリティに言及があれば整合
- [ ] `docs/directory.md`: `agent_loop/event.rs`, `runtime/tool_progress.rs` 追加

### Step 9: 動作確認

- 全テスト通過コマンド: `cargo test --all-features`
- Lint / フォーマット / 型チェック:
  - `cargo fmt --check`
  - `cargo check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- 手動確認（Discord 実機）:
  - 設定 ON（`tool_progress: true`）で遅いターン（`web_fetch` 数回）を仕込み、進捗メッセージが編集で累積更新されること
  - 速いターン（<5s）は進捗出ず最終応答のみ
  - 設定 OFF で従来通り（最終応答のみ）
- 失敗時に戻る Step: 該当 TDD Cycle（Step 1-7）

---

## Step 10: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリストと各 Cycle が完了条件を満たしている
- 関連仕様書（channels.md / config.md）の What と実装結果が一致している
- 実装中に変更した設計判断が関連 docs へ反映されている
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している

---

## Step 11: PR 作成

- PR タイトル: `feat: tool progress indicator for Discord/Telegram`
- PR description:
  - 概要: Discord / Telegram の長時間ターンでツール実行状況を編集式累積ログで可視化。5s 未満のターンは表示せずノイズゼロ。設定 `tool_progress: bool` で per-channel 制御、デフォルト `false` で既存完全互換
  - テスト: ユニットテスト（coordinator 状態機械・close 契約・間引き、Discord/Telegram sink シーケンス、設定パース）＋ Discord 実機確認
  - Close #<issue-number>（該当する場合）

---

## Step 12: 初回レビューバック

PR 作成後、レビュー生成を待ってから `pr-review-back-workflow` Skill を実行し、未対応のレビューコメントがあれば修正・検証・コミット・push まで完了する。

- 初回待機: `sleep 15m`
- レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだレビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- レビューコメントが無い、または最大待機後もレビューが無い場合は、その結果を PR に記録して完了扱いにする
- レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## Step 13: レビュー対応後の再レビューバック

レビュー対応を push した後、追加レビュー生成を待ってから `pr-review-back-workflow` Skill を再実行し、残った指摘や新規指摘があれば同じ品質基準で対応する。

- 対象: Step 12 でレビュー対応の変更を push した場合
- 初回待機: `sleep 15m`
- 再レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだ追加レビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- 追加レビューコメントが無い、または最大待機後も追加レビューが無い場合は、その結果を PR に記録して完了扱いにする
- 再レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

## リスクと緩和

| リスク | 影響 | 緩和 |
|---|---|---|
| Discord 編集 API のレート制限 (429) | 進捗更新失敗 | 間引き 800ms + 既存 429 リトライ（3 回 / Retry-After）を再利用 |
| 進捗メッセージが最終応答と重複して見える | UI 冗長 | 進捗はツールログのみ。最終応答は別メッセージ |
| Telegram edit 48h 制限 | close 失敗 | ターン中（数秒〜数十秒）なので実質無害。失敗時は warn ログのみ |
| 本文フォーマットの文字数超過 | 編集失敗 | 古い行から折りたたみ（最新 N 行保持）。上限内に収まらなければ warn のみで編集スキップ |
| イベント経路切替の回帰 | 既存ターン挙動破壊 | Step 7 で既存テスト全 green を必須条件化 |
| 設定デフォルト `tool_progress: false` | 本機能が効かない | デフォルトで既存完全互換。ユーザー能動的有効化 |
| ツール引数・結果 preview の露出 | 公開チャネルへの情報漏洩 | Discord/Telegram 進捗本文では input / preview を使用禁止。本文ビルダーの単体テストで検証 |
| Edit API 遅延 | 最終応答送信の遅延 | coordinator await に短い timeout を設定し、進捗失敗は warn のみにする |

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/agent_loop/event.rs` | **新規** | `AgentEvent` enum（`web/sse.rs` から引越し） |
| `src/agent_loop/mod.rs` | 変更 | `event` モジュール公開 |
| `src/agent_loop/turn.rs` | 変更 | import 切替（`web/sse::AgentEvent` → `agent_loop::event::AgentEvent`） |
| `src/channels/adapter.rs` | 変更 | `ToolProgressSink` / `Handle` trait・`tool_progress_sink()` デフォルト |
| `src/channels/discord.rs` | 変更 | `DiscordToolProgressSink`/`Handle`・adapter 登録 |
| `src/channels/telegram.rs` | 変更 | `TelegramToolProgressSink`/`Handle`・adapter 登録 |
| `src/channels/web/sse.rs` | 変更 | `AgentEvent` を `agent_loop` から re-export に縮小 |
| `src/config/types.rs` | 変更 | `DiscordChannelConfig`/`TelegramChatConfig` に `tool_progress: bool` |
| `src/config/loader.rs` | 変更 | `File*ChannelConfig` に `tool_progress: Option<bool>` + normalize |
| `src/config/persist.rs` | 変更 | `tool_progress` の保存 round-trip 対応 |
| `src/runtime/tool_progress.rs` | **新規** | `ToolProgressCoordinator` |
| `src/runtime/mod.rs` | 変更 | `execute_turn_with_retry` のイベント購読化・coordinator 配線 |
| `docs/channels.md` | 変更 | §3 / §4 にツール進捗表示セクション |
| `docs/config.md` | 変更 | §3.4 / §3.5 に `tool_progress` フィールド表 |
| `docs/architecture.md` | 変更 | §7 設計パターン表に Tool Progress Coordinator |
| `docs/directory.md` | 変更 | モジュール一覧に新規 2 ファイル追加 |

---

## コミット分割

1. `refactor: move AgentEvent to agent_loop module` - `agent_loop/event.rs`（新規）/ `turn.rs` / `web/sse.rs` / `mod.rs` の import 切替
2. `feat: add ToolProgressSink trait to ChannelAdapter` - `channels/adapter.rs`（trait とデフォルト `None`）
3. `feat: add tool_progress config flag for Discord/Telegram` - `config/types.rs` / `config/loader.rs` / `config/persist.rs`
4. `feat: implement Discord tool progress sink` - `channels/discord.rs`
5. `feat: implement Telegram tool progress sink` - `channels/telegram.rs`
6. `feat: add ToolProgressCoordinator` - `runtime/tool_progress.rs`（新規）
7. `feat: wire tool progress into Discord/Telegram turn loop` - `runtime/mod.rs`
8. `docs: document tool progress indicator` - `docs/channels.md` / `docs/config.md` / `docs/architecture.md` / `docs/directory.md`

---

## 自動テスト一覧（全 19 件）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストとTDD Cycleを追加して対応する。

### agent_loop / config（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `agent_event_relocation_regression`（既存 web stream テスト再利用） | Step 1 | `cargo test --features discord channels::web` |
| T2 | `default_tool_progress_sink_is_none` | Step 2 | `cargo test channels::adapter` |
| T3 | `tool_progress_defaults_to_false_when_omitted` | Step 3 | `cargo test config` |
| T3 | `tool_progress_save_load_round_trip_preserves_true`（同一テストリストIDの追加境界） | Step 3 | `cargo test config::persist` |

### Discord / Telegram sink（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T4 | `discord_sink_begin_update_close_sequence` | Step 4 | `cargo test --features discord channels::discord` |
| T5 | `discord_sink_truncates_over_2000_chars` | Step 4 | `cargo test --features discord channels::discord` |
| T6 | `discord_sink_edit_failure_does_not_post_fallback` | Step 4 | `cargo test --features discord channels::discord` |
| T7 | `telegram_sink_begin_update_close_sequence` | Step 5 | `cargo test --features telegram channels::telegram` |

### ToolProgressCoordinator（全 7 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T8 | `coordinator_no_post_below_delay_threshold` | Step 6 | `cargo test runtime::tool_progress` |
| T9 | `coordinator_begins_on_delay_timer_for_long_tool` | Step 6 | `cargo test runtime::tool_progress` |
| T10 | `coordinator_builds_cumulative_log` | Step 6 | `cargo test runtime::tool_progress` |
| T14 | `coordinator_body_excludes_tool_input_and_preview` | Step 6 | `cargo test runtime::tool_progress` |
| T11 | `coordinator_closes_on_event_stream_eof` | Step 6 | `cargo test runtime::tool_progress` |
| T12 | `coordinator_noop_when_sink_none` | Step 6 | `cargo test runtime::tool_progress` |
| T13 | `coordinator_throttles_updates_within_interval` | Step 6 | `cargo test runtime::tool_progress` |

### runtime 経路統合（全 5 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T15 | `turn_loop_no_progress_when_config_off` | Step 7 | `cargo test runtime` |
| T16 | `turn_loop_closes_progress_on_turn_failure` | Step 7 | `cargo test runtime` |
| T17 | `turn_loop_drop_evt_tx_terminates_coordinator` | Step 7 | `cargo test runtime` |
| T18 | `turn_loop_single_progress_across_retries` | Step 7 | `cargo test runtime` |
| T19 | `turn_loop_progress_close_timeout_does_not_block_response` | Step 7 | `cargo test runtime` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | 済 |
| Step 1 | `AgentEvent` 引越し TDD Cycle | ~30 行 |
| Step 2 | `ChannelAdapter` trait 拡張 TDD Cycle | ~60 行 |
| Step 3 | `tool_progress` フラグ追加 TDD Cycle | ~110 行 |
| Step 4 | Discord sink TDD Cycle | ~200 行 |
| Step 5 | Telegram sink TDD Cycle | ~180 行 |
| Step 6 | `ToolProgressCoordinator` TDD Cycle | ~350 行 |
| Step 7 | 経路切替 TDD Cycle | ~150 行 |
| Step 8 | ドキュメント更新 | ~150 行 |
| Step 9-13 | 動作確認 / 自己チェック / PR 作成 / レビューバック ×2 | ~20 行 |
| **合計** | | **~1250 行** |

---

## 自己レビュー（Plan と実装の照合・メタレビュー）

> 実装完了後に本セクションを埋める。Plan からの逸脱・漏れ・スコープ超過を確認する。

### Plan 達成状況
-（実装後に記入）

### 逸脱・設計変更
-（実装後に記入）

### 振り返り
-（実装後に記入）
