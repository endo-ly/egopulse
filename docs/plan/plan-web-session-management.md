# Web セッション管理

Web UI をメインの会話面として使うため、セッションに人間が読める名前を与え、一覧を整理できるようにする。

## 1. 概要

現状の Web セッションは UI が生成する `session-YYYYMMDDHHMMSS` 形式の使い捨てキーで識別され、一覧は `external_chat_id` の生文字列と最終発言プレビューだけを並べる。会話を重ねるほど「どのセッションが何の会話か」が分からず、Web をメインにしても整理・再開ができない。

本計画は、Discord のチャンネルと同じ構造でセッションを扱えるようにする。

| 概念 | Discord | 本計画 |
|---|---|---|
| 住所（不変） | channel ID | `chat_id`（既存。canonical key は `chat:{id}`） |
| 表示名（可変・自由） | channel name | `chat_title`（既存カラム。ユーザー設定専用にする） |
| 整理 | ピン / 非表示 | `pinned` / `archived` |

## 2. 決定事項

| 論点 | 決定 | 理由 |
|---|---|---|
| 削除 | 行わない。アーカイブのみ | messages / tool_calls / turn_runs / session snapshot / 記憶参照への波及が大きい。要件は「見え方の整理」 |
| 名前と識別情報 | 分離する（名前変更は `chat_title` のみ更新） | リネームで Pulse / agent_send の配送先や session key が壊れない |
| slug | 作らない | 住所は `chat_id` で足りる。セッション ID は Web UI からコピーできるようにする |
| 外部チャネルのセッション | 名前・ピン・アーカイブすべて可 | `chat_title` はローカル表示ラベルで、チャネル本体に干渉しない |
| 一覧の初期表示 | 全チャネル（チャネルフィルタは既存の `SessionsSection` を使う） | Discord 履歴の参照性を残す |
| 名前の付け方 | 手動のみ・任意（未指定でも会話開始可） | 自動命名は要件外 |
| `pinned` と `archived` の同時成立 | 表示はアーカイブを優先し、ピン状態は保持する（解除で復元） | 状態を失わず可逆にする |
| 表示名の参照範囲 | Storage の `surface_thread` は識別子から導出し、表示は `chat_title` を優先する | 表示名変更が識別に波及しない |
| アーカイブ済みへの送信 | 禁止しない | アーカイブは一覧の見え方の制御のみ。履歴・記憶・Turn は保持し会話も継続できる |

## 3. スコープ

### 3.1 対象

- `chats` への `archived` / `pinned` 追加と `chat_title` の意味整理
- Storage のセッション一覧拡張（フィールド追加・並び順変更）と表示名メタの更新 API
- Web API のセッション一覧拡張と `PATCH /api/sessions/{chat_id}`
- Web UI のセッション一覧（名前・セクション・操作・ID コピー）と新規作成時の任意命名
- Local API protocol へのフィールド追加（TUI は壊さない範囲）
- 関連ドキュメント

### 3.2 対象外（将来タスク）

- Pulse の Web 配送対応（`SENDABLE_CHANNELS` への追加、`chat_id` による home surface 解決）
- 最初の発言からの自動命名
- セッションの完全削除
- 複数エージェント共有ルーム
- 手動並べ替え（ソート順カラム）
- TUI 側のセッション整理 UI

## 4. 変更一覧

### 4.1 DB

`chats` に 2 カラムを追加する。3 状態を持たないフラグとして `INTEGER NOT NULL DEFAULT 0` を使う（既存の `messages.is_from_bot` と同じ慣習）。

| カラム | 型 | 既定 | 意味 |
|---|---|---|---|
| `archived` | INTEGER | 0 | 1 のとき一覧のアーカイブセクションに退避 |
| `pinned` | INTEGER | 0 | 1 のとき一覧の先頭に固定 |

- main DB: `SCHEMA_VERSION` 18 → 19（`add_column_if_missing` を使い再実行可能にする）
- secret DB: `SECRET_SCHEMA_VERSION` 7 → 8（`Database` のメソッドを scope 非依存に保つため同じカラムを追加）
- `chat_title` の seed 値クリーンアップ: main DB の `SCHEMA_VERSION` 19 → 20。`chat_title == 導出 surface_thread` の行のみ NULL にする（`SurfaceContext::surface_thread_from_session_key` と同じ変換を Rust で 1 パス実行）

