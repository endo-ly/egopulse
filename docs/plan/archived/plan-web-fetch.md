# Plan: web_fetch built-in tool の追加

安全な Web ページ取得プリミティブを EgoPulse の built-in tool として追加する。URL 検証・SSRF 対策・Markdown 変換・コンテンツ検査・FeedSync・続きを読む機能を含む。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **MicroClaw 参照実装をベースに移植しつつ EgoPulse の要件に拡張**: URL 検証・redirect 検証・コンテンツ検査(regex)はほぼそのまま。HTML→Markdown 変換は `htmd` クレートで置き換え。SSRF 対策は新規追加
- **SSRF 対策を最優先**: DNS 解決後の IP アドレス分類で private/loopback/link-local/metadata をデフォルトブロック。redirect 先も都度検証
- **htmd による HTML→Markdown 変換**: MicroClaw の自前 `html_to_text` は plaintext 変換しかしないため、`htmd`（html5ever ベース）で heading/list/link/blockquote/code 等を適切に Markdown 化。`skip_tags` で script/style/nav 等を除去
- **ステートレスな start_index ページネーション**: 長いページの「続きを読む」のため `start_index` パラメータを追加。毎回 URL から再取得・再変換するがステート管理が不要
- **既存パターンに一貫**: `Tool` trait 実装、`ToolResult` の `content`(文字列) + `details`(JSON) 形式、`sanitize_tool_result()` 経由の secret redaction は既存ツールと同じ
- **設定はホットリロード可能**: `WebFetchTool` は設定を直接保持せず、`execute()` のたびに `ToolExecutionContext` 経由で最新の `Arc<Config>` を参照して `web_fetch` 設定を引く。これにより設定変更が再起動なしでツールに反映される

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| `WebFetchConfig` 設定型 + Config 統合 | 新規 + 既存 Config 変更 |
| URL 検証（scheme / host / SSRF / redirect） | MicroClaw 拡張移植 |
| コンテンツ検査（prompt injection regex） | MicroClaw 移植 |
| HTML 処理（extract_primary_html + htmd ラッパー） | MicroClaw 移植 + htmd |
| FeedSync（外部 URL からの denylist/allowlist 同期） | MicroClaw 移植 |
| `WebFetchTool` (Tool trait 実装) | 新規 |
| ToolRegistry への登録 | 既存変更 |
| docs/tools.md / docs/config.md 更新 | 既存変更 |

---

## Step 0: Worktree 作成

Issue #35 に紐づくブランチで worktree を作成する。

```
git worktree add ../egopulse-35 feat/35-web-fetch-tool
```

---

## Step 1: 設定型の定義 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `config_default_values` | WebFetchConfig のデフォルト値検証（timeout=15, max_bytes=20000, schemes=["https"], allow_private_ips=false, content_validation.enabled=true, feed_sync.enabled=false） |
| `config_deserialize_full_yaml` | 全フィールドを指定した YAML からのデシリアライズ |
| `config_deserialize_missing_optional` | web_fetch セクションなしでデシリアライズ → デフォルト値 |
| `config_normalize_empty_schemes` | allowed_schemes が空 → デフォルト ["https"] にフォールバック |
| `config_normalize_hosts` | host の大文字小文字・空白・末尾ドット正規化 |
| `config_normalize_zero_max_bytes` | max_bytes=0 → デフォルト値 |
| `feed_source_normalize` | FeedSource の空 URL 除外・ゼロ値フォールバック |

### GREEN: 実装

`src/config/web_fetch.rs` に以下の型を定義:

- `WebFetchConfig`: トップレベル設定
- `WebFetchContentValidationConfig`: コンテンツ検査設定
- `WebFetchFeedSyncConfig` / `WebFetchFeedSource`: FeedSync 設定

`Config` 構造体に `web_fetch: WebFetchConfig` フィールドを追加（`#[serde(default)]`）。

**Config 統合（loader / persist / Web API）**:

現行 Config は手書きの loader / persist / Web API で管理されているため、以下の対応が必須:

