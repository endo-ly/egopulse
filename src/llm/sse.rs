//! SSE line parser for streaming LLM responses.
//!
//! Reassembles `data:` lines split across TCP segment boundaries and yields
//! each complete payload, skipping blank lines, `[DONE]`, and malformed input.

use async_stream::stream;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;

/// Filters a `bytes_stream` into complete SSE `data:` payloads.
///
/// Each yielded `String` is the JSON text following `data:`. Stream item
/// errors are logged and treated as end-of-stream.
///
/// Bytes are accumulated raw and lines are decoded individually so that a
/// multibyte UTF-8 character bisected by a TCP segment boundary is reassembled
/// rather than dropped.
pub(crate) fn data_lines<S>(stream: S) -> BoxStream<'static, String>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let produced = stream! {
        let mut stream = stream.boxed();
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                tracing::warn!("SSE stream chunk error, stopping consumption");
                break;
            };
            buffer.extend_from_slice(chunk.as_ref());
            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = buffer.drain(..=newline).collect();
                match std::str::from_utf8(&line_bytes) {
                    Ok(line) => {
                        if let Some(payload) = data_payload(line.trim()) {
                            yield payload.to_string();
                        }
                    }
                    Err(_) => tracing::warn!("non-UTF-8 line in SSE stream, skipping"),
                }
            }
        }
        if let Ok(line) = std::str::from_utf8(&buffer)
            && let Some(payload) = data_payload(line.trim())
        {
            yield payload.to_string();
        }
    };
    produced.boxed()
}

/// Extracts the JSON payload from a single SSE `data:` line.
///
/// Returns `None` for blank payloads, `[DONE]` markers, and non-`data:` lines.
pub(crate) fn data_payload(line: &str) -> Option<&str> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    Some(payload)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::StreamExt;
    use futures_util::stream;

    use super::data_lines;

    fn chunk(bytes: &[u8]) -> Result<Bytes, reqwest::Error> {
        Ok(Bytes::copy_from_slice(bytes))
    }

    #[tokio::test]
    async fn yields_data_payloads_in_order() {
        let body = "data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: [DONE]\n\n";
        let mut lines = data_lines(stream::iter([chunk(body.as_bytes())]));

        assert_eq!(lines.next().await.as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(lines.next().await.as_deref(), Some(r#"{"a":2}"#));
        assert_eq!(lines.next().await, None);
    }

    #[tokio::test]
    async fn reassembles_line_split_across_chunks() {
        let mut lines = data_lines(stream::iter([chunk(b"data: {\"par"), chunk(b"t\":1}\n\n")]));

        assert_eq!(lines.next().await.as_deref(), Some(r#"{"part":1}"#));
        assert_eq!(lines.next().await, None);
    }

    #[tokio::test]
    async fn skips_blank_done_and_non_data_lines() {
        let body = "event: ping\n\ngarbage\ndata: ok\n\ndata: [DONE]\n\n: comment\n";
        let mut lines = data_lines(stream::iter([chunk(body.as_bytes())]));

        assert_eq!(lines.next().await.as_deref(), Some("ok"));
        assert_eq!(lines.next().await, None);
    }

    #[tokio::test]
    async fn flushes_trailing_line_without_newline() {
        let mut lines = data_lines(stream::iter([chunk(b"data: tail")]));
        assert_eq!(lines.next().await.as_deref(), Some("tail"));
        assert_eq!(lines.next().await, None);
    }

    #[tokio::test]
    async fn reassembles_multibyte_char_split_across_chunks() {
        // Split the SSE line `data: {"a":"ファイル"}\n\n` at a byte offset
        // that falls inside the 3-byte sequence of フ (U+30D5), simulating a
        // TCP segment boundary that bisects a multibyte UTF-8 character.
        let full = "data: {\"a\":\"ファイル\"}\n\n";
        let bytes = full.as_bytes();
        let char_start = full.find('フ').expect("フ present");
        let split_at = char_start + 1;

        let mut lines = data_lines(stream::iter([
            chunk(&bytes[..split_at]),
            chunk(&bytes[split_at..]),
        ]));

        assert_eq!(lines.next().await.as_deref(), Some(r#"{"a":"ファイル"}"#));
        assert_eq!(lines.next().await, None);
    }
}
