# Plan: Discord/Telegram ファイル添付対応（送受信）

Discord/Telegram 両チャネルでユーザーからの添付ファイルを受信してローカル保存し、新規 `send_message` ツールでエージェントからファイルを送信できるようにする。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **テキストパス通知方式**: 受信ファイルは `/workspace/media/inbound/` に保存し、`[attachment: path]` をテキストに付与して agent_loop に渡す。`process_turn` のシグネチャは変更しない
- **共通ユーティリティ分離**: ファイルダウンロード・保存・命名規則はプラットフォーム非依存の `media` モジュールに集約し、Discord/Telegram ハンドラーから利用する
- **ChannelAdapter 拡張**: アウトバウンド送信のため `ChannelAdapter` トレイトに `send_attachment()` を追加。Discord は REST API multipart、Telegram は `send_photo` / `send_document` に振り分け
- **既存パターン踏襲**: ツール定義は `Tool` トレイトに従い `ToolRegistry::new()` に登録。エラーは `thiserror` で構造化。既存の `ReadTool` や `BashTool` を参考にする
- **プラットフォーム制限に委ねる**: 独自のファイルサイズ上限は設けず、Discord/Telegram API の制限に従う（Issue の Won't 项规定）

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| 共通メディアユーティリティ（ダウンロード・保存・命名） | **新規** `src/media.rs` |
| Discord 受信: 添付ファイルのダウンロード・保存・パス通知 | 変更 `src/channels/discord.rs` |
| Telegram 受信: photo/document/voice のダウンロード・保存・パス通知 | 変更 `src/channels/telegram.rs` |
| ChannelAdapter トレイト: `send_attachment()` 追加 | 変更 `src/channel_adapter.rs` |
| Discord アダプター: ファイル送信実装 | 変更 `src/channels/discord.rs` |
| Telegram アダプター: ファイル送信実装 | 変更 `src/channels/telegram.rs` |
| `send_message` ツール: 新規ツール実装 | **新規** `src/tools/send_message.rs` |
| ツール登録: `ToolRegistry` への追加 | 変更 `src/tools/mod.rs` |

---

## Step 0: Worktree 作成

`worktree-create` skill を使用し、`feat/discord-telegram-file-attachment` ブランチで Worktree を作成。

---

## Step 1: 共通メディアユーティリティ (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `save_inbound_file_creates_file_with_timestamp_name` | バイトデータを渡すと `media/inbound/{YYYYMMDD-HHMMSS}-{name}` に保存される |
| `save_inbound_file_creates_directory_if_missing` | `media/inbound/` が存在しない場合に自動作成される |
| `save_inbound_file_rejects_path_traversal` | ファイル名に `..` や `/` が含まれる場合はエラー |
| `save_inbound_file_rejects_empty_filename` | 空のファイル名でエラー |
| `format_attachment_text_with_user_text` | `[attachment: path]\nユーザーテキスト` の形式でフォーマットされる |
| `format_attachment_text_without_user_text` | テキストなしの場合 `[attachment: path]` のみ |
| `format_attachment_text_multiple_files` | 複数ファイルの場合 `[attachment: path1]\n[attachment: path2]\nテキスト` |

### GREEN: 実装

`src/media.rs` に以下を実装:

- `save_inbound_file(workspace_dir: &Path, filename: &str, data: &[u8]) -> Result<PathBuf, MediaError>`
  - `media/inbound/` ディレクトリを作成
  - ファイル名をサニタイズ（パストラバーサル対策）
  - `{YYYYMMDD-HHMMSS}-{original_filename}` 形式で保存
- `format_attachment_text(paths: &[PathBuf], user_text: &str) -> String`
  - `[attachment: path]` をテキストに付与
- `MediaError` を `thiserror` で定義

### コミット

`feat: add shared media utility for inbound file handling`

---