- `src/config/loader.rs`: `FileConfig` に `web_fetch` フィールド追加、`build_config()` で WebFetchConfig 構築
- `src/config/persist.rs`: `SerializableConfig` に `web_fetch` フィールド追加、`From<&Config>` 実装更新
- `src/config/mod.rs`: モジュール公開
- `src/config/types.rs`: Config 構造体にフィールド追加
- `src/channels/web/config.rs`: WebUI の ConfigPayload / ConfigUpdateRequest は現在 web_fetch を扱わない（読み取り専用設定）ため、保存時に `web_fetch` セクションが消えないよう persist ロジックでの保持を確認

`test_config()` ヘルパーに `web_fetch: WebFetchConfig::default()` を追加。

`Cargo.toml` に `htmd = "0.5"` を追加。

### コミット

`feat(web_fetch): add WebFetchConfig types with defaults and normalization`

---

## Step 2: URL 検証 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `allows_https_by_default` | `https://example.com` → Ok |
| `blocks_http_by_default` | `http://example.com` → Err("not allowed") |
| `allows_http_when_configured` | `allowed_schemes: ["https","http"]` → Ok |
| `blocks_ftp_scheme` | `ftp://example.com` → Err |
| `blocks_invalid_url` | `not a url` → Err |
| `blocks_url_without_host` | `https:///path` → Err |
| `blocks_denylist_host` | denylist: ["example.com"] → Err |
| `blocks_denylist_subdomain` | denylist: ["example.com"], url: `sub.example.com` → Err |
| `allows_denylist_unrelated` | denylist: ["bad.com"], url: `good.com` → Ok |
| `enforces_allowlist` | allowlist: ["ok.com"] → ok.com Ok, other.com Err |
| `denylist_precedes_allowlist` | 両方に same host → denylist 優先 |
| `validation_disabled_allows_all` | enabled: false → ftp も Ok |
| `ssrf_blocks_loopback` | `https://127.0.0.1` → Err("private/loopback") |
| `ssrf_blocks_private_10` | `https://10.0.0.1` → Err |
| `ssrf_blocks_private_172_16` | `https://172.16.0.1` → Err |
| `ssrf_blocks_private_192_168` | `https://192.168.1.1` → Err |
| `ssrf_blocks_link_local` | `https://169.254.1.1` → Err |
| `ssrf_blocks_cloud_metadata` | `https://169.254.169.254` → Err |
| `ssrf_blocks_localhost` | `https://localhost` → Err |
| `ssrf_allows_when_flag_enabled` | allow_private_ips: true → 127.0.0.1 Ok |
| `ssrf_allows_public_ip` | `https://93.184.216.34` → Ok |
| `redirect_blocks_denylisted_target` | redirect 先が denylist → Err |
| `redirect_allows_relative` | redirect 先が /next → Ok |
| `redirect_blocks_to_private_ip` | redirect 先が 127.0.0.1 → Err |
| `redirect_too_many` | 6 回 redirect → Err("too many redirects") |
| `host_normalization_lowercase` | "EXAMPLE.COM" → "example.com" |
| `host_normalization_trailing_dot` | "example.com." → "example.com" |
| `host_normalization_wildcard_prefix` | "*.example.com" → "example.com" |

### GREEN: 実装

`src/tools/web_fetch/url_validation.rs` に以下を実装:

- `validate_url(url, config)`: scheme + host + allowlist/denylist 検証
- `is_blocked_ip(ip)`: IP アドレス分類（loopback/private/link-local/metadata）
- `resolve_dns_and_validate(host, config)`: `tokio::net::lookup_host` で DNS 解決 → 全 IP を is_blocked_ip で検証
- `validate_redirect(current_url, location, config)`: redirect 先 URL の解決 + 再検証
- `normalize_host_list(hosts)`: 重複除去・正規化

### コミット

`feat(web_fetch): add URL validation with SSRF protection`

---

