//! HTML processing utilities for the web-fetch tool.
//!
//! Provides content extraction via [`readability_js`] (Mozilla's Readability.js)
//! with fallback to basic HTML-to-Markdown conversion using [`htmd`].

use std::fmt;

use htmd::HtmlToMarkdownBuilder;

const SKIPPED_TAGS: &[&str] = &["script", "style", "nav", "footer", "header"];

/// Result of processing an HTTP response body.
pub(crate) struct ProcessedBody {
    pub text: String,
    pub extraction: ExtractionMethod,
}

/// Method used to extract content from an HTTP response.
pub(crate) enum ExtractionMethod {
    /// Mozilla Readability.js extracted the main article content.
    ReadabilityJs,
    /// Readability failed or returned empty; fell back to basic HTML→Markdown.
    FallbackHtmlToMarkdown,
    /// Non-HTML content returned verbatim (text/plain, JSON, etc.).
    Verbatim,
}

impl fmt::Display for ExtractionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadabilityJs => write!(f, "readability-js"),
            Self::FallbackHtmlToMarkdown => write!(f, "fallback-html-to-markdown"),
            Self::Verbatim => write!(f, "verbatim"),
        }
    }
}

/// Processes an HTTP response body with metadata about the extraction method.
///
/// For HTML content, attempts Readability.js extraction first, then falls back
/// to basic HTML-to-Markdown. For non-HTML content, returns verbatim.
pub(crate) fn process_response_body_with_metadata(
    body: &str,
    content_type: Option<&str>,
    url: &str,
) -> ProcessedBody {
    if is_html_content(content_type) {
        let (text, method) = extract_article(body, url);
        ProcessedBody {
            text,
            extraction: method,
        }
    } else {
        ProcessedBody {
            text: body.to_owned(),
            extraction: ExtractionMethod::Verbatim,
        }
    }
}

/// Processes an HTTP response body according to its content type.
pub(crate) fn process_response_body(body: &str, content_type: Option<&str>) -> String {
    process_response_body_with_metadata(body, content_type, "").text
}

fn is_html_content(content_type: Option<&str>) -> bool {
    match content_type {
        Some(ct) => ct.to_ascii_lowercase().contains("text/html"),
        None => true,
    }
}

fn extract_article(html: &str, url: &str) -> (String, ExtractionMethod) {
    match try_readability(html, url) {
        Some(result) => result,
        None => (
            html_to_markdown(html),
            ExtractionMethod::FallbackHtmlToMarkdown,
        ),
    }
}

fn try_readability(html: &str, url: &str) -> Option<(String, ExtractionMethod)> {
    let result = std::panic::catch_unwind(|| {
        let reader = readability_js::Readability::new().ok()?;
        let article = if url.is_empty() {
            reader.parse(html).ok()?
        } else {
            reader.parse_with_url(html, url).ok()?
        };

        if article.content.is_empty() {
            return None;
        }

        let md = html_to_markdown(&article.content);

        let text = if !article.title.is_empty() && !md.starts_with('#') {
            format!("# {}\n\n{}", article.title, md)
        } else {
            md
        };

        Some(text)
    });

    match result {
        Ok(Some(text)) => Some((text, ExtractionMethod::ReadabilityJs)),
        Ok(None) => None,
        Err(_) => None,
    }
}

/// Extracts the most relevant content region from an HTML document.
///
/// Priority order:
/// 1. `<main>` element
/// 2. `<article>` element
/// 3. `<body>` element
/// 4. Full HTML (fallback)
fn extract_primary_html(html: &str) -> String {
    extract_tag_content(html, "main")
        .or_else(|| extract_tag_content(html, "article"))
        .or_else(|| extract_tag_content(html, "body"))
        .unwrap_or_else(|| html.to_owned())
}

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

