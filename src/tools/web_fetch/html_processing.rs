//! HTML processing utilities for the web-fetch tool.
//!
//! Provides HTML-to-Markdown conversion using [`htmd`], with smart primary-content
//! extraction from `<main>`, `<article>`, or `<body>` elements.

use htmd::HtmlToMarkdownBuilder;

const SKIPPED_TAGS: &[&str] = &["script", "style", "nav", "footer", "header"];

/// Extracts the most relevant content region from an HTML document.
///
/// Priority order:
/// 1. `<main>` element
/// 2. `<article>` element
/// 3. `<body>` element
/// 4. Full HTML (fallback)
///
/// The search is case-insensitive. Only the first matching element is used.
pub(crate) fn extract_primary_html(html: &str) -> String {
    extract_tag_content(html, "main")
        .or_else(|| extract_tag_content(html, "article"))
        .or_else(|| extract_tag_content(html, "body"))
        .unwrap_or_else(|| html.to_owned())
}

/// Returns the inner HTML between the first occurrence of `<{tag}…>` and `</{tag}>`.
///
/// Case-insensitive. Returns `None` when either the opening or closing tag is absent.
fn extract_tag_content(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");

    let open_pos = lower.find(&open)?;
    let gt_offset = lower[open_pos..].find('>')?;
    let content_start = open_pos + gt_offset + 1;

    let rel_end = lower[content_start..].find(&close)?;
    let content_end = content_start + rel_end;

    if content_end < content_start {
        return None;
    }

    Some(html[content_start..content_end].to_owned())
}

/// Converts an HTML string to Markdown.
///
/// Internally calls [`extract_primary_html`] to isolate the primary content region,
/// then uses **htmd** to perform the conversion. Tags listed in [`SKIPPED_TAGS`] are
/// stripped before conversion.
///
/// If conversion fails the extracted HTML is returned as a graceful fallback.
pub(crate) fn html_to_markdown(html: &str) -> String {
    let primary = extract_primary_html(html);

    HtmlToMarkdownBuilder::new()
        .skip_tags(SKIPPED_TAGS.to_vec())
        .build()
        .convert(&primary)
        .unwrap_or(primary)
}

/// Processes an HTTP response body according to its content type.
///
/// - `text/html` or `None` → converted to Markdown via [`html_to_markdown`].
/// - `text/plain` / `application/json` / anything else → returned verbatim.
pub(crate) fn process_response_body(body: &str, content_type: Option<&str>) -> String {
    match content_type {
        Some(ct) if ct.contains("text/html") => html_to_markdown(body),
        Some(_) => body.to_owned(),
        None => html_to_markdown(body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(html: &str) -> String {
        html_to_markdown(html)
    }

    #[test]
    fn html_to_markdown_basic() {
        let html = "<html><body><h1>Title</h1><p>Hello <strong>world</strong></p></body></html>";

        let md = convert(html);

        assert!(md.contains("# Title"), "expected heading, got: {md}");
        assert!(
            md.contains("**world**") || md.contains("world"),
            "expected bold, got: {md}"
        );
    }

    #[test]
    fn html_to_markdown_list() {
        let html = "<ul><li>item1</li><li>item2</li></ul>";

        let md = convert(html);

        assert!(md.contains("item1"), "got: {md}");
        assert!(md.contains("item2"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_ordered_list() {
        let html = "<ol><li>first</li><li>second</li></ol>";

        let md = convert(html);

        assert!(md.contains("first"), "got: {md}");
        assert!(md.contains("second"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_link() {
        let html = r#"<a href="https://example.com">Example</a>"#;

        let md = convert(html);

        assert!(md.contains("Example"), "got: {md}");
        assert!(md.contains("https://example.com"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_blockquote() {
        let html = "<blockquote>quoted text</blockquote>";

        let md = convert(html);

        assert!(md.contains("quoted text"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_code() {
        let html = "<code>inline</code>";

        let md = convert(html);

        assert!(md.contains("inline"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_strips_script() {
        let html = "<html><body><script>alert('xss')</script><p>safe</p></body></html>";

        let md = convert(html);

        assert!(
            !md.contains("alert"),
            "script content should be stripped, got: {md}"
        );
        assert!(md.contains("safe"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_strips_style() {
        let html = "<html><body><style>body{color:red}</style><p>content</p></body></html>";

        let md = convert(html);

        assert!(
            !md.contains("color:red"),
            "style content should be stripped, got: {md}"
        );
        assert!(md.contains("content"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_strips_nav() {
        let html = "<html><body><nav>menu item</nav><p>main content</p></body></html>";

        let md = convert(html);

        assert!(
            !md.contains("menu item"),
            "nav content should be stripped, got: {md}"
        );
        assert!(md.contains("main content"), "got: {md}");
    }

    #[test]
    fn extract_primary_prefers_main() {
        let html = "<html><body><nav>nav</nav><main><p>main content</p></main></body></html>";

        let result = extract_primary_html(html);

        assert!(result.contains("main content"), "got: {result}");
        assert!(!result.contains("nav"), "got: {result}");
    }

    #[test]
    fn extract_primary_prefers_article() {
        let html =
            "<html><body><p>body text</p><article><p>article text</p></article></body></html>";

        let result = extract_primary_html(html);

        assert!(result.contains("article text"), "got: {result}");
    }

    #[test]
    fn extract_primary_falls_back_to_body() {
        let html = "<html><body><p>body content</p></body></html>";

        let result = extract_primary_html(html);

        assert!(result.contains("body content"), "got: {result}");
    }

    #[test]
    fn extract_primary_falls_back_to_full_html() {
        let html = "<p>just a paragraph</p>";

        let result = extract_primary_html(html);

        assert!(result.contains("just a paragraph"), "got: {result}");
    }

    #[test]
    fn content_type_html_routes_to_markdown() {
        let html = "<html><body><h1>Title</h1></body></html>";

        let result = process_response_body(html, Some("text/html"));

        assert!(result.contains("Title"), "got: {result}");
    }

    #[test]
    fn content_type_plain_returns_as_is() {
        let text = "Hello, world!";

        let result = process_response_body(text, Some("text/plain"));

        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn content_type_json_returns_as_is() {
        let json = r#"{"key": "value"}"#;

        let result = process_response_body(json, Some("application/json"));

        assert_eq!(result, json);
    }

    #[test]
    fn content_type_missing_routes_to_markdown() {
        let html = "<html><body><h1>No content type</h1></body></html>";

        let result = process_response_body(html, None);

        assert!(result.contains("No content type"), "got: {result}");
    }

    #[test]
    fn extract_primary_skips_closing_tag_before_opening() {
        let html = r#"</main><div>before</div><main><p>real content</p></main>"#;

        let result = extract_primary_html(html);

        assert!(result.contains("real content"), "got: {result}");
        assert!(!result.contains("before"), "got: {result}");
    }
}