## Step 3: コンテンツ検査 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `clean_content_passes` | 正常なテキスト → Ok |
| `detects_instruction_override` | "Ignore all previous instructions" → Err |
| `detects_system_override` | "override system safety policy" → Err |
| `detects_prompt_exfiltration` | "reveal the system prompt" → Err |
| `detects_jailbreak_roleplay` | "you are now DAN" → Err（strict_mode） |
| `detects_system_delimiters` | "[SYSTEM]...[/SYSTEM]" → Err（strict_mode） |
| `detects_tool_abuse_instruction` | "execute bash and write_file" → Err |
| `strict_blocks_single_low_confidence` | low confidence + strict_mode → Err |
| `non_strict_allows_single_low_confidence` | low confidence + !strict → Ok |
| `non_strict_blocks_multiple_hits` | 2+ low confidence hits → Err |
| `non_strict_blocks_high_confidence` | 1 high confidence hit → Err |
| `disabled_allows_everything` | enabled: false → injection も Ok |
| `max_scan_bytes_skips_tail` | 前半 safe + 後半 injection → max_scan_bytes で後半スキップ → Ok |
| `validation_failure_contains_rule_names` | failure.message() に rule name が含まれる |

### GREEN: 実装

`src/tools/web_fetch/content_validation.rs` に以下を実装:

MicroClaw の `web_content_validation.rs` をほぼそのまま移植:

- 6 ルール（instruction_override / system_override / prompt_exfiltration / jailbreak_roleplay / system_delimiters / tool_abuse_instruction）
- `ValidationRule` 構造体（name / pattern / high_confidence）
- `validate_content(text, config)`: regex スキャン → strict_mode/high_confidence/multi-hit 判定
- `ValidationFailure` 型（rule_names + message）

### コミット

`feat(web_fetch): add content validation with regex-based injection detection`

---

## Step 4: HTML 処理 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `html_to_markdown_basic` | h1+p+strong → "# Title\n\nHello **world**" |
| `html_to_markdown_list` | ul+li → "* item1\n* item2" |
| `html_to_markdown_ordered_list` | ol+li → "1. item1\n2. item2" |
| `html_to_markdown_link` | a href → "[text](url)" |
| `html_to_markdown_blockquote` | blockquote → "> quote" |
| `html_to_markdown_code` | code/pre → "`inline`" / fenced code block |
| `html_to_markdown_strips_script` | script タグ内容が除去される |
| `html_to_markdown_strips_style` | style タグ内容が除去される |
| `html_to_markdown_strips_nav` | nav タグ内容が除去される |
| `extract_primary_prefers_main` | body + main 両方 → main の内容 |
| `extract_primary_prefers_article` | body + article → article の内容 |
| `extract_primary_falls_back_to_body` | body のみ → body の内容 |
| `extract_primary_falls_back_to_full_html` | タグなし → 全文 |
| `content_type_html_routes_to_markdown` | Content-Type: text/html → Markdown 変換 |
| `content_type_plain_returns_as_is` | Content-Type: text/plain → そのまま |
| `content_type_json_returns_as_is` | Content-Type: application/json → そのまま |
| `content_type_missing_routes_to_markdown` | Content-Type なし → HTML として扱う |

### GREEN: 実装

`src/tools/web_fetch/html_processing.rs` に以下を実装:

- `extract_primary_html(html)`: `<main>` > `<article>` > `<body>` 優先抽出。見つからない → 全文
- `html_to_markdown(html)`: extract_primary_html → `htmd::HtmlToMarkdownBuilder::new().skip_tags(...).build().convert()`
- `process_response_body(body, content_type)`: Content-Type で分岐。text/html → html_to_markdown、それ以外 → そのまま

### コミット

`feat(web_fetch): add HTML processing with htmd-based Markdown conversion`

---

## Step 5: FeedSync (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `inline_feed_merges_denylist` | inline: ソース → denylist にマージ |
| `inline_feed_merges_allowlist` | inline: ソース → allowlist にマージ |
| `csv_first_column` | CSV フォーマット → 1 列目をホストとして抽出 |
| `skips_disabled_sources` | enabled: false → スキップ |
| `skips_comment_lines` | `#` 行をスキップ |
| `deduplicates_hosts` | 重複ホストが 1 つに |
| `fails_closed_on_error` | fail_open: false + 到達不能 URL → Err |
| `fails_open_on_error` | fail_open: true + 到達不能 URL → Ok（空リスト） |
| `feed_sync_disabled_is_noop` | feed_sync.enabled: false → 設定変更なし |
| `normalizes_feed_hosts` | フィード内ホストの正規化（大文字・空白） |
| `max_entries_per_source` | 上限で打ち切り |

