//! The cache index: a redb-backed map from request to content address, and from
//! content address to blob metadata with a refcount.
//!
//! The separation of those two tables is what gives dedupe, portability, and the
//! ability to satisfy a fetch from a peer: an entry names a hash, and the hash is
//! all a peer ever needs to be checked against.

use crate::blob::{Compression, ContentId};
use crate::error::{CacheError, Result};
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Bumped whenever the layout of anything `bincode` writes into this file
/// changes. `bincode` has no field names and no tolerance for change: a record
/// written under one layout deserializes into garbage under another, silently.
/// The version is written before any record is, and checked before any record
/// is read, so a layout change is a clear error rather than a mis-parse.
///
/// Version 1 is the first layout to carry a version at all. An index written by
/// the unversioned build reads as `None` and is refused for the same reason a
/// version we do not recognise is.
pub const SCHEMA_VERSION: u64 = 1;

/// Index-wide scalars. Not `bincode`, so it stays readable across any change to
/// the record layout — a version check that could itself mis-parse is no check.
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const SCHEMA_KEY: &str = "schema_version";

const ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");
const BLOBS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blobs");
const VARIANTS: TableDefinition<&str, &[u8]> = TableDefinition::new("variants");
const COUNTERS: TableDefinition<&str, u64> = TableDefinition::new("counters");
/// Subresource Integrity digest to content address. A peer can be asked for a
/// body by its SRI digest, which is the only hash a page declares, and the
/// answer is still self-verifying.
const SRI: TableDefinition<&[u8], &[u8]> = TableDefinition::new("sri");

/// Where a stored body came from. A peer-supplied body is only ever recorded
/// after it has been checked against a hash obtained independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    Origin,
    Peer,
    Import,
}

/// One contiguous run of bytes from a partial response, held as its own blob.
///
/// `[start, end)`. Segments in an entry are kept sorted and non-overlapping, so
/// the set of them is exactly the coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub content: [u8; 32],
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn content_id(&self) -> ContentId {
        ContentId(self.content)
    }
}

/// What an entry actually holds.
///
/// A cache that only ever stores whole bodies misses video, large PDFs and every
/// resumed download entirely. A partial entry is the alternative: it names the
/// runs it has, so a later range either falls inside them or is fetched and
/// merged, and the entry becomes complete when the runs meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredBody {
    Complete {
        content: [u8; 32],
        len: u64,
    },
    Partial {
        /// The length of the whole representation, when the origin said. A 206
        /// carrying `bytes 0-99/*` does not, and such an entry can never be
        /// promoted to complete because nothing knows when it would be done.
        complete_len: Option<u64>,
        segments: Vec<Segment>,
    },
}

impl StoredBody {
    pub fn is_complete(&self) -> bool {
        matches!(self, StoredBody::Complete { .. })
    }

    /// The single content address of a complete body.
    pub fn complete_content(&self) -> Option<ContentId> {
        match self {
            StoredBody::Complete { content, .. } => Some(ContentId(*content)),
            StoredBody::Partial { .. } => None,
        }
    }

    /// Length of the whole representation, when it is known.
    pub fn complete_len(&self) -> Option<u64> {
        match self {
            StoredBody::Complete { len, .. } => Some(*len),
            StoredBody::Partial { complete_len, .. } => *complete_len,
        }
    }

    /// Bytes actually held, which is the whole body for a complete entry and the
    /// sum of the segments for a partial one.
    pub fn held_len(&self) -> u64 {
        match self {
            StoredBody::Complete { len, .. } => *len,
            StoredBody::Partial { segments, .. } => segments.iter().map(|s| s.len()).sum(),
        }
    }

    /// Which byte runs this entry holds.
    pub fn coverage(&self) -> crate::range::Coverage {
        match self {
            StoredBody::Complete { len, .. } => {
                crate::range::Coverage::from_sorted(vec![(0, *len)])
            }
            StoredBody::Partial { segments, .. } => crate::range::Coverage::from_sorted(
                segments.iter().map(|s| (s.start, s.end)).collect(),
            ),
        }
    }

