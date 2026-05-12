//! Home Surface resolver — finds the best chat for pulse delivery.

use std::sync::Arc;

use crate::storage::Database;

/// Resolved home surface for pulse delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HomeSurface {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
    pub chat_type: String,
}

const SENDABLE_CHANNELS: &[&str] = &["discord", "telegram"];

/// Resolve the home surface for an agent.
///
/// Queries the database for the agent's chats ordered by recency,
/// then returns the first chat on a sendable channel (Discord or Telegram).
///
/// # Errors
/// Returns `StorageError` when the database query fails.
pub(crate) async fn resolve_home_surface(
    db: &Arc<Database>,
    agent_id: &str,
    available_channels: &[&str],
) -> Result<Option<HomeSurface>, crate::error::StorageError> {
    let _ = available_channels;

    let agent_id = agent_id.to_string();

    let chats = crate::storage::call_blocking(db.clone(), move |db| {
        db.get_agent_chats_by_recent(&agent_id, SENDABLE_CHANNELS)
    })
    .await?;

    let Some(first) = chats.into_iter().next() else {
        return Ok(None);
    };

    Ok(Some(HomeSurface {
        chat_id: first.chat_id,
        channel: first.channel,
        external_chat_id: first.external_chat_id,
        chat_type: first.chat_type,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (Arc::new(db), dir)
    }

    fn insert_chat(
        db: &Database,
        channel: &str,
        external_chat_id: &str,
        chat_type: &str,
        agent_id: &str,
        last_message_time: &str,
    ) -> i64 {
        let chat_id = db
            .resolve_or_create_chat_id(channel, external_chat_id, None, chat_type, agent_id)
            .expect("create chat");
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "UPDATE chats SET last_message_time = ?1 WHERE chat_id = ?2",
            rusqlite::params![last_message_time, chat_id],
        )
        .expect("set last_message_time");
        chat_id
    }

    #[tokio::test]
    async fn home_surface_uses_latest_sendable_agent_chat() {
        // Arrange: discord (old), telegram (recent) → should pick telegram
        let (db, _dir) = test_db();

        let _discord_id = insert_chat(
            &db,
            "discord",
            "discord:111",
            "dm",
            "agent-a",
            "2024-01-01T00:00:00Z",
        );

        let telegram_id = insert_chat(
            &db,
            "telegram",
            "telegram:222",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        // Act
        let surface = resolve_home_surface(&db, "agent-a", &[])
            .await
            .expect("resolve")
            .expect("should find surface");

        // Assert: telegram is more recent
        assert_eq!(surface.chat_id, telegram_id);
        assert_eq!(surface.channel, "telegram");
    }

    #[tokio::test]
    async fn home_surface_skips_web_cli_tui_and_uses_previous_sendable_chat() {
        // Arrange: discord (old), web (recent) → should skip web, pick discord
        let (db, _dir) = test_db();

        let discord_id = insert_chat(
            &db,
            "discord",
            "discord:333",
            "dm",
            "agent-a",
            "2024-01-01T00:00:00Z",
        );

        let _web_id = insert_chat(
            &db,
            "web",
            "web:444",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        // Act
        let surface = resolve_home_surface(&db, "agent-a", &[])
            .await
            .expect("resolve")
            .expect("should find surface");

        // Assert
        assert_eq!(surface.chat_id, discord_id);
        assert_eq!(surface.channel, "discord");
    }

    #[tokio::test]
    async fn home_surface_skips_when_no_sendable_chat() {
        // Arrange: agent only has web chats
        let (db, _dir) = test_db();

        insert_chat(
            &db,
            "web",
            "web:555",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        // Act
        let surface = resolve_home_surface(&db, "agent-a", &[])
            .await
            .expect("resolve");

        // Assert
        assert!(surface.is_none());
    }

    #[tokio::test]
    async fn home_surface_does_not_use_default_delivery() {
        // Arrange: no sendable chats at all
        let (db, _dir) = test_db();

        insert_chat(
            &db,
            "cli",
            "cli:666",
            "cli",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );
        insert_chat(
            &db,
            "tui",
            "tui:777",
            "tui",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        // Act
        let surface = resolve_home_surface(&db, "agent-a", &[])
            .await
            .expect("resolve");

        // Assert: returns None without any fallback
        assert!(surface.is_none());
    }
}