### GREEN: 実装

`src/tools/web_fetch/feed_sync.rs` に以下を実装:

MicroClaw から移植:

- `WebFetchFeedMode` (Allowlist/Denylist), `WebFetchFeedFormat` (Lines/CsvFirstColumn)
- `fetch_feed_entries(source, ...)`: インライン or HTTP 取得 → パース
- `resolve_feed_sync(config)`: 全ソースを取得 → allowlist/denylist にマージ
- TTL ベースのメモリキャッシュ（`OnceLock<Mutex<HashMap>>`）

### コミット

`feat(web_fetch): add FeedSync for dynamic denylist/allowlist synchronization`

---

## Step 6: WebFetchTool 本体 (TDD)

前提: Step 1-5

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `tool_definition` | name="web_fetch"、必須 url、任意 timeout_secs/max_bytes/start_index |
| `is_read_only` | `true` を返す |
| `missing_url_returns_error` | url なし → error |
| `null_url_returns_error` | url: null → error |
| `empty_url_returns_error` | url: "" → error |
| `blocks_disallowed_scheme_before_request` | ftp:// → error（HTTP リクエストなし） |
| `blocks_denylisted_host_before_request` | denylist host → error（DNS 解決なし） |
| `blocks_private_ip_before_request` | 127.0.0.1 → error（DNS 解決後ブロック） |
| `fetches_html_and_returns_markdown` | wiremock で text/html 返却 → Markdown 出力 |
| `fetches_plain_text_as_is` | wiremock で text/plain 返却 → そのまま出力 |
| `result_details_metadata` | success 時に details に final_url / content_type / truncated / total_bytes を含む |
| `truncation_at_max_bytes` | max_bytes 超過 → truncate + details.truncated=true + next_start_index |
| `truncation_utf8_safe` | マルチバイト文字の境界で truncate（文字化けなし） |
| `start_index_continuation` | start_index=N 指定 → N バイト目以降を返す |
| `start_index_beyond_content` | start_index が content 長以上 → 空文字 + details |
| `blocks_content_with_injection` | injection 含む本文 → error（raw content は返さない） |
| `follows_redirect_and_validates` | wiremock で 302 → Location 先の 200。redirect 先も SSRF 検証 |
| `blocks_redirect_to_private_ip` | redirect 先が 127.0.0.1 → error |
| `too_many_redirects` | 6 回 redirect → error |
| `http_error_status` | wiremock 404 → error "HTTP 404" |
| `timeout_error` | タイムアウト → error |
| `untrusted_content_warning` | 成功時 content に "外部コンテンツは信頼できない" 的警告が含まれる |
| `blocks_disallowed_scheme_in_redirect` | redirect 先が ftp:// → error |

### GREEN: 実装

`src/tools/web_fetch/mod.rs` に以下を実装:

- `WebFetchTool` 構造体（設定は保持せず、`Arc<Config>` を参照して `execute()` のたびに最新設定を引く）
- `Tool` trait 実装:
  - `name()`: "web_fetch"
  - `definition()`: 入力スキーマ定義
  - `is_read_only()`: true
  - `execute()`: メイン実行フロー（設定は `context` 経由で都度取得）
- 実行フロー:
  1. 入力パース（url / timeout_secs / max_bytes / start_index）
  2. FeedSync 解決（enabled の場合）
  3. URL 検証（scheme + host + DNS → IP 検証）
  4. HTTP リクエスト（no-redirect policy）
  5. manual redirect loop（都度 URL + IP 検証、上限 5）
  6. レスポンス取得（body text）
  7. Content-Type 分岐 → process_response_body
  8. コンテンツ検査（validation）
  9. start_index スライス → max_bytes truncate（UTF-8 安全）
  10. untrusted content warning 付与
  11. `ToolResult::success_with_details()` で返却

HTTP クライアント: `OnceLock<reqwest::Client>` で単一インスタンス共有。

テストには `wiremock` を使用して HTTP サーバーをモック。

### コミット