fn html_to_markdown(html: &str) -> String {
    let primary = extract_primary_html(html);

    HtmlToMarkdownBuilder::new()
        .skip_tags(SKIPPED_TAGS.to_vec())
        .build()
        .convert(&primary)
        .unwrap_or(primary)
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

    // --- Readability integration tests ---

    fn article_html(title: &str, body: &str) -> String {
        format!(
            "<html><head><title>{title}</title></head>\
             <body>\
             <nav><a href=\"/home\">Home</a><a href=\"/about\">About</a></nav>\
             <article><h1>{title}</h1><p>{body}</p></article>\
             <footer><p>Copyright 2024</p></footer>\
             </body></html>"
        )
    }

    #[test]
    fn readability_extracts_article_body() {
        let html = article_html(
            "Test Article",
            "This is the main article body content that is long enough to pass readability checks.",
        );

        let result =
            process_response_body_with_metadata(&html, Some("text/html"), "https://example.com");

        assert!(
            matches!(result.extraction, ExtractionMethod::ReadabilityJs),
            "expected ReadabilityJs, got: {}",
            result.extraction
        );
        assert!(
            result.text.contains("main article body"),
            "got: {}",
            result.text
        );
    }

    #[test]
    fn readability_excludes_nav_footer_header() {
        let html = "\
            <html><head><title>News</title></head><body>\
            <nav><a>Home</a><a>About</a></nav>\
            <header><p>Site header content</p></header>\
            <aside><p>Sidebar content here</p></aside>\
            <article><h1>Real Article Title</h1>\
            <p>This is the actual article content with enough text to be considered the main readable content of the page by the algorithm.</p>\
            <p>Second paragraph adds more substance to ensure the article passes readability checks and is properly extracted from the page.</p></article>\
            <footer><p>Footer content</p></footer>\
            <noscript>Enable JavaScript</noscript>\
            </body></html>";

        let result =
            process_response_body_with_metadata(html, Some("text/html"), "https://example.com");

        assert!(
            matches!(result.extraction, ExtractionMethod::ReadabilityJs),
            "expected ReadabilityJs, got: {}",
            result.extraction
        );
        assert!(
            !result.text.contains("Home"),
            "nav should be excluded, got: {}",
            result.text
        );
        assert!(
            result.text.contains("actual article content"),
            "article body should be included, got: {}",
            result.text
        );
    }

    #[test]
    fn readability_falls_back_on_minimal_content() {
        let html = "<html><body><div>x</div></body></html>";

        let result =
            process_response_body_with_metadata(html, Some("text/html"), "https://example.com");

        assert!(!result.text.is_empty(), "should produce output, got empty");
    }

    #[test]
    fn readability_failure_falls_back_to_html_to_markdown() {
        let html = "<html><body><p>Short</p></body></html>";

        let result =
            process_response_body_with_metadata(html, Some("text/html"), "https://example.com");

        assert!(
            !result.text.is_empty(),
            "fallback should produce output, got empty"
        );
        assert!(result.text.contains("Short"), "got: {}", result.text);
    }

    #[test]
    fn verbatim_for_non_html_content_types() {
        let json = r#"{"key": "value"}"#;

        let result = process_response_body_with_metadata(
            json,
            Some("application/json"),
            "https://example.com/api",
        );

        assert!(matches!(result.extraction, ExtractionMethod::Verbatim));
        assert_eq!(result.text, json);
    }

    #[test]
    fn verbatim_for_text_plain() {
        let text = "plain text content";

        let result = process_response_body_with_metadata(
            text,
            Some("text/plain"),
            "https://example.com/file.txt",
        );

        assert!(matches!(result.extraction, ExtractionMethod::Verbatim));
        assert_eq!(result.text, text);
    }

    #[test]
    fn verbatim_for_xml_content() {
        let xml = r#"<?xml version="1.0"?><rss><channel></channel></rss>"#;

        let result = process_response_body_with_metadata(
            xml,
            Some("application/xml"),
            "https://example.com/feed",
        );

        assert!(matches!(result.extraction, ExtractionMethod::Verbatim));
        assert_eq!(result.text, xml);
    }

    #[test]
    fn extraction_method_display() {
        assert_eq!(
            ExtractionMethod::ReadabilityJs.to_string(),
            "readability-js"
        );
        assert_eq!(
            ExtractionMethod::FallbackHtmlToMarkdown.to_string(),
            "fallback-html-to-markdown"
        );
        assert_eq!(ExtractionMethod::Verbatim.to_string(), "verbatim");
    }
}