## Step 2: Discord 受信 - 添付ファイル対応 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_message_with_attachment_builds_text` | `msg.attachments` がある場合、`[attachment: path]` + テキストが構築される |
| `discord_message_without_attachment_text_only` | 添付なしの場合、既存の `text` のみの挙動が変わらない |
| `discord_message_attachment_only_no_text` | テキストなし・添付のみの場合でも `process_turn` に渡る |

### GREEN: 実装

`src/channels/discord.rs` の `Handler::message()` を変更:

1. **line 243-245 の空テキスト早期 return を変更**: 添付ファイルがある場合は return しない
2. `msg.attachments` をループし、各 attachment を `reqwest::get(&attachment.url)` でダウンロード
3. `media::save_inbound_file()` で保存
4. `media::format_attachment_text()` でテキスト構築
5. 構築したテキストを `process_turn()` に渡す

参考: `serenity::model::channel::Attachment` の `url`, `filename`, `content_type`, `size` フィールドを利用。

### コミット

`feat(discord): handle inbound file attachments`

---

## Step 3: Telegram 受信 - 添付ファイル対応 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `telegram_message_with_photo_builds_text` | `msg.photo()` がある場合、ダウンロード→保存→`[attachment: path]` 構築 |
| `telegram_message_with_document_builds_text` | `msg.document()` がある場合、同様 |
| `telegram_message_with_voice_builds_text` | `msg.voice()` がある場合、同様 |
| `telegram_message_text_only_no_regression` | テキストのみの場合、既存挙動が変わらない |
| `telegram_message_file_only_no_text` | テキストなし・ファイルのみでも process_turn に渡る |

### GREEN: 実装

`src/channels/telegram.rs` の `handle_message()` を変更:

1. **lines 105-112 のテキストのみフィルタを変更**: photo / document / voice がある場合も処理を継続
2. 各メディアタイプのダウンロード:
   - `msg.photo()`: 最大サイズの写真を `bot.get_file()` → download で取得
   - `msg.document()`: `bot.get_file()` → download で取得
   - `msg.voice()`: `bot.get_file()` → download で取得
3. `media::save_inbound_file()` で保存
4. `media::format_attachment_text()` でテキスト構築
5. テキスト＋スラッシュコマンド判定（既存ロジック）は変更なし。メディアのみのメッセージはスラッシュコマンド判定をスキップ

参考: teloxide 0.17 の `Bot::get_file(&file_id)` → `Bot::download_file(&file_path, destination)` API を利用。

### コミット

`feat(telegram): handle inbound photo, document, and voice`

---

## Step 4: ChannelAdapter send_attachment 拡張 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `channel_adapter_send_attachment_default_returns_error` | デフォルト実装が "not supported" エラーを返す |
| `discord_send_attachment_posts_multipart` | Discord REST API で multipart/form-data が送信される |
| `discord_send_attachment_with_text_sends_both` | テキスト + ファイル両方送信 |
| `telegram_send_attachment_photo_uses_send_photo` | 画像ファイル → `send_photo` が呼ばれる |
| `telegram_send_attachment_non_photo_uses_send_document` | 非画像ファイル → `send_document` が呼ばれる |
| `telegram_send_attachment_with_caption` | キャプション付きで送信 |

### GREEN: 実装

**`src/channel_adapter.rs`** に `send_attachment()` を追加:

```rust
async fn send_attachment(
    &self,
    external_chat_id: &str,
    text: Option<&str>,
    file_path: &Path,
    caption: Option<&str>,
) -> Result<(), String> {
    // デフォルト: not supported
    Err("file attachments not supported".to_string())
}
```

**DiscordAdapter (`src/channels/discord.rs`)**:
- Discord REST API `POST /channels/{id}/messages` に `multipart/form-data` で `file` + `content` を送信
- `reqwest::multipart` を利用

**TelegramAdapter (`src/channels/telegram.rs`)**:
- 画像拡張子 (jpg, jpeg, png, gif, webp) → `bot.send_photo(chat_id, InputFile::file(path)).caption(caption)`
- その他 → `bot.send_document(chat_id, InputFile::file(path)).caption(caption)`
- `text` のみの場合は既存 `send_text()` に委譲