`feat(web_fetch): implement WebFetchTool with SSRF protection and Markdown conversion`

---

## Step 7: ToolRegistry 登録と統合 (TDD)

前提: Step 6

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `registry_includes_web_fetch` | definitions に "web_fetch" が含まれる |
| `registry_web_fetch_is_read_only` | is_read_only("web_fetch") == true |
| `execute_dispatches_to_web_fetch` | registry.execute("web_fetch", ...) が動作 |
| `secret_redaction_on_fetch_result` | web_fetch 結果内の secret が redaction される |
| `config_web_fetch_reflected_in_tool` | Config の web_fetch 設定がツールに反映される |

### GREEN: 実装

- `ToolRegistry::new()` で `WebFetchTool` を `Box::new()` して tools ベクタに追加
- `WebFetchTool::new(config: Arc<Config>)` で Config 参照を注入（execute 時に最新 web_fetch 設定を引く）
- `src/tools/mod.rs` に `mod web_fetch;` を追加

### コミット

`feat(web_fetch): register WebFetchTool in ToolRegistry`

---

## Step 8: ドキュメント更新

### 対象ファイル

| ファイル | 変更内容 |
|---|---|
| `docs/tools.md` | web_fetch セクション追加（入力・挙動・エラー・details 仕様） |
| `docs/config.md` | web_fetch 設定フィールド追記 + ホットリロード対応表更新 |

### コミット

`docs: add web_fetch tool and config documentation`

---

## Step 9: 動作確認

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

---

## Step 10: PR 作成

- ブランチ: `feat/35-web-fetch-tool`
- PR description: 日本語、`Close #35` 明記
- レビュー待ち: Coderabbit 自動レビュー

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `Cargo.toml` | 変更 | `htmd = "0.5"` 追加 |
| `src/config/web_fetch.rs` | **新規** | WebFetchConfig 型群 |
| `src/config/mod.rs` | 変更 | web_fetch モジュール公開 + Config にフィールド追加 |
| `src/config/types.rs` | 変更 | Config 構造体に `web_fetch` フィールド追加 |
| `src/config/loader.rs` | 変更 | FileConfig に web_fetch 追加 + build_config で WebFetchConfig 構築 |
| `src/config/persist.rs` | 変更 | SerializableConfig に web_fetch 追加 + From<&Config> 更新 |
| `src/channels/web/config.rs` | 変更 | web_fetch セクションの保持確認（保存時消失防止） |
| `src/tools/web_fetch/mod.rs` | **新規** | WebFetchTool (Tool trait impl) |
| `src/tools/web_fetch/url_validation.rs` | **新規** | URL 検証 + SSRF 対策 |
| `src/tools/web_fetch/content_validation.rs` | **新規** | コンテンツ検査 |
| `src/tools/web_fetch/html_processing.rs` | **新規** | HTML 処理 + htmd ラッパー |
| `src/tools/web_fetch/feed_sync.rs` | **新規** | FeedSync |
| `src/tools/mod.rs` | 変更 | `mod web_fetch` 追加 + ToolRegistry へ登録 |
| `src/test_util.rs` | 変更 | test_config に web_fetch 追加 |
| `docs/tools.md` | 変更 | web_fetch セクション追加 |
| `docs/config.md` | 変更 | web_fetch 設定追記 |

---

## コミット分割

1. `feat(web_fetch): add WebFetchConfig types with defaults and normalization` — `Cargo.toml`, `src/config/web_fetch.rs`, `src/config/mod.rs`, `src/config/types.rs`, `src/config/loader.rs`, `src/config/persist.rs`, `src/channels/web/config.rs`, `src/test_util.rs`
2. `feat(web_fetch): add URL validation with SSRF protection` — `src/tools/web_fetch/url_validation.rs`
3. `feat(web_fetch): add content validation with regex-based injection detection` — `src/tools/web_fetch/content_validation.rs`
4. `feat(web_fetch): add HTML processing with htmd-based Markdown conversion` — `src/tools/web_fetch/html_processing.rs`
5. `feat(web_fetch): add FeedSync for dynamic denylist/allowlist synchronization` — `src/tools/web_fetch/feed_sync.rs`
6. `feat(web_fetch): implement WebFetchTool with SSRF protection and Markdown conversion` — `src/tools/web_fetch/mod.rs`
7. `feat(web_fetch): register WebFetchTool in ToolRegistry` — `src/tools/mod.rs`
8. `docs: add web_fetch tool and config documentation` — `docs/tools.md`, `docs/config.md`

