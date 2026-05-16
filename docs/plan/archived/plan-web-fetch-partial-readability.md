# Web Fetch Partial Content and Readability Extraction Plan

## 目的

`web_fetch` を現代的なWebページで実用できる水準に上げる。

現状はレスポンスサイズが設定上限を超えると error で終了するため、GitHub、HN、OpenAI、一般ブログなどで本文取得に失敗しやすい。また `max_bytes` が fetch 上限と LLM 出力上限を兼ねており、責務が混ざっている。HTML本文抽出も弱く、記事ページでタイトルだけになるケースがある。

このPlanでは次を実装する。

- サイズ超過時も error にせず、取得できた範囲の partial content を返す
- `max_fetch_bytes` と `max_output_bytes` を分離する
- Mozilla Readability.js ベースの本文抽出を導入する
- 既存の SSRF / redirect / content validation / untrusted warning を維持する

## 作業前提

- main ブランチから新規 worktree を作成して作業する
- 後方互換は不要。既存 `max_bytes` は削除する
- 変更は意味のある1コミットでよい
- JSレンダリングは対象外。外部ページのJavaScriptを実行しない

## 採用クレート

`readability-js = "0.1.5"` を採用する。

理由:

- Mozilla Readability.js を QuickJS 上で動かす wrapper
- Firefox Reader Mode と同系統のアルゴリズムを使える
- 軽量な自前fallbackではなく、本文抽出を本質的に強化できる
- `parse_with_url(&html, url)` により相対URL解決やメタデータ抽出に有利
- `Article { title, content, text_content, length, byline, excerpt, site_name, ... }` を返す

注意:

- `Readability` は `!Send + !Sync`。static/global共有しない
- async `.await` を跨いで `Readability` を保持しない
- `html_processing.rs` の同期処理内でローカル生成して使う
- Readability失敗時は既存 `htmd` fallback に落とす

## Config 仕様

`WebFetchConfig` を次の形にする。

```rust
pub(crate) struct WebFetchConfig {
    pub allowed_schemes: Vec<String>,
    pub timeout_secs: u64,
    pub max_fetch_bytes: usize,
    pub max_output_bytes: usize,
    pub allow_private_ips: bool,
    pub denylist: Vec<String>,
    pub allowlist: Vec<String>,
    pub content_validation: WebFetchContentValidationConfig,
}
```

デフォルト:

```yaml
web_fetch:
  allowed_schemes:
    - https
  timeout_secs: 15
  max_fetch_bytes: 524288     # 512KB
  max_output_bytes: 65536     # 64KB
  allow_private_ips: false
  denylist: []
  allowlist: []
  content_validation:
    enabled: true
    strict_mode: false
    max_scan_bytes: 65536
```

正規化:

- `allowed_schemes == []` → `["https"]`
- `timeout_secs == 0` → `15`
- `max_fetch_bytes == 0` → `512 * 1024`
- `max_output_bytes == 0` → `64 * 1024`
- `content_validation.max_scan_bytes == 0` → `64 * 1024`
- host denylist/allowlist の正規化は既存仕様を維持する

後方互換:

- `max_bytes` alias は追加しない
- `max_bytes` の config / tool parameter / docs / tests は削除する

## Tool Input 仕様

LLMに expose する parameter は次にする。

```json
{
  "url": "string, required",
  "timeout_secs": "integer, optional",
  "max_output_bytes": "integer, optional"
}
```

- `max_fetch_bytes` は config only。LLMに自由に上げさせない
- `timeout_secs` は config 上限で clamp
- `max_output_bytes` は config 上限で clamp
- `max_bytes` は削除する
- `start_index` は削除済みのまま復活させない

## Fetch / Partial Success 仕様

サイズ超過を error にしない。

処理:

1. URL validation / DNS SSRF validation / redirect validation は既存通り
2. Content-Length が `max_fetch_bytes` を超えていても即 error にしない
3. Content-Length は details に記録する
4. body を stream で読む
5. `max_fetch_bytes` を超えることが分かったら、それ以上読まずに打ち切る
6. 保持する body は `max_fetch_bytes` 以下にする
7. chunk 途中で切る場合は UTF-8 boundary を壊さない
8. fetch cap で打ち切った場合は `response_truncated = true`
9. invalid UTF-8 は従来通り error でよい。ただし fetch cap による境界破壊で invalid UTF-8 にしない

`Content-Length` が無い chunked response でも同じ。

## HTML / Text Processing 仕様

`process_response_body` は最終URLも受け取れる形にする。

