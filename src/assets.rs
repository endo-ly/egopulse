use std::collections::HashSet;
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StorageError;

#[derive(Debug, Clone)]
pub struct AssetStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImageAsset {
    pub image_ref: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ImageAssetMetadata {
    image_ref: String,
    mime_type: String,
    byte_len: usize,
    created_at: String,
    sha256: String,
}

impl AssetStore {
    pub fn new(data_dir: &str) -> Result<Self, StorageError> {
        let root = Path::new(data_dir).join("assets");
        std::fs::create_dir_all(root.join("images"))?;
        Ok(Self { root })
    }

    pub fn persist_image_data_url(&self, data_url: &str) -> Result<StoredImageAsset, StorageError> {
        let (mime_type, bytes) = parse_image_data_url(data_url)?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let data_path = self.image_data_path(&sha256);
        let metadata_path = self.image_metadata_path(&sha256);

        if !data_path.exists() {
            std::fs::write(&data_path, &bytes)?;
        }

        if !metadata_path.exists() {
            let metadata = ImageAssetMetadata {
                image_ref: sha256.clone(),
                mime_type: mime_type.clone(),
                byte_len: bytes.len(),
                created_at: Utc::now().to_rfc3339(),
                sha256: sha256.clone(),
            };
            let json = serde_json::to_string(&metadata).map_err(StorageError::SessionSerialize)?;
            std::fs::write(metadata_path, json)?;
        }

        Ok(StoredImageAsset {
            image_ref: sha256,
            mime_type,
        })
    }

    pub fn load_image_data_url(
        &self,
        image_ref: &str,
        mime_type: &str,
    ) -> Result<String, StorageError> {
        let bytes = std::fs::read(self.image_data_path(image_ref))
            .map_err(|error| map_missing_asset(error, image_ref))?;
        Ok(format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    pub fn sweep_unreferenced_images(
        &self,
        referenced: &HashSet<String>,
    ) -> Result<Vec<String>, StorageError> {
        let mut removed = Vec::new();
        let images_dir = self.root.join("images");
        if !images_dir.exists() {
            return Ok(removed);
        }

        for entry in std::fs::read_dir(images_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("bin") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if referenced.contains(stem) {
                continue;
            }

            std::fs::remove_file(&path)?;
            let metadata_path = self.image_metadata_path(stem);
            if metadata_path.exists() {
                std::fs::remove_file(metadata_path)?;
            }
            removed.push(stem.to_string());
        }

        Ok(removed)
    }

    fn image_data_path(&self, image_ref: &str) -> PathBuf {
        self.root.join("images").join(format!("{image_ref}.bin"))
    }

    fn image_metadata_path(&self, image_ref: &str) -> PathBuf {
        self.root.join("images").join(format!("{image_ref}.json"))
    }
}

fn parse_image_data_url(data_url: &str) -> Result<(String, Vec<u8>), StorageError> {
    let Some(rest) = data_url.strip_prefix("data:") else {
        return Err(StorageError::InvalidAsset(
            "image data URL must start with data:".to_string(),
        ));
    };
    let Some((mime_type, base64_payload)) = rest.split_once(";base64,") else {
        return Err(StorageError::InvalidAsset(
            "image data URL must contain ;base64,".to_string(),
        ));
    };
    if mime_type.trim().is_empty() {
        return Err(StorageError::InvalidAsset(
            "image data URL is missing MIME type".to_string(),
        ));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_payload)
        .map_err(|error| StorageError::InvalidAsset(format!("invalid image base64: {error}")))?;
    Ok((mime_type.to_string(), bytes))
}

fn map_missing_asset(error: std::io::Error, image_ref: &str) -> StorageError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StorageError::NotFound(format!("image_ref:{image_ref}"))
    } else {
        StorageError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::AssetStore;

    #[test]
    fn persists_and_hydrates_images_by_reference() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AssetStore::new(dir.path().to_str().expect("utf8")).expect("store");
        let data_url = "data:image/png;base64,AAAA";

        let stored = store
            .persist_image_data_url(data_url)
            .expect("persist image");
        let hydrated = store
            .load_image_data_url(&stored.image_ref, &stored.mime_type)
            .expect("hydrate image");

        assert_eq!(stored.mime_type, "image/png");
        assert_eq!(hydrated, data_url);
    }

    #[test]
    fn deduplicates_identical_images_and_sweeps_unreferenced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AssetStore::new(dir.path().to_str().expect("utf8")).expect("store");
        let data_url = "data:image/png;base64,AAAA";

        let first = store
            .persist_image_data_url(data_url)
            .expect("persist first image");
        let second = store
            .persist_image_data_url(data_url)
            .expect("persist second image");

        assert_eq!(first.image_ref, second.image_ref);

        let removed = store
            .sweep_unreferenced_images(&HashSet::new())
            .expect("sweep images");
        assert_eq!(removed, vec![first.image_ref]);
    }
}
