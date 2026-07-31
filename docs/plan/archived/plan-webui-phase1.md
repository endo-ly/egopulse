# Plan: WebUI Phase 1 — Chat 体験の基盤

Phase 1 として Chat 体験の基盤（デザインシステム・レイアウト・Chat タブ・Command Palette）と、それらが依存するバックエンド拡張を実装する。後続フェーズ（Sleep/Pulse/Metrics/Config タブ）は対象外。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **既存資産は残さない**: `web/src/` は破棄して新設計へ一直線に向かう。段階的移行の複雑さを排除する
- **デザイン仕様**: [docs/webui/](../webui/) 配下の9ドキュメント + [docs/webui/mockup.html](../webui/mockup.html) を正本とする
- **バックエンド拡張**: 既存の `src/channels/web/`・`src/runtime/`・`src/agent_loop/` に追加実装。既存機能を壊さない
- **トランスポート**: WebSocket に一本化。SSE (`/api/stream`) は廃止
- **状態管理**: キャッシュ層・URL router は新規導入（[overview.md §4](../webui/overview.md)）
- **テスト**: フロントは Vitest + Testing Library（既存 `web/src/__tests__/` パターン）、バックエンドは `cargo test`

## TDD 方針

テストリスト項目（T1, T2...）と自動テスト（test_name）を区別する。1回の Red で追加する自動テストは1件のみ。Green では Red を通す最小実装に集中し、別ケース対応やリファクタリングは混ぜない。Refactor では全テストが通る状態で設計を整える。実装中に新たな不安を見つけたらテストリストへ追加し、必要な Cycle を続ける。テストリスト項目の完了は「予定テストを1件通したこと」ではなく、その項目が表す振る舞いと主要な失敗境界への不安が解消されたことで判断する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → 動作確認 → Plan/仕様書との自己チェック → E2E(Playwright) → PR作成 → レビューバック

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/channels/web/sessions.rs` | 変更 | `list_sessions`・`get_history` 既存実装 | `/api/agents` に active フィールド追加、`/api/history` に message_kind 追加 |
| `src/channels/web/stream.rs` | 変更 | WS/SSE 既存実装 | chat event ペイロードに sessionKey 追加、delta state 配信 |
| `src/channels/web/ws.rs` | 変更 | WS 既存実装 | chat event への sessionKey・delta 反映 |
| `src/channels/web/sse.rs` | 変更 | `AgentEvent` enum | `Delta { text }` バリアント追加 |
| `src/agent_loop/turn.rs` | 変更 | `process_turn_with_events` | LLM トークン刻みで delta event を emit |
| `src/runtime/turn_scheduler.rs` | 変更 | `ActiveTurnTracker` 既存 | `is_active` は既存、`/api/agents` から呼び出し |
| `web/src/` 全体 | **新規** | 既存 `web/src/` は破棄 | design-system・layout・chat・palette モジュール新設 |
| `web/package.json` | 変更 | 既存 | router・キャッシュライブラリ追加 |
| `web/index.html` | 変更 | 既存 | 必要に応じてメタ情報調整 |

## テストリスト / 不安リスト

### バックエンド（全6件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | `GET /api/agents` が **設定済み全 agent** を `AgentInfo { id, label, is_default, active }` の配列で返す。`label` と `is_default` は config から、`active` は `ActiveTurnTracker::is_active(agent_id)` から算出。従来の「Sleep run のある agent のみ」から意味変更 | High | Step 1 | 未着手 |
| T2 | 正常系 | `GET /api/history` が各メッセージに `message_kind` フィールドを返す。Pulse 由来は `"pulse_notification"` | High | Step 2 | 未着手 |
| T3 | 正常系 | `GET /api/sessions` が各セッションに `agent_id` フィールドを返す。SESSIONS Section の agent 絞り込みに必要。ストレージ層の `SessionSummary.agent_id` を API で公開 | High | Step 3 | 未着手 |
| T4 | 正常系 | WS `chat` event ペイロードに `sessionKey` が含まれる。read-only セッションのリアルタイム更新に必要 | High | Step 4 | 未着手 |
| T5 | 正常系 | agent_loop が LLM トークン刻みで delta を emit し、WS経由で配信される。従来の `done` 一括から差し替え | High | Step 5 | 未着手 |
| T6 | 正常系 | WS `chat.send` メソッドがチャット送信を受け付け、`run_id` を返す。従来の REST `POST /api/send_stream` + SSE から WS へ一本化 | High | Step 6 | 未着手 |

### デザインシステム（全3件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T7 | 正常系 | `web/src/app.css` に design-system.md §1-3 の全トークン（color/spacing/radius）が定義される | High | Step 7 | 未着手 |
| T8 | 正常系 | Button コンポーネントが primary/secondary/icon/danger の4バリアントを描画し、disabled/busy 状態を反映する | High | Step 8 | 未着手 |
| T9 | 正常系 | Badge・StatusDot・Modal・Toast・EmptyState・Spinner・Card が仕様通りに描画される | High | Step 9 | 未着手 |

### レイアウト（全5件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T10 | 正常系 | App shell が Sidebar(260px) / Top Bar(56px) / Main の3領域で構成される。Mobile(<640px) では Sidebar が hamburger overlay になる | High | Step 10 | 未着手 |
| T11 | 正常系 | Sidebar に Brand Header・New Session ボタン・Runtime Status Footer が表示される | Medium | Step 11 | 未着手 |
| T12 | 正常系 | Sidebar AGENTS Section が agent 一覧を表示し、選択中 agent を強調する。StatusDot が active フィールドを反映して点滅する | High | Step 12 | 未着手 |
| T13 | 正常系 | Sidebar SESSIONS Section が channel フィルタ付きでセッション一覧を表示する。フィルタは agent 選択と AND 条件（`agent_id` で絞り込み） | High | Step 13 | 未着手 |
| T14 | 正常系 | Top Bar が palette trigger・5タブ・health badge を表示する | High | Step 14 | 未着手 |

### Chat タブ（全9件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T15 | 正常系 | Chat Tab が Header / Timeline / Composer の3領域で構成される。Header は session label・channel badge・metadata を表示 | High | Step 15 | 未着手 |
| T16 | 正常系 | Timeline がメッセージを時系列表示し、最下部付近で自動スクロールする。最下部から離れている場合は "Jump to latest" ボタンを表示 | High | Step 16 | 未着手 |
| T17 | 正常系 | MessageBubble が sender_kind (user/assistant/system/tool) 別に配置・スタイルを切り替える | High | Step 17 | 未着手 |
| T18 | 正常系 | Markdown レンダリングが見出し・リスト・code block・リンクを描画し、code block に Copy ボタンが付く | High | Step 18 | 未着手 |
| T19 | 正常系 | Streaming 中の draft メッセージに点滅カーソルが表示され、done 受信で確定状態へ移行する | High | Step 19 | 未着手 |
| T20 | 正常系 | Tool Card が tool_start/tool_result の状態を描画し、closed では1行 summary・open では入出力を表示する | High | Step 20 | 未着手 |
| T21 | 正常系 | `message_kind === "pulse_notification"` のメッセージに Pulse アイコンバッジが付く | High | Step 21 | 未着手 |
| T22 | 正常系 | Composer が Enter で送信・Shift+Enter で改行・空文字は送信しない。サジェストは矢印・Tab・Enter で操作 | High | Step 22 | 未着手 |
| T23 | 正常系 | channel !== "web" のセッション選択時、Composer が read-only banner に置き換わる | High | Step 23 | 未着手 |

### Command Palette（全3件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T24 | 正常系 | Command Palette が `Cmd/Ctrl+K` で開閉し、`Esc`・backdrop click で閉じる | High | Step 24 | 未着手 |
| T25 | 正常系 | Palette が Recent / Quick Actions / Navigation / Agents / Sessions / Sleep-Pulse Runs のセクション構成を持つ | High | Step 25 | 未着手 |
| T26 | 正常系 | Recent セクションが localStorage から履歴（最大5件）を表示する。localStorage 利用不可時は Recent を隠す | Medium | Step 26 | 未着手 |

### トランスポート・状態（全3件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T27 | 正常系 | WS 接続が `chat` event を受信し、delta は draft へ追記・done は確定へ差し替え・tool_start/tool_result は Tool Card へ反映する。**送信も WS `chat.send` で行い、従来の REST `/api/send_stream` + SSE は廃止** | High | Step 27 | 未着手 |
| T28 | 正常系 | Server state がキャッシュ層を経由して取得され、agent 選択やフィルタ条件でキーが切り替わる | High | Step 28 | 未着手 |
| T29 | 正常系 | チャット送信完了後に session 一覧と当該 session の履歴が無効化され、再取得される | High | Step 29 | 未着手 |

### Chat UX 改善（全4件）

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 |
| -- | -- | -- | -- | -- | -- |
| T30 | 正常系 | Sidebar が [<] ボタンで collapsed (48px) / expanded (260px) を切り替え。collapsed ではアイコンのみ表示。状態は URL query で永続化 | Medium | Step 30 | 未着手 |
| T31 | 正常系 | Timeline 内キーワード検索（`Cmd+F`）。マッチ箇所ハイライト・次/前へジャンプ・マッチ件数表示 | Medium | Step 31 | 未着手 |
| T32 | 正常系 | 20行超の code block は折りたたみ、"Show all (N lines)" ボタンで展開 | Medium | Step 32 | 未着手 |
| T33 | 正常系 | Composer の入力中テキストを localStorage に保存（セッションキー毎）。リロードで復元、送信成功で削除 | Medium | Step 33 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/webui-phase1`
- 作成コマンド:
  - `git worktree add ../egopulse-webui-phase1 -b feat/webui-phase1`

