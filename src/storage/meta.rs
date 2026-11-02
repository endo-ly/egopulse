//! Durable metadata that identifies the installation.
//!
//! Request-scoped identifiers derived from conversation keys must not collide
//! between separate installations that use the same upstream account, so the
//! installation identity is generated once and stored in `db_meta`.

use rusqlite::{OptionalExtension, params};

use super::Database;
use crate::error::StorageError;

/// `db_meta` key holding the installation identity.
const INSTALLATION_ID_KEY: &str = "installation_id";

impl Database {
    /// Returns the durable identity of this installation, creating it on first use.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the metadata cannot be read or written.
    pub(crate) fn installation_id(&self) -> Result<&str, StorageError> {
        if let Some(value) = self.installation_id.get() {
            return Ok(value);
        }
        let value = self.load_or_create_installation_id()?;
        Ok(self.installation_id.get_or_init(|| value))
    }

    fn load_or_create_installation_id(&self) -> Result<String, StorageError> {
        let conn = self.get_conn()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = ?1",
                params![INSTALLATION_ID_KEY],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(value) = existing {
            return Ok(value);
        }

        conn.execute(
            "INSERT INTO db_meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO NOTHING",
            params![INSTALLATION_ID_KEY, uuid::Uuid::new_v4().to_string()],
        )?;
        let value = conn.query_row(
            "SELECT value FROM db_meta WHERE key = ?1",
            params![INSTALLATION_ID_KEY],
            |row| row.get(0),
        )?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::INSTALLATION_ID_KEY;

    fn database(dir: &tempfile::TempDir) -> crate::storage::Database {
        crate::storage::Database::new(&dir.path().join("test.db")).expect("database")
    }

    #[test]
    fn installation_id_is_created_once_and_reused() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db = database(&dir);

        // Act
        let first = db.installation_id().expect("first").to_string();
        let second = db.installation_id().expect("second").to_string();

        // Assert
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn installation_id_survives_reopen() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.db");
        let created = {
            let db = crate::storage::Database::new(&path).expect("database");
            db.installation_id().expect("id").to_string()
        };

        // Act
        let reopened = crate::storage::Database::new(&path).expect("database");

        // Assert
        assert_eq!(reopened.installation_id().expect("id"), created);
    }

    #[test]
    fn separate_databases_get_different_installation_ids() {
        // Arrange
        let first_dir = tempfile::tempdir().expect("tempdir");
        let second_dir = tempfile::tempdir().expect("tempdir");

        // Act
        let first = database(&first_dir)
            .installation_id()
            .expect("id")
            .to_string();
        let second = database(&second_dir)
            .installation_id()
            .expect("id")
            .to_string();

        // Assert
        assert_ne!(first, second);
    }

    #[test]
    fn installation_id_is_stored_under_its_own_meta_key() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db = database(&dir);

        // Act
        let id = db.installation_id().expect("id").to_string();
        let conn = db.get_conn().expect("conn");
        let stored: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = ?1",
                rusqlite::params![INSTALLATION_ID_KEY],
                |row| row.get(0),
            )
            .expect("stored");

        // Assert
        assert_eq!(stored, id);
    }
}