### 4.2 Storage

| 対象 | 変更 |
|---|---|
| `src/storage/mod.rs` `SessionSummary` | `archived: bool` / `pinned: bool` を追加 |
| `src/storage/chat.rs` `resolve_or_create_chat_id` | `chat_title` 引数と UPDATE / INSERT 内の `chat_title` 更新を削除し、セッションの識別・生成に専念させる |
| `src/storage/chat.rs` `update_chat_display_meta` | 新設。`chat_title` / `archived` / `pinned` の唯一の書き込み経路。未指定フィールドは更新しない単一 UPDATE。戻り値は更新行の有無 |
| `src/storage/chat.rs` `list_sessions` | `archived` / `pinned` を SELECT し、並びを `pinned DESC, last_message_time DESC, chat_id DESC` に変更 |
| `src/storage/chat.rs` `logical_session_thread` | 削除。`surface_thread` は `SurfaceContext::surface_thread_from_session_key(channel, external_chat_id, agent_id)` で `external_chat_id` から導出し、`chat_title` を参照しない |
| `src/agent_loop/session.rs` `resolve_chat_id`、`src/slash_commands.rs` | 引数削除に伴う呼び出し修正（表示名は設定しない） |

`surface_thread` が識別子由来になることで、`:agent:` サフィックスの剥離が加わる。title が未設定のチャットでは `999:agent:vega` ではなく `999` を返し、TUI の `--session` 解決は raw thread で行われる（リネームの影響を受けない）。

### 4.3 Web API

`GET /api/sessions` の項目を次のとおり変更する。

| フィールド | 変更 |
|---|---|
| `label` | 削除（`external_chat_id` 生文字列。新 UI では使わない） |
| `name` | 追加（`chat_title`。未命名は `null`） |
| `archived` / `pinned` | 追加（boolean） |
| その他 | 現状維持（`session_key`, `chat_id`, `channel`, `agent_id`, `last_message_time`, `last_message_preview`） |

`PATCH /api/sessions/{chat_id}` を新設する。

```jsonc
// request（すべて任意。1 つ以上必須）
{ "name": "設計相談" | null, "archived": true, "pinned": false }
// response 200
{ "ok": true, "session": { "chat_id": 42, "name": "設計相談", "archived": false, "pinned": true } }
```

- `name`: trim 後 1〜200 文字。空文字は 400、`null` は名前解除
- 変更フィールドなしは 400、存在しない `chat_id` は 404
- 認証は既存の `channels.web.auth_token`
- 成功時に `ConversationChanged { scope: Normal, chat_id }` を publish し、既存の `session_updated` 経由で一覧を再取得させる

### 4.4 Web UI

セクションへの振り分けは次のとおり。アーカイブを優先し、ピン状態は保持する。

| `archived` | `pinned` | 表示先 |
|---|---|---|
| true | 任意 | Archived |
| false | true | Pinned |
| false | false | Active |

| 対象 | 変更 |
|---|---|
| `web/src/shared/api/types.ts` | `SessionEntry` を `chat_id` / `name` / `archived` / `pinned` を持つ形へ変更（`label` 削除） |
| `web/src/shared/api/sessions.ts` | 新フィールドのマッピングと `updateSession(authToken, chatId, patch)` を追加 |
| `web/src/app/shell/SessionsSection.tsx` | 表示名（`name` → 最終発言プレビュー → `Untitled`）と時刻を表示。`Pinned` / `Active` / `Archived`（折りたたみ）のセクション分割。項目の操作（名前変更・ピン切替・アーカイブ切替・`chat:{id}` コピー）。既存のチャネルフィルタは全セクションに適用。クライアント側の `last_message_time` 降順ソートは廃止し、サーバーが返す順序をそのまま使う（ピン先頭を壊さない）。未読バッジは維持 |
| `web/src/app/WebUI.tsx` | 新規作成時に任意の名前入力を受け付け、`chat:{id}` へ解決した初回送信後に `updateSession` で名前を確定。`sessions` キャッシュを invalidate。エージェント選択時の初期セッション自動選択からはアーカイブ済みを除外し、アーカイブ済みしか存在しない場合は新規会話の状態を表示する（明示的に開いたアーカイブ済みは従来どおり閲覧・送信できる） |
| `web/src/app/command-palette/buildPaletteItems.ts` | セッション項目のラベルを `name ?? プレビュー` に変更し、アーカイブ済みを除外 |

