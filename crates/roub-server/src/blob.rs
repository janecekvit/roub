//! Blob contents on a local or mounted file system.
//!
//! Each blob is one file named by its id, under a directory per repository:
//! `<root>/<repository id>/<first two hex digits>/<id>`. Writes are atomic: the content goes to a
//! temporary file, is flushed to disk and renamed into place, so a crash never leaves a partial
//! blob under its id. Reads verify the content against the id, so disk corruption surfaces as an
//! error instead of wrong data. Cloud object stores (Azure Blob Storage, S3) will sit behind the
//! same operations.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use roub_core::{ObjectId, ObjectKind};

/// Blob contents in a directory tree.
pub struct LocalBlobStore {
    root: PathBuf,
}

/// Makes temporary file names unique within the process; the process id makes them unique
/// across servers sharing a file system.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl LocalBlobStore {
    /// Uses `root`, creating it if needed.
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Stores a blob unless its file already exists. The caller has checked that `id` is the blob
    /// id of `content`.
    pub async fn put(&self, repository: i64, id: ObjectId, content: Vec<u8>) -> io::Result<()> {
        let path = self.path(repository, &id);
        tokio::task::spawn_blocking(move || write_atomically(&path, &content))
            .await
            .map_err(io::Error::other)?
    }

    /// Returns a blob's content, or `None` if it is not stored.
    pub async fn get(&self, repository: i64, id: ObjectId) -> io::Result<Option<Vec<u8>>> {
        let path = self.path(repository, &id);
        let content = tokio::task::spawn_blocking(move || match std::fs::read(&path) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
        .await
        .map_err(io::Error::other)??;
        if let Some(content) = &content {
            let actual = ObjectId::compute(ObjectKind::Blob, content);
            if actual != id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("blob {id} is corrupt on disk: its content hashes to {actual}"),
                ));
            }
        }
        Ok(content)
    }

    fn path(&self, repository: i64, id: &ObjectId) -> PathBuf {
        let hex = id.to_string();
        self.root
            .join(repository.to_string())
            .join(&hex[..2])
            .join(hex)
    }
}

fn write_atomically(path: &Path, content: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let directory = path.parent().expect("blob paths have a parent directory");
    std::fs::create_dir_all(directory)?;
    let temp = directory.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::File::create_new(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        // Replacing a file a concurrent writer just renamed into place is harmless: both hold the
        // same content, because the name is the content's hash.
        std::fs::rename(&temp, path)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Makes the rename durable. Windows has no directory handle to flush; NTFS journals the rename.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    std::fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("roub-blob-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[tokio::test]
    async fn stores_and_verifies_blobs() {
        let root = temp_root("verify");
        let store = LocalBlobStore::new(&root).unwrap();
        let id = ObjectId::compute(ObjectKind::Blob, b"hello");
        assert_eq!(store.get(1, id).await.unwrap(), None);

        store.put(1, id, b"hello".to_vec()).await.unwrap();
        store.put(1, id, b"hello".to_vec()).await.unwrap();
        assert_eq!(
            store.get(1, id).await.unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        // Repositories do not share blobs.
        assert_eq!(store.get(2, id).await.unwrap(), None);

        // Corruption on disk is an error, not wrong content.
        std::fs::write(store.path(1, &id), b"jello").unwrap();
        let error = store.get(1, id).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        // No temporary files are left behind.
        let directory = store.path(1, &id).parent().unwrap().to_owned();
        let names: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(id.to_string())]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