---

## Step 1: Backend TDD Cycle - /api/agents を AgentInfo 構造体へ拡張

### この Step の目的

`GET /api/agents` を「Sleep run のある agent のみ」から「設定済み全 agent」へ意味変更し、各 agent を `AgentInfo { id, label, is_default, active }` で返すようにする。Sidebar AGENTS Section が依存。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: frontend の live indicator・agent 切替が依存する最初のバックエンド拡張
- この時点では扱わないこと: なし（`label`・`is_default`・`active` 全て扱う。Step 12 の UI テストが `is_default` に依存するため）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `api_agents_returns_all_configured_agents_with_active_flag`
- Given: config に3 agent（`lyre` default・`ace`・`vega`）定義済み。`ActiveTurnTracker` に `agent_id="lyre"` の turn が begin されている状態
- When: `GET /api/agents` を呼ぶ
- Then: レスポンスの `agents` 配列が3件で、各要素が `{id, label, is_default, active}` を持つ。`lyre` は `is_default=true, active=true`、`ace`・`vega` は `is_default=false, active=false`
- 失敗理由の想定: 現状の `list_agents`（`src/channels/web/sleep.rs`）は `Vec<String>`（Sleep run のある agent のみ）を返す

### GREEN: 最小実装

1. `src/channels/web/sleep.rs::list_agents` を変更して、`app_state.config.agents` から全 agent を取り出し `AgentInfo { id, label, is_default, active }` を構築
2. `label` は `config.agents.<id>.label`（未設定なら `id` をフォールバック）
3. `is_default` は `config.default_agent == id`
4. `active` は `app_state.active_turns.is_active(agent_id)` を呼び出し
5. **Sleep Tab の agent フィルタ**は従来 `/api/agents` に依存していたが、本 Plan では Sleep Tab を実装しないため影響なし。後続フェーズで `/api/sleep/agents` を新設して対応

### REFACTOR: 設計の整理

- 重複: `AgentInfo` 構造体は `sessions.rs` または新規 `agents.rs` に定義。`sleep.rs` に置くべきではない（Sleep モジュールの責務外）。「`/api/agents` ルートハンドラを `sleep.rs` から `sessions.rs` または新設 `agents.rs` へ移動」を検討
- 命名: `AgentInfo` は serializable struct。`active` は boolean
- 責務: `ActiveTurnTracker::is_active` は既存 API をそのまま使う
- 次の項目へ進める身軽さ: T2 は別エンドポイント

### テストリスト更新

- 完了: `T1`
- 追加: なし
- 次候補: `T2`

### コミット

`feat(web): return all configured agents with active flag from /api/agents`

---

## Step 2: Backend TDD Cycle - /api/history message_kind フィールド追加

### この Step の目的

`GET /api/history` が各メッセージに `message_kind` フィールドを返すようにする。Chat Tab の Pulse 通知識別（T19）が依存する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: Pulse 通知識別の前提。バックエンド単体で完結する
- この時点では扱わないこと: Pulse 由来メッセージの `message_kind` が `"pulse_notification"` になるという Pulse 側の保存ロジック（後続フェーズで対応）。ここでは既存の `messages.message_kind` カラムを API で公開するだけ

### RED: 失敗する自動テストを書く

- 追加するテスト名: `api_history_returns_message_kind`
- Given: `messages` テーブルに `message_kind="message"` と `message_kind="system_event"` の2レコードが保存済み
- When: `GET /api/history` を呼ぶ
- Then: 各メッセージに `message_kind` フィールドが含まれ、DB の値と一致する
- 失敗理由の想定: 現状の `get_history` レスポンスは `id/sender_id/sender_kind/content/timestamp` のみで `message_kind` なし

### GREEN: 最小実装

`sessions.rs::get_history` のレスポンス JSON に `message_kind` フィールドを追加。`StoredMessage.message_kind` は既存フィールド。

### REFACTOR: 設計の整理

- 重複: `MessageKind` enum の Display 実装は既存
- 責務: API レスポンスの形式拡張のみ
- 次の項目へ進める身軽さ: T3 は WS 側

### テストリスト更新

- 完了: `T2`
- 追加: なし
- 次候補: `T3`

### コミット

`feat(web): expose message_kind in /api/history response`

---

## Step 3: Backend TDD Cycle - /api/sessions agent_id フィールド追加

### この Step の目的

`GET /api/sessions` が各セッションに `agent_id` フィールドを返すようにする。Sidebar SESSIONS Section の agent 絞り込み（[layout.md §2.4](../webui/layout.md)）が依存。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: SESSIONS Section で agent フィルタを実現するために必須。ストレージ層（`SessionSummary.agent_id`）には既にデータがあるので API で公開するだけ
- この時点では扱わないこと: 他のフィールド追加

### RED: 失敗する自動テストを書く

- 追加するテスト名: `api_sessions_returns_agent_id`
- Given: 3セッション（`agent_id="lyre"`・`"ace"`・`"vega"`）が DB に保存済み
- When: `GET /api/sessions` を呼ぶ
- Then: 各セッションに `agent_id` フィールドが含まれ、DB の値と一致する
- 失敗理由の想定: 現状の `sessions.rs::SessionItem` は `session_key`・`label`・`chat_id`・`channel`・`last_message_time`・`last_message_preview` のみで `agent_id` なし

### GREEN: 最小実装

`src/channels/web/sessions.rs::SessionItem` に `agent_id: String` フィールドを追加（`SessionSummary.agent_id` は必須 `String` なので nullable にしない）。`list_sessions` ハンドラで `SessionSummary.agent_id`（既存）をマッピング。

### REFACTOR: 設計の整理

- 重複: `SessionSummary` は既に `agent_id` を持っているので単なるマッピング追加
- 責務: API レスポンス形式の拡張のみ
- 次の項目へ進める身軽さ: T4 は WS 側

### テストリスト更新

- 完了: `T3`
- 追加: なし
- 次候補: `T4`

### コミット

`feat(web): expose agent_id in /api/sessions response`

---

## Step 4: Backend TDD Cycle - WS chat event sessionKey 拡張

### この Step の目的

WS `chat` event ペイロードに `sessionKey` を含める。Chat Tab の read-only セッションリアルタイム更新（T21・T25）が依存。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: read-only リアルタイム更新の前提
- この時点では扱わないこと: delta event 自体の実装（T4）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `ws_chat_event_includes_session_key`
- Given: クライアントが WS 接続済み・`chat.send` で sessionKey="test-session" を送信
- When: サーバーが `chat` event を push する
- Then: event ペイロードに `sessionKey: "test-session"` が含まれる
- 失敗理由の想定: 現状の chat event ペイロードは runId・state・message のみで sessionKey なし

### GREEN: 最小実装

`ws.rs` の chat event publish 箇所で sessionKey をペイロードへ追加。`stream.rs::start_stream_run` で sessionKey を算出済みなので、それを run context から引く。

### REFACTOR: 設計の整理

- 重複: sessionKey は既に `StartedRun` が持っている
- 責務: WS publish 関数のシグネチャ変更のみ
- 次の項目へ進める身軽さ: T4 は別 event 種別の追加

### テストリスト更新

- 完了: `T4`
- 追加: なし
- 次候補: `T5`

### コミット

`feat(web): include sessionKey in WS chat event payload`

---

## Step 5: Backend TDD Cycle - agent_loop delta event 配信

### この Step の目的

agent_loop が LLM のトークン刻み出力を `AgentEvent::Delta { text }` として emit し、WS 経由で frontend へ配信されるようにする。従来の `done` 一括応答から差し替え。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: 実ストリーミング（overview.md §1.4）の中核。Chat Tab の Streaming indicator（T17）が依存
- この時点では扱わないこと: delta event の UI 側ハンドリング（T25）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `agent_loop_emits_delta_events_during_llm_stream`
- Given: FakeProvider が "Hello world" をトークン刻みでストリーミングする設定
- When: `process_turn_with_events` を実行
- Then: `on_event` callback が `AgentEvent::Delta { text: "Hello" }` と `AgentEvent::Delta { text: " world" }` をこの順で受信し、最後に `AgentEvent::FinalResponse` を受信する
- 失敗理由の想定: 現状の `AgentEvent` enum には Delta バリアントが存在しない

### GREEN: 最小実装

1. `sse.rs::AgentEvent` に `Delta { text: String }` バリアント追加
2. `agent_loop/turn.rs` の LLM 呼び出し箇所で、streaming response を token 毎に `on_event.emit(AgentEvent::Delta { text })` する
3. `stream.rs` の event forwarder で Delta を WS に publish する
4. `done` event は最終全文を含めて維持（二重送信になるが、frontend 側で差し替え前提）

### REFACTOR: 設計の整理

- 重複: Delta と FinalResponse の二重送信。最終的には FinalResponse の text を空にして Delta のみで構成する選択肢もあるが、まずは二重で安全側に倒す
- 命名: `Delta` は `AgentEvent` の他バリアントと整合
- 次の項目へ進める身軽さ: ここから frontend 実装へ

### テストリスト更新

- 完了: `T5`
- 追加: なし
- 次候補: `T6`

