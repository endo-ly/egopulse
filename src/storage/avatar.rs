//! Agent avatar image persistence for the WebUI.

use rusqlite::OptionalExtension;

use super::Database;
use crate::error::StorageError;

/// A stored agent avatar image.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentAvatar {
    pub(crate) content_type: String,
    pub(crate) image: Vec<u8>,
    /// Row version (RFC 3339 timestamp); doubles as the HTTP cache buster.
    pub(crate) updated_at: String,
}

impl Database {
    /// Returns the stored avatar for `agent_id`, or `None` when unset.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite read fails.
    pub(crate) fn get_agent_avatar(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentAvatar>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT content_type, image, updated_at FROM agent_avatars WHERE agent_id = ?1",
            [agent_id],
            |row| {
                Ok(AgentAvatar {
                    content_type: row.get(0)?,
                    image: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)
    }

    /// Inserts or replaces the avatar for `agent_id` and returns the new row
    /// version.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite write fails.
    pub(crate) fn upsert_agent_avatar(
        &self,
        agent_id: &str,
        content_type: &str,
        image: &[u8],
    ) -> Result<String, StorageError> {
        let updated_at = chrono::Utc::now().to_rfc3339();
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO agent_avatars (agent_id, content_type, image, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id) DO UPDATE SET
                 content_type = excluded.content_type,
                 image = excluded.image,
                 updated_at = excluded.updated_at",
            rusqlite::params![agent_id, content_type, image, updated_at],
        )?;
        Ok(updated_at)
    }

    /// Deletes the avatar for `agent_id`. Returns whether a row was removed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite write fails.
    pub(crate) fn delete_agent_avatar(&self, agent_id: &str) -> Result<bool, StorageError> {
        let conn = self.get_conn()?;
        let changed = conn.execute("DELETE FROM agent_avatars WHERE agent_id = ?1", [agent_id])?;
        Ok(changed > 0)
    }

    /// Returns `(agent_id, updated_at)` pairs for all stored avatars, so list
    /// endpoints can build cache-busting URLs without loading image bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite read fails.
    pub(crate) fn agent_avatar_versions(&self) -> Result<Vec<(String, String)>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare("SELECT agent_id, updated_at FROM agent_avatars")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    #[test]
    fn avatar_roundtrip_and_replace_bumps_version() {
        let (db, _dir) = test_db();

        assert_eq!(db.get_agent_avatar("lyre").expect("get"), None);

        let v1 = db
            .upsert_agent_avatar("lyre", "image/png", b"one")
            .expect("upsert");
        let stored = db.get_agent_avatar("lyre").expect("get").expect("some");
        assert_eq!(stored.content_type, "image/png");
        assert_eq!(stored.image, b"one");
        assert_eq!(stored.updated_at, v1);

        let v2 = db
            .upsert_agent_avatar("lyre", "image/webp", b"two-longer")
            .expect("upsert");
        let stored = db.get_agent_avatar("lyre").expect("get").expect("some");
        assert_eq!(stored.content_type, "image/webp");
        assert_eq!(stored.image, b"two-longer");
        assert_ne!(v1, v2, "replace must bump the row version");

        let versions = db.agent_avatar_versions().expect("versions");
        assert_eq!(versions, vec![("lyre".to_string(), v2)]);
    }

    #[test]
    fn delete_agent_avatar_reports_presence() {
        let (db, _dir) = test_db();

        assert!(
            !db.delete_agent_avatar("lyre").expect("delete"),
            "deleting an unset avatar is a no-op"
        );

        db.upsert_agent_avatar("lyre", "image/png", b"one")
            .expect("upsert");
        assert!(db.delete_agent_avatar("lyre").expect("delete"));
        assert_eq!(db.get_agent_avatar("lyre").expect("get"), None);
    }
}