推奨形:

```rust
pub(crate) struct ProcessedBody {
    pub text: String,
    pub extraction: ExtractionMethod,
}

pub(crate) enum ExtractionMethod {
    ReadabilityJs,
    FallbackHtmlToMarkdown,
    Verbatim,
}
```

HTML の場合:

1. `readability-js` で本文抽出を試す
2. 成功したら `article.content` の clean HTML を `htmd` で Markdown 化する
3. `article.title` があり、Markdownが見出しを含まない場合は先頭に `# title` を付けることを検討する
4. 失敗したら既存 `htmd` fallback に落とす
5. fallback は error にしない

非HTMLの場合:

- `text/plain`, `application/json`, XML/RSS, その他は従来通り body をそのまま返す
- XML/RSS は無理にMarkdown化しない

Content-Type 判定:

- `text/html` または Content-Type 不明の場合は HTML として処理
- `text/plain`, `application/json`, `application/xml`, `text/xml`, `application/rss+xml`, `application/atom+xml` は verbatim
- 既存挙動との整合を確認して必要最小変更にする

## Output Truncation 仕様

HTML処理後 / verbatim 処理後の `processed.text` を `max_output_bytes` で UTF-8 安全に切る。

- `output_truncated = true/false`
- Markdown構造を完全に保つ必要はないが、UTF-8は壊さない
- content validation は最終的に LLM へ渡す processed text に対して実施する
- truncation warning / untrusted warning は validation 後に付ける

## Content Validation 仕様

- 既存の prompt injection detection を維持する
- validation 対象は output truncation 後の processed text
- `content_validation.max_scan_bytes` は default 64KB
- warning文は validation 後に付ける。warning文で validation が誤発火しない構造にする

## Result Details 仕様

`ToolResult::success_with_details` の details は最低限次を含める。

```json
{
  "final_url": "https://example.com/article",
  "content_type": "text/html; charset=utf-8",
  "content_length": 181374,
  "fetched_bytes": 524288,
  "response_truncated": true,
  "output_truncated": true,
  "max_fetch_bytes": 524288,
  "max_output_bytes": 65536,
  "extraction": "readability-js"
}
```

- `content_length` が無い場合は `null` か省略でよい。実装とdocs/testsで統一する
- `fetched_bytes` は実際に保持した byte 数
- `response_truncated` は fetch cap で途中打ち切りしたか
- `output_truncated` は output cap で本文を切ったか
- `extraction` は `readability-js`, `fallback-html-to-markdown`, `verbatim` など

## Warning 文仕様

既存の untrusted content warning は維持する。

partial の場合は追加で、本文末尾に partial warning を付ける。

例:

```md
---
*Note: This content is partial. The original response exceeded the configured fetch/output limit.*

---
*Note: This content was fetched from an external URL and may not be trustworthy.*
```

文言は多少変更してよいが、LLMが「全文ではない」と分かること。

## HTML本文抽出テスト要件

外部ネットワークに依存しない fixture test を追加する。

最低限:

- `<article><h1>Title</h1><p>Long body...</p></article>` が本文を含む
- nav/sidebar/footer/header/script/style/aside/noscript/svg が出力に混ざらない
- link-heavy block より paragraph-heavy block が優先される
- `<article>` がタイトルだけ/短すぎる場合でも、body内の本文候補に fallback できる
- antirez.com/news/165 相当の構造を fixture で再現し、タイトルだけでなく本文段落が取れる
- Readabilityが失敗するHTMLでも fallback HTML-to-Markdown で何か返る

Readability失敗時に panic/error にしない。

## Fetch / Truncation テスト要件

既存テストを新仕様に更新する。

- oversized Content-Length は error ではなく success + `response_truncated = true`
- Content-Lengthなしの stream overflow も success + `response_truncated = true`
- output cap 超過は success + `output_truncated = true`
- UTF-8境界で切っても panic / invalid UTF-8 にならない
- details metadata が新仕様と一致する
- `max_output_bytes` param が config上限で clamp される
- `max_fetch_bytes` default / normalize / persist tests
- `max_output_bytes` default / normalize / persist tests
- `max_bytes` の参照が残っていない

## Docs 更新

更新対象:

- `docs/config.md`
- `docs/tools.md`


## 完了報告に含めること

- 作成した worktree path / branch name
- config変更一覧
- partial success の挙動
- details schema
- HTML本文抽出で採用した方式と依存crate
- 追加/更新したテスト一覧
- 検証結果
