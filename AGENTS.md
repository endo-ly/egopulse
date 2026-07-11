# EgoPulse

Self-hosted AI agent runtime (Rust/Tokio). TUI / Web UI / Discord / Telegram in a single binary.

## モジュール構成（責務別）

リポジトリは `src/`(Rust 単一バイナリ) / `web/`(React Web UI) / `docs/`(仕様書) / `scripts/`(セットアップ) からなる。以下は `src/` の主要モジュール。

- **Agent-First 設計**: `agent_id` を支配的識別子とし、同一ランタイム上に複数のエージェントが独立した記憶を持って並立・委譲し合う。チャネルもツールもすべてエージェントに紐づく
- `main.rs` / `lib.rs` - CLI エントリポイント（`chat` / `run` / `ask` / `setup` / `gateway`）とモジュール公開インターフェース
- `runtime/` - AppState 構築・チャネル起動・ライフサイクル管理。Web / Discord / Telegram を tokio task として同時起動し、graceful shutdownで安全停止。TurnScheduler による同時実行制御と暴走防止も担う
- `agent_loop/` - 会話ターン処理。システムプロンプト構築・LLM 呼び出し・ツール実行を最大 50 イテレーション繰り返し、セッションのロード/保存・compactionを行う
- `channels/` - チャネル実装。Web（Axum + SSE/WebSocket）/ Discord / Telegram / TUI / CLI を統一インターフェース（ChannelAdapter）で扱う
- `llm/` - LLM プロバイダー抽象化（OpenAI 互換 API）と Codex 認証の解決
- `config/` - YAML 設定（`~/.egopulse/egopulse.config.yaml`）の読み込み・永続化・モデル/チャネル解決。SecretRef による秘密参照もここ
- `storage/` - SQLite（WAL モード）永続化。会話・メッセージ・セッション・ツール呼び出し・LLM 利用量を保存。マイグレーション管理も含む
- `tools/` - ツールシステム。ファイル操作・シェル実行・検索・メッセージ送信などの built-in ツールに加え、MCP クライアントで外部ツールサーバーを接続。コマンド検閲・機密パスブロックも担う
- `setup/` - 初回セットアップウィザード
- `sleep/` - Sleep バッチ。会話履歴を episodic（エピソード）/ semantic（意味）/ prospective（展望）の 3 層に蒸留し、過去を整理して長期記憶へ昇格。手動実行とスケジューラ双方に対応
- `pulse/` - Pulse。時間・記憶・外界からの signal を受け取り、いま意識へ上げるべきものを選んで短く活性化
- `memory.rs` - 長期記憶ファイル（episodic / semantic / prospective）の読み込み
- `skills.rs` - スキル管理。発見・読み込み・カタログ生成。SOUL.md / AGENTS.md によるエージェント人格と規約の定義もここで読み込む
- `slash_commands.rs` - スラッシュコマンドのディスパッチと LLM プロファイル管理
- `error.rs` - エラー型

## 開発コマンド

```bash
# === Rust ===

# --- ローカル反復（実装中・軽量）---
# 目的: 変更箇所に絞ってビルド範囲を最小化し
cargo fmt --check
cargo clippy --lib              # 変更が main.rs 周りなら --bin egopulse に切替
cargo test --lib <test_name>    # テスト名 or モジュールパス(tools::files::) で絞り。全体は PR 前へ

# --- PR前 / CI（低頻度・フル）---
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo llvm-cov --summary-only   # カバレッジ計測
cargo audit                     # 脆弱性スキャン
cargo deny check                # ライセンス・重複・脆弱性
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps  # ドキュメント lint

# === WebUI ===
npm ci --prefix web          # CI と同じ install
npm run typecheck --prefix web
npm test --prefix web
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
- **本来の目的を見失わない**こと。特にtestなどの失敗が続くと、目の前の問題解消に目的が移りがち。本来の目的が最も大事なため注意する。
- 場当たり的な対応は禁止（バグフォールバック、ビルド/テスト通過のためだけの本質的でない修正）
- 「後方互換」は負債。既存利用維持のための互換分岐や旧仕様フォールバックは追加しない。新仕様へ一直線に置き換える
- 公開範囲は最小化する。新規APIは`private`から始め、必要に応じて `pub(crate)` / `pub(super)` を選択する。`pub` は binary entrypoint や外部公開が必要な場合だけ使う
- 大きめの実装・リファクタ後は `cargo clippy --all-targets --all-features -- -D warnings` に加え、公開範囲起因の dead code を疑って確認する
- PR前は `fmt`, `clippy`, `test` 必須。ただし重いため、開発時は最小限にとどめる。
- その他ルール
  - Logging: `tracing` を使用、機密情報禁止
  - Error: `thiserror` で構造化、`Display` は lower-case
  - Doc: public item に doc comment、`# Errors` / `# Panics` / `# Safety`
  - Test: AAA パターン必須