**Telegram の text / caption 扱い（重要）**:
- Telegram API では「本文テキスト」と「添付キャプション」が別物
- `attachment_path` + `text` 両方指定時:
  - ファイル送信に `caption` として `text` を付与（512文字制限）
  - 512文字を超える場合は `text` を本文として別メッセージで送信
- `attachment_path` + `caption` 指定時:
  - `caption` をファイルのキャプションとして送信
  - `text` は別メッセージとして送信

### コミット

`feat: add send_attachment to ChannelAdapter, implement for Discord and Telegram`

---

## Step 5: send_message ツール (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `send_message_text_only_sends_text` | `text` のみ指定 → テキストメッセージ送信 |
| `send_message_attachment_only_sends_file` | `attachment_path` のみ → ファイル送信 |
| `send_message_text_and_attachment_sends_both` | 両方指定 → テキスト + ファイル送信 |
| `send_message_with_caption` | `caption` 指定 → キャプション付きファイル送信 |
| `send_message_attachment_not_found_returns_error` | 存在しないファイルパス → エラー |
| `send_message_attachment_path_traversal_returns_error` | `..` を含むパス → エラー |
| `send_message_no_text_no_attachment_returns_error` | 両方なし → エラー |

### GREEN: 実装

`src/tools/send_message.rs` に新規ツールを実装:

```rust
struct SendMessageTool {
    workspace_dir: PathBuf,
    channels: Arc<ChannelRegistry>,
    db: Arc<Database>,
}
```

- **パラメータ**: `text` (任意), `attachment_path` (任意), `caption` (任意)
- **処理フロー**:
  1. `attachment_path` があれば `resolve_workspace_path()` で検証 + 存在確認
  2. `ToolExecutionContext.chat_id` から `storage::get_chat_info()` で `external_chat_id` と `channel` を取得
  3. `channels.get(&channel)` でアダプターを取得
  4. アダプターの `send_attachment()` / `send_text()` を呼び出し
  5. 成功時は `"Message sent successfully"` を返す

`src/tools/mod.rs` の `ToolRegistry::new()` で `SendMessageTool` を登録。ただし `ToolRegistry` には `channels` と `db` への参照も必要になるため、`ToolRegistry::new()` のシグネチャを拡張するか、`register_tool()` で後から登録するパターンを採用。

**アプローチ**: `ToolRegistry::new()` のシグネチャに `channels: Arc<ChannelRegistry>` と `db: Arc<Database>` を追加し、`SendMessageTool` を初期ツールとして登録。`runtime.rs` の `build_app_state()` で `channels` と `db` を渡す。

### コミット

`feat: add send_message tool for outbound file/text delivery`

---

## Step 6: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```

---

## Step 7: PR 作成

```bash
gh pr create --title "feat: Discord/Telegram file attachment support (send/receive)" --base main
```

