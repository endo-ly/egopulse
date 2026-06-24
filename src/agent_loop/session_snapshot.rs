//! Persisted session snapshot types.
//!
//! LLM request messages are hydrated runtime values. Session snapshots are a
//! separate disk format that can point at images stored in the asset store.

use serde::{Deserialize, Serialize};

use crate::assets::AssetStore;
use crate::error::StorageError;
use crate::llm::{Message, MessageContent, MessageContentPart, ToolCall};

/// A message in the persisted session snapshot format.
///
/// This mirrors the fields needed to resume a conversation while keeping
/// storage-only content, such as asset references, out of LLM request types.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SnapshotMessage {
    /// Role name preserved from the LLM message stream.
    pub(crate) role: String,
    /// Stored content for the message.
    pub(crate) content: SnapshotContent,
    /// Provider-specific reasoning text preserved across turns when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_content: Option<String>,
    /// Tool calls requested by this message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tool_calls: Vec<ToolCall>,
    /// Tool call ID this message responds to when the role is `tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call_id: Option<String>,
}

/// Stored message content in a session snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum SnapshotContent {
    /// Plain text content.
    Text(String),
    /// Multimodal content parts.
    Parts(Vec<SnapshotContentPart>),
}

/// One stored multimodal content part in a session snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub(crate) enum SnapshotContentPart {
    /// Text content included inline in the snapshot.
    #[serde(rename = "input_text")]
    Text { text: String },
    /// Hydrated image content, accepted for snapshot imports and tests.
    #[serde(rename = "input_image")]
    Image {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Asset-store image reference used by normal persisted snapshots.
    #[serde(rename = "input_image_ref")]
    ImageRef {
        image_ref: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Converts hydrated LLM messages into persisted session snapshot messages.
///
/// Image data URLs are stored in the asset store and replaced with
/// `SnapshotContentPart::ImageRef`.
///
/// # Errors
///
/// Returns `StorageError` when an image data URL cannot be persisted in the
/// asset store.
pub(crate) fn messages_to_snapshot(
    assets: &AssetStore,
    messages: &[Message],
) -> Result<Vec<SnapshotMessage>, StorageError> {
    messages
        .iter()
        .map(|message| message_to_snapshot(assets, message))
        .collect()
}

/// Converts persisted session snapshot messages into hydrated LLM messages.
///
/// Valid image references are loaded from the asset store. Missing or malformed
/// references become explicit text notes so the LLM sees why an image is absent.
pub(crate) fn messages_from_snapshot(
    assets: &AssetStore,
    messages: Vec<SnapshotMessage>,
) -> Vec<Message> {
    messages
        .into_iter()
        .map(|message| message_from_snapshot(assets, message))
        .collect()
}

fn message_to_snapshot(
    assets: &AssetStore,
    message: &Message,
) -> Result<SnapshotMessage, StorageError> {
    Ok(SnapshotMessage {
        role: message.role.clone(),
        content: content_to_snapshot(assets, &message.content)?,
        reasoning_content: message.reasoning_content.clone(),
        tool_calls: message.tool_calls.clone(),
        tool_call_id: message.tool_call_id.clone(),
    })
}

fn content_to_snapshot(
    assets: &AssetStore,
    content: &MessageContent,
) -> Result<SnapshotContent, StorageError> {
    match content {
        MessageContent::Text(text) => Ok(SnapshotContent::Text(text.clone())),
        MessageContent::Parts(parts) => Ok(SnapshotContent::Parts(
            parts
                .iter()
                .map(|part| part_to_snapshot(assets, part))
                .collect::<Result<Vec<_>, _>>()?,
        )),
    }
}

fn part_to_snapshot(
    assets: &AssetStore,
    part: &MessageContentPart,
) -> Result<SnapshotContentPart, StorageError> {
    match part {
        MessageContentPart::InputText { text } => {
            Ok(SnapshotContentPart::Text { text: text.clone() })
        }
        MessageContentPart::InputImage { image_url, detail } => {
            let stored = assets.persist_image_data_url(image_url)?;
            Ok(SnapshotContentPart::ImageRef {
                image_ref: stored.image_ref,
                mime_type: stored.mime_type,
                detail: detail.clone(),
            })
        }
    }
}

fn message_from_snapshot(assets: &AssetStore, message: SnapshotMessage) -> Message {
    Message {
        role: message.role,
        content: content_from_snapshot(assets, message.content),
        reasoning_content: message.reasoning_content,
        tool_calls: message.tool_calls,
        tool_call_id: message.tool_call_id,
    }
}

fn content_from_snapshot(assets: &AssetStore, content: SnapshotContent) -> MessageContent {
    match content {
        SnapshotContent::Text(text) => MessageContent::Text(text),
        SnapshotContent::Parts(parts) => MessageContent::Parts(
            parts
                .into_iter()
                .map(|part| part_from_snapshot(assets, part))
                .collect(),
        ),
    }
}

fn part_from_snapshot(assets: &AssetStore, part: SnapshotContentPart) -> MessageContentPart {
    match part {
        SnapshotContentPart::Text { text } => MessageContentPart::InputText { text },
        SnapshotContentPart::Image { image_url, detail } => {
            MessageContentPart::InputImage { image_url, detail }
        }
        SnapshotContentPart::ImageRef {
            image_ref,
            mime_type,
            detail,
        } => {
            if !is_sha256_hex(&image_ref) {
                return missing_image_text_part(
                    &image_ref,
                    StorageError::InvalidAsset(format!("malformed image_ref {image_ref}")),
                );
            }
            assets
                .load_image_data_url(&image_ref, &mime_type)
                .map(|image_url| MessageContentPart::InputImage { image_url, detail })
                .unwrap_or_else(|error| missing_image_text_part(&image_ref, error))
        }
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn missing_image_text_part(image_ref: &str, error: StorageError) -> MessageContentPart {
    let reason = match error {
        StorageError::NotFound(_) => format!("missing image_ref {image_ref}"),
        other => other.to_string(),
    };
    MessageContentPart::InputText {
        text: format!("Previously attached image could not be restored: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{SnapshotContent, SnapshotContentPart, SnapshotMessage, messages_from_snapshot};
    use crate::assets::AssetStore;
    use crate::llm::{MessageContent, MessageContentPart};

    #[test]
    fn text_only_snapshot_restores_to_llm_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let snapshot = vec![
            SnapshotMessage {
                role: "user".to_string(),
                content: SnapshotContent::Text("hello".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            SnapshotMessage {
                role: "assistant".to_string(),
                content: SnapshotContent::Text("world".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ];

        let messages = messages_from_snapshot(&assets, snapshot);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_text_lossy(), "hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content.as_text_lossy(), "world");
    }

    #[test]
    fn image_snapshot_hydrates_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let data_url = "data:image/png;base64,AAAA";
        let stored = assets.persist_image_data_url(data_url).expect("persist");

        let snapshot = vec![SnapshotMessage {
            role: "tool".to_string(),
            content: SnapshotContent::Parts(vec![
                SnapshotContentPart::Text {
                    text: "screenshot".to_string(),
                },
                SnapshotContentPart::ImageRef {
                    image_ref: stored.image_ref.clone(),
                    mime_type: stored.mime_type.clone(),
                    detail: Some("auto".to_string()),
                },
            ]),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".to_string()),
        }];

        let messages = messages_from_snapshot(&assets, snapshot);

        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            MessageContent::Parts(parts) => {
                assert!(matches!(
                    &parts[1],
                    MessageContentPart::InputImage { image_url, detail }
                    if image_url == data_url && detail.as_deref() == Some("auto")
                ));
            }
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn mixed_snapshot_restores_to_llm_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let data_url = "data:image/png;base64,iVBORw==";
        let stored = assets.persist_image_data_url(data_url).expect("persist");

        let snapshot = vec![
            SnapshotMessage {
                role: "user".to_string(),
                content: SnapshotContent::Text("look at this".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            SnapshotMessage {
                role: "tool".to_string(),
                content: SnapshotContent::Parts(vec![
                    SnapshotContentPart::Text {
                        text: "file read".to_string(),
                    },
                    SnapshotContentPart::ImageRef {
                        image_ref: stored.image_ref.clone(),
                        mime_type: stored.mime_type.clone(),
                        detail: None,
                    },
                ]),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call_img".to_string()),
            },
            SnapshotMessage {
                role: "assistant".to_string(),
                content: SnapshotContent::Text("I see the image".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ];

        let restored = messages_from_snapshot(&assets, snapshot);

        assert_eq!(restored.len(), 3);
        assert_eq!(restored[0].role, "user");
        assert_eq!(restored[0].content.as_text_lossy(), "look at this");
        assert_eq!(restored[1].role, "tool");
        assert_eq!(restored[1].tool_call_id.as_deref(), Some("call_img"));
        match &restored[1].content {
            MessageContent::Parts(parts) => {
                assert!(matches!(
                    &parts[1],
                    MessageContentPart::InputImage { image_url, detail }
                    if image_url == data_url && detail.is_none()
                ));
            }
            other => panic!("expected hydrated tool parts, got {other:?}"),
        }
        assert_eq!(restored[2].role, "assistant");
        assert_eq!(restored[2].content.as_text_lossy(), "I see the image");
    }

    #[test]
    fn malformed_image_ref_restores_to_text_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let snapshot = vec![SnapshotMessage {
            role: "tool".to_string(),
            content: SnapshotContent::Parts(vec![SnapshotContentPart::ImageRef {
                image_ref: "../not-a-sha".to_string(),
                mime_type: "image/png".to_string(),
                detail: None,
            }]),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some("call_bad_ref".to_string()),
        }];

        let restored = messages_from_snapshot(&assets, snapshot);

        assert_eq!(restored[0].role, "tool");
        assert_eq!(restored[0].tool_call_id.as_deref(), Some("call_bad_ref"));
        match &restored[0].content {
            MessageContent::Parts(parts) => {
                assert!(matches!(
                    &parts[0],
                    MessageContentPart::InputText { text }
                    if text.contains("malformed image_ref ../not-a-sha")
                ));
            }
            other => panic!("expected restored parts, got {other:?}"),
        }
    }

    #[test]
    fn large_snapshot_restores_all_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let messages: Vec<SnapshotMessage> = (0..1000)
            .map(|index| SnapshotMessage {
                role: if index % 2 == 0 { "user" } else { "assistant" }.to_string(),
                content: SnapshotContent::Text(format!("message-{index}")),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            })
            .collect();

        let restored = messages_from_snapshot(&assets, messages);

        assert_eq!(restored.len(), 1000);
    }
}
