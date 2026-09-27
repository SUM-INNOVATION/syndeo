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
    /// Test-only pause points, for driving an interleaving deterministically.
    #[cfg(test)]
    pause: std::sync::Mutex<Option<PauseHook>>,
}

#[cfg(test)]
type PauseHook = std::sync::Arc<dyn Fn(&'static str) + Send + Sync>;

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(BlobStore {
            root,
            zstd_level: 3,
            compress_threshold: 1024,
            #[cfg(test)]
            pause: std::sync::Mutex::new(None),
        })
    }

    fn pause_point(&self, _name: &'static str) {
        #[cfg(test)]
        {
            let hook = self.pause.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(_name);
            }
        }
    }

    /// Write `payload` under `path`, through a temporary file of its own.
    ///
    /// The temporary name is unique to this write — process, a counter and
    /// randomness — and created with `create_new`, so two writes of the same
    /// bytes, in one process or two, never share one: a clash is an error,
    /// never a silent truncation of someone else's half-written file. What is
    /// renamed into place is only ever a whole file, and a write that fails
    /// on the way removes its own temporary file.
    fn write_in_place(&self, path: &Path, compression: Compression, payload: &[u8]) -> Result<()> {
        let tmp = temporary_beside(path);
        let written = (|| -> Result<()> {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(&[compression_tag(compression)])?;
            f.write_all(payload)?;
            f.sync_all()?;
            self.pause_point("write:before-rename");
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    /// Remove what interrupted writes left behind: everything in `staging/`,
    /// and every temporary file in the fan-out directories, in this build's
    /// naming or the one before it.
    ///
    /// Only safe while nothing else can be writing, which is when the caller
    /// holds the cache index — redb takes it exclusively, so this is called
    /// from `Cache::open` and nowhere else. Returns how many were removed.
    pub(crate) fn sweep_abandoned(&self) -> Result<usize> {
        let mut removed = 0;
        let staging = self.root.join("staging");
        if staging.is_dir() {
            for entry in fs::read_dir(&staging)? {
                let entry = entry?;
                if entry.file_type()?.is_file() && fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        for first in fs::read_dir(&self.root)? {
            let first = first?;
            if !first.file_type()?.is_dir() || !is_fan_out(&first.file_name()) {
                continue;
            }
            for second in fs::read_dir(first.path())? {
                let second = second?;
                if !second.file_type()?.is_dir() || !is_fan_out(&second.file_name()) {
                    continue;
                }
                for file in fs::read_dir(second.path())? {
                    let file = file?;
                    if file.file_type()?.is_file()
                        && is_temporary(&file.file_name())
                        && fs::remove_file(file.path()).is_ok()
                    {
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/ab/cd/<full hex>` — two levels of fan-out keeps directory sizes sane
    /// at hundreds of millions of objects.
    pub(crate) fn path_for(&self, id: ContentId) -> PathBuf {
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

        // A torn write can never be observed under the content address.
        self.write_in_place(&path, compression, &payload)?;

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

        self.write_in_place(&path, compression, &payload)?;
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
        let digests = self.digests.take().map(|d| d.finish()).unwrap_or_default();
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

/// A temporary name beside `path`, used by exactly one write:
/// `.<file name>.tmp-<pid>-<unique>`. The leading dot keeps it from ever
/// looking like a content address.
fn temporary_beside(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        hex::encode(rand_suffix())
    ))
}

/// A two-hex-digit fan-out directory name.
fn is_fan_out(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// A temporary file, as this build names them or as 0.1.3 did
/// (`<hex>.tmp<pid>`). Never a content address, which is 64 hex digits and
/// nothing else.
fn is_temporary(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    if name.starts_with('.') && name.contains(".tmp-") {
        return true;
    }
    match name.split_once(".tmp") {
        Some((stem, pid)) => {
            stem.len() == 64
                && stem.chars().all(|c| c.is_ascii_hexdigit())
                && !pid.is_empty()
                && pid.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
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
        let body: Vec<u8> = (0..40_000u32)
            .map(|i| blake3::hash(&i.to_le_bytes()).as_bytes()[0])
            .collect();
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

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn two_writes_of_the_same_bytes_never_share_a_temporary_file() {
        // Writer A has written its temporary file in full and is paused before
        // renaming it; writer B writes the same bytes start to finish. With one
        // temporary name per process, B truncates A's file, renames it away,
        // and A's rename then fails. Driven here on purpose, not by chance.
        let (_dir, store) = store();
        let store = std::sync::Arc::new(store);
        let body = vec![b'q'; 5_000];
        let path = store.path_for(ContentId::of(&body));

        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();
        let resume_rx = std::sync::Mutex::new(resume_rx);
        let first = std::sync::atomic::AtomicBool::new(true);
        *store.pause.lock().unwrap() = Some(std::sync::Arc::new(move |point| {
            if point == "write:before-rename"
                && first.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                paused_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
            }
        }));

        let a = {
            let store = store.clone();
            let body = body.clone();
            std::thread::spawn(move || store.put(&body))
        };
        paused_rx.recv().unwrap();
        let dir = path.parent().unwrap().to_path_buf();
        let a_temporary = files_in(&dir);
        assert_eq!(a_temporary.len(), 1, "{a_temporary:?}");
        let a_len = fs::metadata(dir.join(&a_temporary[0])).unwrap().len();

        // B, entirely, while A is paused.
        let b = store.put(&body).unwrap();
        assert!(b.newly_written);
        assert_eq!(
            fs::metadata(dir.join(&a_temporary[0])).unwrap().len(),
            a_len,
            "B touched A's temporary file"
        );

        resume_tx.send(()).unwrap();
        let a = a.join().unwrap().expect("A's write failed");
        assert_eq!(a.id, b.id);
        assert_eq!(store.get(a.id).unwrap(), body);
        assert_eq!(
            files_in(&dir),
            vec![ContentId::of(&body).to_hex()],
            "a temporary file was left behind"
        );
    }

    #[test]
    fn a_write_that_cannot_be_renamed_leaves_no_temporary_file() {
        let (_dir, store) = store();
        let body = b"cannot land".to_vec();
        let path = store.path_for(ContentId::of(&body));
        let blocker = path.clone();
        // Something occupies the address between the existence check and the
        // rename: a directory, which a file cannot be renamed over.
        *store.pause.lock().unwrap() = Some(std::sync::Arc::new(move |point| {
            if point == "write:before-rename" {
                fs::create_dir_all(blocker.join("occupied")).unwrap();
            }
        }));
        assert!(store.put(&body).is_err());
        let dir = path.parent().unwrap();
        assert_eq!(
            files_in(dir),
            vec![ContentId::of(&body).to_hex()],
            "only the blocking directory should remain"
        );
    }

    #[test]
    fn abandoned_temporary_files_are_swept_and_blobs_are_not() {
        let (_dir, store) = store();
        let kept = store.put(b"a real blob").unwrap();
        let dir = store.path_for(kept.id).parent().unwrap().to_path_buf();
        let hex = kept.id.to_hex();

        // This build's naming, the 0.1.3 naming, and a streaming write.
        fs::write(
            dir.join(format!(".{hex}.tmp-123-00ff00ff00ff00ff")),
            b"torn",
        )
        .unwrap();
        fs::write(dir.join(format!("{hex}.tmp4567")), b"torn").unwrap();
        fs::create_dir_all(store.root().join("staging")).unwrap();
        fs::write(store.root().join("staging").join("99-abcdef"), b"torn").unwrap();

        assert_eq!(store.sweep_abandoned().unwrap(), 3);
        assert_eq!(files_in(&dir), vec![hex]);
        assert!(files_in(&store.root().join("staging")).is_empty());
        assert_eq!(store.get(kept.id).unwrap(), b"a real blob");
    }

    #[test]
    fn only_temporary_names_are_recognised_as_temporary() {
        let hex = "ab".repeat(32);
        for temporary in [
            format!(".{hex}.tmp-1-00"),
            format!("{hex}.tmp1"),
            format!("{hex}.tmp123456"),
        ] {
            assert!(
                is_temporary(std::ffi::OsStr::new(&temporary)),
                "{temporary}"
            );
        }
        for kept in [
            hex.clone(),
            format!("{hex}.tmp"),
            format!("{hex}.tmpx"),
            "notes.tmp1".to_string(),
        ] {
            assert!(!is_temporary(std::ffi::OsStr::new(&kept)), "{kept}");
        }
    }
}