PR description に `Close #18` を記載。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/media.rs` | **新規** | 共通メディアユーティリティ（保存・命名・フォーマット） |
| `src/tools/send_message.rs` | **新規** | `send_message` ツール実装 |
| `src/channels/discord.rs` | 変更 | 受信: attachment ダウンロード/保存。送信: `send_attachment()` 実装 |
| `src/channels/telegram.rs` | 変更 | 受信: photo/document/voice ダウンロード/保存。送信: `send_attachment()` 実装 |
| `src/channel_adapter.rs` | 変更 | `send_attachment()` メソッド追加 |
| `src/tools/mod.rs` | 変更 | `SendMessageTool` 登録、`ToolRegistry::new()` シグネチャ拡張 |
| `src/runtime.rs` | 変更 | `build_app_state()` で `channels`/`db` を `ToolRegistry` に渡す |
| `src/lib.rs` または `src/main.rs` | 変更 | `mod media` 追加（新規モジュール宣言） |

---

## コミット分割

1. `feat: add shared media utility for inbound file handling` — `src/media.rs`, `src/lib.rs`
2. `feat(discord): handle inbound file attachments` — `src/channels/discord.rs`
3. `feat(telegram): handle inbound photo, document, and voice` — `src/channels/telegram.rs`
4. `feat: add send_attachment to ChannelAdapter, implement for Discord and Telegram` — `src/channel_adapter.rs`, `src/channels/discord.rs`, `src/channels/telegram.rs`
5. `feat: add send_message tool for outbound file/text delivery` — `src/tools/send_message.rs`, `src/tools/mod.rs`, `src/runtime.rs`

---

## テストケース一覧（全 25 件）

### media (7)
1. `save_inbound_file_creates_file_with_timestamp_name` — バイトデータ保存時にタイムスタンプ付きファイル名で作成
2. `save_inbound_file_creates_directory_if_missing` — media/inbound/ ディレクトリ自動作成
3. `save_inbound_file_rejects_path_traversal` — `..` を含むファイル名を拒否
4. `save_inbound_file_rejects_empty_filename` — 空ファイル名を拒否
5. `format_attachment_text_with_user_text` — `[attachment: path]\nテキスト` 形式
6. `format_attachment_text_without_user_text` — テキストなし時 `[attachment: path]` のみ
7. `format_attachment_text_multiple_files` — 複数ファイルのフォーマット

### Discord 受信 (3)
8. `discord_message_with_attachment_builds_text` — 添付あり → `[attachment: path]` + テキスト構築
9. `discord_message_without_attachment_text_only` — 添付なし → 既存挙動不变
10. `discord_message_attachment_only_no_text` — テキストなし・添付のみ → process_turn に渡る

### Telegram 受信 (5)
11. `telegram_message_with_photo_builds_text` — photo ダウンロード→保存→パス通知
12. `telegram_message_with_document_builds_text` — document ダウンロード→保存→パス通知
13. `telegram_message_with_voice_builds_text` — voice ダウンロード→保存→パス通知
14. `telegram_message_text_only_no_regression` — テキストのみ → 既存挙動不变
15. `telegram_message_file_only_no_text` — テキストなし・ファイルのみ → process_turn に渡る

### ChannelAdapter send_attachment (6)
16. `channel_adapter_send_attachment_default_returns_error` — デフォルト実装のエラー
17. `discord_send_attachment_posts_multipart` — Discord multipart 送信
18. `discord_send_attachment_with_text_sends_both` — テキスト + ファイル送信
19. `telegram_send_attachment_photo_uses_send_photo` — 画像 → send_photo
20. `telegram_send_attachment_non_photo_uses_send_document` — 非画像 → send_document
21. `telegram_send_attachment_with_caption` — キャプション付き送信

### send_message ツール (7)
22. `send_message_text_only_sends_text` — テキストのみ送信
23. `send_message_attachment_only_sends_file` — ファイルのみ送信
24. `send_message_text_and_attachment_sends_both` — テキスト + ファイル送信
25. `send_message_with_caption` — キャプション付き送信
26. `send_message_attachment_not_found_returns_error` — 存在しないファイル → エラー
27. `send_message_attachment_path_traversal_returns_error` — パストラバーサル → エラー
28. `send_message_no_text_no_attachment_returns_error` — 両方なし → エラー

### 動作確認 (1)
29. `cargo test clippy fmt 全通過` — ビルド・テスト・Lint

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | WT作成 | ~10 行 |
| Step 1 | media モジュール | ~120 行 (util 60 + tests 60) |
| Step 2 | Discord 受信 | ~80 行 (impl 40 + tests 40) |
| Step 3 | Telegram 受信 | ~130 行 (impl 70 + tests 60) |
| Step 4 | ChannelAdapter 拡張 | ~200 行 (trait 20 + discord 80 + telegram 60 + tests 40) |
| Step 5 | send_message ツール | ~180 行 (tool 100 + tests 80) |
| Step 6 | 動作確認 | ~0 行 |
| Step 7 | PR作成 | ~0 行 |
| **合計** | | **~720 行** |