### コミット

`feat(agent_loop): emit Delta events for token streaming`

---

## Step 6: Backend TDD Cycle - WS chat.send メソッドによるチャット送信

### この Step の目的

WS `chat.send` メソッドがチャット送信を受け付け、`run_id` を返すようにする。従来の REST `POST /api/send_stream` + SSE `/api/stream` 経路から WS への一本化（[overview.md §3.1](../webui/overview.md)）。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: WS 一本化（仕様 の中核）の前提。Step 27 の frontend WS handler が送信も担うため
- この時点では扱わないこと: frontend 側の送信切替（Step 27 で実施）。ここではサーバー側の `chat.send` 受け口が仕様通り動くことを検証

### RED: 失敗する自動テストを書く

- 追加するテスト名: `ws_chat_send_accepts_message_and_returns_run_id`
- Given: クライアントが WS 接続済み・認証済み
- When: `chat.send` メソッドで `{sessionKey, message}` を送信
- Then: サーバーから `res` タイプ・`ok: true`・`payload: { runId, status: "accepted" }` のレスポンス。当該 runId の chat event が配信開始される
- 失敗理由の想定: 現状の `chat.send` ハンドラ（`src/channels/web/ws.rs:287` 付近）は存在するが、`stream.rs::start_stream_run` と統合されていない可能性

### GREEN: 最小実装

`ws.rs` の `chat.send` ハンドラを修正。`stream.rs::start_stream_run` を呼び出して run を開始し、クライアントへ `runId` を返す。以降の chat event は既存の publish 経路で配信。REST `/api/send_stream` は外部 voice client 等のために残すが、Phase 1 frontend は使わない。

### REFACTOR: 設計の整理

- 重複: `start_stream_run` の呼び出しを `/api/send_stream` と `chat.send` で共有
- 責務: WS ハンドラは認証・メッセージ検証・run 開始・runId 返却のみ
- 次の項目へ進める身軽さ: ここから frontend 実装へ

### テストリスト更新

- 完了: `T6`
- 追加: なし
- 次候補: `T7`

### コミット

`feat(web): accept chat.send via WebSocket and return runId`

---

## Step 7: Frontend TDD Cycle - デザイントークン定義

### この Step の目的

[design-system.md §1-3](../webui/design-system.md) の全トークン（color・spacing・radius）を `web/src/app.css` に定義する。全コンポーネントが依存する基盤。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: 全 frontend 実装の前提。CSS 変数として定義し、コンポーネントから参照可能にする
- この時点では扱わないこと: typography・motion・z-index・background pattern（Step 6 以降のコンポーネント実装で順次反映）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `design_tokens_are_defined_as_css_variables`
- Given: `web/src/app.css` を読み込み
- When: `getComputedStyle(document.documentElement)` を取得
- Then: `--color-bg`・`--color-panel`・`--color-accent`・`--radius-lg` 等の主要トークンが定義されている（empty でない）
- 失敗理由の想定: `app.css` が未更新または破棄されているため

### GREEN: 最小実装

`web/src/app.css` を新規作成し、`@theme inline` ブロックで design-system.md §1-3 のトークンを定義。`@layer base` で body の背景グラデーション・基本フォントも設定。

### REFACTOR: 設計の整理

- トークン名は design-system.md と完全一致
- Tailwind v4 の `@theme inline` 構文を使用（既存スタック維持）

### テストリスト更新

- 完了: `T7`
- 追加: なし
- 次候補: `T8`

### コミット

`feat(web): define design tokens in app.css`

---

## Step 8: Frontend TDD Cycle - Button コンポーネント

### この Step の目的

