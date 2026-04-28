# Plan: OpenAI Codex OAuth 認証プロバイダー追加

OpenAI Codex CLI の OAuth トークン（`~/.codex/auth.json`）を読み取り、ChatGPT サブスクリプション経由で OpenAI モデルを利用可能にする新プロバイダー `openai-codex` を追加する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **既存パターンへの追従**: `ProviderConfig` / `ResolvedLlmConfig` / `OpenAiProvider` の既存アーキテクチャに乗る。特別扱いのif分岐は最小限にし、汎用的な拡張ポイント（`auth_type` 等）として設計する。
- **codex CLI への認証委任**: OAuth フローそのものは実装せず、`codex login` が生成した `~/.codex/auth.json` を読み取る方式（案A）。JWT の期限チェックと refresh_token による自動更新のみ行う。
- **Responses API 専用**: Codex プロバイダーは `chatgpt.com/backend-api/codex/responses` を叩くため、Chat Completions ではなく常に Responses API を使用する。
- **既存プロバイダーへの非影響**: 標準 `openai` プロバイダーをはじめ、既存の全プロバイダーに一切影響しない。`openai-codex` は独立したプロバイダーIDとして追加する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| codex 認証モジュール（token 読み取り / JWT期限チェック / 自動リフレッシュ） | **新規** `src/codex_auth.rs` |
| プロバイダー設定の拡張（api_key 不要なプロバイダー判定） | **変更** `src/config/loader.rs` |
| プロバイダー設定構造体（auth_type / account_id の伝播） | **変更** `src/config/mod.rs`, `src/config/resolve.rs` |
| OpenAiProvider の codex 対応（Responses API強制 / account_id ヘッダー） | **変更** `src/llm/openai.rs` |
| セットアップウィザード用プロバイダープリセット | **変更** `src/setup/provider.rs` |
| 設定仕様ドキュメント | **変更** `docs/config.md` |

---

## Step 0: Worktree 作成

`worktree-create` skill を使用して `feat/codex-oauth` ブランチの Worktree を作成。

---

## Step 1: codex_auth モジュール (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `resolve_auth_prefers_env_var` | `OPENAI_CODEX_ACCESS_TOKEN` が設定されていれば env var を優先して返す |
| `resolve_auth_reads_access_token` | `~/.codex/auth.json` の `tokens.access_token` を読み取る |
| `resolve_auth_falls_back_to_openai_api_key` | access_token がない場合 `OPENAI_API_KEY` フィールドにフォールバック |
| `resolve_auth_returns_account_id` | `tokens.account_id` も返す |
| `resolve_auth_errors_when_no_source` | env var も auth file もない場合はエラー |
| `is_jwt_expired_returns_true_for_expired` | `exp` が過去の JWT を expired と判定 |
| `is_jwt_expired_returns_false_for_valid` | `exp` が未来の JWT を valid と判定 |
| `is_jwt_expired_returns_false_for_malformed` | 不正な JWT は expired と判定しない |
| `refresh_writes_new_access_token` | refresh_token で新しい access_token を取得し auth.json を更新 |
| `refresh_skips_when_not_expired` | 有効期限内ならリフレッシュしない |
| `refresh_skips_when_no_file` | auth.json が存在しない場合は何もしない |
| `default_auth_path_uses_codex_home` | `CODEX_HOME` env var でパスを上書き可能 |
| `provider_allows_empty_api_key` | `openai-codex` は api_key 不要と判定 |

### GREEN: 実装

新規ファイル `src/codex_auth.rs`:

- `resolve_codex_auth()` → `CodexAuth { bearer_token, account_id }`
  - 優先度: `OPENAI_CODEX_ACCESS_TOKEN` env → `~/.codex/auth.json` tokens.access_token → 同 OPENAI_API_KEY
- `is_jwt_expired(token)` → JWT ペイロードの `exp` を base64 デコードして現在時刻と比較
- `refresh_if_needed()` → `https://auth.openai.com/oauth/token` に `grant_type=refresh_token` + `client_id` を POST。成功時 auth.json を更新
- `default_codex_auth_path()` → `$CODEX_HOME/auth.json` or `~/.codex/auth.json`
- `provider_requires_codex_auth(provider: &str) -> bool`
- `provider_allows_empty_api_key(provider: &str) -> bool`

