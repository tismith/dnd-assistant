//! Download and verify model files without shelling out to curl or another
//! helper. Downloads are written to a sibling temporary file and renamed only
//! after the optional SHA-256 check succeeds.

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
use thiserror::Error;

pub fn default_model_cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"));
    base.join("dnd-assistant").join("models")
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model file operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("model download failed: {0}")]
    Download(String),
    #[error("model checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
}

pub fn ensure_model(
    url: &str,
    destination: impl AsRef<Path>,
    expected_sha256: Option<&str>,
) -> Result<PathBuf, ModelError> {
    let destination = destination.as_ref();
    if destination.is_file() {
        if let Some(expected) = expected_sha256 {
            verify_checksum(destination, expected)?;
        }
        return Ok(destination.to_owned());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension("download");
    let mut response = ureq::get(url)
        .call()
        .map_err(|error| ModelError::Download(error.to_string()))?;
    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(&temporary)?;
    io::copy(&mut reader, &mut file)?;
    file.sync_all()?;
    if let Some(expected) = expected_sha256 {
        verify_checksum(&temporary, expected)?;
    }
    fs::rename(&temporary, destination)?;
    Ok(destination.to_owned())
}

fn verify_checksum(path: &Path, expected: &str) -> Result<(), ModelError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected.to_ascii_lowercase() {
        return Err(ModelError::Checksum {
            expected: expected.into(),
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_model;
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn verifies_existing_model_without_network() {
        let root = std::env::temp_dir().join(format!(
            "dnd-model-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("model.bin");
        fs::write(&path, b"model-fixture").unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"model-fixture");
        let checksum = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            ensure_model("https://invalid.example/model.bin", &path, Some(&checksum)).unwrap(),
            path
        );
        fs::remove_dir_all(root).unwrap();
    }
}
