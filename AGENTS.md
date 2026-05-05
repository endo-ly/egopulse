# EgoPulse

Self-hosted AI agent runtime (Rust/Tokio). TUI / Web UI / Discord / Telegram in a single binary.

## 開発コマンド

```bash
# === Rust ===
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test

# === WebUI ===
npm install --prefix web
npm run build --prefix web
cd web && npm run dev

# === 実行 ===
cargo run -- setup          # 初回セットアップウィザード
cargo run -- run            # 全チャネル起動
cargo run -- chat           # CLI チャットセッション

# === リリースビルド＆稼働中差し替え ===
cargo build --release -p egopulse
systemctl --user stop egopulse
install -m 0755 target/release/egopulse ~/.local/bin/egopulse
systemctl --user start egopulse

# === Coderabbit review ===
coderabbit --prompt-only -t uncommitted
coderabbit --prompt-only -t committed --base main
```

## 規約

### コーディング

- **「長期的な保守性」「コードの美しさ」「堅牢性」** を担保するコーディング
  - SOLID 原則
  - KISS (Keep It Simple, Stupid) & YAGNI (You Ain't Gonna Need It)
  - DRY (Don't Repeat Yourself)
  - 責務の分離: ビジネスロジック、UI、データアクセスなどが適切に分離されているか
  - 可読性と美しさ
- **コードレビューで一つも指摘されないレベル**のコード品質を目指す。Coderabbit, Codex のレビューはとても細かいです。
- 場当たり的な対応は禁止（バグフォールバック、ビルド/テスト通過のためだけの本質的でない修正）
- 「後方互換」は負債。既存利用維持のための互換分岐や旧仕様フォールバックは追加しない。新仕様へ一直線に置き換える
- 公開範囲は最小化する。新規APIは`private`から始め、必要に応じて `pub(crate)` / `pub(super)` を選択する。`pub` は binary entrypoint や外部公開が必要な場合だけ使う
- 大きめの実装・リファクタ後は `cargo clippy --all-targets --all-features -- -D warnings` に加え、公開範囲起因の dead code を疑って確認する
- Rust 実装後は `fmt`, `test`, `check`, `clippy` 必須

| 項目      | ルール                                          |
| --------- | ----------------------------------------------- |
| Logging   | `tracing` を使用、機密情報禁止                  |
| Error     | `thiserror` で構造化、`Display` は lower-case   |
| Doc       | public item に doc comment、`# Errors` / `# Panics` / `# Safety` |
| Test      | AAA パターン必須                                |

### 文書

- 文書作成時はどんな時でも **MECE** を意識する（セクション構成、要件定義、設計書、PR詳細などすべて）
- 実装後、関連内容が `docs/` にある場合は必ず反映する

#### ドキュメント一覧

| トピック | ファイル |
|---|---|
| アーキテクチャ概要 | [architecture.md](./docs/architecture.md) |
| コマンド仕様 | [commands.md](./docs/commands.md) |
| 設定仕様 | [config.md](./docs/config.md) |
| チャネル仕様 (Web/Discord/Telegram/TUI/CLI) | [channels.md](./docs/channels.md) |
| セッションライフサイクル | [session-lifecycle.md](./docs/session-lifecycle.md) |
| Built-in Tools | [tools.md](./docs/tools.md) |
| MCP 統合 | [mcp.md](./docs/mcp.md) |
| OpenAI Codex Provider | [openai-codex.md](./docs/openai-codex.md) |
| System Prompt 構築 | [system-prompt.md](./docs/system-prompt.md) |
| セキュリティ | [security.md](./docs/security.md) |
| デプロイ手順 | [deploy.md](./docs/deploy.md) |
| ディレクトリ構成 | [directory.md](./docs/directory.md) |
| DB スキーマ | [db.md](./docs/db.md) |
| WebUI API | [api.md](./docs/api.md) |

### Git / CI / PR

- GitHub Flow: ブランチ `<type>/<desc>`
- コミット: Conventional Commits（英語）
- ワークフロー: `ci.yml`(テスト), `release.yml`(リリース)
- Issue, Planなど、**計画あり**で進めた実装: Git Worktreeを作成しその中で作業する（`worktree-create` skill使用）
- ブレインストーミングや壁打ち系など、**計画なし**で進めた実装: mainブランチで作業やプッシュしてよい
- PR description は日本語。該当Issueがある場合は `Close #XX` 明記
- PR レビューはCoderabbitが自動で提供。PR作成後10分程度の時間差あり。レビューバックは`pr-review-back-workflow` skill使用

### セキュリティ

- `.env` 系・ローカル秘密設定ファイルの読み取り禁止。秘密が必要な場合はユーザーに明示してもらう
- `~/.egopulse/egopulse.config.yaml` も読み取り禁止

## Plan作成方針

- Planのスコープ: WT作成 -> 実装(TDD) -> コミット(意味ごとに分離) -> PR作成 （必ずWT作成と明示する）
- 計画には必ずUTや動作確認などの検証を入れる
- プランではコード(How) を書きすぎない。また、プラン冒頭に以下文言を記載する
  - 「Howはあくまで参考であり、よりよい設計方針があれば各自で判断し採用する」

- プラン作成後は以下の方法でレビュー依頼

初回:
```bash
codex exec -m gpt-5.4 "このプランをレビューして。致命的な点だけ指摘して: {plan_path}"
```
更新:
```bash
codex exec resume --last -m gpt-5.4 "プランを更新したからレビューして。致命的な点だけ指摘して: {plan_path}"
```