### 文書

- 文書作成時はどんな時でも **MECE** を意識する（セクション構成、要件定義、設計書、PR詳細などすべて）
- 実装後、関連内容が `docs/` にある場合は必ず反映する
- メタ記述は一切書かない。（直近の文脈に引っ張られた記述、ユーザーの指摘された修正理由や、過去の状態、「phaseNでは~」のようなその時の計画だけの文脈 など）
- 文書を書く際は、視野を広く、大局観を意識。あとから見て違和感なく読めるように。

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

### 開発運用

- GitHub Flow: ブランチ `<type>/<desc>`
- コミット: Conventional Commits（英語）
- ワークフロー: `ci.yml`(テスト), `release.yml`(リリース)
- ブレインストーミングや壁打ち系など、**計画なし**で進めた実装: mainブランチで作業してよい
- Issue, Planなど、**計画あり**で進めた実装: Git Worktreeを作成しその中で作業する（`worktree-create` skill使用）
- PR description は日本語。該当Issueがある場合は `Close #XX` 明記
- PR レビューはCoderabbitが自動で提供。PR作成後10分程度の時間差あり。PR作成後、`sleep 15m`をした後、レビューバック用Skill（`pr-review-back-workflow`）を実行する。
#### Plan
  - Planのスコープ: WT作成 -> 実装・コミット(TDD Cycle) -> 自己レビュー -> PR作成 -> PRレビュー対応
  - 計画には必ずUTや動作確認などの検証を入れる

- プラン自体のレビューは以下の方法でCodexにレビュー依頼する。最大3回。ただしあなたがGPT系モデルの場合はレビュー不要。

初回:
```bash
codex exec -m gpt-5.5 "このプランをレビューして。致命的な点だけ指摘して: {plan_path}"
```
更新:
```bash
codex exec resume --last -m gpt-5.5 "プランを更新したからレビューして。致命的な点だけ指摘して: {plan_path}"
```

#### Review

- PR作成前には必ず自己レビューを行う。

- **目的**: 自分が持つコンテキスト（Plan・設計意図）を活かし、CodeRabbit（他者レビュー）が見る前に、自分で見つけられる実装不正をすべて見つけて潰すこと。実装者だからこそ効率よく発見できる類の欠陥（意図からの乖離・可視性の過剰・デッドコード・テストの assert 実効性・docs 整合）を、自分の手で撲滅する。
- **完了の定義**: 上記の目的を達成したとき。証跡（チェックリスト・行番号など）は目的を達成する**手段**であり、リストを埋めることが完了ではない。ゲートを満たすための形式的証跡はレビューではなく rubber-stamp である。
- **整合性チェック**: 本物のレビューなら、コンテキストを持つ者が実装を見直せばほぼ必ず何か見つかる。「何も見つからなかった」場合は未レビュー扱いと考える。
- **補助観点**（網羅目標ではなく目的達成の補助）: テストが約束した振る舞いを本当に assert しているか / 可視性が最小か / 「共有コア」が両経路から本当に呼ばれているか / テストからしか呼ばれないコードがないか / docs とコードの整合 / Plan「対象一覧」と実際の diff の照合。

## 禁止事項

- 虚偽報告は禁止。終わっていないタスクを完了扱いにしない。勝手な判断でタスクをスキップしない。
- トークンの無駄なので、「Exploreタスクを委譲中に自分でもコードベースを探索すること」は禁止
- 最大並列委譲数は2程度が目安。4以上は一般的に過剰になりやすい。
- 現状の変更差分を確認せずにcheckoutで履歴を戻すことは禁止。ユーザーが意図的に残している差分がある可能性があるため。
- `#[allow(dead_code)]` は絶対に禁止。何があっても使用しない。デッドコードは削除する。テストからしか呼ばれていないものも本質的にはデッドコードなので削除する。

### セキュリティ

- `.env` 系・ローカル秘密設定ファイルの読み取り禁止。秘密が必要な場合はユーザーに明示してもらう
- `~/.egopulse/egopulse.config.yaml` も言われない限りは読み取り禁止

## さいごに

- 重要なのは「早く終わること」ではなく、「確実に品質を担保すること」。時間はいくらかかってもよいので、正確で質の高い作業を行うこと。どれだけ早く終わろうと、クソコードを増やすのは何もしていないのと同じ。
