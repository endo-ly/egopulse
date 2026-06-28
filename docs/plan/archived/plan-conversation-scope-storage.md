# Plan: ConversationScope による secret 保存境界の整理

`secret: true` の外部設定と既存の secret mode の振る舞いを維持したまま、内部実装を `is_secret: bool` の分散判定から `ConversationScope` と scoped storage 解決へ寄せる。DB だけでなく compaction archive などの永続化境界を同じ概念で扱えるようにし、今後の media / Web / Sleep / Pulse 対応時に secret 固有知識が各処理へ漏れない構造にする。

> **Note**: Howはあくまで参考であり、よりよい設計方針があれば各自で判断し採用する。振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- 外部仕様は維持する。Config YAML は引き続き Discord / Telegram の `secret: true` を利用者向けの具体語彙として残し、内部で `ConversationScope::Secret` へ変換する。
- 内部の責務境界は「secret かどうか」ではなく「この会話が属する保存境界」を中心にする。`SurfaceContext` / `ToolExecutionContext` は bool ではなく scope を持ち、保存先解決は `AppState` に集約する。
- 最初の到達点は `ConversationScope::{Normal, Secret}` と `state.db_for(scope)` / `state.storage_for(scope)` 相当の導入に留める。`IsolationScope` や任意の scope registry など、3種類目の scope が必要になるまで過剰な一般化はしない。
- `secret.db`, `secret_groups`, backup など既存の secret mode の保存先は変えない。DB ファイル名や YAML 名を `isolated` へ変更しない。
- 後方互換分岐は禁止する。`is_secret` の alias、`db_for(bool)` の旧API、`scope` と bool の二重保持、旧仕様フォールバックは追加しない。呼び出し側は新しい `ConversationScope` / scoped storage API へ一直線に置換する。
- 既存の `secret_turn_leaves_egopulse_db_untouched` などの隔離テストを守りつつ、利用側が `is_secret` を直接見ないことを統合テストと検索で確認する。
- production code の `is_secret` フィールド / 引数 / tracing field は削除し、観測情報も `scope = "normal" | "secret"` に統一する。`secret` という語は外部設定名、DB名、`ConversationScope::Secret` variant、ユーザー向け説明に限定する。
- 関連参照元: `docs/plan/plan-secret-mode-phase1.md`, `docs/plan/secret-mode-design.md`, `docs/architecture.md`, `docs/config.md`, `docs/db.md`, `docs/security.md`, `src/runtime/mod.rs`, `src/agent_loop/*`, `src/channels/discord.rs`, `src/channels/telegram.rs`, `src/tools/*`。

## TDD 方針