---

## テストケース一覧（全 89 件）

### 設定型 (7)

1. `config_default_values` — デフォルト値の検証
2. `config_deserialize_full_yaml` — 全フィールド指定時のデシリアライズ
3. `config_deserialize_missing_optional` — web_fetch セクションなし → デフォルト
4. `config_normalize_empty_schemes` — 空 schemes → デフォルト
5. `config_normalize_hosts` — host 正規化
6. `config_normalize_zero_max_bytes` — max_bytes=0 → デフォルト
7. `feed_source_normalize` — FeedSource 正規化

### URL 検証 (28)

8. `allows_https_by_default` — https 許可
9. `blocks_http_by_default` — http ブロック
10. `allows_http_when_configured` — 設定で http 許可
11. `blocks_ftp_scheme` — ftp ブロック
12. `blocks_invalid_url` — 不正 URL ブロック
13. `blocks_url_without_host` — host なしブロック
14. `blocks_denylist_host` — denylist ホストブロック
15. `blocks_denylist_subdomain` — サブドメインもブロック
16. `allows_denylist_unrelated` — 関係ないホストは許可
17. `enforces_allowlist` — allowlist 強制
18. `denylist_precedes_allowlist` — denylist 優先
19. `validation_disabled_allows_all` — disabled ですべて許可
20. `ssrf_blocks_loopback` — 127.0.0.1 ブロック
21. `ssrf_blocks_private_10` — 10.x ブロック
22. `ssrf_blocks_private_172_16` — 172.16.x ブロック
23. `ssrf_blocks_private_192_168` — 192.168.x ブロック
24. `ssrf_blocks_link_local` — 169.254.x ブロック
25. `ssrf_blocks_cloud_metadata` — 169.254.169.254 ブロック
26. `ssrf_blocks_localhost` — localhost ブロック
27. `ssrf_allows_when_flag_enabled` — allow_private_ips 時許可
28. `ssrf_allows_public_ip` — パブリック IP 許可
29. `redirect_blocks_denylisted_target` — redirect 先 denylist ブロック
30. `redirect_allows_relative` — 相対 redirect 許可
31. `redirect_blocks_to_private_ip` — redirect 先 private IP ブロック
32. `redirect_too_many` — redirect 上限超過
33. `host_normalization_lowercase` — 小文字化
34. `host_normalization_trailing_dot` — 末尾ドット除去
35. `host_normalization_wildcard_prefix` — ワイルドカード除去

### コンテンツ検査 (14)

36. `clean_content_passes` — 正常テキスト通過
37. `detects_instruction_override` — instruction_override 検知
38. `detects_system_override` — system_override 検知
39. `detects_prompt_exfiltration` — prompt_exfiltration 検知
40. `detects_jailbreak_roleplay` — jailbreak_roleplay 検知
41. `detects_system_delimiters` — system_delimiters 検知
42. `detects_tool_abuse_instruction` — tool_abuse_instruction 検知
43. `strict_blocks_single_low_confidence` — strict で low confidence ブロック
44. `non_strict_allows_single_low_confidence` — non-strict で low confidence 許可
45. `non_strict_blocks_multiple_hits` — 複数 hit でブロック
46. `non_strict_blocks_high_confidence` — high confidence は常にブロック
47. `disabled_allows_everything` — disabled ですべて許可
48. `max_scan_bytes_skips_tail` — スキャン範囲制限
49. `validation_failure_contains_rule_names` — failure message にルール名

### HTML 処理 (17)

