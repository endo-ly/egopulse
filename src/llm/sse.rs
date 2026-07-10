//! SSE line parser for streaming LLM responses.
//!
//! Reassembles `data:` lines split across TCP segment boundaries and yields
//! each complete payload or `[DONE]` sentinel. Stream errors are propagated
//! instead of being swallowed.

use async_stream::stream;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;

use crate::error::LlmError;

/// A parsed SSE event from the LLM stream.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SseItem {
    /// A `data:` payload (JSON text without the `data:` prefix).
    Data(String),
    /// The `[DONE]` sentinel indicating stream completion.
    Done,
}

/// Classifies a single SSE line into a data payload or `[DONE]` marker.
///
/// Returns `None` for blank payloads, non-`data:` lines, and empty `data:`
/// lines. Returns `Some(SseItem::Done)` for `data: [DONE]`.
pub(super) fn classify_line(line: &str) -> Option<SseItem> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload == "[DONE]" {
        return Some(SseItem::Done);
    }
    if payload.is_empty() {
        return None;
    }
    Some(SseItem::Data(payload.to_string()))
}

/// Filters a `bytes_stream` into classified SSE events.
///
/// Yields `Ok(SseItem::Data(payload))` for each complete `data:` line,
/// `Ok(SseItem::Done)` when the `[DONE]` sentinel is encountered (then stops),
/// and `Err(LlmError::RequestFailed(e))` when the underlying stream errors.
///
/// At natural EOF without `[DONE]`, only trailing buffered data is flushed —
/// no `SseItem::Done` is yielded, leaving completion detection to the caller.
///
/// Bytes are accumulated raw and lines are decoded individually so that a
/// multibyte UTF-8 character bisected by a TCP segment boundary is reassembled
/// rather than dropped.
pub(super) fn data_lines<S>(stream: S) -> BoxStream<'static, Result<SseItem, LlmError>>
where
    S: futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let produced = stream! {
        let mut stream = stream.boxed();
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => buffer.extend_from_slice(bytes.as_ref()),
                Err(e) => {
                    yield Err(LlmError::RequestFailed(e));
                    return;
                }
            }
            while let Some(newline) = buffer.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = buffer.drain(..=newline).collect();
                match std::str::from_utf8(&line_bytes) {
                    Ok(line) => {
                        if let Some(item) = classify_line(line.trim()) {
                            let is_done = matches!(item, SseItem::Done);
                            yield Ok(item);
                            if is_done {
                                return;
                            }
                        }
                    }
                    Err(_) => tracing::warn!("non-UTF-8 line in SSE stream, skipping"),
                }
            }
        }
        if let Ok(line) = std::str::from_utf8(&buffer)
            && let Some(item) = classify_line(line.trim())
            && let SseItem::Data(payload) = item
        {
            yield Ok(SseItem::Data(payload));
        }
    };
    produced.boxed()
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::StreamExt;
    use futures_util::stream;

    use super::{SseItem, data_lines};

    fn chunk(bytes: &[u8]) -> Result<Bytes, reqwest::Error> {
        Ok(Bytes::copy_from_slice(bytes))
    }

    fn assert_data(actual: Option<Result<SseItem, crate::error::LlmError>>, expected: &str) {
        match actual {
            Some(Ok(SseItem::Data(s))) => assert_eq!(s, expected),
            other => panic!("expected Ok(Data({expected:?})), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn yields_data_payloads_in_order() {
        let body = "data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: [DONE]\n\n";
        let mut lines = data_lines(stream::iter([chunk(body.as_bytes())]));

        assert_data(lines.next().await, r#"{"a":1}"#);
        assert_data(lines.next().await, r#"{"a":2}"#);
        assert!(matches!(lines.next().await, Some(Ok(SseItem::Done))));
        assert!(lines.next().await.is_none());
    }

    #[tokio::test]
    async fn reassembles_line_split_across_chunks() {
        let mut lines = data_lines(stream::iter([chunk(b"data: {\"par"), chunk(b"t\":1}\n\n")]));

        assert_data(lines.next().await, r#"{"part":1}"#);
        assert!(lines.next().await.is_none());
    }

    #[tokio::test]
    async fn skips_blank_done_and_non_data_lines() {
        let body = "event: ping\n\ngarbage\ndata: ok\n\ndata: [DONE]\n\n: comment\n";
        let mut lines = data_lines(stream::iter([chunk(body.as_bytes())]));

        assert_data(lines.next().await, "ok");
        assert!(matches!(lines.next().await, Some(Ok(SseItem::Done))));
        assert!(lines.next().await.is_none());
    }

    #[tokio::test]
    async fn flushes_trailing_line_without_newline() {
        let mut lines = data_lines(stream::iter([chunk(b"data: tail")]));
        assert_data(lines.next().await, "tail");
        assert!(lines.next().await.is_none());
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

        assert_data(lines.next().await, r#"{"a":"ファイル"}"#);
        assert!(lines.next().await.is_none());
    }

    #[tokio::test]
    async fn propagates_stream_chunk_error() {
        // Arrange: create a real reqwest::Error by attempting to connect to
        // a closed port. Port 1 is privileged and should refuse connections
        // immediately (ECONNREFUSED).
        let stream_error = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("client")
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("connection to closed port should fail");

        let mut lines = data_lines(stream::iter([
            chunk(b"data: {\"a\":1}\n\n"),
            Err(stream_error),
        ]));

        // Act & Assert: first Data item is yielded, then the error propagates
        assert_data(lines.next().await, r#"{"a":1}"#);
        assert!(matches!(lines.next().await, Some(Err(_))));
        assert!(lines.next().await.is_none());
    }
}