セッション ID コピーは `navigator.clipboard` を使い、コピー成功を一時表示する。clipboard が使えない環境では `chat:{id}` を選択可能なテキストとして表示する。

### 4.5 Local API / TUI

| 対象 | 変更 |
|---|---|
| `src/runtime/local_api/protocol.rs` `SessionSummary` | `archived: bool` / `pinned: bool` を追加（`chat_title` は既存） |
| `src/runtime/local_api/protocol.rs` `SessionView` | `chat_title: Option<String>` を追加 |
| `src/runtime/local_api/service.rs` `session_summary` / `resolve_session` | 新フィールドのマッピング |
| `src/channels/tui/draw.rs` | ステータス行の表示を `chat_title ?? surface_thread` にする（識別子は `surface_thread` のまま） |
| `src/channels/tui/sessions.rs` | テスト用フィクスチャの更新のみ。一覧の表示は既存どおり `chat_title` 優先 |

### 4.6 Docs

| ファイル | 内容 |
|---|---|
| `docs/db.md` | `chats.archived` / `chats.pinned`、`chat_title` の意味（ユーザー表示名専用）、並び順 |
| `docs/api.md` | `GET /api/sessions` のフィールド変更と `PATCH /api/sessions/{chat_id}` |
| `docs/channels.md` | Web UI のセッション管理（命名・ピン・アーカイブ・ID コピー） |
| `docs/commands.md` | `--session` が raw thread（`surface_thread`）で解決され、リネームの影響を受けないこと |
| `docs/session-lifecycle.md` | アーカイブは表示制御のみで、履歴・記憶・Turn を保持し送信も可能であること |

`docs/issues/issues.md` は編集・コミットしない。

## 5. 検証

### 5.1 自動

| 層 | 内容 |
|---|---|
| Storage UT | 部分更新が他フィールドを壊さない / `name: null` で解除できる / 存在しない `chat_id` で false / `list_sessions` の並び順（pinned 先頭） / `resolve_or_create_chat_id` が `chat_title` を変更しない / `surface_thread` が `chat_title` に依存しない（リネーム後も不変） / seed 済み `chat_title` のクリーンアップがユーザー設定名を消さない / メッセージ送信がユーザー設定名を上書きしない / マイグレーションの再実行 |
| Web API IT | `PATCH` 正常系・400（空 name / 長すぎる name / フィールドなし）・404・認証 / `GET /api/sessions` の新フィールド / `ConversationChanged` の publish |
| Frontend | `SessionsSection` の未命名フォールバック・セクション分割（`archived` と `pinned` が同時に true なら Archived）・各操作の API 呼び出し・ID コピー / 初期セッション自動選択がアーカイブ済みを除外する / `sessions.ts` のマッピングと `updateSession` / パレットのラベルとアーカイブ除外 |
| Local API | `SessionSummary` / `SessionView` のマッピングと protocol のシリアライズ / `--session` 解決がリネームの影響を受けない |

### 5.2 手動

- Web: 新規作成で名前付き/名前なしの両方を開始し、一覧の表示名とセクションを確認
- Web: 名前変更・ピン・アーカイブ・再表示が即時反映され、別タブにも伝播することを確認
- Web: アーカイブ済みセッションを開いて送信でき、履歴が保持されることを確認
- Web: ID コピーで `chat:{id}` が取得できることを確認
- TUI: `/sessions` の一覧が壊れないこと、ピン留めセッションが先頭に来ることを確認
- 再起動後も名前・ピン・アーカイブが保持されることを確認