まずテストリストで「外部仕様が変わらないこと」と「内部の保存境界解決が scope に集約されること」を分けて不安を書き出す。各 Red では自動テストを 1 件だけ追加し、Green ではそのテストを通す最小変更に集中し、Refactor で命名と責務境界を整える。1つのテストリスト項目に複数の確認が必要な場合は Cycle を分ける。実装中に新しい漏れ経路や不安が見つかった場合は、即実装に混ぜずテストリストへ追加して次の Cycle で扱う。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/agent_loop/mod.rs` または新規 `src/agent_loop/scope.rs` | 変更 / 新規 | `SurfaceContext` 定義 | `ConversationScope` の置き場所は依存方向を見て決める |
| `src/runtime/mod.rs` | 変更 | `AppState::db_for`, `build_app_state_dependencies` | scope から DB / storage を解決する責務を集約 |
| `src/agent_loop/session.rs` | 変更 | `resolve_chat_id`, `load_messages_for_turn`, `persist_phase*` | bool 引数を scope に置換 |
| `src/agent_loop/turn.rs` | 変更 | `process_turn`, `ToolExecutionContext` 構築 | turn 全体に scope を伝播 |
| `src/agent_loop/prompt_builder.rs` | 変更 | `build_secret_prompt_section` | `SECRET.md` 注入条件を scope 化 |
| `src/agent_loop/compaction.rs` | 変更 | archive path / LLM usage logging | `secret_groups` 分岐を scoped storage へ寄せる |
| `src/agent_loop/tool_phase.rs` | 変更 | `ToolPhaseRequest`, `log_llm_usage` | usage logging の DB 解決を scope 化 |
| `src/channels/discord.rs` | 変更 | `make_context`, channel log | YAML `secret: true` を `ConversationScope::Secret` に変換 |
| `src/channels/telegram.rs` | 変更 | `make_context`, channel log | Discord と同じ変換規則 |
| `src/slash_commands.rs` | 変更 | `/new`, `/compact`, `/status` | slash command DB ルーティングを scope 化 |
| `src/tools/mod.rs` | 変更 | `ToolExecutionContext` | bool フィールドを scope へ置換 |
| `src/tools/agent_send.rs` | 変更 | `AgentSendTool::db_for`, scheduled turn | target turn へ scope を引き継ぐ |
| `src/tools/send_message.rs` | 変更 | `SendMessageTool::db_for` | chat info lookup を scope 化 |
| `src/pulse/output.rs`, `src/pulse/runner.rs` | 変更 | 通常保存固定の明示 | Pulse は `ConversationScope::Normal` を明示 |
| `src/config/*` | 変更 | `secret` parse / persist tests | YAML は維持し、必要なら scope 変換 helper を追加 |
| `docs/architecture.md`, `docs/config.md`, `docs/db.md`, `docs/security.md`, `docs/session-lifecycle.md`, `docs/system-prompt.md` | 変更 | secret mode docs | 外部仕様維持と内部 scope 設計を反映 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | `secret: true` の Discord / Telegram 設定から作られる `SurfaceContext` が `ConversationScope::Secret` を持つ | High | Step 1 | 未着手 |
| T2 | 正常系 | `secret` 未指定または `false` の context は `ConversationScope::Normal` を持つ | High | Step 2 | 未着手 |
| T3 | 正常系 | `AppState` は scope から通常 DB / secret DB を返し、既存の DB ルーティング契約を維持する | High | Step 3 | 未着手 |
| T4 | 統合 | secret turn 実行後も `egopulse.db` の turn-writable tables は空で、`secret.db` に会話が保存される | High | Step 4 | 未着手 |
| T5 | 統合 | secret compaction archive は `runtime/secret_groups` に保存され、通常 archive は `runtime/groups` に保存される | High | Step 5 | 未着手 |
| T6 | 正常系 | `agent_send` / `send_message` / slash command / LLM usage logging が context scope で DB を解決する | High | Step 6 | 未着手 |
| T7 | 統合 | scheduled turn の stop condition / bot response channel log が secret scope では secret DB に保存される | High | Step 7 | 未着手 |
| T8 | 正常系 | `ConversationScope::Secret` の context だけ `SECRET.md` を system prompt に注入し、Normal では注入しない | High | Step 8 | 未着手 |
| T9 | 空・ゼロ状態 | secret DB が不要な設定では通常 scope の処理が secret DB を要求せず起動・実行できる | High | Step 9 | 未着手 |
| T10 | 異常系 | secret scope なのに secret DB が初期化されていない場合は既存同等の明確な失敗になる | Medium | Step 10 | 未着手 |
| T11 | 継承可能性 | docs は YAML `secret: true` と内部 `ConversationScope` の役割差を説明する | Medium | Step 11 | 未着手 |
| T12 | 設計回帰 | production code に `is_secret` bool フィールド / bool 引数 / tracing field が残らず、観測情報は `scope` field になる | Medium | Step 12 | 未着手 |
| T13 | 対象外 | Inbound media の保存先を scope 化する | Low | 今回対象外 | 現時点では inbound 画像音声の通常 workspace 保存を許容するため |
| T14 | 対象外 | DB 名 / YAML 名を `isolated` 系へ変更する | Low | 今回対象外 | 利用者向け語彙は `secret: true` を維持する判断のため |

---

## Step 0: Worktree 作成

- ブランチ名: `refactor/conversation-scope-storage`
- 作成コマンド:
  - `git fetch origin main`
  - `git worktree add ../wt-conversation-scope-storage -b refactor/conversation-scope-storage origin/main`
- 注意:
  - PR #109 merge 後の `origin/main` を基点にする。
  - 既存の未コミット差分がある main worktree では作業しない。

---

## Step 1: Channel Context TDD Cycle - secret 設定を scope へ変換する

### この Step の目的

Discord / Telegram の `secret: true` が、外部設定名を変えずに内部の `ConversationScope::Secret` として表現される状態にする。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: scope 導入の入口であり、以降の保存ルーティングの前提になるため
- この時点では扱わないこと: DB 解決、compaction、tools、docs

### RED: 失敗する自動テストを書く

- 追加するテスト名: `make_context_sets_secret_scope_for_secret_channel`
- Given: Discord / Telegram の channel config に `secret: true` がある
- When: `make_context` で context を構築する
- Then: context の scope が `ConversationScope::Secret` である
- 失敗理由の想定: `SurfaceContext` がまだ bool または scope 未実装である

### GREEN: 最小実装

`ConversationScope` を導入し、`SurfaceContext` に `scope` を持たせる。`is_secret` alias や bool 互換 helper は追加せず、呼び出し側を scope へ置換する。

### REFACTOR: 設計の整理

- 重複: Discord / Telegram の secret-to-scope 変換が過剰に重複していないか
- 命名: `ConversationScope` が「会話の保存境界」を表せているか
- 責務: Config parse と context 構築の境界が崩れていないか
- テストの構造的結合: private 実装ではなく context の外部観測可能な状態を見ているか
- 次の項目へ進める身軽さ: 通常 scope のテストを追加できる状態か

### テストリスト更新

- 完了: `T1`
- 追加: Green/Refactor 中に見つかった不安。なければ「なし」
- 次候補: `T2`

### コミット

`refactor: introduce conversation scope for channel contexts`

---

## Step 2: Channel Context TDD Cycle - 通常 channel は Normal scope になる

### この Step の目的

`secret` 未指定または `false` の既存 channel が、外部振る舞いを変えず `ConversationScope::Normal` になることを固定する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: scope 導入で通常チャネルを壊さないことを早期に固定するため
- この時点では扱わないこと: secret DB 初期化条件、tools

### RED: 失敗する自動テストを書く

- 追加するテスト名: `make_context_defaults_to_normal_scope`
- Given: `secret` 未指定 / `false` の Discord / Telegram channel config
- When: `make_context` で context を構築する
- Then: context の scope が `ConversationScope::Normal` である
- 失敗理由の想定: default 値が scope へ移行されていない

### GREEN: 最小実装

`SurfaceContext::new` の default を `ConversationScope::Normal` にし、既存テストの context fixture も Normal を明示または default 利用へ寄せる。

### REFACTOR: 設計の整理

- 重複: test fixture の scope 指定が冗長になっていないか
- 命名: default の意味が `Normal` として読み取れるか
- 責務: 通常チャネルが secret DB を知らない構造か
- テストの構造的結合: config の内部表現に依存しすぎていないか
- 次の項目へ進める身軽さ: `AppState` の DB 解決へ進める状態か

### テストリスト更新

- 完了: `T2`
- 追加: なし
- 次候補: `T3`

### コミット

`refactor: default surface contexts to normal scope`

---

## Step 3: Runtime Storage TDD Cycle - scope から DB を解決する

### この Step の目的

`AppState` の DB ルーターを bool ではなく `ConversationScope` を受ける形へ移行し、通常 / secret の DB 解決契約を維持する。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: 保存経路の中核であり、以降の session / tools 移行の土台になるため
- この時点では扱わないこと: archive root, media root, docs

### RED: 失敗する自動テストを書く

- 追加するテスト名: `db_for_returns_database_for_conversation_scope`
- Given: 通常 DB と secret DB を持つ `AppState`
- When: `db_for(ConversationScope::Normal)` と `db_for(ConversationScope::Secret)` を呼ぶ
- Then: それぞれ通常 DB / secret DB を返す
- 失敗理由の想定: `db_for` が bool のまま

### GREEN: 最小実装

`AppState::db_for(scope: ConversationScope)` へ変更する。必要なら `ConversationScope::requires_secret_db()` のような小さな helper を追加し、`config.needs_secret_db()` の意味とは混ぜない。

### REFACTOR: 設計の整理

- 重複: runtime と tool の個別 `db_for` が同じ分岐を持たないようにする準備があるか
- 命名: `db_for` が scope を受けることが明確か
- 責務: DB 選択が `AppState` に集約されているか
- テストの構造的結合: Arc pointer equality など既存パターンに留まっているか
- 次の項目へ進める身軽さ: session 永続化に scope を渡せるか

### テストリスト更新

- 完了: `T3`
- 追加: secret DB 未初期化の失敗確認が不足していれば `T10` を維持
- 次候補: `T4`

### コミット

`refactor: route databases by conversation scope`

---

## Step 4: Agent Turn TDD Cycle - secret turn の DB 隔離を維持する

### この Step の目的

session / turn / tool call 永続化の bool 引数を scope に置換しつつ、secret turn が通常 DB を汚さない既存契約を維持する。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: リファクタ後も最重要の安全性契約を守るため
- この時点では扱わないこと: compaction archive, docs, inbound media

### RED: 失敗する自動テストを書く

- 追加するテスト名: `secret_turn_routes_by_conversation_scope`
- Given: secret DB を持つ state と `ConversationScope::Secret` の context
- When: `process_turn` を実行する
- Then: `egopulse.db` の `chats/messages/sessions/tool_calls/llm_usage_logs` は空で、`secret.db` の `chats/messages/sessions` に保存される
- 失敗理由の想定: session / turn の一部が bool 依存または通常 DB 参照のまま

### GREEN: 最小実装

`load_messages_for_turn`, `persist_phase_once`, `persist_phase`, `persist_phase_messages`, tool call 永続化 skip 判定などを scope 引数へ置換する。`ConversationScope::Secret` の場合は既存通り `tool_calls` テーブル保存を skip する。

### REFACTOR: 設計の整理

- 重複: `scope == Secret` の分岐が低レベルに散らばっていないか
- 命名: `scope` と `context.scope` の呼び分けが自然か
- 責務: session 層が secret の意味を知りすぎていないか
- テストの構造的結合: DB table count は隔離契約として妥当か
- 次の項目へ進める身軽さ: compaction へ scope を渡せるか

### テストリスト更新

- 完了: `T4`
- 追加: tool_calls skip の scope helper が必要なら `T6` に統合
- 次候補: `T5`

### コミット

`refactor: persist agent turns through conversation scope`

---

## Step 5: Scoped Storage TDD Cycle - archive root を scope で解決する

### この Step の目的

DB 以外の永続化境界も scope で扱えるようにし、compaction archive の `groups` / `secret_groups` 分岐を利用側から隠す。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: `db_for` だけでは今回の設計改善の価値が半分に留まるため
- この時点では扱わないこと: inbound media の保存先変更

### RED: 失敗する自動テストを書く

- 追加するテスト名: `storage_for_returns_archive_root_for_conversation_scope`
- Given: state root を持つ state
- When: `storage_for(ConversationScope::Normal)` / `storage_for(ConversationScope::Secret)` を呼ぶ
- Then: archive root がそれぞれ `runtime/groups` / `runtime/secret_groups` を指す
- 失敗理由の想定: scoped storage が未実装、または compaction 側に分岐が残っている

### GREEN: 最小実装

`ScopedConversationStorage` などの小さな構造体を追加し、少なくとも `db` と `archive_root` を返す。compaction は `context.scope` から `state.storage_for(scope).archive_root` を使う。

### REFACTOR: 設計の整理

- 重複: `runtime/secret_groups` 文字列が複数箇所に残っていないか
- 命名: `storage_for` が DB 以外の保存境界も表しているか
- 責務: `Config` は path の原材料、`AppState` は scope 解決という分担になっているか
- テストの構造的結合: path 文字列ではなく config contract として検証できているか
- 次の項目へ進める身軽さ: tools / slash command の移行へ進めるか

### テストリスト更新

- 完了: `T5`
- 追加: archive root 以外の保存境界が見つかれば新規 T を追加
- 次候補: `T6`

### コミット

`refactor: resolve conversation storage by scope`

---

## Step 6: Tools and Commands TDD Cycle - scope を各実行コンテキストへ伝播する

### この Step の目的

slash command, `agent_send`, `send_message`, LLM usage logging が bool ではなく scope で DB を解決し、secret turn の派生処理も同じ保存境界を使う。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: secret turn から呼ばれる周辺処理の漏れを防ぐため
- この時点では扱わないこと: docs, production code の検索チェック

### RED: 失敗する自動テストを書く

- 追加するテスト名: `agent_send_preserves_conversation_scope`
- Given: `ConversationScope::Secret` の `ToolExecutionContext`
- When: `agent_send` が scheduled turn を作る
- Then: scheduled turn の context scope が `Secret` のまま引き継がれる
- 失敗理由の想定: `ToolExecutionContext` が bool のまま、または target context へ scope 未伝播

### GREEN: 最小実装

`ToolExecutionContext`, `ToolPhaseRequest`, slash command 呼び出し、`AgentSendTool`, `SendMessageTool` を scope に置換する。必要に応じて既存の slash command secret routing テストを scope 名へ更新する。

### REFACTOR: 設計の整理

- 重複: tools が独自に secret DB Option を持って分岐していないか。可能なら `AppState` / shared resolver を使う方向へ寄せる
- 命名: `context.scope` が tool 実行中の保存境界として理解できるか
- 責務: tool は「どのDBか」ではなく「このscopeのchat info」を要求しているか
- テストの構造的結合: scheduled turn の公開的な context を検証しているか
- 次の項目へ進める身軽さ: zero-state と failure path を確認できるか

### テストリスト更新

- 完了: `T6`
- 追加: `send_message` lookup の secret routing が不安なら同一項目の追加 Cycle を作る
- 次候補: `T7`

### コミット

`refactor: propagate conversation scope through tools and commands`

---

## Step 7: Scheduled Turn TDD Cycle - 後続ログ保存を scope で隔離する

### この Step の目的

scheduled turn の stop condition system event と bot response channel log が、secret scope では secret DB に保存されることを固定する。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: `agent_send` が scope を引き継いでも、scheduled turn 実行側が通常 DB を参照すると隔離が破綻するため
- この時点では扱わないこと: channel adapter の外部送信、turn scheduler の停止条件そのものの変更

### RED: 失敗する自動テストを書く

- 追加するテスト名: `scheduled_turn_logs_route_by_conversation_scope`
- Given: `ConversationScope::Secret` の scheduled turn と channel log chat
- When: stop condition system event または bot response channel log を保存する経路を実行する
- Then: 通常 DB には保存されず、secret DB に保存される
- 失敗理由の想定: `execute_scheduled_turn` が `turn.context.is_secret` または通常 DB 参照のまま

### GREEN: 最小実装

`execute_scheduled_turn` 内の `store_system_event` と `store_channel_log_bot_response` を `turn.context.scope` / `state.storage_for(scope).db` 経由へ置換する。テストが重くなる場合は、保存処理を小さな helper に切り出して公開挙動を保ったまま検証する。

### REFACTOR: 設計の整理

- 重複: scheduled turn 内で DB 解決分岐が複数回露出していないか
- 命名: helper を切る場合、stop condition と bot response の保存意図が明確か
- 責務: turn scheduler は scope の意味を知らず、保存先解決だけを委譲しているか
- テストの構造的結合: scheduler 内部状態ではなく DB 副作用を見ているか
- 次の項目へ進める身軽さ: prompt builder の scope 化へ進めるか

### テストリスト更新

- 完了: `T7`
- 追加: scheduled turn の failure path が別途必要なら同一項目の追加 Cycle を作る
- 次候補: `T8`

### コミット

`refactor: route scheduled turn logs by conversation scope`

---

## Step 8: Prompt Builder TDD Cycle - SECRET.md 注入条件を scope 化する

### この Step の目的

`SECRET.md` の system prompt 注入条件を `context.is_secret` から `ConversationScope::Secret` へ移行し、Normal scope では注入されない既存契約を維持する。

### 今回選ぶ項目

- 対象: `T8`
- 選ぶ理由: `SurfaceContext` の bool 削除時にビルド破壊や旧 bool 残存を起こしやすい経路であるため
- この時点では扱わないこと: `SECRET.md` の読み込み仕様変更、prompt の配置順変更

### RED: 失敗する自動テストを書く

- 追加するテスト名: `system_prompt_includes_secret_md_for_secret_scope_only`
- Given: `agents/default/SECRET.md` が存在する state
- When: Secret scope / Normal scope それぞれで `build_system_prompt` を呼ぶ
- Then: Secret scope だけ `<secret>` block を含み、Normal scope では含まない
- 失敗理由の想定: prompt builder が `context.is_secret` に依存している

### GREEN: 最小実装

`build_secret_prompt_section` の条件を `context.scope == ConversationScope::Secret` へ置換する。既存の secret prompt tests は scope 用語へ更新し、AGENTS と Memory の間に入る順序は維持する。

### REFACTOR: 設計の整理

- 重複: secret 判定 helper が prompt builder 独自に増えていないか
- 命名: prompt 上の `<secret>` block と内部 `ConversationScope::Secret` の関係が読みやすいか
- 責務: prompt builder は保存先詳細を知らず、scope の意味だけを見る形か
- テストの構造的結合: prompt 全体一致ではなく必要な block と順序を検証しているか
- 次の項目へ進める身軽さ: zero-state runtime へ進めるか

### テストリスト更新

- 完了: `T8`
- 追加: なし
- 次候補: `T9`

### コミット

`refactor: gate secret prompt by conversation scope`

---

## Step 9: Runtime TDD Cycle - secret DB 不要時の通常処理を維持する

### この Step の目的

secret channel がない設定では secret DB を初期化しない既存の軽量性を維持し、通常 scope の処理が secret DB を要求しないことを確認する。

### 今回選ぶ項目

- 対象: `T9`
- 選ぶ理由: scope 化で secret DB を常時必要にする退行を防ぐため
- この時点では扱わないこと: secret scope の未初期化失敗

### RED: 失敗する自動テストを書く

- 追加するテスト名: `normal_scope_does_not_require_secret_db`
- Given: secret channel がない config で作った state
- When: `storage_for(ConversationScope::Normal)` または通常 turn を実行する
- Then: secret DB が `None` でも成功する
- 失敗理由の想定: scoped storage 初期化が secret DB を常時 unwrap している

### GREEN: 最小実装

Normal scope の storage 解決では通常 DB と通常 archive root のみを使う。`secret_db` は `ConversationScope::Secret` のときだけ要求する。

### REFACTOR: 設計の整理

- 重複: Normal / Secret の path 構築が読みやすいか
- 命名: `requires_secret_db` などの helper が必要十分か
- 責務: config の `needs_secret_db` と runtime の `storage_for` が混ざっていないか
- テストの構造的結合: secret_db の内部 Option に依存しすぎていないか
- 次の項目へ進める身軽さ: 異常系へ進めるか

### テストリスト更新

- 完了: `T9`
- 追加: なし
- 次候補: `T10`

### コミット

`test: cover normal scope without secret database`

---

## Step 10: Runtime TDD Cycle - secret DB 未初期化時の失敗を明確にする

### この Step の目的

secret scope が要求されたのに secret DB がない場合、既存同等またはより明確なエラー / panic message で壊れることを固定する。

### 今回選ぶ項目

- 対象: `T10`
- 選ぶ理由: 設定不整合時の失敗が通常 DB へのフォールバックにならないことを保証するため
- この時点では扱わないこと: 起動時 validation の追加。必要と判断した場合は別 TDD Cycle を追加する

### RED: 失敗する自動テストを書く

- 追加するテスト名: `secret_scope_requires_secret_database`
- Given: `secret_db: None` の state
- When: `db_for(ConversationScope::Secret)` または `storage_for(ConversationScope::Secret)` を呼ぶ
- Then: 明確な message で失敗する
- 失敗理由の想定: bool 由来の panic message のまま、または通常 DB へフォールバックしてしまう

### GREEN: 最小実装

Secret scope の DB 解決では通常 DB へフォールバックしない。panic を維持する場合も message を scope 用語へ更新する。Result 化がより自然なら、その変更範囲と呼び出し側のエラー処理を最小に保つ。

### REFACTOR: 設計の整理

- 重複: secret DB unwrap の message が複数箇所に散らばらないか
- 命名: 失敗 message が運用時に理解しやすいか
- 責務: 起動 validation と runtime invariant の境界が明確か
- テストの構造的結合: panic message への依存が過度でないか
- 次の項目へ進める身軽さ: docs 更新へ進めるか

### テストリスト更新

- 完了: `T10`
- 追加: 起動時 validation が必要なら新規 Medium 項目を追加
- 次候補: `T11`

### コミット

`refactor: clarify secret scope database invariant`

---

## Step 11: Docs TDD Cycle - 外部 secret と内部 scope の説明を更新する

### この Step の目的

利用者向けには `secret: true` を維持し、開発者向けには内部の `ConversationScope` と scoped storage の責務境界を説明する。

### 今回選ぶ項目

- 対象: `T11`
- 選ぶ理由: 設計判断を暗黙知にせず、今後の変更者が同じ境界で拡張できるようにするため
- この時点では扱わないこと: DB 名 / YAML 名の変更

### RED: 失敗する自動テストを書く

- 追加するテスト名: `docs_mention_secret_yaml_and_conversation_scope`
- Given: 関連 docs
- When: docs の内容を検索する
- Then: `secret: true` の設定例と `ConversationScope` / scoped storage の内部設計説明が存在する
- 失敗理由の想定: docs が PR #109 の secret DB 説明のまま

### GREEN: 最小実装

`docs/architecture.md`, `docs/config.md`, `docs/db.md`, `docs/security.md`, `docs/session-lifecycle.md` を必要最小限更新する。`secret.db` と `secret: true` の外部仕様は維持する。

### REFACTOR: 設計の整理

- 重複: 複数 docs で同じ説明を長く繰り返していないか
- 命名: 利用者語彙と内部語彙の違いが明確か
- 責務: config docs は設定、architecture docs は内部境界、security docs は隔離保証に集中しているか
- テストの構造的結合: docs smoke test が文言に過度依存していないか
- 次の項目へ進める身軽さ: 検索チェックへ進めるか

### テストリスト更新

- 完了: `T11`
- 追加: なし
- 次候補: `T12`

### コミット

`docs: document conversation scope storage boundary`

---

## Step 12: Design Regression TDD Cycle - bool 分岐の残存を確認する

### この Step の目的

production code の保存境界判定と観測 field が `is_secret: bool` に戻らないことを確認し、リファクタの目的を満たしているかを機械的にチェックする。

### 今回選ぶ項目

- 対象: `T12`
- 選ぶ理由: この Plan の主目的である「Secret の事情を各処理に漏らさない」を最後に確認するため
- この時点では扱わないこと: テストコード内の fixture 名や PR #109 docs 内の履歴文言

### RED: 失敗する自動テストを書く

- 追加するテスト名: `production_code_does_not_use_is_secret_storage_flag`
- Given: `src/` の production code
- When: `is_secret` フィールド / 引数 / tracing field / `db_for(true/false)` 相当の残存を検索する
- Then: production code には残っておらず、agent turn span などの観測情報は `scope` field で記録される
- 失敗理由の想定: 一部モジュールが bool のまま

### GREEN: 最小実装

残存箇所を scope へ置換する。`agent_turn` span は `is_secret` ではなく `scope = %context.scope` のような field に更新し、対応する span capture test も scope 文字列を検証する。

### REFACTOR: 設計の整理

- 重複: scope 判定が低レベルに散らばっていないか
- 命名: `secret` は外部設定・DB名・明示的な Secret variant・ユーザー向け説明に限定され、production code の bool 名として残っていないか
- 責務: `storage_for(scope)` で解ける処理が直接 path / DB を分岐していないか
- テストの構造的結合: 検索ベースのチェックを過信していないか
- 次の項目へ進める身軽さ: 全体動作確認へ進めるか

### テストリスト更新

- 完了: `T12`
- 追加: 見つかった漏れがあれば新規 T を追加
- 次候補: 動作確認

### コミット

`refactor: remove secret bool routing from production code`

---

## Step 13: 動作確認

- `cargo fmt --check`
- `cargo test secret --all-targets`
- `cargo test conversation_scope --all-targets`（追加テスト名に合わせて調整）
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `rg -n "is_secret|db_for\\((true|false)|secret_db required" src --glob '!**/tests/**'`
  - 期待: production code に `is_secret` と保存境界の bool ルーティングが残っていない。
- `rg -n "is_secret" src/agent_loop src/runtime src/tools src/channels src/slash_commands.rs src/pulse --glob '!**/tests/**'`
  - 期待: 0件。tracing / metrics / logs も `scope` field へ移行済みである。
- 失敗時に戻る Step:
  - context / channel 由来なら Step 1-2
  - DB routing 由来なら Step 3-4
  - archive / scoped storage 由来なら Step 5
  - tools / slash command 由来なら Step 6
  - scheduled turn log 由来なら Step 7
  - SECRET.md prompt 由来なら Step 8
  - docs 由来なら Step 11

---

## Step 14: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリストと各 Cycle が完了条件を満たしている
- `secret: true` の外部仕様、`secret.db`、`secret_groups` の既存 What が維持されている
- `ConversationScope` / scoped storage の導入が、DB 以外の永続化境界にも効く形になっている
- inbound media 保存先を変更していないことが意図どおりである
- 実装中に変更した設計判断が関連 docs へ反映されている
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している

---

## Step 15: PR 作成

- PR タイトル: `refactor: route secret storage through conversation scope`
- PR description:
  - 概要
    - `secret: true` の外部仕様は維持
    - 内部の `is_secret` routing を `ConversationScope` / scoped storage へ置換
    - DB と compaction archive の保存境界解決を `AppState` に集約
  - テスト
    - `cargo fmt --check`
    - `cargo test secret --all-targets`
    - `cargo test`
    - `cargo check`
    - `cargo clippy --all-targets --all-features -- -D warnings`
    - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  - Close #...（該当 Issue がある場合のみ記載）

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/agent_loop/mod.rs` または `src/agent_loop/scope.rs` | 変更 / 新規 | `ConversationScope` 定義、`SurfaceContext` の scope 化 |
| `src/runtime/mod.rs` | 変更 | `db_for(scope)` / `storage_for(scope)` 導入、secret DB invariant 更新 |
| `src/agent_loop/session.rs` | 変更 | session load / persist API の scope 化 |
| `src/agent_loop/turn.rs` | 変更 | turn processing と tool call 永続化 skip 判定の scope 化 |
| `src/agent_loop/prompt_builder.rs` | 変更 | `SECRET.md` 注入条件を scope 化 |
| `src/agent_loop/compaction.rs` | 変更 | archive root / usage logging の scoped storage 化 |
| `src/agent_loop/tool_phase.rs` | 変更 | `ToolPhaseRequest` と LLM usage logging の scope 化 |
| `src/channels/discord.rs` | 変更 | `secret: true` から `ConversationScope::Secret` への変換 |
| `src/channels/telegram.rs` | 変更 | `secret: true` から `ConversationScope::Secret` への変換 |
| `src/slash_commands.rs` | 変更 | slash command DB access の scope 化 |
| `src/tools/mod.rs` | 変更 | `ToolExecutionContext` の scope 化 |
| `src/tools/agent_send.rs` | 変更 | agent_send message 保存 / scheduled turn scope 引き継ぎ |
| `src/tools/send_message.rs` | 変更 | send_message chat lookup の scope 化 |
| `src/pulse/output.rs` | 変更 | Pulse persistence は Normal scope 固定と明示 |
| `src/pulse/runner.rs` | 変更 | Pulse tool context は Normal scope 固定と明示 |
| `src/config/resolve.rs` | 変更 | 必要なら config から scope を解く helper を追加 |
| `src/config/tests.rs` | 変更 | `secret: true` の外部設定維持テスト更新 |
| `docs/architecture.md` | 変更 | `ConversationScope` / scoped storage の責務説明 |
| `docs/config.md` | 変更 | YAML は `secret: true` 維持と明記 |
| `docs/db.md` | 変更 | `secret.db` は `ConversationScope::Secret` の DB と説明 |
| `docs/security.md` | 変更 | 隔離境界の保証範囲を scope 用語で整理 |
| `docs/session-lifecycle.md` | 変更 | turn/session persistence の scope flow を更新 |
| `docs/system-prompt.md` | 変更 | `SECRET.md` 注入条件を scope 用語で補足 |

---

## コミット分割

1. `refactor: introduce conversation scope for channel contexts` - `ConversationScope`, `SurfaceContext`, Discord / Telegram context tests
2. `refactor: route databases by conversation scope` - `AppState::db_for(scope)`, session / turn DB routing foundation
3. `refactor: persist agent turns through conversation scope` - agent turn persistence, tool call skip, LLM usage routing
4. `refactor: resolve conversation storage by scope` - `storage_for(scope)`, compaction archive root
5. `refactor: propagate conversation scope through tools and commands` - slash commands, `agent_send`, `send_message`, Pulse Normal scope
6. `refactor: route scheduled turn logs by conversation scope` - stop condition / bot response channel log routing
7. `refactor: gate secret prompt by conversation scope` - `SECRET.md` prompt condition
8. `docs: document conversation scope storage boundary` - architecture / config / DB / security / session docs
9. `test: cover conversation scope regression checks` - zero-state / invariant / remaining regression tests if not folded into earlier commits

---

## 自動テスト一覧（全 12 件）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストとTDD Cycleを追加して対応する。

### Channel Context（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `make_context_sets_secret_scope_for_secret_channel` | Step 1 | `cargo test make_context_sets_secret_scope_for_secret_channel --all-targets` |
| T2 | `make_context_defaults_to_normal_scope` | Step 2 | `cargo test make_context_defaults_to_normal_scope --all-targets` |

### Runtime / Storage（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T3 | `db_for_returns_database_for_conversation_scope` | Step 3 | `cargo test db_for_returns_database_for_conversation_scope --all-targets` |
| T5 | `storage_for_returns_archive_root_for_conversation_scope` | Step 5 | `cargo test storage_for_returns_archive_root_for_conversation_scope --all-targets` |
| T9 | `normal_scope_does_not_require_secret_db` | Step 9 | `cargo test normal_scope_does_not_require_secret_db --all-targets` |
| T10 | `secret_scope_requires_secret_database` | Step 10 | `cargo test secret_scope_requires_secret_database --all-targets` |

### Agent Turn / Tools / Prompt（全 5 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T4 | `secret_turn_routes_by_conversation_scope` | Step 4 | `cargo test secret_turn_routes_by_conversation_scope --all-targets` |
| T6 | `agent_send_preserves_conversation_scope` | Step 6 | `cargo test agent_send_preserves_conversation_scope --all-targets` |
| T7 | `scheduled_turn_logs_route_by_conversation_scope` | Step 7 | `cargo test scheduled_turn_logs_route_by_conversation_scope --all-targets` |
| T8 | `system_prompt_includes_secret_md_for_secret_scope_only` | Step 8 | `cargo test system_prompt_includes_secret_md_for_secret_scope_only --all-targets` |
| T12 | `production_code_does_not_use_is_secret_storage_flag` | Step 12 | `cargo test production_code_does_not_use_is_secret_storage_flag --all-targets` |

### Docs（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T11 | `docs_mention_secret_yaml_and_conversation_scope` | Step 11 | `cargo test docs_mention_secret_yaml_and_conversation_scope --all-targets` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | ~5 行 |
| Step 1 | `ConversationScope` 導入と secret context 化 | ~80 行 |
| Step 2 | Normal default と fixture 整理 | ~60 行 |
| Step 3 | `AppState::db_for(scope)` 化 | ~80 行 |
| Step 4 | session / turn persistence scope 化 | ~180 行 |
| Step 5 | `storage_for(scope)` と archive root scope 化 | ~120 行 |
| Step 6 | tools / slash command / Pulse scope 化 | ~200 行 |
| Step 7 | scheduled turn log routing scope 化 | ~90 行 |
| Step 8 | SECRET.md prompt condition scope 化 | ~70 行 |
| Step 9 | secret DB 不要時の通常処理テスト | ~50 行 |
| Step 10 | secret DB invariant 整理 | ~40 行 |
| Step 11 | docs 更新 | ~200 行 |
| Step 12 | bool routing 残存チェック / 仕上げ | ~50 行 |
| Step 13-15 | 動作確認 / 自己チェック / PR 作成 | ~30 行 |
| **合計** |  | **~1,255 行** |