参考: `base64::Engine`, `serde_json`, `reqwest::blocking`（リフレッシュは同期I/O）

### コミット

`feat: add codex_auth module for reading ~/.codex/auth.json`

---

## Step 2: 設定レイヤー拡張 — api_key 不要なプロバイダー (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `loads_openai_codex_without_api_key` | `openai-codex` プロバイダーで `api_key` 未設定でも config 読み込み成功 |
| `openai_codex_uses_default_base_url` | base_url 未設定時のデフォルトが `https://chatgpt.com/backend-api/codex` |
| `openai_codex_custom_base_url` | ユーザーが base_url を明示指定した場合はそちらを使用 |
| `resolved_llm_config_carries_provider_name` | `ResolvedLlmConfig.provider` に `openai-codex` が伝播する |

### GREEN: 実装

- `src/config/loader.rs` の `base_url_allows_empty_api_key()` を拡張: プロバイダーIDベースの判定に変更
  - 従来: base_url が localhost かどうか
  - 新規: 加えて provider id が `openai-codex` の場合も許可
- `src/config/loader.rs` の `build_provider_config()` で、`openai-codex` の base_url デフォルト値を注入
- `ProviderConfig` 構造体は変更不要（`api_key: Option<ResolvedValue>` のまま、None で許容）

### コミット

`feat: allow openai-codex provider without api_key in config`

---

## Step 3: OpenAiProvider の codex 対応 (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `codex_provider_resolves_auth_from_file` | mock した auth.json から bearer_token を取得し Authorization ヘッダーにセット |
| `codex_provider_includes_account_id_header` | account_id がある場合 `ChatGPT-Account-ID` ヘッダーを付与 |
| `codex_provider_uses_responses_api` | codex プロバイダーは常に `/responses` エンドポイントにリクエスト |
| `codex_provider_skips_chat_completions` | codex プロバイダーは `/chat/completions` を呼ばない |
| `codex_provider_refreshes_expired_token` | 期限切れ JWT を検知してリフレッシュ後にリクエスト |
| `codex_provider_works_without_account_id` | account_id がなくても動作する |

### GREEN: 実装

`src/llm/openai.rs` の `OpenAiProvider` を拡張:

- フィールド追加: `account_id: Option<String>`
- `new()` で provider が `openai-codex` の場合:
  - `codex_auth::resolve_codex_auth()` を呼び出し bearer_token を `api_key` としてセット
  - `account_id` を保存
  - 期限切れの場合は `codex_auth::refresh_if_needed()` を呼び出してから再度 resolve
- `build_headers()` で `account_id` がある場合 `ChatGPT-Account-ID` ヘッダーを追加
- `send_message()` / `send_message_stream()` で、codex プロバイダーの場合は常に `send_message_via_responses()` を使用

### コミット

`feat: integrate codex auth into OpenAiProvider`

---

## Step 4: セットアッププリセット + 設定ドキュメント (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `codex_preset_has_correct_id` | プリセット ID が `openai-codex` |
| `codex_preset_has_correct_base_url` | デフォルト base_url が `https://chatgpt.com/backend-api/codex` |
| `codex_preset_has_codex_models` | モデル一覧に codex モデルが含まれる |

### GREEN: 実装

- `src/setup/provider.rs` の `PROVIDER_PRESETS` に `openai-codex` を追加
- `docs/config.md` に `openai-codex` プロバイダーの設定例を追記

### コミット

`feat: add openai-codex provider preset and docs`

---

## Step 5: 動作確認

- `cargo test -p egopulse` — 全テスト通過
- `cargo fmt --check` — フォーマット確認
- `cargo clippy --all-targets --all-features -- -D warnings` — Lint 確認
- 手動確認（任意）: `codex login` 済みの環境で `openai-codex` プロバイダーを設定し `cargo run -- chat` で動作確認

---

## Step 6: PR 作成