### 5.3 ゲート

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings   # PR 前
cargo test
npm run typecheck --prefix web
npm test --prefix web
npm run build --prefix web
```

## 6. 実装ステップ

### Step 0: Worktree 作成

- `worktree-create` skill で `feat/web-session-management` の worktree を作成する

### Step 1: Storage — アーカイブ・ピンのメタ情報

**目的**: セッションの整理状態を永続化し、一覧に載せる。

- main DB v19 / secret DB v8 に `archived` / `pinned` を追加
- `SessionSummary` 拡張、`list_sessions` の SELECT と並び順変更
- `update_chat_display_meta` を新設
- UT を追加

**検証**: `cargo test --lib storage::`

**コミット**: `feat(storage): add session archive and pin metadata`

### Step 2: 表示名のユーザー管理化

**目的**: `chat_title` を自動 seed から切り離し、表示名の書き込み経路を `update_chat_display_meta` に一本化する。

- `resolve_or_create_chat_id` から `chat_title` 引数と更新処理を削除し、呼び出し元（`resolve_chat_id` / `slash_commands`）を修正
- `logical_session_thread` を削除し、`surface_thread` を `SurfaceContext::surface_thread_from_session_key` で導出
- main DB v20 で seed 済み `chat_title` を NULL にクリーンアップ
- UT（クリーンアップがユーザー設定名を保持する / 送信で名前が消えない / `resolve_or_create_chat_id` が名前を変えない / リネーム後も `surface_thread` が不変）

**検証**: `cargo test --lib storage:: agent_loop::`

**コミット**: `refactor(storage): separate display names from session identity`

### Step 3: Web API — セッション一覧の拡張と更新エンドポイント

**目的**: Web UI が名前・整理状態の読み書きを行えるようにする。

- `SessionItem` を `name` / `archived` / `pinned` へ変更（`label` 削除）
- `PATCH /api/sessions/{chat_id}` を追加（バリデーション・404・`ConversationChanged` publish）
- ルーティング登録
- IT を追加

**検証**: `cargo test --lib channels::web::sessions`

**コミット**: `feat(web): expose session metadata in the sessions API`

### Step 4: Web UI — セッション一覧の管理操作

**目的**: 名前・整理状態を UI から扱えるようにする。

- `types.ts` / `sessions.ts` の型と API クライアント更新
- `SessionsSection` の表示名・セクション・操作・ID コピー
- `WebUI` の新規作成時任意命名とパレットの追従
- vitest を追加

**検証**: `npm run typecheck --prefix web` / `npm test --prefix web`

**コミット**: `feat(web): manage sessions from the Web UI`

### Step 5: Local API へのフィールド追加

**目的**: TUI が同一の一覧情報を取得できるようにし、表示名の変更後も TUI の表示を保つ。

- protocol の `SessionSummary` に `archived` / `pinned`、`SessionView` に `chat_title` を追加
- サービス側マッピングと TUI ステータス行の表示（`chat_title ?? surface_thread`）を更新
- TUI のテストフィクスチャ更新

**検証**: `cargo test --lib runtime::local_api channels::tui`

**コミット**: `feat(local-api): expose session archive and pin state`

### Step 6: ドキュメント反映

- 4.6 の 4 ファイルを更新

**検証**: 記述とコードの整合を目視確認

**コミット**: `docs: document web session management`

### Step 7: 自己レビュー・PR

- `5.3` のゲートを全実行
- 差分と Plan の対象一覧を照合し、可視性・デッドコード・テストの実効性・docs 整合をレビュー
- PR 作成（description は日本語、該当 issue があれば `Close #`）
- PR 作成 15 分後にレビュー対応ワークフローを実行

## 7. リスクと対応

| リスク | 対応 |
|---|---|
| 既存 DB へのマイグレーション失敗 | `add_column_if_missing` による再実行可能な追加。バージョン番号で順序保証 |
| `label` 削除によるクライアント破壊 | 消費者は `web/src` のみ。パレットと一覧を同一コミット系列で更新 |
| 並び順変更が TUI に及ぶ | 意図した変更（ピン先頭）。TUI は同じ storage 一覧を使うため表示順のみ変化 |
| `--session` の解決基準が変わる | リネームの影響を受けない raw thread に統一される。`:agent:` サフィックスが剥離されるため、未命名チャットで旧キーを指定すると新規コンテキストになる旨を docs に明記 |
| TUI ステータス行の表示名が消える | `SessionView` に `chat_title` を追加し `chat_title ?? surface_thread` を表示して解消 |
| 名前がメッセージ送信で上書きされる | seed 廃止により `None` が渡り `COALESCE` で既存値を保持。UT で担保 |
| 新規作成で名前を入れても初回送信まで永続化されない | 名前はクライアント draft として保持し、`chat:{id}` 解決直後に PATCH。未送信のまま離脱した場合は保存されない旨を UI 上で明示 |
| clipboard が使えない環境 | `chat:{id}` を選択可能なテキストとして表示し手動コピーできるようにする |
| secret DB のスキーマ不整合 | 同一カラムを追加して `Database` メソッドを scope 非依存に保つ |
