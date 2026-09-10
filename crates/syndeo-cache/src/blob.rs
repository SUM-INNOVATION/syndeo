//! Content-addressed blob storage.
//!
//! Bodies live on the filesystem under a hash-prefix layout, never in the index.
//! The address is BLAKE3 of the *uncompressed* bytes, so the same body reached
//! through ten different URLs is stored exactly once.

use crate::error::{CacheError, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A 32-byte BLAKE3 content address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentId(pub [u8; 32]);

impl ContentId {
    pub fn of(bytes: &[u8]) -> Self {
        ContentId(*blake3::hash(bytes).as_bytes())
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s).ok()?;
        let arr: [u8; 32] = bytes.try_into().ok()?;
        Some(ContentId(arr))
    }
}

impl std::fmt::Display for ContentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Compression {
    None,
    Zstd,
}

#[derive(Debug, Clone)]
pub struct WriteReceipt {
    pub id: ContentId,
    /// Length of the original body.
    pub len: u64,
    /// Bytes actually occupied on disk.
    pub stored_len: u64,
    pub compression: Compression,
    /// False when an identical body was already present — this is the dedupe signal.
    pub newly_written: bool,
}

pub struct BlobStore {
    root: PathBuf,
    zstd_level: i32,
    /// Bodies below this size are not worth compressing.
    compress_threshold: usize,
}

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(BlobStore {
            root,
            zstd_level: 3,
            compress_threshold: 1024,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/ab/cd/<full hex>` — two levels of fan-out keeps directory sizes sane
    /// at hundreds of millions of objects.
    fn path_for(&self, id: ContentId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(&hex)
    }

    pub fn contains(&self, id: ContentId) -> bool {
        self.path_for(id).exists()
    }

    /// Store a body, returning its address. Writing the same bytes twice is a
    /// no-op on disk and reports `newly_written: false`.
    pub fn put(&self, bytes: &[u8]) -> Result<WriteReceipt> {
        let id = ContentId::of(bytes);
        let path = self.path_for(id);
        let len = bytes.len() as u64;

        if path.exists() {
            let stored_len = fs::metadata(&path)?.len();
            return Ok(WriteReceipt {
                id,
                len,
                stored_len,
                compression: read_compression(&path)?,
                newly_written: false,
            });
        }

        let (payload, compression) = if bytes.len() >= self.compress_threshold {
            let compressed = zstd::stream::encode_all(bytes, self.zstd_level)?;
            if compressed.len() < bytes.len() {
                (compressed, Compression::Zstd)
            } else {
                (bytes.to_vec(), Compression::None)
            }
        } else {
            (bytes.to_vec(), Compression::None)
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write to a temp name in the same directory, then rename: a torn write
        // can never be observed under the content address.
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&[compression_tag(compression)])?;
            f.write_all(&payload)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;

        let stored_len = payload.len() as u64 + 1;
        Ok(WriteReceipt {
            id,
            len,
            stored_len,
            compression,
            newly_written: true,
        })
    }

    /// Begin a body whose length is not known yet.
    ///
    /// The address of a body is a hash of all of it, which reads like an
    /// argument for buffering: you cannot name the bytes until you have them
    /// all. You can, though, hash them as they pass and name them at the end —
    /// which is what this is. Bytes land in a temporary file under the store,
    /// and only a completed write is ever renamed into place under its address,
    /// so a torn or abandoned transfer leaves nothing that could be served.
    pub fn writer(&self) -> Result<BlobWriter> {
        let staging = self.root.join("staging");
        fs::create_dir_all(&staging)?;
        let path = staging.join(format!(
            "{}-{}",
            std::process::id(),
            hex::encode(rand_suffix())
        ));
        let file = fs::File::create(&path)?;
        Ok(BlobWriter {
            file: Some(file),
            path,
            hasher: blake3::Hasher::new(),
            digests: Some(crate::sri::StreamingDigests::new()),
            len: 0,
        })
    }

    /// Take ownership of a completed streaming write.
    ///
    /// Compression is decided here rather than while streaming: it needs the
    /// whole body to be worth doing, and by this point the whole body is on
    /// disk. A body already present under this address costs a delete rather
    /// than a rewrite, which is the dedupe path.
    fn adopt(&self, writer: &FinishedWrite) -> Result<WriteReceipt> {
        let id = writer.id;
        let path = self.path_for(id);
        if path.exists() {
            let _ = fs::remove_file(&writer.path);
            let stored_len = fs::metadata(&path)?.len();
            return Ok(WriteReceipt {
                id,
                len: writer.len,
                stored_len,
                compression: read_compression(&path)?,
                newly_written: false,
            });
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let raw = fs::read(&writer.path)?;
        let (payload, compression) = if raw.len() >= self.compress_threshold {
            let compressed = zstd::stream::encode_all(raw.as_slice(), self.zstd_level)?;
            if compressed.len() < raw.len() {
                (compressed, Compression::Zstd)
            } else {
                (raw, Compression::None)
            }
        } else {
            (raw, Compression::None)
        };

        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&[compression_tag(compression)])?;
            f.write_all(&payload)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        let _ = fs::remove_file(&writer.path);

        Ok(WriteReceipt {
            id,
            len: writer.len,
            stored_len: payload.len() as u64 + 1,
            compression,
            newly_written: true,
        })
    }

    /// Finish a streaming write and put the body under its address.
    ///
    /// Returns the receipt and the Subresource Integrity digests, which were
    /// computed on the way past rather than by re-reading the body.
    pub fn commit(&self, writer: BlobWriter) -> Result<(WriteReceipt, crate::sri::Digests)> {
        let finished = writer.finish()?;
        let receipt = self.adopt(&finished)?;
        Ok((receipt, finished.digests))
    }

    /// Read a body back, verifying that the bytes still hash to their address.
    pub fn get(&self, id: ContentId) -> Result<Vec<u8>> {
        let path = self.path_for(id);
        let raw = fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CacheError::MissingBlob(id.to_hex())
            } else {
                CacheError::Io(e)
            }
        })?;
        let Some((tag, payload)) = raw.split_first() else {
            return Err(CacheError::MissingBlob(id.to_hex()));
        };
        let bytes = match compression_from_tag(*tag) {
            Compression::None => payload.to_vec(),
            Compression::Zstd => zstd::stream::decode_all(payload)?,
        };
        let actual = ContentId::of(&bytes);
        if actual != id {
            // Silent corruption: drop it rather than serve it.
            let _ = fs::remove_file(&path);
            return Err(CacheError::Integrity {
                expected: id.to_hex(),
                actual: actual.to_hex(),
            });
        }
        Ok(bytes)
    }

    pub fn remove(&self, id: ContentId) -> Result<()> {
        match fs::remove_file(self.path_for(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CacheError::Io(e)),
        }
    }

    /// Total bytes on disk, walking the fan-out directories.
    pub fn disk_usage(&self) -> Result<u64> {
        fn walk(dir: &Path, total: &mut u64) -> Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let ft = entry.file_type()?;
                if ft.is_dir() {
                    walk(&entry.path(), total)?;
                } else if ft.is_file() {
                    *total += entry.metadata()?.len();
                }
            }
            Ok(())
        }
        let mut total = 0;
        walk(&self.root, &mut total)?;
        Ok(total)
    }
}

/// A body being written as it arrives.
///
/// Dropping one without committing removes the partial file: an interrupted
/// download costs nothing and leaves nothing.
pub struct BlobWriter {
    file: Option<fs::File>,
    path: PathBuf,
    hasher: blake3::Hasher,
    /// `Option` only so `finish` can take it out past the `Drop` implementation.
    digests: Option<crate::sri::StreamingDigests>,
    len: u64,
}

struct FinishedWrite {
    id: ContentId,
    path: PathBuf,
    len: u64,
    digests: crate::sri::Digests,
}

impl BlobWriter {
    /// Bytes written so far.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Err(CacheError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "this body has already been finished",
            )));
        };
        file.write_all(chunk)?;
        self.hasher.update(chunk);
        if let Some(digests) = self.digests.as_mut() {
            digests.update(chunk);
        }
        self.len += chunk.len() as u64;
        Ok(())
    }

    fn finish(mut self) -> Result<FinishedWrite> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            file.sync_all()?;
        }
        let id = ContentId(*self.hasher.finalize().as_bytes());
        let digests = self
            .digests
            .take()
            .map(|d| d.finish())
            .unwrap_or_default();
        Ok(FinishedWrite {
            id,
            // Taking the path also disarms `Drop`, which would otherwise delete
            // the file we are about to adopt.
            path: std::mem::take(&mut self.path),
            len: self.len,
            digests,
        })
    }

    /// Throw the partial body away. Also what `Drop` does.
    pub fn abandon(mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
        self.path = PathBuf::new();
    }
}

impl Drop for BlobWriter {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            self.file.take();
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Enough randomness to keep two concurrent writes in one process apart.
fn rand_suffix() -> [u8; 8] {
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (nanos ^ n.rotate_left(32)).to_le_bytes()
}

fn compression_tag(c: Compression) -> u8 {
    match c {
        Compression::None => 0,
        Compression::Zstd => 1,
    }
}

fn compression_from_tag(tag: u8) -> Compression {
    match tag {
        1 => Compression::Zstd,
        _ => Compression::None,
    }
}

fn read_compression(path: &Path) -> Result<Compression> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut tag = [0u8; 1];
    f.read_exact(&mut tag)?;
    Ok(compression_from_tag(tag[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BlobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path().join("blobs")).unwrap();
        (dir, store)
    }

    #[test]
    fn round_trips_small_and_large_bodies() {
        let (_dir, store) = store();
        for body in [vec![], b"hi".to_vec(), vec![b'x'; 200_000]] {
            let receipt = store.put(&body).unwrap();
            assert_eq!(store.get(receipt.id).unwrap(), body);
        }
    }

    #[test]
    fn identical_bodies_are_stored_once() {
        let (_dir, store) = store();
        let body = vec![b'a'; 50_000];
        let first = store.put(&body).unwrap();
        let second = store.put(&body).unwrap();
        assert!(first.newly_written);
        assert!(!second.newly_written);
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn compressible_bodies_shrink_on_disk() {
        let (_dir, store) = store();
        let body = vec![b'z'; 100_000];
        let receipt = store.put(&body).unwrap();
        assert_eq!(receipt.compression, Compression::Zstd);
        assert!(receipt.stored_len < receipt.len / 10);
    }

    #[test]
    fn incompressible_bodies_are_stored_raw() {
        let (_dir, store) = store();
        let body: Vec<u8> = (0..40_000u32).map(|i| blake3::hash(&i.to_le_bytes()).as_bytes()[0]).collect();
        let receipt = store.put(&body).unwrap();
        assert_eq!(receipt.compression, Compression::None);
        assert_eq!(store.get(receipt.id).unwrap(), body);
    }

    #[test]
    fn corrupted_blobs_are_refused_and_evicted() {
        let (_dir, store) = store();
        let receipt = store.put(b"trustworthy").unwrap();
        let path = store.path_for(receipt.id);
        fs::write(&path, [0u8, b'e', b'v', b'i', b'l']).unwrap();
        assert!(matches!(
            store.get(receipt.id),
            Err(CacheError::Integrity { .. })
        ));
        assert!(!path.exists());
    }

    #[test]
    fn missing_blob_is_reported_as_such() {
        let (_dir, store) = store();
        let id = ContentId::of(b"never stored");
        assert!(matches!(store.get(id), Err(CacheError::MissingBlob(_))));
    }
}
