//! The cache index: a redb-backed map from request to content address, and from
//! content address to blob metadata with a refcount.
//!
//! The separation of those two tables is what gives dedupe, portability, and the
//! ability to satisfy a fetch from a peer: an entry names a hash, and the hash is
//! all a peer ever needs to be checked against.

use crate::blob::{Compression, ContentId};
use crate::error::Result;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryRecord {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub vary_fields: Vec<String>,
    pub vary_key: String,
    pub content: [u8; 32],
    pub body_len: u64,
    pub request_time: u64,
    pub response_time: u64,
    pub stored_at: u64,
    pub last_used: u64,
    pub hits: u64,
    pub provenance: Provenance,
}

impl EntryRecord {
    pub fn content_id(&self) -> ContentId {
        ContentId(self.content)
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

impl Index {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path.as_ref())?;
        // Materialise every table up front so read transactions never trip over
        // a table that has not been written yet.
        let tx = db.begin_write()?;
        {
            tx.open_table(ENTRIES)?;
            tx.open_table(BLOBS)?;
            tx.open_table(VARIANTS)?;
            tx.open_table(COUNTERS)?;
            tx.open_table(SRI)?;
        }
        tx.commit()?;
        Ok(Index { db })
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
    /// on the blob it names. All in one transaction so the refcount cannot drift.
    pub fn put_entry(&self, record: &EntryRecord, blob: BlobRecord) -> Result<()> {
        let key = entry_key(&record.method, &record.url, &record.vary_key);
        let pkey = primary_key(&record.method, &record.url);
        let encoded = bincode::serialize(record)?;

        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut blobs = tx.open_table(BLOBS)?;
            let mut variants = tx.open_table(VARIANTS)?;

            // Replacing an entry releases the reference it used to hold.
            let previous: Option<EntryRecord> = match entries.get(key.as_str())? {
                Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                None => None,
            };
            if let Some(prev) = &previous {
                if prev.content != record.content {
                    release(&mut blobs, &prev.content)?;
                }
            }

            let existing: Option<BlobRecord> = match blobs.get(record.content.as_slice())? {
                Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                None => None,
            };
            let updated = match existing {
                Some(mut b) => {
                    // Only count a new reference when this key did not already hold one.
                    let already = previous.map(|p| p.content == record.content).unwrap_or(false);
                    if !already {
                        b.refcount = b.refcount.saturating_add(1);
                    }
                    b
                }
                None => BlobRecord {
                    refcount: 1,
                    ..blob
                },
            };
            blobs.insert(record.content.as_slice(), bincode::serialize(&updated)?.as_slice())?;
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
        Ok(())
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
                    if release(&mut blobs, &record.content)? {
                        orphaned.push(ContentId(record.content));
                    }
                }
            }
            variants.remove(pkey.as_str())?;
        }
        tx.commit()?;
        Ok(orphaned)
    }

    pub fn remove_entry(&self, key: &str) -> Result<Option<ContentId>> {
        let mut orphan = None;
        let tx = self.db.begin_write()?;
        {
            let mut entries = tx.open_table(ENTRIES)?;
            let mut blobs = tx.open_table(BLOBS)?;
            let removed: Option<EntryRecord> = match entries.remove(key)? {
                Some(bytes) => Some(bincode::deserialize(bytes.value())?),
                None => None,
            };
            if let Some(record) = removed {
                if release(&mut blobs, &record.content)? {
                    orphan = Some(ContentId(record.content));
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