[design-system.md §9.1](../webui/design-system.md#91-button) に従い Button コンポーネントを実装する。

### 今回選ぶ項目

- 対象: `T8`
- 選ぶ理由: 全画面で使う最頻出コンポーネント。variants・disabled・busy の状態を持つ
- この時点では扱わないこと: 他の共通コンポーネント（T7）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `button_renders_all_variants_and_states`
- Given: Button に variant="primary"・"secondary"・"icon"・"danger" を渡して描画
- When: 各 variant の button を取得
- Then: 対応する class（`btn-primary`・`btn-secondary`・`btn-icon`・`btn-danger`）が付与される。`disabled` prop で `disabled` 属性が付く。`busy` prop で `aria-busy="true"` と spinner が描画される
- 失敗理由の想定: Button コンポーネント未実装

### GREEN: 最小実装

`web/src/components/Button.tsx` を新規作成。variant・disabled・busy・onClick・children を props に持つ。`app.css` に `.btn-*` クラスを追加してスタイル定義。

### REFACTOR: 設計の整理

- 重複: spinner は独立コンポーネント（Step 9 で実装）する予定だが、Step 8 ではインラインで簡易描画してよい
- 命名: `variant`・`busy` は一般的な React 慣習に従う

### テストリスト更新

- 完了: `T8`
- 追加: なし
- 次候補: `T9`

### コミット

`feat(web): add Button component with 4 variants`

---

## Step 9: Frontend TDD Cycle - 共通コンポーネント群

### この Step の目的

[design-system.md §9.2-9.10](../webui/design-system.md) に従い Badge・StatusDot・Modal・Toast・EmptyState・Spinner・Card を実装する。

### 今回選ぶ項目

- 対象: `T9`
- 選ぶ理由: レイアウト・Chat タブ両方で使う基盤コンポーネント群
- この時点では扱わないこと: Code Block（Step 16 で Markdown と一緒に実装）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `common_components_render_according_to_spec`
- Given: 各コンポーネントを描画
- When: 各々の DOM を取得
- Then:
  - Badge: `kind="channel"` で `badge-channel` class
  - StatusDot: `tone="live"` で `dot-live` class、`tone="idle"` で `dot-idle`
  - Modal: `onClose`・`labelledBy` props、ESC キーで onClose が呼ばれる
  - Toast: info/success/error/warning の tone で表示時間が異なる
  - EmptyState: icon・title・description・action を縦積み表示
  - Spinner: size sm/md/lg と `aria-label="Loading"`
  - Card: hover・active 状態の class 付き
- 失敗理由の想定: 各コンポーネント未実装

### GREEN: 最小実装

`web/src/components/` 配下に各コンポーネントファイルを新規作成。`app.css` に対応するスタイルクラスを追加。

### REFACTOR: 設計の整理

- Modal は focus trap・ESC 処理を hook に切り出してもよい
- Toast は context provider 経由で呼び出し可能にする（`useToast()` hook）

### テストリスト更新

- 完了: `T9`
- 追加: なし
- 次候補: `T10`

### コミット

`feat(web): add common components (Badge, StatusDot, Modal, Toast, EmptyState, Spinner, Card)`

---

## Step 10: Frontend TDD Cycle - App shell と レスポンシブ

### この Step の目的

[layout.md §1・§4](../webui/layout.md) に従い App shell（Sidebar / Top Bar / Main の3領域）を実装し、Mobile では Sidebar が hamburger overlay になる。

### 今回選ぶ項目

- 対象: `T10`
- 選ぶ理由: 全画面の枠組み。Sidebar・Top Bar・Main が依存
- この時点では扱わないこと: Sidebar・Top Bar の中身（T9-T12）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `app_shell_renders_three_regions_and_mobile_overlay`
- Given: App コンポーネントを描画
- When: DOM を取得
- Then: `.app-shell` 配下に `.sidebar`・`.topbar`・`.main` の3領域が存在。window 幅を 600px に設定すると `.sidebar` に `closed` class が付き、hamburger button を click すると `open` class に切り替わる
- 失敗理由の想定: App shell 未実装

### GREEN: 最小実装

`web/src/components/App.tsx` を新規作成。`app.css` に `.app-shell` の grid layout・レスポンシブブレークポイント（sm < 640px）を定義。`useState` で Sidebar 開閉状態を管理。

### REFACTOR: 設計の整理

- Sidebar 開閉状態は URL に含めない（ephemeral state、[overview.md §4.1](../webui/overview.md)）
- Media query は CSS 側で処理

### テストリスト更新

- 完了: `T10`
- 追加: なし
- 次候補: `T11`

### コミット

`feat(web): add app shell with responsive sidebar overlay`

---

## Step 11: Frontend TDD Cycle - Sidebar Brand・New Session・Runtime Status

### この Step の目的

[layout.md §2.2・§2.5・§2.6](../webui/layout.md) に従い Sidebar の静的要素（Brand Header・New Session・Runtime Status Footer）を実装する。

### 今回選ぶ項目

- 対象: `T11`
- 選ぶ理由: AGENTS・SESSIONS section と独立して実装できる静的要素
- この時点では扱わないこと: AGENTS・SESSIONS section（T10・T11）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sidebar_renders_brand_new_session_and_runtime_status`
- Given: App を描画（health status と version を mock で注入）
- When: Sidebar 内の各要素を取得
- Then:
  - Brand Header にロゴ・product name・version が表示
  - New Session ボタンが存在、click で `onNewSession` callback が呼ばれる
  - Runtime Status Footer に StatusDot と "ok · N turns live" 形式のテキストが表示
- 失敗理由の想定: Sidebar コンポーネント未実装

### GREEN: 最小実装

`web/src/components/Sidebar.tsx` を新規作成。Brand Header・New Session button・Runtime Status Footer の3領域を描画。AGENTS・SESSIONS section は placeholder で空表示。

### REFACTOR: 設計の整理

- Health polling はこの Step では mock（T26 でキャッシュ層経由に切り替え）

### テストリスト更新

- 完了: `T11`
- 追加: なし
- 次候補: `T12`

### コミット

`feat(web): add Sidebar brand, New Session button, and Runtime Status footer`

---

## Step 12: Frontend TDD Cycle - Sidebar AGENTS Section

### この Step の目的

[layout.md §2.3](../webui/layout.md) に従い Sidebar AGENTS Section を実装。agent 一覧表示・選択・StatusDot（active 連動）を含む。

### 今回選ぶ項目

- 対象: `T12`
- 選ぶ理由: live indicator 表示の中核
- この時点では扱わないこと: SESSIONS section（T11）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `agents_section_renders_list_and_active_state`
- Given: mock agent data `[{id:"lyre", active:true, is_default:true}, {id:"ace", active:false}]` を注入
- When: AGENTS section を描画
- Then:
  - 各 agent が1行に表示される
  - `active=true` の agent には `dot-live` class の StatusDot、`active=false` には `dot-idle` class
  - `is_default=true` の agent には default tag が表示
  - agent を click で `onSelectAgent(agentId)` callback が呼ばれる
  - 選択中 agent は `active` class（CSS の強調表示用）
- 失敗理由の想定: AGENTS Section 未実装

### GREEN: 最小実装

`web/src/components/AgentsSection.tsx` を新規作成。`GET /api/agents` を fetch して描画。Step 1 で拡張した API を利用。

### REFACTOR: 設計の整理

- `/api/agents` の polling（5秒）は T26 でキャッシュ層に統合。ここでは単純 fetch でよい
- StatusDot コンポーネント（Step 7）を利用

### テストリスト更新

- 完了: `T12`
- 追加: なし
- 次候補: `T13`

### コミット

`feat(web): add Sidebar AGENTS section with live status`

---

## Step 13: Frontend TDD Cycle - Sidebar SESSIONS Section

### この Step の目的

[layout.md §2.4](../webui/layout.md) に従い Sidebar SESSIONS Section を実装。channel フィルタ・セッション一覧・選択を含む。

### 今回選ぶ項目

- 対象: `T13`
- 選ぶ理由: Chat タブのセッション選択の前提
- この時点では扱わないこと: Top Bar（T12）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sessions_section_renders_list_with_channel_and_agent_filter`
- Given: mock sessions `[{label:"Web Chat", channel:"web", agent_id:"lyre"}, {label:"Dev", channel:"discord", agent_id:"lyre"}, {label:"Notes", channel:"cli", agent_id:"ace"}]` を注入。選択中 agent = `"lyre"`
- When: SESSIONS section を描画
- Then:
  - 選択中 agent のセッションのみ初期表示（"Web Chat" と "Dev" の2件、"Notes" は agent 違いで非表示）
  - channel filter select に `All / Web / Discord / ...` が並ぶ
  - filter を `Web` に設定すると、"Web Chat" のみ表示（agent と channel の AND 条件）
  - セッション click で `onSelectSession(sessionKey)` callback が呼ばれる
- 失敗理由の想定: SESSIONS Section 未実装

### GREEN: 最小実装

`web/src/components/SessionsSection.tsx` を新規作成。`GET /api/sessions` を fetch・filter state で絞り込み・描画。

### REFACTOR: 設計の整理

- agent 選択と filter の AND 条件を localStorage ではなく URL query で表現（T10 の App shell が前提とする router 経由）
- まだ router 導入前の場合は ephemeral state で仮置き

### テストリスト更新

- 完了: `T13`
- 追加: なし
- 次候補: `T14`

### コミット

`feat(web): add Sidebar SESSIONS section with channel filter`

---

## Step 14: Frontend TDD Cycle - Top Bar

### この Step の目的

[layout.md §3](../webui/layout.md) に従い Top Bar を実装。palette trigger・5タブ・health badge を含む。

### 今回選ぶ項目

- 対象: `T14`
- 選ぶ理由: ナビゲーションと palette 起動の入口
- この時点では扱わないこと: Command Palette の中身（T22-T24）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `topbar_renders_palette_trigger_tabs_and_health`
- Given: App を描画（現在タブ="chat"、health="ok"）
- When: Top Bar 内の各要素を取得
- Then:
  - palette trigger ボタンが存在、click で `onOpenPalette` callback
  - 5タブ（Chat / Sleep / Pulse / Metrics / Config）が表示
  - 現在タブ（Chat）に `active` class、他は muted
  - 未実装タブ（Sleep/Pulse/Metrics/Config）には disabled 表現
  - health badge に StatusDot と簡易テキスト
- 失敗理由の想定: Top Bar 未実装

### GREEN: 最小実装

`web/src/components/TopBar.tsx` を新規作成。palette trigger・Tab nav・Health badge を配置。

### REFACTOR: 設計の整理

- 未実装タブは disabled 表示
- Tabs click は URL 遷移（router 導入後）または callback で画面切替

### テストリスト更新

- 完了: `T14`
- 追加: なし
- 次候補: `T15`

### コミット

`feat(web): add Top Bar with palette trigger, tabs, and health badge`

---

## Step 15: Frontend TDD Cycle - Chat Tab Container と Header

### この Step の目的

[chat.md §1・§2](../webui/chat.md) に従い Chat Tab の3領域構造（Header / Timeline / Composer）と Chat Header を実装する。

### 今回選ぶ項目

- 対象: `T15`
- 選ぶ理由: Chat タブの枠組み。Timeline・Composer は後続 Step で実装
- この時点では扱わないこと: Timeline の中身（T14-T19）、Composer の中身（T20）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `chat_tab_renders_header_timeline_composer_structure`
- Given: Chat Tab を描画（session 情報を mock で注入）
- When: DOM を取得
- Then:
  - `.chat-tab` 配下に `.chat-header`・`.timeline`・`.composer` の3領域が grid 配置
  - Chat Header に session label・channel badge・metadata（開始時刻・メッセージ数）が表示
  - read-only セッションの場合は metadata に "read-only" が含まれる
- 失敗理由の想定: Chat Tab 未実装

### GREEN: 最小実装

`web/src/components/ChatTab.tsx` を新規作成。3領域の grid layout を定義。Header は props の session 情報から表示。Timeline と Composer は placeholder。

### REFACTOR: 設計の整理

- channel 毎の metadata 表示ルールは helper 関数に切り出し
- OverflowMenu（rename / delete）は後で追加、ここでは Refresh button のみ

### テストリスト更新

- 完了: `T15`
- 追加: なし
- 次候補: `T16`

### コミット

`feat(web): add Chat Tab container with header`

---

## Step 16: Frontend TDD Cycle - Timeline と自動スクロール

### この Step の目的

[chat.md §3](../webui/chat.md) に従い Timeline を実装。メッセージ時系列表示・自動スクロール・"Jump to latest" ボタンを含む。

### 今回選ぶ項目

- 対象: `T16`
- 選ぶ理由: Chat タブの主領域。MessageBubble（T15）の前提
- この時点では扱わないこと: MessageBubble の中身（T15）、Tool Card（T18）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `timeline_auto_scrolls_when_near_bottom`
- Given: メッセージ100件の Timeline を描画・最下部にスクロール
- When: 新しいメッセージを1件追加
- Then: 自動的に最下部へスクロールする（`scrollTop === scrollHeight - clientHeight`）
- 付随するテスト名: `timeline_shows_jump_to_latest_when_scrolled_up`
- Given: メッセージ100件の Timeline を描画・上へスクロール
- When: 最下部から離れる
- Then: "Jump to latest" ボタンが表示される。click で最下部へスクロール
- 失敗理由の想定: Timeline 未実装

### GREEN: 最小実装

`web/src/components/Timeline.tsx` を新規作成。`useRef`・`useEffect` でスクロール位置監視・自動スクロール・"Jump to latest" 表示切り替え。メッセージ描画は MessageBubble コンポーネント（Step 15 で実装）を呼ぶ想定だが、ここではインラインでプレーン表示してよい。

### REFACTOR: 設計の整理

- 自動スクロール判定（最下部からの距離 < 画面高の 10%）は hook に切り出してもよい

### テストリスト更新

- 完了: `T16`
- 追加: なし
- 次候補: `T17`

### コミット

`feat(web): add Timeline with auto-scroll and Jump to latest`

---

## Step 17: Frontend TDD Cycle - MessageBubble

### この Step の目的

[chat.md §4.1-4.4](../webui/chat.md) に従い MessageBubble を実装。sender_kind 別の配置・スタイル・avatar・label・timestamp を含む。

### 今回選ぶ項目

- 対象: `T17`
- 選ぶ理由: チャットタイムラインの主要素
- この時点では扱わないこと: Markdown レンダリング（T16）、Pulse 識別（T19）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `message_bubble_renders_per_sender_kind`
- Given: sender_kind="user"・"assistant"・"system"・"tool" の4パターンのメッセージを描画
- When: 各 bubble の class・配置を取得
- Then:
  - user: `bubble-user` class、右寄せ
  - assistant: `bubble-assistant` class、左寄せ
  - system: `bubble-system` class、中央配置
  - tool: `bubble-tool` class、左寄せ
  - 全 bubble に sender label・avatar・timestamp が表示
- 失敗理由の想定: MessageBubble 未実装

### GREEN: 最小実装

`web/src/components/MessageBubble.tsx` を新規作成。`sender_kind` で class と layout を切り替え。本文はプレーン text 表示（Markdown は T16）。

### REFACTOR: 設計の整理

- avatar・label・timestamp の表示ルールは helper 関数に切り出し

### テストリスト更新

- 完了: `T17`
- 追加: なし
- 次候補: `T18`

### コミット

`feat(web): add MessageBubble with sender-kind variants`

---

## Step 18: Frontend TDD Cycle - Markdown レンダリング と Code Block

### この Step の目的

[chat.md §4.6](../webui/chat.md) と [design-system.md §9.9](../webui/design-system.md#99-code-block) に従い Markdown レンダリングと Code Block の Copy ボタンを実装する。

### 今回選ぶ項目

- 対象: `T18`
- 選ぶ理由: メッセージ本文表示の中核
- この時点では扱わないこと: Streaming indicator（T17）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `markdown_renders_elements_and_code_block_has_copy`
- Given: Markdown 入力 `# Title\n\n- list\n\n`と ```` ```js\nconsole.log("hi")\n``` ```` を描画
- When: DOM を取得
- Then:
  - `<h1>`・`<ul>`・`<li>` が描画される
  - `<pre><code>` が描画され、Copy ボタンが存在
  - Copy ボタン click で `navigator.clipboard.writeText` が呼ばれ、"Copied" トーストが表示される
- 失敗理由の想定: Markdown renderer 未実装

### GREEN: 最小実装

`web/src/components/MarkdownRenderer.tsx` を新規作成。`react-markdown` + `remark-gfm` を使用（既存スタック）。Code Block はカスタム renderer で Copy ボタンを追加。

### REFACTOR: 設計の整理

- react-markdown の `components` prop で code/pre をカスタマイズ
- Copy ボタンは hover 時表示

### テストリスト更新

- 完了: `T18`
- 追加: なし
- 次候補: `T19`

### コミット

`feat(web): add Markdown renderer with Code Block copy button`

---

## Step 19: Frontend TDD Cycle - Streaming Indicator

### この Step の目的

[chat.md §4.7](../webui/chat.md) に従い Streaming 中の draft メッセージに点滅カーソルを表示する。

### 今回選ぶ項目

- 対象: `T19`
- 選ぶ理由: 実ストリーミング（T5 で実装済み）を UI で体感できる最初の Step
- この時点では扱わないこと: Tool Card（T18）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `streaming_indicator_renders_for_draft_message`
- Given: `id="draft:abc"` の draft メッセージを描画
- When: その bubble を取得
- Then: 末尾に点滅カーソル（`.streaming-cursor`）が表示される
- 付随するテスト名: `streaming_indicator_removed_on_done`
- Given: draft メッセージの id を `"draft:abc:done"` に変更
- When: 再描画
- Then: カーソルが消えている
- 失敗理由の想定: Streaming indicator 未実装

### GREEN: 最小実装

`MessageBubble` に streaming 判定（`message.id.startsWith("draft:") && !message.id.endsWith(":done")`）を追加。true なら `.streaming-cursor` を描画。

### REFACTOR: 設計の整理

- カーソルの CSS animation は `app.css` に定義

### テストリスト更新

- 完了: `T19`
- 追加: なし
- 次候補: `T20`

### コミット

`feat(web): add streaming cursor for draft messages`

---

## Step 20: Frontend TDD Cycle - Tool Card

### この Step の目的

[chat.md §5](../webui/chat.md) に従い Tool Card を実装。tool_start/tool_result の状態・summary・展開表示を含む。

### 今回選ぶ項目

- 対象: `T20`
- 選ぶ理由: Chat タブで LLM の挙動を可視化する重要要素
- この時点では扱わないこと: Pulse 識別（T19）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `tool_card_renders_states_and_expansion`
- Given: tool_start event（状態=pending）と tool_result event（状態=success、duration=120ms）を模擬
- When: Tool Card を描画
- Then:
  - pending: summary="running…"・右端に spinner
  - success: summary に主要引数・右端に "120ms" badge
  - error: summary にエラーメッセージ先頭40字・右端に "error" badge・自動展開
  - closed 状態では input/output を非表示
  - header click で開閉が切り替わる（aria-expanded 反映）
- 失敗理由の想定: Tool Card 未実装

### GREEN: 最小実装

`web/src/components/ToolCard.tsx` を新規作成。tool_start/tool_result イベントから状態を算出・summary 生成ルール（[chat.md §5.2](../webui/chat.md#52-summary-の生成ルール)）を適用・展開状態管理。

### REFACTOR: 設計の整理

- summary 生成ルールは helper 関数に切り出し
- closed 状態では入出力 DOM を描画しない（chat.md §10.3 → 削除済み、閉じた状態は render しない）

### テストリスト更新

- 完了: `T20`
- 追加: なし
- 次候補: `T21`

### コミット

`feat(web): add Tool Card with collapsible state`

---

## Step 21: Frontend TDD Cycle - Pulse 通知識別

### この Step の目的

[chat.md §4.5](../webui/chat.md#45-pulse-通知の識別) に従い `message_kind === "pulse_notification"` のメッセージに Pulse アイコンバッジを付ける。

### 今回選ぶ項目

- 対象: `T21`
- 選ぶ理由: Pulse 通知と通常会話の区別をつける
- この時点では扱わないこと: Composer（T20）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `pulse_notification_renders_pulse_badge`
- Given: `message_kind="pulse_notification"` の assistant メッセージを描画
- When: bubble を取得
- Then: meta 領域に Pulse アイコン + "Pulse" ラベル（`pulse-badge` class）が表示
- 付随するテスト名: `normal_assistant_message_has_no_pulse_badge`
- Given: `message_kind="message"` の assistant メッセージを描画
- When: bubble を取得
- Then: pulse-badge が存在しない
- 失敗理由の想定: Pulse 識別ロジック未実装

### GREEN: 最小実装

`MessageBubble` に `message_kind` prop を追加。`pulse_notification` の場合に Pulse badge を描画。

### REFACTOR: 設計の整理

- Pulse badge の tooltip に intention_id・started_at を表示するため、message にこれらのメタデータが含まれることが前提（Pulse 実装時に追加）

### テストリスト更新

- 完了: `T21`
- 追加: なし
- 次候補: `T22`

### コミット

`feat(web): add Pulse notification badge in chat`

---

## Step 22: Frontend TDD Cycle - Composer と CommandSuggest

### この Step の目的

[chat.md §6](../webui/chat.md#6-composer) に従い Composer（textarea・送信）と CommandSuggest（`/` 入力時のサジェスト）を実装する。

### 今回選ぶ項目

- 対象: `T22`
- 選ぶ理由: ユーザー入力の入口。送信フローの前提
- この時点では扱わないこと: Read-only mode（T21）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `composer_handles_enter_shift_enter_and_suggest`
- Given: Composer を描画
- When: textarea に "hello" を入力し Enter 押下
- Then: `onSubmit` callback が "hello" で呼ばれる、textarea が空になる
- 付随ケース:
  - Shift+Enter で改行挿入（送信されない）
  - 空文字で Enter 押下 → 送信されない
  - `/` で始まる入力でサジェスト表示、`↑↓` で選択・Tab/Enter で確定・Esc で閉じる
- 失敗理由の想定: Composer 未実装

### GREEN: 最小実装

`web/src/components/Composer.tsx` と `web/src/components/CommandSuggest.tsx` を新規作成（既存の同名ファイルがあれば破棄して新規）。キー操作は `onKeyDown` で処理。

### REFACTOR: 設計の整理

- 既存の `commands.ts`（スラッシュコマンド定義）は web/src 破棄で消えるため、[docs/commands.md](../commands.md) を参照して新規作成

### テストリスト更新

- 完了: `T22`
- 追加: なし
- 次候補: `T23`

### コミット

`feat(web): add Composer with slash command suggest`

---

## Step 23: Frontend TDD Cycle - Read-only Mode

### この Step の目的

[chat.md §7](../webui/chat.md#7-read-only-mode) に従い channel !== "web" のセッション選択時に Composer を read-only banner に差し替える。

### 今回選ぶ項目

- 対象: `T23`
- 選ぶ理由: Observe, then Act 設計思想（[overview.md §1.2](../webui/overview.md)）の具体化
- この時点では扱わないこと: Command Palette（T22）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `readonly_session_shows_banner_instead_of_composer`
- Given: channel="discord" のセッションを選択
- When: Chat Tab を描画
- Then: Composer が表示されず、代わりに read-only banner が表示される
- banner 内容: "This is a discord session. To reply, use Discord directly."
- 失敗理由の想定: Read-only mode 未実装

### GREEN: 最小実装

ChatTab で `channel !== "web"` の場合、Composer の代わりに ReadOnlyBanner コンポーネントを描画。channel 毎の表示名は helper 関数で解決。

### REFACTOR: 設計の整理

- banner は ChatTab 内に inline で描画してもよい。独立コンポーネント化は必須でない

### テストリスト更新

- 完了: `T23`
- 追加: なし
- 次候補: `T24`

### コミット

`feat(web): add read-only banner for non-web sessions`

---

## Step 24: Frontend TDD Cycle - Command Palette 開閉

### この Step の目的

[command-palette.md §1・§2・§6](../webui/command-palette.md) に従い Palette の開閉・入力・キーボード操作を実装する。

### 今回選ぶ項目

- 対象: `T24`
- 選ぶ理由: Palette の枠組み。セクション内容（T23）の前提
- この時点では扱わないこと: セクション内容（T23）、Recent history（T24）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `command_palette_opens_and_closes_with_keyboard`
- Given: App を描画（Palette 非表示）
- When: `Cmd+K`（Mac）または `Ctrl+K`（他）を押下
- Then: Palette overlay が表示・input にフォーカス
- 付随ケース:
  - `Esc` で閉じる
  - backdrop click で閉じる
  - 開いている状態で再度 `Cmd+K` で閉じる（トグル）
- 失敗理由の想定: Command Palette 未実装

### GREEN: 最小実装

`web/src/components/CommandPalette.tsx` を新規作成。Modal（Step 7）を利用。グローバルキーリスナーで `Cmd+K` を処理。

### REFACTOR: 設計の整理

- Palette の開閉状態は ephemeral state（URL に乗せない）
- focus trap を Modal 側で実装済みなので利用

### テストリスト更新

- 完了: `T24`
- 追加: なし
- 次候補: `T25`

### コミット

`feat(web): add Command Palette modal with keyboard shortcuts`

---

## Step 25: Frontend TDD Cycle - Command Palette セクション構成

### この Step の目的

[command-palette.md §3・§4](../webui/command-palette.md) に従い Palette のセクション構成（Recent / Quick Actions / Navigation / Agents / Sessions）を実装する。Sleep/Pulse Runs セクションは空表示でよい（当該タブ未実装のため）。

### 今回選ぶ項目

- 対象: `T25`
- 選ぶ理由: Palette の中身
- この時点では扱わないこと: Recent history（T24）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `command_palette_renders_all_sections`
- Given: Palette を開く
- When: 各 section を取得
- Then:
  - Quick Actions: "New Session"・"Refresh current tab"・"Show shortcuts"
  - Navigation: 5タブへの遷移項目（未実装タブは disabled）
  - Agents: 他 agent への切替項目（選択中 agent は除外）
  - Sessions: 選択中 agent のセッション一覧
- 失敗理由の想定: セクション内容未実装

### GREEN: 最小実装

CommandPalette に各セクションを描画。入力クエリで部分一致フィルタ。`↑↓` で項目移動・Enter で実行。

### REFACTOR: 設計の整理

- 各項目の実行（navigation・agent 切替・session 選択）は callback で上位へ通知
- Sleep/Pulse Runs セクションは当該タブ実装時に有効化

### テストリスト更新

- 完了: `T25`
- 追加: なし
- 次候補: `T26`

### コミット

`feat(web): add Command Palette sections and items`

---

## Step 26: Frontend TDD Cycle - Recent History (localStorage)

### この Step の目的

[command-palette.md §5](../webui/command-palette.md) に従い Recent セクションを localStorage から取得・表示する。

### 今回選ぶ項目

- 対象: `T26`
- 選ぶ理由: Palette の UX 完成度向上
- この時点では扱わないこと: WS・状態管理（T25-T27）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `palette_recent_reads_from_localstorage`
- Given: localStorage に `egopulse.paletteHistory` で2件の履歴を保存
- When: Palette を開く
- Then: Recent セクションに2件が表示
- 付随ケース:
  - 項目実行で `lastUsed` が更新される
  - localStorage 利用不可（プライベートモード想定）の場合は Recent セクションを非表示
- 失敗理由の想定: Recent history 機能未実装

### GREEN: 最小実装

`usePaletteHistory` hook を新規作成。localStorage の読み書き・最大20件保持・5件表示。

### REFACTOR: 設計の整理

- localStorage アクセスは try-catch で囲み、例外時は機能無効化

### テストリスト更新

- 完了: `T26`
- 追加: なし
- 次候補: `T27`

### コミット

`feat(web): add Command Palette recent history via localStorage`

---

## Step 27: Frontend TDD Cycle - WS メッセージハンドリング

### この Step の目的

[chat.md §8](../webui/chat.md) + [overview.md §3・§5](../webui/overview.md) に従い WS 接続・chat event 受信・Timeline へ反映を実装する。

### 今回選ぶ項目

- 対象: `T27`
- 選ぶ理由: 実ストリーミング（T4 で実装済み）を frontend で受け取る中核
- この時点では扱わないこと: Server state キャッシュ（T26）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `ws_handler_processes_chat_events_and_send_via_chat_send`
- Given: WS message handler に以下の event を順に投入:
  - `{state: "delta", message: {content: [{text: "Hello"}]}}`
  - `{state: "delta", message: {content: [{text: " world"}]}}`
  - `{state: "done", message: {content: [{text: "Hello world"}]}}`
- When: ハンドラを実行
- Then:
  - 1件目の delta で draft メッセージを作成（content="Hello"）
  - 2件目の delta で draft に " world" を追記（content="Hello world"）
  - done で draft の id を確定値へ差し替え
- 付随ケース: tool_start/tool_result event で Tool Card へ反映
- 失敗理由の想定: WS handler 未実装

### GREEN: 最小実装

`web/src/hooks/useWebSocket.tsx` を新規作成（既存の同名ファイルは破棄）。`chat` event の `state` フィールドで処理を分岐。`tool_start`・`tool_result` state の新値にも対応。

### REFACTOR: 設計の整理

- WS 接続状態（connecting / open / closed）も管理
- 再接続ロジックは simple backoff で実装

### テストリスト更新

- 完了: `T27`
- 追加: なし
- 次候補: `T28`

### コミット

`feat(web): add WebSocket message handler and migrate chat send to WS`

---

## Step 28: Frontend TDD Cycle - Server State キャッシュ

### この Step の目的

[overview.md §4](../webui/overview.md) に従い Server state（agents / sessions / history / config / health）をキャッシュ層経由で取得する。

### 今回選ぶ項目

- 対象: `T28`
- 選ぶ理由: Sidebar・Chat 両方で使うデータ取得の統一
- この時点では扱わないこと: チャット送信後の無効化（T27）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `server_state_caches_and_invalidates`
- Given: agents API を1回 fetch 済み
- When: 同じキーで再度取得
- Then: API は呼ばれずキャッシュが返される
- 付随ケース: invalidate すると次回取得で API が呼ばれる
- 失敗理由の想定: キャッシュ層未導入

### GREEN: 最小実装

キャッシュライブラリを `web/package.json` に追加。`web/src/hooks/` 配下に各データソース（agents・sessions・history・config・health）の query hook を新規作成。

### REFACTOR: 設計の整理

- キャッシュライブラリの選定は実装時に判断（TanStack Query 等の定番候補）
- query key は agent 選択・フィルタ条件を含む階層構造

### テストリスト更新

- 完了: `T28`
- 追加: なし
- 次候補: `T29`

### コミット

`feat(web): add server state cache layer`

---

## Step 29: Frontend TDD Cycle - チャット送信後のキャッシュ無効化

### この Step の目的

[overview.md §4.2](../webui/overview.md) に従いチャット送信完了後に session 一覧と当該 session の履歴を無効化・再取得する。

### 今回選ぶ項目

- 対象: `T29`
- 選ぶ理由: ユーザーが送信後に最新状態を見られる仕組み
- この時点では扱わないこと: 未実装タブのキャッシュ無効化

### RED: 失敗する自動テストを書く

- 追加するテスト名: `chat_send_invalidates_sessions_and_history`
- Given: session 一覧と履歴がキャッシュ済み
- When: チャット送信の done event を受信
- Then: `["sessions"]` と `["history", {sessionKey}]` のキャッシュが無効化され、再 fetch が走る
- 失敗理由の想定: 無効化ロジック未実装

### GREEN: 最小実装

WS handler の done 処理で invalidateQueries を呼ぶ。送信時の楽観的メッセージ追加もここで統合。

### REFACTOR: 設定の整理

- Pulse・Sleep 完了時の無効化は当該タブ実装時に追加

### テストリスト更新

- 完了: `T29`
- 追加: なし
- 次候補: `T30`

### コミット

`feat(web): invalidate sessions and history cache on chat done`

---

## Step 30: Frontend TDD Cycle - Sidebar 畳み込み

### この Step の目的

[layout.md §2.1](../webui/layout.md) に従い Sidebar に [<] ボタンを追加し、collapsed (48px) / expanded (260px) を切り替えられるようにする。

### 今回選ぶ項目

- 対象: `T30`
- 選ぶ理由: Desktop でチャットに集中する際の画面活用
- この時点では扱わないこと: Mobile では hamburger overlay のみ（折りたたみ機能なし）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sidebar_collapses_to_icon_only_bar`
- Given: Sidebar を expanded 状態で描画
- When: [<] ボタンを click
- Then: Sidebar の幅が 48px になり、agent StatusDot・New Session icon・StatusDot のみ表示。ラベル・SESSIONS 一覧は非表示
- 付随ケース:
  - URL query に `?sidebar=collapsed` が設定される
  - collapsed 状態でアイコン click すると expanded に戻る
  - リロード後も状態が維持される
- 失敗理由の想定: 畳み込み機能未実装

### GREEN: 最小実装

`Sidebar.tsx` に collapse button と state を追加。`app.css` に `.sidebar.collapsed` スタイル（48px 幅・ラベル非表示）。状態は URL query (`?sidebar=collapsed`) で永続化。

### REFACTOR: 設計の整理

- 状態管理は router の search params 経由（T10 の App shell が前提）
- Mobile (sm) では collapse button を非表示（hamburger overlay のみ）

### テストリスト更新

- 完了: `T30`
- 追加: なし
- 次候補: `T31`

### コミット

`feat(web): add Sidebar collapse toggle`

---

## Step 31: Frontend TDD Cycle - Timeline メッセージ検索

### この Step の目的

[chat.md §3.3](../webui/chat.md) に従い Timeline 内キーワード検索（`Cmd+F`）を実装する。

### 今回選ぶ項目

- 対象: `T31`
- 選ぶ理由: 長セッションでの情報検索 UX 向上
- この時点では扱わないこと: sender label・timestamp は検索対象外

### RED: 失敗する自動テストを書く

- 追加するテスト名: `timeline_search_highlights_and_navigates_matches`
- Given: 3メッセージ（"hello world"・"goodbye"・"world peace"）を含む Timeline を描画
- When: `Cmd+F` → 検索バーに "world" を入力
- Then:
  - マッチ箇所（2件）がハイライトされる
  - マッチ件数 "1 / 2" が表示される
  - `Enter` で次のマッチへジャンプ（スクロール）
  - `Shift+Enter` で前のマッチへ
  - `Esc` で検索バーを閉じる・ハイライト解除
- 失敗理由の想定: 検索機能未実装

### GREEN: 最小実装

`Timeline.tsx` に search bar を追加。全メッセージのプレーンテキストから部分一致検索。`useRef`・`scrollIntoView` でマッチへジャンプ。

### REFACTOR: 設計の整理

- 検索ロジックは `useTimelineSearch` hook に切り出し
- 大文字小文字は区別しない

### テストリスト更新

- 完了: `T31`
- 追加: なし
- 次候補: `T32`

### コミット

`feat(web): add Timeline in-message search with Cmd+F`

---

## Step 32: Frontend TDD Cycle - Code Block 折りたたみ

### この Step の目的

[chat.md §4.6](../webui/chat.md) + [design-system.md §9.9](../webui/design-system.md) に従い20行超の code block を折りたたみ表示にする。

### 今回選ぶ項目

- 対象: `T32`
- 選ぶ理由: 長大なツール結果等で Timeline が圧迫されるのを防止
- この時点では扱わないこと: code block の Copy ボタン（Step 18 で実装済み）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `code_block_folds_when_longer_than_threshold`
- Given: 25行の code block を含む Markdown を描画
- When: 描画結果を取得
- Then:
  - 最初の20行のみ表示
  - "Show all (25 lines)" ボタンが表示
  - click で全行表示され、ボタンが "Collapse" に切り替わる
  - "Collapse" click で20行表示に戻る
- 失敗理由の想定: 折りたたみ機能未実装

### GREEN: 最小実装

`MarkdownRenderer.tsx` の code block renderer に行数判定と state を追加。20行超で折りたたみ表示。

### REFACTOR: 設計の整理

- 閾値（20行）は定数として定義
- 展開状態は code block 単位（メッセージ単位でない）

### テストリスト更新

- 完了: `T32`
- 追加: なし
- 次候補: `T33`

### コミット

`feat(web): fold long code blocks in Markdown renderer`

---

## Step 33: Frontend TDD Cycle - Composer Draft 永続化

### この Step の目的

[chat.md §6.5](../webui/chat.md) に従い Composer の入力中テキストを localStorage に保存・リロードで復元する。

### 今回選ぶ項目

- 対象: `T33`
- 選ぶ理由: 誤リロード・クラッシュ対策
- この時点では扱わないこと: セッションをまたぐドラフト（sessionKey 毎に独立）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `composer_draft_persists_across_reload`
- Given: Composer に "hello world" を入力（debounce 300ms 経過後）
- When: Composer を unmount → 再 mount（リロード想定）
- Then: 入力欄に "hello world" が復元される
- 付随ケース:
  - 送信成功で localStorage から当該キーが削除される
  - セッション切替時、旧セッションの draft を保存・新セッションの draft を復元
  - localStorage 利用不可時は例外を握り潰して機能無効化
- 失敗理由の想定: Draft 永続化未実装

### GREEN: 最小実装

`useComposerDraft(sessionKey)` hook を新規作成。localStorage の読み書き（キー: `egopulse.composerDraft.{sessionKey}`）。Composer に組み込み。

### REFACTOR: 設計の整理

- localStorage アクセスは try-catch で囲み例外時は機能無効化（Step 26 の palette history と同じパターン）

### テストリスト更新

- 完了: `T33`
- 追加: なし
- 次候補: なし（TDD Cycle 完了）

### コミット

`feat(web): persist Composer draft to localStorage per session`

---

## Step 34: 動作確認（UT）

### Frontend

- `cd web && npm test`（vitest run）
- `cd web && npx tsc --noEmit`
- `cd web && npm run lint`（設定があれば）

### Backend

- `cargo fmt --check`
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`

### 失敗時に戻る Step

- 該当テストが属する TDD Cycle へ戻る

---

## Step 35: Plan・仕様書との自己チェック

実装完了後にこの Plan と [docs/webui/](../webui/) 配下の仕様書を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリスト（T1-T29）と各 Cycle が完了条件を満たしている
- [overview.md](../webui/overview.md) §2-5 の振る舞いが全て実装に反映されている
- [layout.md](../webui/layout.md) §2-7 の振る舞いが全て実装に反映されている
- [chat.md](../webui/chat.md) §2-9 の振る舞いが全て実装に反映されている
- [command-palette.md](../webui/command-palette.md) §1-9 の振る舞いが全て実装に反映されている
- [design-system.md](../webui/design-system.md) のトークンとコンポーネントが全て定義されている
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している

---

## Step 36: E2E テスト（Playwright MCP）

UT とは別フェーズとして、実際にアプリを起動して E2E 検証を行う。TDD ではなく、検証シナリオとして実行する。

### 起動

- `cargo run -- run`（全チャネル起動・WebUI 含む）
- または frontend 開発サーバー単体：`cd web && npm run dev`

### 検証シナリオ

1. **基本レイアウト**：
   - `/docs/webui/mockup.html` と見た目を比較（スクリーンショット取得）
   - Sidebar・Top Bar・Chat の3領域が仕様通り配置されている

2. **Chat 送信フロー**：
   - Web セッション選択 → メッセージ送信 → ストリーミング応答表示 → done で確定
   - Tool Card の表示・開閉

3. **セッション切替**：
   - Web セッション（writable）→ Discord セッション（read-only）切替で banner が表示される
   - channel filter でリストが絞り込まれる

4. **Command Palette**：
   - `Cmd+K` で開閉
   - 各セクション表示・項目選択で遷移
   - Recent history が localStorage から読み込まれる

5. **AGENTS Section**：
   - 別 agent を選択すると SESSIONS が切り替わる
   - agent 処理中に StatusDot が点滅する

6. **レスポンシブ**：
   - window 幅 600px で hamburger overlay 動作

### 失敗時に戻る Step

- 該当機能の TDD Cycle へ戻り、UT または実装を修正して Step 34 から再実行

---

## Step 37: PR 作成

- PR タイトル: `feat(webui): Phase 1 - Chat foundation`
- PR description:
  - 概要: WebUI 新設計の Phase 1 実装。デザインシステム・レイアウト・Chat タブ・Command Palette + 関連バックエンド拡張
  - 詳細: 各 Step のコミット内容を簡潔に箇条書き
  - テスト: UT 結果（29件）・E2E シナリオ実施結果
  - Close #<issue-number>（該当する場合）
- レビュアーの目線: 設計仕様書（docs/webui/）との整合・TDD サイクルの妥当性・E2E 結果

---

## Step 38: 初回レビューバック

PR 作成後、レビュー生成を待ってから `pr-review-back-workflow` Skill を実行し、未対応のレビューコメントがあれば修正・検証・コミット・push まで完了する。

- 初回待機: `sleep 15m`
- レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだレビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- レビューコメントが無い、または最大待機後もレビューが無い場合は、その結果を PR に記録して完了扱いにする
- レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## Step 39: レビュー対応後の再レビューバック

レビュー対応を push した後、追加レビュー生成を待ってから `pr-review-back-workflow` Skill を再実行し、残った指摘や新規指摘があれば同じ品質基準で対応する。

- 対象: Step 38 でレビュー対応の変更を push した場合
- 初回待機: `sleep 15m`
- 再レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだ追加レビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- 追加レビューコメントが無い、または最大待機後も追加レビューが無い場合は、その結果を PR に記録して完了扱いにする
- 再レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/channels/web/sleep.rs` または 新規 `agents.rs` | 変更 | `/api/agents` を `Vec<String>` から `Vec<AgentInfo { id, label, is_default, active }>` へ拡張。ルートハンドラの移動も検討 |
| `src/channels/web/sessions.rs` | 変更 | `/api/history` に message_kind 追加・`/api/sessions` に agent_id 追加 |
| `src/channels/web/stream.rs` | 変更 | WS chat event ペイロードに sessionKey 追加・delta event forward |
| `src/channels/web/ws.rs` | 変更 | chat event ペイロード拡張・`chat.send` ハンドラで `start_stream_run` を呼び出し |
| `src/channels/web/sse.rs` | 変更 | `AgentEvent::Delta { text }` バリアント追加 |
| `src/agent_loop/turn.rs` | 変更 | LLM トークン刻みで delta event を emit |
| `web/src/` 全体 | **新規** | 既存コードは破棄・design-system/layout/chat/palette 各モジュール新設 |
| `web/src/app.css` | **新規** | デザイントークン・コンポーネントクラス・feature クラス定義 |
| `web/src/components/Button.tsx` | **新規** | Button コンポーネント |
| `web/src/components/{Badge,StatusDot,Modal,Toast,EmptyState,Spinner,Card}.tsx` | **新規** | 共通コンポーネント群 |
| `web/src/components/App.tsx` | **新規** | App shell |
| `web/src/components/Sidebar.tsx` | **新規** | Sidebar（Brand・AGENTS・SESSIONS・New Session・Status） |
| `web/src/components/TopBar.tsx` | **新規** | Top Bar（palette trigger・tabs・health） |
| `web/src/components/ChatTab.tsx` | **新規** | Chat Tab container |
| `web/src/components/ChatHeader.tsx` | **新規** | Chat Header |
| `web/src/components/Timeline.tsx` | **新規** | Timeline + 自動スクロール |
| `web/src/components/MessageBubble.tsx` | **新規** | MessageBubble |
| `web/src/components/MarkdownRenderer.tsx` | **新規** | Markdown + Code Block |
| `web/src/components/ToolCard.tsx` | **新規** | Tool Card |
| `web/src/components/Composer.tsx` | **新規** | Composer + CommandSuggest |
| `web/src/components/CommandPalette.tsx` | **新規** | Command Palette |
| `web/src/hooks/useWebSocket.tsx` | **新規** | WS message handler + chat.send 送信 |
| `web/src/hooks/useChat*.ts` | **新規** | Server state query hooks |
| `web/package.json` | 変更 | キャッシュライブラリ追加 |

---

## コミット分割

1. `feat(web): return all configured agents with active flag from /api/agents` - Step 1
2. `feat(web): expose message_kind in /api/history response` - Step 2
3. `feat(web): expose agent_id in /api/sessions response` - Step 3
4. `feat(web): include sessionKey in WS chat event payload` - Step 4
5. `feat(agent_loop): emit Delta events for token streaming` - Step 5
6. `feat(web): accept chat.send via WebSocket and return runId` - Step 6
7. `feat(web): define design tokens in app.css` - Step 7
8. `feat(web): add Button component with 4 variants` - Step 8
9. `feat(web): add common components (Badge, StatusDot, Modal, Toast, EmptyState, Spinner, Card)` - Step 9
10. `feat(web): add app shell with responsive sidebar overlay` - Step 10
11. `feat(web): add Sidebar brand, New Session button, and Runtime Status footer` - Step 11
12. `feat(web): add Sidebar AGENTS section with live status` - Step 12
13. `feat(web): add Sidebar SESSIONS section with channel filter` - Step 13
14. `feat(web): add Top Bar with palette trigger, tabs, and health badge` - Step 14
15. `feat(web): add Chat Tab container with header` - Step 15
16. `feat(web): add Timeline with auto-scroll and Jump to latest` - Step 16
17. `feat(web): add MessageBubble with sender-kind variants` - Step 17
18. `feat(web): add Markdown renderer with Code Block copy button` - Step 18
19. `feat(web): add streaming cursor for draft messages` - Step 19
20. `feat(web): add Tool Card with collapsible state` - Step 20
21. `feat(web): add Pulse notification badge in chat` - Step 21
22. `feat(web): add Composer with slash command suggest` - Step 22
23. `feat(web): add read-only banner for non-web sessions` - Step 23
24. `feat(web): add Command Palette modal with keyboard shortcuts` - Step 24
25. `feat(web): add Command Palette sections and items` - Step 25
26. `feat(web): add Command Palette recent history via localStorage` - Step 26
27. `feat(web): migrate chat send from REST/SSE to WebSocket` - Step 27
28. `feat(web): add server state cache layer` - Step 28
29. `feat(web): invalidate sessions and history cache on chat done` - Step 29
30. `feat(web): add Sidebar collapse toggle` - Step 30
31. `feat(web): add Timeline in-message search with Cmd+F` - Step 31
32. `feat(web): fold long code blocks in Markdown renderer` - Step 32
33. `feat(web): persist Composer draft to localStorage per session` - Step 33

---

## 自動テスト一覧（全33件 + E2E）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストとTDD Cycleを追加して対応する。

### Backend（全6件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `api_agents_returns_all_configured_agents_with_active_flag` | Step 1 | `cargo test` |
| T2 | `api_history_returns_message_kind` | Step 2 | `cargo test` |
| T3 | `api_sessions_returns_agent_id` | Step 3 | `cargo test` |
| T4 | `ws_chat_event_includes_session_key` | Step 4 | `cargo test` |
| T5 | `agent_loop_emits_delta_events_during_llm_stream` | Step 5 | `cargo test` |
| T6 | `ws_chat_send_accepts_message_and_returns_run_id` | Step 6 | `cargo test` |

### Frontend - デザインシステム（全3件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T7 | `design_tokens_are_defined_as_css_variables` | Step 7 | `npm test --prefix web` |
| T8 | `button_renders_all_variants_and_states` | Step 8 | `npm test --prefix web` |
| T9 | `common_components_render_according_to_spec` | Step 9 | `npm test --prefix web` |

### Frontend - レイアウト（全5件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T10 | `app_shell_renders_three_regions_and_mobile_overlay` | Step 10 | `npm test --prefix web` |
| T11 | `sidebar_renders_brand_new_session_and_runtime_status` | Step 11 | `npm test --prefix web` |
| T12 | `agents_section_renders_list_and_active_state` | Step 12 | `npm test --prefix web` |
| T13 | `sessions_section_renders_list_with_channel_and_agent_filter` | Step 13 | `npm test --prefix web` |
| T14 | `topbar_renders_palette_trigger_tabs_and_health` | Step 14 | `npm test --prefix web` |

### Frontend - Chat タブ（全9件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T15 | `chat_tab_renders_header_timeline_composer_structure` | Step 15 | `npm test --prefix web` |
| T16 | `timeline_auto_scrolls_when_near_bottom` + `timeline_shows_jump_to_latest_when_scrolled_up` | Step 16 | `npm test --prefix web` |
| T17 | `message_bubble_renders_per_sender_kind` | Step 17 | `npm test --prefix web` |
| T18 | `markdown_renders_elements_and_code_block_has_copy` | Step 18 | `npm test --prefix web` |
| T19 | `streaming_indicator_renders_for_draft_message` + `streaming_indicator_removed_on_done` | Step 19 | `npm test --prefix web` |
| T20 | `tool_card_renders_states_and_expansion` | Step 20 | `npm test --prefix web` |
| T21 | `pulse_notification_renders_pulse_badge` + `normal_assistant_message_has_no_pulse_badge` | Step 21 | `npm test --prefix web` |
| T22 | `composer_handles_enter_shift_enter_and_suggest` | Step 22 | `npm test --prefix web` |
| T23 | `readonly_session_shows_banner_instead_of_composer` | Step 23 | `npm test --prefix web` |

### Frontend - Command Palette（全3件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T24 | `command_palette_opens_and_closes_with_keyboard` | Step 24 | `npm test --prefix web` |
| T25 | `command_palette_renders_all_sections` | Step 25 | `npm test --prefix web` |
| T26 | `palette_recent_reads_from_localstorage` | Step 26 | `npm test --prefix web` |

### Frontend - トランスポート・状態（全3件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T27 | `ws_handler_processes_chat_events_and_send_via_chat_send` | Step 27 | `npm test --prefix web` || T28 | `server_state_caches_and_invalidates` | Step 28 | `npm test --prefix web` |
| T29 | `chat_send_invalidates_sessions_and_history` | Step 29 | `npm test --prefix web` |

### Frontend - Chat UX 改善（全4件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T30 | `sidebar_collapses_to_icon_only_bar` | Step 30 | `npm test --prefix web` |
| T31 | `timeline_search_highlights_and_navigates_matches` | Step 31 | `npm test --prefix web` |
| T32 | `code_block_folds_when_longer_than_threshold` | Step 32 | `npm test --prefix web` |
| T33 | `composer_draft_persists_across_reload` | Step 33 | `npm test --prefix web` |

### E2E（Playwright MCP、Step 36）

TDD 管理外の検証シナリオ。失敗時は該当 Step へ戻る。

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | ~5分 |
| Step 1-6 | Backend TDD Cycle 6件（API 拡張・WS・delta・chat.send） | ~2日 |
| Step 7-9 | Design system TDD Cycle 3件 | ~1日 |
| Step 10-14 | Layout TDD Cycle 5件 | ~2日 |
| Step 15-23 | Chat Tab TDD Cycle 9件 | ~3日 |
| Step 24-26 | Command Palette TDD Cycle 3件 | ~1.5日 |
| Step 27-29 | Transport/State TDD Cycle 3件（WS handler・cache・invalidation） | ~1.5日 |
| Step 30-33 | Chat UX 改善 TDD Cycle 4件（Sidebar collapse・search・code fold・draft） | ~2日 |
| Step 34 | UT 動作確認 | ~0.5日 |
| Step 35 | Plan・仕様書との自己チェック | ~0.5日 |
| Step 36 | E2E (Playwright) | ~1日 |
| Step 37-39 | PR 作成・レビューバック | ~1日（待機時間除く） |
| **合計** | | **~16日（実働）** |
