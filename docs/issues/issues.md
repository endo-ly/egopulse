# Issue一覧

Issue一覧を記載する。完了したら削除する

### 13. Config ホットリロードの仕組みが不明

「ホットリロード可能」と分類されているフィールドがあるが、**どのタイミングで再読込されるのか**が未文書化。inotify watch？ API 呼び出し時の都度読み込み？ YAML を直接編集した場合いつ反映されるか不明確。

### 16. Pulse 通知が `require_mention` をバイパス

Pulse が Discord チャネルに通知を送る際、`require_mention` 設定を確認せず直接送信。`require_mention: true` のチャネルに突然 Bot から通知が飛んでくることをユーザーが予期していない可能性。

### 20. `context_window_tokens` の手動設定

各モデルの token 数は手動設定。プロバイダーの models API から自動取得する仕組みなし。OpenRouter 等は `/models` で取得可能なので将来自動解決を検討すべき。

---

### P0. Built-in Tools ドキュメントの実装参照が古い

分類: 変更・運用の持続性

根拠:

- `docs/tools.md` は実装本体を `src/tools.rs` としているが、現行は `src/tools/` 配下に分割されている。
- `docs/tools.md` は `src/tools/mcp_adapter.rs` を参照しているが、現行ツリーに該当ファイルはなく MCP 実装は `src/tools/mcp.rs` にある。
- `edit` の節には top-level `oldText` / `newText` 受け入れが記載されていた。

問題:

- 新規変更者が誤ったファイルを追い、調査コストが増える。
- ツール仕様は LLM に読ませる可能性が高く、古い参照は実装ミスを誘発しやすい。

対応候補:

- `docs/tools.md` の実装リンクを現行ファイルへ更新する。
- 各 tool の実装所在地を `read/write/edit: src/tools/files.rs`, `bash: src/tools/shell.rs`, `grep/find/ls: src/tools/search.rs`, `web_fetch: src/tools/web_fetch/` のように明示する。
- 正規 schema から外れる入力は受け付けない。

検証:

- `rg "src/tools.rs|mcp_adapter" docs` が archived plan 以外でヒットしないこと。


### P1. Storage query が巨大化し、DB 契約の変更影響が読みにくい

分類: 境界・依存の健全性 / 変更・運用の持続性

根拠:

- `src/storage/queries.rs` は 4,500 行超。
- chats / messages / sessions / sleep_runs / episode_events / rollups / pulse_runs など複数の集約が同一ファイルに集中している。
- テストも同一ファイル後半にまとまっており、対象領域ごとの差分レビューが重くなっている。

問題:

- DB スキーマ変更時の影響範囲がファイル単位で大きく見え、レビューしづらい。
- `Database` の impl が大きくなり、クエリ契約のまとまりが見えにくい。

対応候補:

- `storage/queries/` 配下に `chats.rs`, `messages.rs`, `sessions.rs`, `sleep_runs.rs`, `episode_events.rs`, `pulse_runs.rs` などへ段階分割する。
- まずテストだけを領域別ファイルへ移し、次に impl を移すと安全。
- DB public facade は `Database` のまま維持し、呼び出し側の変更を最小にする。

検証:

- 分割前後で `cargo test storage::queries storage::migration` が通ること。
- `cargo clippy --all-targets --all-features -- -D warnings` で dead code が増えないこと。

### P1. Session / messages / episode_events の正本関係をコード境界で明示する余地がある

分類: 目的・概念の妥当性

根拠:

- `sessions.messages_json` は turn context のスナップショットとして扱われ、sleep 後に `clear_session_messages` で空配列になる。
- `messages` テーブルは永続履歴として残り、Event Extraction の backfill は `messages` から実行されている。
- `episode_events` は episodic memory の正本として扱われる。

問題:

- 3つのデータソースがそれぞれ「正本」「作業用 snapshot」「生成物」のどれなのか、コード上の型境界だけでは追いにくい。
- 今後の memory / pulse / search 機能追加で、誤って `sessions.messages_json` を履歴正本として使うリスクがある。

対応候補:

- `SessionSnapshot`、`StoredMessage`、`EpisodeEvent` 周辺に「用途の違い」を doc comment と module 境界で明示する。
- sleep の chunk builder 名を `build_memory_update_chunks_from_session_snapshot` のように用途寄りへ寄せる。
- `docs/db.md` と `docs/sleep.md` に「正本関係」セクションを追加する。

検証:

- Event Extraction / Memory Update / archive clear のテスト名が、どのデータソースを使うかを明示していること。

### P2. 公開モジュール境界が広く、private-first 方針とズレがある

分類: 境界・依存の健全性

根拠:

- `src/lib.rs` で `agent_loop`, `channels`, `config`, `runtime`, `setup`, `sleep`, `storage` が `pub mod` になっている。
- 実体は単一バイナリ中心で、外部ライブラリ API としての安定公開が必要な範囲は限定的に見える。

問題:

- `pub` が広いほど public item doc / API 互換の責任が増える。
- 内部都合の型が crate 外から見えると、将来のリファクタに制約がかかる。

対応候補:

- binary entrypoint から必要な API だけを `pub(crate)` または小さな facade に寄せる。
- `main.rs` から必要なものだけを公開する `app` / `cli` facade を検討する。
- まず `cargo clippy --all-targets --all-features -- -D warnings` で公開範囲起因の dead code を確認する。

検証:

- 公開範囲縮小後も `cargo doc --no-deps` が通ること。

### P2. Runtime 起動・監視の責務が `runtime/mod.rs` に集中している

分類: 変更・運用の持続性 / 境界・依存の健全性

根拠:

- `runtime/mod.rs` は AppState 構築、agent turn worker、MCP reconnect loop、Web / Discord / Telegram / sleep scheduler / pulse scheduler 起動、監視ループを持つ。
- `start_channels` 周辺にチャネルごとの起動詳細がまとまっている。

問題:

- 新しい常駐タスク追加時に `runtime/mod.rs` の変更が増え、既存チャネル起動に影響しやすい。
- 起動失敗、再接続、graceful shutdown の契約がタスク種別ごとに散らばりやすい。

対応候補:

- `runtime/tasks.rs` または `runtime/supervisor.rs` を作り、常駐タスクの登録・監視・停止契約を切り出す。
- Web / Discord / Telegram / sleep / pulse を同じ `RuntimeTaskSpec` のような形で登録する。
- まず挙動を変えずに、起動処理の切り出しだけを行う。

検証:

- チャネル無効時の `NoActiveChannels`、チャネル起動失敗時の shutdown、scheduler 起動有無のテストを追加する。