    /// Every distinct blob this entry refers to.
    pub fn contents(&self) -> Vec<[u8; 32]> {
        let mut out = match self {
            StoredBody::Complete { content, .. } => vec![*content],
            StoredBody::Partial { segments, .. } => segments.iter().map(|s| s.content).collect(),
        };
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryRecord {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub vary_fields: Vec<String>,
    pub vary_key: String,
    pub body: StoredBody,
    pub request_time: u64,
    pub response_time: u64,
    pub stored_at: u64,
    pub last_used: u64,
    pub hits: u64,
    pub provenance: Provenance,
}

impl EntryRecord {
    /// The content address of a complete body. A partial entry has none, which
    /// is why this is an `Option` rather than a lie.
    pub fn content_id(&self) -> Option<ContentId> {
        self.body.complete_content()
    }

    pub fn contents(&self) -> Vec<[u8; 32]> {
        self.body.contents()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobRecord {
    pub len: u64,
    pub stored_len: u64,
    pub compression: Compression,
    pub refcount: u32,
    pub created: u64,
}

/// Primary key for a URL, before the `Vary` secondary key is applied.
pub fn primary_key(method: &str, url: &str) -> String {
    format!("{}\u{1}{}", method.to_ascii_uppercase(), url)
}

/// Full cache key: primary key plus the secondary (`Vary`) key.
pub fn entry_key(method: &str, url: &str, vary_key: &str) -> String {
    format!("{}\u{1}{}", primary_key(method, url), vary_key)
}

pub struct Index {
    db: Database,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Index")
    }
}

impl Index {
    /// Open an index, refusing one written under a layout we do not understand.
    ///
    /// The refusal is the point. Reading a foreign record with `bincode` does not
    /// fail cleanly; it produces a struct full of plausible nonsense. So the
    /// version is settled before the first record is touched.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // A path that does not exist yet, or exists and is empty, is ours to
        // stamp. Anything else has to say which layout it was written under.
        let fresh = std::fs::metadata(path)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
        let db = Database::create(path)?;

        if !fresh {
            let found = read_schema_version(&db)?;
            if found != Some(SCHEMA_VERSION) {
                return Err(CacheError::SchemaMismatch {
                    found,
                    expected: SCHEMA_VERSION,
                });
            }
        }

        // Materialise every table up front so read transactions never trip over
        // a table that has not been written yet.
        let tx = db.begin_write()?;
        {
            tx.open_table(META)?.insert(SCHEMA_KEY, SCHEMA_VERSION)?;
            tx.open_table(ENTRIES)?;
            tx.open_table(BLOBS)?;
            tx.open_table(VARIANTS)?;
            tx.open_table(COUNTERS)?;
            tx.open_table(SRI)?;
        }
        tx.commit()?;
        Ok(Index { db })
    }

    /// The layout this index was written under.
    pub fn schema_version(&self) -> Result<Option<u64>> {
        read_schema_version(&self.db)
    }

    // ---- entries -----------------------------------------------------------

    pub fn get_entry(&self, key: &str) -> Result<Option<EntryRecord>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        match table.get(key)? {
            Some(bytes) => Ok(Some(bincode::deserialize(bytes.value())?)),
            None => Ok(None),
        }
    }

    /// Insert an entry, register it as a variant of its URL, and take a reference
    /// on every blob it names. All in one transaction so refcounts cannot drift.
    ///
    /// `blobs` supplies the metadata for content addresses that are not already
    /// in the blob table; addresses already there keep the record they have.
    /// Returns the blobs whose last reference this write released, which happens
    /// when an entry is replaced by one naming different bytes.
    pub fn put_entry(
        &self,
        record: &EntryRecord,
        new_blobs: &[(ContentId, BlobRecord)],
    ) -> Result<Vec<ContentId>> {
        let key = entry_key(&record.method, &record.url, &record.vary_key);
        let pkey = primary_key(&record.method, &record.url);
        let encoded = bincode::serialize(record)?;
        let wanted = record.contents();
        let mut orphaned = Vec::new();

        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut blobs = tx.open_table(BLOBS)?;
            let mut variants = tx.open_table(VARIANTS)?;

            // Replacing an entry releases the references it used to hold, but
            // only the ones the new entry does not hold too — a partial entry
            // that grew a segment still refers to all its old ones.
            let held: Vec<[u8; 32]> = match entries.get(key.as_str())? {
                Some(bytes) => {
                    let previous: EntryRecord = bincode::deserialize(bytes.value())?;
                    previous.contents()
                }
                None => Vec::new(),
            };
            for content in &held {
                if !wanted.contains(content) && release(&mut blobs, content)? {
                    orphaned.push(ContentId(*content));
                }
            }

            for content in &wanted {
                if held.contains(content) {
                    continue;
                }
                let existing: Option<BlobRecord> = match blobs.get(content.as_slice())? {
                    Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                    None => None,
                };
                let updated = match existing {
                    Some(mut b) => {
                        b.refcount = b.refcount.saturating_add(1);
                        b
                    }
                    None => {
                        let Some((_, blob)) = new_blobs.iter().find(|(id, _)| id.0 == *content)
                        else {
                            return Err(CacheError::MissingBlob(ContentId(*content).to_hex()));
                        };
                        BlobRecord {
                            refcount: 1,
                            ..blob.clone()
                        }
                    }
                };
                blobs.insert(content.as_slice(), bincode::serialize(&updated)?.as_slice())?;
            }

            entries.insert(key.as_str(), encoded.as_slice())?;

            let mut keys: Vec<String> = match variants.get(pkey.as_str())? {
                Some(bytes) => bincode::deserialize(bytes.value())?,
                None => Vec::new(),
            };
            if !keys.contains(&record.vary_key) {
                keys.push(record.vary_key.clone());
                variants.insert(pkey.as_str(), bincode::serialize(&keys)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(orphaned)
    }

    /// Overwrite an entry in place without touching refcounts — used after a 304
    /// folds fresh headers into an unchanged body.
    pub fn refresh_entry(&self, record: &EntryRecord) -> Result<()> {
        let key = entry_key(&record.method, &record.url, &record.vary_key);
        let encoded = bincode::serialize(record)?;
        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            entries.insert(key.as_str(), encoded.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The `Vary` keys stored for a URL. An empty list means nothing is stored.
    pub fn variant_keys(&self, method: &str, url: &str) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(VARIANTS)?;
        match table.get(primary_key(method, url).as_str())? {
            Some(bytes) => Ok(bincode::deserialize(bytes.value())?),
            None => Ok(Vec::new()),
        }
    }

    /// Drop every variant of a URL. Returns the content ids whose refcount hit
    /// zero, which the caller may then delete from the blob store.
    pub fn invalidate(&self, method: &str, url: &str) -> Result<Vec<ContentId>> {
        let pkey = primary_key(method, url);
        let mut orphaned = Vec::new();
        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut blobs = tx.open_table(BLOBS)?;
            let mut variants = tx.open_table(VARIANTS)?;

            let keys: Vec<String> = match variants.get(pkey.as_str())? {
                Some(bytes) => bincode::deserialize(bytes.value())?,
                None => Vec::new(),
            };
            for vary_key in keys {
                let key = format!("{pkey}\u{1}{vary_key}");
                let removed: Option<EntryRecord> = match entries.remove(key.as_str())? {
                    Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                    None => None,
                };
                if let Some(record) = removed {
                    for content in record.contents() {
                        if release(&mut blobs, &content)? {
                            orphaned.push(ContentId(content));
                        }
                    }
                }
            }
            variants.remove(pkey.as_str())?;
        }
        tx.commit()?;
        Ok(orphaned)
    }

    /// Drop one entry. Returns the blobs whose last reference it held.
    pub fn remove_entry(&self, key: &str) -> Result<Vec<ContentId>> {
        let mut orphan = Vec::new();
        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut blobs = tx.open_table(BLOBS)?;
            let removed: Option<EntryRecord> = match entries.remove(key)? {
                Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                None => None,
            };
            if let Some(record) = removed {
                for content in record.contents() {
                    if release(&mut blobs, &content)? {
                        orphan.push(ContentId(content));
                    }
                }
                let pkey = primary_key(&record.method, &record.url);
                let mut variants = tx.open_table(VARIANTS)?;
                let mut keys: Vec<String> = match variants.get(pkey.as_str())? {
                    Some(v) => bincode::deserialize(v.value())?,
                    None => Vec::new(),
                };
                keys.retain(|k| k != &record.vary_key);
                if keys.is_empty() {
                    variants.remove(pkey.as_str())?;
                } else {
                    variants.insert(pkey.as_str(), bincode::serialize(&keys)?.as_slice())?;
                }
            }
        }
        tx.commit()?;
        Ok(orphan)
    }

    /// Everything one served request changes, in a single transaction.
    ///
    /// The hit path used to be four separate write transactions — the request
    /// counter, the entry's last-used stamp, the hit counter, the byte counter
    /// — each one committing, and each commit an fsync. redb allows one writer
    /// at a time, so on a page with seventy-eight subresources that is three
    /// hundred serialised fsyncs before the page can paint, and the measurement
    /// was unambiguous: a *cache hit*, with no network in it at all, took a
    /// median of 488ms to answer.
    ///
    /// One transaction now, and `Durability::None` on it. These are statistics
    /// and an eviction timestamp: losing the last few seconds of them to a
    /// crash costs a slightly wrong hit rate and a slightly wrong eviction
    /// order. Paying an fsync each to avoid that made the cache slower than the
    /// network it exists to replace, which is the only way this component can
    /// truly fail.
    ///
    /// Stored bytes are not written here. Those go through `put`, which commits
    /// durably, because losing *those* loses the body itself.
    pub fn record_access(
        &self,
        key: Option<&str>,
        now: u64,
        counters: &[(&str, u64)],
    ) -> Result<()> {
        let mut tx = self.db.begin_write()?;
        tx.set_durability(redb::Durability::None);
        {
            if let Some(key) = key {
                let mut entries = tx.open_table(ENTRIES)?;
                let existing: Option<EntryRecord> = match entries.get(key)? {
                    Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                    None => None,
                };
                if let Some(mut record) = existing {
                    record.last_used = now;
                    record.hits += 1;
                    entries.insert(key, bincode::serialize(&record)?.as_slice())?;
                }
            }
            let mut table = tx.open_table(COUNTERS)?;
            for (name, by) in counters {
                let current = { table.get(*name)?.map(|v| v.value()).unwrap_or(0) };
                table.insert(*name, current.saturating_add(*by))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Record a hit against an entry.
    pub fn touch(&self, key: &str, now: u64) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let existing: Option<EntryRecord> = match entries.get(key)? {
                Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                None => None,
            };
            if let Some(mut record) = existing {
                record.last_used = now;
                record.hits += 1;
                entries.insert(key, bincode::serialize(&record)?.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn all_entries(&self) -> Result<Vec<(String, EntryRecord)>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(ENTRIES)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (k, v) = row?;
            out.push((k.value().to_string(), bincode::deserialize(v.value())?));
        }
        Ok(out)
    }

    pub fn get_blob(&self, id: ContentId) -> Result<Option<BlobRecord>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(BLOBS)?;
        match table.get(id.0.as_slice())? {
            Some(bytes) => Ok(Some(bincode::deserialize(bytes.value())?)),
            None => Ok(None),
        }
    }

    /// Blob-table totals: (distinct blobs, unique bytes, on-disk bytes, logical
    /// bytes). Logical counts every reference, so logical/unique is the dedupe ratio.
    pub fn blob_totals(&self) -> Result<(u64, u64, u64, u64)> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(BLOBS)?;
        let (mut count, mut unique, mut on_disk, mut logical) = (0, 0, 0, 0);
        for row in table.iter()? {
            let (_, v) = row?;
            let record: BlobRecord = bincode::deserialize(v.value())?;
            if record.refcount == 0 {
                continue;
            }
            count += 1;
            unique += record.len;
            on_disk += record.stored_len;
            logical += record.len * record.refcount as u64;
        }
        Ok((count, unique, on_disk, logical))
    }

    pub fn entry_count(&self) -> Result<u64> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(ENTRIES)?.len()?)
    }

    /// Blobs nothing references any more.
    pub fn orphaned_blobs(&self) -> Result<Vec<ContentId>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(BLOBS)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (k, v) = row?;
            let record: BlobRecord = bincode::deserialize(v.value())?;
            if record.refcount == 0 {
                if let Ok(arr) = <[u8; 32]>::try_from(k.value()) {
                    out.push(ContentId(arr));
                }
            }
        }
        Ok(out)
    }

    pub fn forget_blob(&self, id: ContentId) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut blobs = tx.open_table(BLOBS)?;
            blobs.remove(id.0.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- integrity index ---------------------------------------------------

    /// Record the SRI digests of a body so it can be found by the hash a page
    /// declares rather than by our internal address.
    pub fn index_sri(&self, digests: &[(Vec<u8>, [u8; 32])]) -> Result<()> {
        if digests.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(SRI)?;
            for (key, content) in digests {
                table.insert(key.as_slice(), content.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop integrity rows pointing at content that is no longer stored.
    ///
    /// Nothing else removes them, so without this they accumulate for the life
    /// of the cache: a lookup succeeds, `body_by_content` then fails, and the
    /// peer answers `Missing` — correct, but the row is still there.
    pub fn prune_sri(&self) -> Result<usize> {
        let stale: Vec<Vec<u8>> = {
            let tx = self.db.begin_read()?;
            let sri = tx.open_table(SRI)?;
            let blobs = tx.open_table(BLOBS)?;
            let mut out = Vec::new();
            for row in sri.iter()? {
                let (key, value) = row?;
                if blobs.get(value.value())?.is_none() {
                    out.push(key.value().to_vec());
                }
            }
            out
        };
        if stale.is_empty() {
            return Ok(0);
        }
        let tx = self.db.begin_write()?;
        {
            let mut sri = tx.open_table(SRI)?;
            for key in &stale {
                sri.remove(key.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(stale.len())
    }

    pub fn sri_row_count(&self) -> Result<u64> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(SRI)?.len()?)
    }

    pub fn content_for_sri(&self, key: &[u8]) -> Result<Option<ContentId>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(SRI)?;
        match table.get(key)? {
            Some(bytes) => Ok(<[u8; 32]>::try_from(bytes.value()).ok().map(ContentId)),
            None => Ok(None),
        }
    }

    // ---- counters ----------------------------------------------------------

    pub fn bump(&self, name: &str, by: u64) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(COUNTERS)?;
            let current = { table.get(name)?.map(|v| v.value()).unwrap_or(0) };
            table.insert(name, current.saturating_add(by))?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn counter(&self, name: &str) -> Result<u64> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(COUNTERS)?;
        Ok(table.get(name)?.map(|v| v.value()).unwrap_or(0))
    }

    pub fn counters(&self) -> Result<Vec<(String, u64)>> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(COUNTERS)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (k, v) = row?;
            out.push((k.value().to_string(), v.value()));
        }
        Ok(out)
    }
}

