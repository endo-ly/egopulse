//! チャネルへ配信した添付ファイルのメタデータ。
//!
//! Discord / Telegram はファイルを即時送信するためメタデータを必要としない。
//! Web は配信済みコピーを保持し、履歴から再生できるようにするため、
//! ツール結果とタイムラインの両方でこの型を運ぶ。

use serde::{Deserialize, Serialize};

/// A stored attachment that a client can fetch and render later.
///
/// `id` is an opaque `{uuid}.{ext}` token. The original file name is kept only
/// for display and download; the stored file is named by `id` so it cannot
/// escape the attachment directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AttachmentInfo {
    /// Fetch identifier in `{uuid}.{ext}` form.
    pub(crate) id: String,
    /// Original file name for display and download.
    pub(crate) filename: String,
    /// MIME type used by the Web UI to decide whether to render an audio player.
    pub(crate) content_type: String,
    /// Size of the stored attachment in bytes.
    pub(crate) size_bytes: u64,
}
