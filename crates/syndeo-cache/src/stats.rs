//! Cache counters, and the two ratios that decide whether any of this is worth
//! building: hit rate and dedupe ratio.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub requests: u64,
    pub hits: u64,
    pub stale_hits: u64,
    pub misses: u64,
    pub revalidations: u64,
    pub not_modified: u64,
    pub stores: u64,
    pub rejects: u64,
    pub bytes_from_cache: u64,
    pub bytes_from_origin: u64,
    pub bytes_deduped: u64,
    pub peer_accepted: u64,
    pub peer_rejected: u64,
    /// Requests answered with a byte range out of the store.
    pub range_hits: u64,
    /// Stores that left an entry still missing bytes.
    pub partial_stores: u64,
    /// Live entries dropped to stay inside the size budget.
    pub evictions: u64,

    pub entries: u64,
    pub blobs: u64,
    /// Sum of distinct body sizes.
    pub unique_bytes: u64,
    /// What those bodies actually occupy after compression.
    pub on_disk_bytes: u64,
    /// Sum of body sizes counted once per referencing entry.
    pub logical_bytes: u64,
    /// Rows in the Subresource Integrity index.
    pub sri_rows: u64,
}

impl Stats {
    pub(crate) fn apply_counter(&mut self, name: &str, value: u64) {
        use crate::cache::counters as c;
        match name {
            c::REQUESTS => self.requests = value,
            c::HITS => self.hits = value,
            c::STALE_HITS => self.stale_hits = value,
            c::MISSES => self.misses = value,
            c::REVALIDATIONS => self.revalidations = value,
            c::NOT_MODIFIED => self.not_modified = value,
            c::STORES => self.stores = value,
            c::REJECTS => self.rejects = value,
            c::BYTES_FROM_CACHE => self.bytes_from_cache = value,
            c::BYTES_FROM_ORIGIN => self.bytes_from_origin = value,
            c::BYTES_DEDUPED => self.bytes_deduped = value,
            c::PEER_ACCEPTED => self.peer_accepted = value,
            c::PEER_REJECTED => self.peer_rejected = value,
            c::RANGE_HITS => self.range_hits = value,
            c::PARTIAL_STORES => self.partial_stores = value,
            c::EVICTIONS => self.evictions = value,
            _ => {}
        }
    }

    /// Fraction of requests answered without a full origin body transfer. A 304
    /// counts: the body did not cross the wire.
    pub fn hit_rate(&self) -> f64 {
        if self.requests == 0 {
            return 0.0;
        }
        let served = self.hits + self.stale_hits;
        served as f64 / self.requests as f64
    }

    /// Logical bytes divided by distinct bytes. 1.0 means no duplication found.
    pub fn dedupe_ratio(&self) -> f64 {
        if self.unique_bytes == 0 {
            return 1.0;
        }
        self.logical_bytes as f64 / self.unique_bytes as f64
    }

    /// Distinct bytes divided by bytes on disk, i.e. what zstd bought.
    pub fn compression_ratio(&self) -> f64 {
        if self.on_disk_bytes == 0 {
            return 1.0;
        }
        self.unique_bytes as f64 / self.on_disk_bytes as f64
    }

    /// Fraction of body bytes that never had to be fetched.
    pub fn byte_hit_rate(&self) -> f64 {
        let total = self.bytes_from_cache + self.bytes_from_origin;
        if total == 0 {
            return 0.0;
        }
        self.bytes_from_cache as f64 / total as f64
    }

    pub fn render(&self) -> String {
        format!(
            "requests {}  hits {}  stale {}  misses {}  revalidations {} (304: {})\n\
             hit rate {:.1}%  byte hit rate {:.1}%\n\
             entries {}  blobs {}  dedupe {:.2}x  compression {:.2}x\n\
             unique {}  on disk {}  logical {}\n\
             range hits {}  partial stores {}  evictions {}\n\
             peer accepted {}  peer rejected {}",
            self.requests,
            self.hits,
            self.stale_hits,
            self.misses,
            self.revalidations,
            self.not_modified,
            self.hit_rate() * 100.0,
            self.byte_hit_rate() * 100.0,
            self.entries,
            self.blobs,
            self.dedupe_ratio(),
            self.compression_ratio(),
            human(self.unique_bytes),
            human(self.on_disk_bytes),
            human(self.logical_bytes),
            self.range_hits,
            self.partial_stores,
            self.evictions,
            self.peer_accepted,
            self.peer_rejected,
        )
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