- PR description は日本語
- conventional commits に従ったコミット履歴

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/codex_auth.rs` | **新規** | Codex OAuth トークン読み取り・リフレッシュ・JWT 期限チェック |
| `src/lib.rs` or `src/main.rs` | 変更 | `mod codex_auth` 追加 |
| `src/config/loader.rs` | 変更 | `base_url_allows_empty_api_key` の拡張、codex プロバイダー用デフォルト base_url 注入 |
| `src/config/mod.rs` | 変更 | （必要に応じて）codex provider 判定ヘルパーの再エクスポート |
| `src/llm/openai.rs` | 変更 | `account_id` フィールド追加、codex auth 解決、Responses API 強制、account_id ヘッダー |
| `src/setup/provider.rs` | 変更 | `openai-codex` プリセット追加 |
| `docs/config.md` | 変更 | `openai-codex` プロバイダーの設定例 |
| `Cargo.toml` | 変更 | `base64` dependency 追加（JWT デコード用） |

---

## コミット分割

1. `feat: add codex_auth module for reading ~/.codex/auth.json` — `src/codex_auth.rs`, `src/lib.rs`
2. `feat: allow openai-codex provider without api_key in config` — `src/config/loader.rs`
3. `feat: integrate codex auth into OpenAiProvider` — `src/llm/openai.rs`
4. `feat: add openai-codex provider preset and docs` — `src/setup/provider.rs`, `docs/config.md`

---

## テストケース一覧（全 25 件）

### codex_auth (13)
1. `resolve_auth_prefers_env_var` — OPENAI_CODEX_ACCESS_TOKEN env var を優先
2. `resolve_auth_reads_access_token` — ~/.codex/auth.json の access_token を読み取り
3. `resolve_auth_falls_back_to_openai_api_key` — access_token がない場合 OPENAI_API_KEY にフォールバック
4. `resolve_auth_returns_account_id` — account_id も返す
5. `resolve_auth_errors_when_no_source` — env var も auth file もない場合はエラー
6. `is_jwt_expired_returns_true_for_expired` — 期限切れ JWT の判定
7. `is_jwt_expired_returns_false_for_valid` — 有効期限内 JWT の判定
8. `is_jwt_expired_returns_false_for_malformed` — 不正 JWT は expired と判定しない
9. `refresh_writes_new_access_token` — リフレッシュで auth.json を更新
10. `refresh_skips_when_not_expired` — 有効期限内はリフレッシュしない
11. `refresh_skips_when_no_file` — auth.json なしでは何もしない
12. `default_auth_path_uses_codex_home` — CODEX_HOME env var によるパス上書き
13. `provider_allows_empty_api_key` — openai-codex は api_key 不要

### config (4)
14. `loads_openai_codex_without_api_key` — api_key なしで config 読み込み成功
15. `openai_codex_uses_default_base_url` — デフォルト base_url の検証
16. `openai_codex_custom_base_url` — ユーザー指定 base_url の反映
17. `resolved_llm_config_carries_provider_name` — provider 名の伝播

### llm/openai (6)
18. `codex_provider_resolves_auth_from_file` — auth.json からの bearer_token 取得
19. `codex_provider_includes_account_id_header` — ChatGPT-Account-ID ヘッダー
20. `codex_provider_uses_responses_api` — 常に /responses エンドポイント使用
21. `codex_provider_skips_chat_completions` — /chat/completions を呼ばない
22. `codex_provider_refreshes_expired_token` — 期限切れ時のリフレッシュ
23. `codex_provider_works_without_account_id` — account_id なしでの動作

### setup/provider (2)
24. `codex_preset_has_correct_base_url` — プリセット base_url 検証
25. `codex_preset_has_codex_models` — プリセットモデル一覧検証

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | codex_auth モジュール（本体 + テスト） | ~350 行 |
| Step 2 | config 拡張（loader + テスト） | ~80 行 |
| Step 3 | OpenAiProvider codex 対応（本体 + テスト） | ~150 行 |
| Step 4 | プリセット + ドキュメント | ~50 行 |
| **合計** | | **~630 行** |