/// Drop one reference to a blob. Returns true when it reached zero.
fn release(
    blobs: &mut redb::Table<'_, &'static [u8], &'static [u8]>,
    content: &[u8; 32],
) -> Result<bool> {
    let existing: Option<BlobRecord> = match blobs.get(content.as_slice())? {
        Some(bytes) => Some(bincode::deserialize(bytes.value())?),
        None => None,
    };
    if let Some(mut record) = existing {
        record.refcount = record.refcount.saturating_sub(1);
        let zero = record.refcount == 0;
        blobs.insert(content.as_slice(), bincode::serialize(&record)?.as_slice())?;
        return Ok(zero);
    }
    Ok(false)
}

/// The recorded layout version, or `None` for an index written before there was
/// one. Deliberately does not touch any `bincode` record.
fn read_schema_version(db: &Database) -> Result<Option<u64>> {
    let tx = db.begin_read()?;
    let table = match tx.open_table(META) {
        Ok(t) => t,
        // No `meta` table at all: an index from the unversioned build.
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(table.get(SCHEMA_KEY)?.map(|v| v.value()))
}

/// Write a chosen schema version into an index file, so a test can produce the
/// thing this check exists to catch without keeping an old build around.
#[cfg(test)]
pub(crate) fn stamp_schema_version(path: &Path, version: Option<u64>) -> Result<()> {
    let db = Database::create(path)?;
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(META)?;
        match version {
            Some(v) => {
                table.insert(SCHEMA_KEY, v)?;
            }
            None => {
                table.remove(SCHEMA_KEY)?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("index.redb")
    }

    #[test]
    fn a_new_index_records_the_schema_it_was_written_under() {
        let dir = tempfile::tempdir().unwrap();
        let index = Index::open(index_path(&dir)).unwrap();
        assert_eq!(index.schema_version().unwrap(), Some(SCHEMA_VERSION));
    }

    #[test]
    fn reopening_the_same_schema_keeps_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(&dir);
        {
            let index = Index::open(&path).unwrap();
            index.bump("requests", 7).unwrap();
        }
        let reopened = Index::open(&path).unwrap();
        assert_eq!(reopened.counter("requests").unwrap(), 7);
    }

    #[test]
    fn a_schema_we_do_not_understand_is_an_error_and_not_a_mis_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = index_path(&dir);
        {
            let _ = Index::open(&path).unwrap();
        }

        // A future layout.
        stamp_schema_version(&path, Some(SCHEMA_VERSION + 1)).unwrap();
        match Index::open(&path) {
            Err(CacheError::SchemaMismatch { found, expected }) => {
                assert_eq!(found, Some(SCHEMA_VERSION + 1));
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected a schema mismatch, got {other:?}"),
        }

        // And the layout that predates the version, which is the one actually
        // out there on disk today.
        stamp_schema_version(&path, None).unwrap();
        match Index::open(&path) {
            Err(CacheError::SchemaMismatch { found: None, .. }) => {}
            other => panic!("expected an unversioned index to be refused, got {other:?}"),
        }
    }
}