50. `html_to_markdown_basic` — h1+p+strong 変換
51. `html_to_markdown_list` — ul+li 変換
52. `html_to_markdown_ordered_list` — ol+li 変換
53. `html_to_markdown_link` — a href 変換
54. `html_to_markdown_blockquote` — blockquote 変換
55. `html_to_markdown_code` — code/pre 変換
56. `html_to_markdown_strips_script` — script 除去
57. `html_to_markdown_strips_style` — style 除去
58. `html_to_markdown_strips_nav` — nav 除去
59. `extract_primary_prefers_main` — main 優先
60. `extract_primary_prefers_article` — article 優先
61. `extract_primary_falls_back_to_body` — body フォールバック
62. `extract_primary_falls_back_to_full_html` — 全文フォールバック
63. `content_type_html_routes_to_markdown` — text/html → Markdown
64. `content_type_plain_returns_as_is` — text/plain → そのまま
65. `content_type_json_returns_as_is` — application/json → そのまま
66. `content_type_missing_routes_to_markdown` — Content-Type なし → HTML 扱い

### FeedSync (11)

67. `inline_feed_merges_denylist` — inline denylist マージ
68. `inline_feed_merges_allowlist` — inline allowlist マージ
69. `csv_first_column` — CSV 1 列目抽出
70. `skips_disabled_sources` — disabled ソーススキップ
71. `skips_comment_lines` — コメント行スキップ
72. `deduplicates_hosts` — 重複除去
73. `fails_closed_on_error` — fail_closed でエラー
74. `fails_open_on_error` — fail_open で空リスト
75. `feed_sync_disabled_is_noop` — disabled で何もしない
76. `normalizes_feed_hosts` — フィードホスト正規化
77. `max_entries_per_source` — エントリ上限

### WebFetchTool 本体 (24)

78. `tool_definition` — ツール定義検証
79. `is_read_only` — read-only フラグ
80. `missing_url_returns_error` — url なしエラー
81. `null_url_returns_error` — url:null エラー
82. `empty_url_returns_error` — url:"" エラー
83. `blocks_disallowed_scheme_before_request` — scheme ブロック（リクエスト前）
84. `blocks_denylisted_host_before_request` — denylist ブロック（DNS 前）
85. `blocks_private_ip_before_request` — SSRF ブロック（DNS 後）
86. `fetches_html_and_returns_markdown` — HTML→Markdown（wiremock）
87. `fetches_plain_text_as_is` — plain text そのまま（wiremock）
88. `result_details_metadata` — details メタデータ
89. `truncation_at_max_bytes` — max_bytes truncate
90. `truncation_utf8_safe` — UTF-8 安全 truncate
91. `start_index_continuation` — start_index 継続読み取り
92. `start_index_beyond_content` — start_index 超過
93. `blocks_content_with_injection` — injection コンテンツブロック
94. `follows_redirect_and_validates` — redirect 追跡 + 検証（wiremock）
95. `blocks_redirect_to_private_ip` — redirect 先 SSRF ブロック
96. `too_many_redirects` — redirect 上限
97. `http_error_status` — HTTP エラーステータス
98. `timeout_error` — タイムアウト
99. `untrusted_content_warning` — 警告付与
100. `blocks_disallowed_scheme_in_redirect` — redirect 先 scheme ブロック

### 統合 (5)

101. `registry_includes_web_fetch` — registry 定義一覧に含まれる
102. `registry_web_fetch_is_read_only` — registry 経由で read-only 確認
103. `execute_dispatches_to_web_fetch` — registry.execute で dispatch
104. `secret_redaction_on_fetch_result` — secret redaction 動作
105. `config_web_fetch_reflected_in_tool` — 設定がツールに反映

---

## 工数見積もり

| Step | 内容 | テスト(行) | 実装(行) | 合計(行) |
|---|---|---|---|---|
| Step 1 | 設定型 + Config 統合 | 80 | 200 | 280 |
| Step 2 | URL 検証 + SSRF | 200 | 180 | 380 |
| Step 3 | コンテンツ検査 | 100 | 120 | 220 |
| Step 4 | HTML 処理 | 130 | 80 | 210 |
| Step 5 | FeedSync | 120 | 130 | 250 |
| Step 6 | WebFetchTool 本体 | 200 | 150 | 350 |
| Step 7 | 統合 | 60 | 30 | 90 |
| Step 8 | ドキュメント | — | 120 | 120 |
| **合計** | | **890** | **1,010** | **~1,900** |
