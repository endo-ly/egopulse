# EgoPulse

Self-hosted AI agent runtime (Rust/Tokio). TUI / Web UI / Discord / Telegram in a single binary.

## ディレクトリ構成

```
src/
├── main.rs              # CLI エントリポイント
├── lib.rs               # モジュール公開インターフェース
├── assets.rs            # 埋め込みアセット（Web UI 用静的ファイル）
├── builtin_skills.rs    # ビルトインスキルのコンパイル時埋め込み
│
├── runtime/             # AppState 構築・チャネル起動・ライフサイクル管理
├── agent_loop/          # LLM 対話ターン実行・プロンプト構築・セッション管理
├── channels/            # チャネル実装 (TUI / CLI / Web / Discord / Telegram)
├── llm/                 # LLM プロバイダー抽象化・Codex 認証
├── config/              # YAML 設定の読み込み・永続化・解決
├── storage/             # SQLite 永続化 (DB・マイグレーション・クエリ)
├── tools/               # ツールシステム (built-in + MCP)
├── setup/               # 初回セットアップウィザード
├── sleep/               # sleep batch 実行・スケジューラ
├── pulse/               # Pulse モジュール
│
├── memory.rs            # 長期記憶ファイルの読み込み
├── skills.rs            # スキル発見・読み込み・カタログ生成
├── slash_commands.rs    # スラッシュコマンド・LLM プロファイル管理
├── error.rs             # エラー型
├── test_env.rs          # テスト用環境ガード
└── test_util.rs         # テストユーティリティ
```

## 開発コマンド

```bash
# === Rust ===
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo llvm-cov --summary-only   # カバレッジ計測
cargo audit                     # 脆弱性スキャン
cargo deny check                # ライセンス・重複・脆弱性
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps  # ドキュメント lint

# === WebUI ===
npm install --prefix web
npm run build --prefix web
cd web && npm run dev

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
- メタ記述は一切書かない。（ユーザーの指摘された修正理由や、過去の状態など。「ユーザーにこういわれたからこうした」的なやつ）

#### ドキュメント一覧

ほぼすべての内容をドキュメントにまとめている。コードベースExploreする前に最初に読むとよい。

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
- ブレインストーミングや壁打ち系など、**計画なし**で進めた実装: mainブランチで作業やプッシュしてよい
- Issue, Planなど、**計画あり**で進めた実装: Git Worktreeを作成しその中で作業する（`worktree-create` skill使用）
- PR description は日本語。該当Issueがある場合は `Close #XX` 明記
- PR レビューはCoderabbitが自動で提供。PR作成後10分程度の時間差あり。PR作成後、`sleep 15m`をした後、レビューバック用Skill（`pr-review-back-workflow`）を実行する。

## Plan作成方針

- Planのスコープ: WT作成 -> 実装・コミット(TDD Cycle) -> 自己レビュー -> PR作成 -> PRレビュー対応
- 計画には必ずUTや動作確認などの検証を入れる
- Planの最後には、必ず実装内容とPlanとの照合（自己レビュー）をおこなう。メタ的視点で自分の作業を見直すことで作業の抜け漏れを防ぐ。**最重要**。

- プラン自体のレビューは以下の方法でCodexにレビュー依頼する。最大3回

初回:
```bash
codex exec -m gpt-5.5 "このプランをレビューして。致命的な点だけ指摘して: {plan_path}"
```
更新:
```bash
codex exec resume --last -m gpt-5.5 "プランを更新したからレビューして。致命的な点だけ指摘して: {plan_path}"
```

## 禁止事項

- 虚偽報告は禁止。終わっていないタスクを完了扱いにしない。勝手な判断でタスクをスキップしない。
- トークンの無駄なので、「Exploreタスクを委譲中に自分でもコードベースを探索すること」は禁止
- 最大並列委譲数は2程度が目安。4以上は一般的に過剰になりやすい。
- 現状の変更差分を確認せずにcheckoutで履歴を戻すことは禁止。ユーザーが意図的に残している差分がある可能性があるため。
- `#[allow(dead_code)]` は絶対に禁止。何があっても使用しない。デッドコードは削除する。テストからしか呼ばれていないものも本質的にはデッドコードなので削除する。

### セキュリティ

- `.env` 系・ローカル秘密設定ファイルの読み取り禁止。秘密が必要な場合はユーザーに明示してもらう
- `~/.egopulse/egopulse.config.yaml` も読み取り禁止

## メンタリティ

- 重要なのは「早く終わること」ではなく、「確実に品質を担保すること」。時間はいくらかかってもよいので、正確で質の高い作業を行うこと。どれだけ早く終わろうと、クソコードを増やすのは何もしていないのと同じ。
