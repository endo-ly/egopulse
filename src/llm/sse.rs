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
pub(crate) fn data_lines<S>(stream: S) -> BoxStream<'static, String>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let produced = stream! {
        let mut stream = stream.boxed();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                tracing::warn!("SSE stream chunk error, stopping consumption");
                break;
            };
            match std::str::from_utf8(chunk.as_ref()) {
                Ok(text) => buffer.push_str(text),
                Err(_) => {
                    tracing::warn!("non-UTF-8 chunk in SSE stream, skipping");
                    continue;
                }
            }
            while let Some(newline) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline).collect();
                if let Some(payload) = data_payload(line.trim()) {
                    yield payload;
                }
            }
        }
        if let Some(payload) = data_payload(buffer.trim()) {
            yield payload;
        }
    };
    produced.boxed()
}

fn data_payload(line: &str) -> Option<String> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    Some(payload.to_string())
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
}
