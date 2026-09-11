//! Byte ranges: parsing `Range` and `Content-Range`, and the arithmetic for
//! deciding which stored bytes answer a partial request (RFC 9110 §14).
//!
//! Serving the wrong range is much worse than serving none, so everything here
//! is total: an unsatisfiable or unparsable range resolves to `None` and the
//! caller falls back to the whole body or to the origin.

use http::HeaderMap;

/// One range as the client wrote it, before it is resolved against a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=first-last`, inclusive.
    FromTo { first: u64, last: u64 },
    /// `bytes=first-`, to the end.
    From { first: u64 },
    /// `bytes=-suffix`, the last `suffix` bytes.
    Suffix { length: u64 },
}

/// A resolved range: inclusive byte offsets into a representation of a known
/// length. `first <= last < complete_len` always holds for a value of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub first: u64,
    pub last: u64,
    pub complete_len: u64,
}

impl Resolved {
    pub fn len(&self) -> u64 {
        self.last - self.first + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Half-open `[start, end)`, which is how segments are stored.
    pub fn half_open(&self) -> (u64, u64) {
        (self.first, self.last + 1)
    }

    /// The `Content-Range` field value describing this range.
    pub fn content_range(&self) -> String {
        format!("bytes {}-{}/{}", self.first, self.last, self.complete_len)
    }
}

/// Parse a `Range` field value. Only `bytes` is a unit we understand; anything
/// else, and anything malformed, yields `None` and the range is ignored.
pub fn parse_range(value: &str) -> Option<Vec<RangeSpec>> {
    let rest = value
        .trim()
        .strip_prefix("bytes")?
        .trim_start()
        .strip_prefix('=')?;
    let mut out = Vec::new();
    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (start, end) = part.split_once('-')?;
        let (start, end) = (start.trim(), end.trim());
        let spec = match (start.is_empty(), end.is_empty()) {
            // `-500`: the last 500 bytes. A zero-length suffix is unsatisfiable.
            (true, false) => {
                let length: u64 = end.parse().ok()?;
                if length == 0 {
                    return None;
                }
                RangeSpec::Suffix { length }
            }
            (false, true) => RangeSpec::From {
                first: start.parse().ok()?,
            },
            (false, false) => {
                let first: u64 = start.parse().ok()?;
                let last: u64 = end.parse().ok()?;
                if last < first {
                    return None;
                }
                RangeSpec::FromTo { first, last }
            }
            (true, true) => return None,
        };
        out.push(spec);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Resolve one spec against a known complete length.
///
/// `None` means unsatisfiable, which RFC 9110 §14.2 says to answer either by
/// ignoring the range or with a 416. We ignore it and serve the whole body,
/// which is the safer of the two.
pub fn resolve(spec: RangeSpec, complete_len: u64) -> Option<Resolved> {
    if complete_len == 0 {
        return None;
    }
    let (first, last) = match spec {
        RangeSpec::FromTo { first, last } => {
            if first >= complete_len {
                return None;
            }
            (first, last.min(complete_len - 1))
        }
        RangeSpec::From { first } => {
            if first >= complete_len {
                return None;
            }
            (first, complete_len - 1)
        }
        RangeSpec::Suffix { length } => {
            let length = length.min(complete_len);
            (complete_len - length, complete_len - 1)
        }
    };
    Some(Resolved {
        first,
        last,
        complete_len,
    })
}

/// A parsed `Content-Range: bytes first-last/complete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub first: u64,
    pub last: u64,
    /// `None` for the `*` form, where the origin declines to say how long the
    /// whole representation is.
    pub complete_len: Option<u64>,
}

impl ContentRange {
    pub fn len(&self) -> u64 {
        self.last - self.first + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn half_open(&self) -> (u64, u64) {
        (self.first, self.last + 1)
    }
}

pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    // An unsatisfied-range form, `bytes */1234`, describes no bytes at all.
    let (range, complete) = rest.split_once('/')?;
    let range = range.trim();
    if range == "*" {
        return None;
    }
    let (first, last) = range.split_once('-')?;
    let first: u64 = first.trim().parse().ok()?;
    let last: u64 = last.trim().parse().ok()?;
    if last < first {
        return None;
    }
    let complete = complete.trim();
    let complete_len = if complete == "*" {
        None
    } else {
        Some(complete.parse().ok()?)
    };
    if let Some(total) = complete_len {
        if last >= total {
            return None;
        }
    }
    Some(ContentRange {
        first,
        last,
        complete_len,
    })
}

/// True when a 206 carries several ranges in one multipart body.
///
/// We do not store these: reassembling a `multipart/byteranges` body into
/// segments is a parser we would have to get exactly right, and getting it
/// slightly wrong means serving the wrong bytes under a hash that says they are
/// the right ones. They pass through instead, explicitly.
pub fn is_multipart(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.trim()
                .to_ascii_lowercase()
                .starts_with("multipart/byteranges")
        })
        .unwrap_or(false)
}

/// Whether an `If-Range` field value still matches what we hold.
///
/// `If-Range` means "send me the range if the representation has not changed,
/// and the whole thing if it has". Against a cache the question is whether the
/// stored representation is the one the client already has part of.
pub fn if_range_matches(value: &str, stored: &HeaderMap) -> bool {
    let value = value.trim();
    // An entity-tag. Weak tags may not be used for If-Range at all (§13.1.5),
    // and a strong comparison is exact.
    if value.starts_with('"') || value.starts_with("W/") {
        if value.starts_with("W/") {
            return false;
        }
        return stored
            .get(http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|etag| etag.trim() == value && !etag.trim().starts_with("W/"))
            .unwrap_or(false);
    }
    // Otherwise an HTTP-date, compared against Last-Modified exactly.
    match (
        crate::headers::parse_http_date(value),
        crate::headers::header_date(stored, http::header::LAST_MODIFIED),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Non-overlapping, ascending half-open byte intervals.
///
/// This is what "the gaps are tracked" actually means: a partial entry knows
/// exactly which bytes it holds, so it can answer the ranges it covers and
/// refuse the ones it does not, rather than guessing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage(Vec<(u64, u64)>);

impl Coverage {
    pub fn from_sorted(intervals: Vec<(u64, u64)>) -> Self {
        let mut coverage = Coverage(intervals);
        coverage.normalize();
        coverage
    }

    pub fn intervals(&self) -> &[(u64, u64)] {
        &self.0
    }

    pub fn add(&mut self, start: u64, end: u64) {
        if end > start {
            self.0.push((start, end));
            self.normalize();
        }
    }

    /// True when every byte of `[start, end)` is held.
    pub fn covers(&self, start: u64, end: u64) -> bool {
        if end <= start {
            return true;
        }
        self.0.iter().any(|(s, e)| *s <= start && *e >= end)
    }

    /// The parts of `[start, end)` we do not hold. This is what a newly received
    /// range contributes, and storing only this is what stops a re-fetched range
    /// from replacing what is already there.
    pub fn missing(&self, start: u64, end: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut cursor = start;
        for (s, e) in &self.0 {
            if *e <= cursor {
                continue;
            }
            if *s >= end {
                break;
            }
            if *s > cursor {
                out.push((cursor, (*s).min(end)));
            }
            cursor = cursor.max(*e);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            out.push((cursor, end));
        }
        out
    }

    fn normalize(&mut self) {
        self.0.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.0.len());
        for (start, end) in self.0.drain(..) {
            match merged.last_mut() {
                // Adjacent counts as contiguous: `[0,10)` and `[10,20)` are one run.
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        self.0 = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_range_forms() {
        assert_eq!(
            parse_range("bytes=0-99"),
            Some(vec![RangeSpec::FromTo { first: 0, last: 99 }])
        );
        assert_eq!(
            parse_range("bytes=100-"),
            Some(vec![RangeSpec::From { first: 100 }])
        );
        assert_eq!(
            parse_range("bytes=-500"),
            Some(vec![RangeSpec::Suffix { length: 500 }])
        );
        assert_eq!(
            parse_range("bytes=0-9, 20-29"),
            Some(vec![
                RangeSpec::FromTo { first: 0, last: 9 },
                RangeSpec::FromTo {
                    first: 20,
                    last: 29
                },
            ])
        );
    }

    #[test]
    fn malformed_ranges_are_ignored_rather_than_guessed_at() {
        for bad in [
            "items=0-9",
            "bytes=9-0",
            "bytes=-",
            "bytes=x-y",
            "bytes=",
            "bytes=-0",
        ] {
            assert_eq!(parse_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn resolution_clamps_to_the_representation() {
        let r = resolve(
            RangeSpec::FromTo {
                first: 0,
                last: 4_000,
            },
            100,
        )
        .unwrap();
        assert_eq!((r.first, r.last), (0, 99));
        assert_eq!(r.content_range(), "bytes 0-99/100");

        let r = resolve(RangeSpec::Suffix { length: 10 }, 100).unwrap();
        assert_eq!((r.first, r.last), (90, 99));

        let r = resolve(RangeSpec::Suffix { length: 400 }, 100).unwrap();
        assert_eq!(
            (r.first, r.last),
            (0, 99),
            "a suffix longer than the body is the body"
        );

        assert!(resolve(
            RangeSpec::FromTo {
                first: 100,
                last: 200
            },
            100
        )
        .is_none());
        assert!(resolve(RangeSpec::From { first: 100 }, 100).is_none());
    }

    #[test]
    fn parses_content_range_including_the_unknown_length_form() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000"),
            Some(ContentRange {
                first: 0,
                last: 99,
                complete_len: Some(1000)
            })
        );
        assert_eq!(
            parse_content_range("bytes 0-99/*"),
            Some(ContentRange {
                first: 0,
                last: 99,
                complete_len: None
            })
        );
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert_eq!(
            parse_content_range("bytes 990-1099/1000"),
            None,
            "past the end"
        );
    }

    #[test]
    fn coverage_merges_adjacent_and_overlapping_runs() {
        let mut c = Coverage::default();
        c.add(0, 10);
        c.add(10, 20);
        assert_eq!(c.intervals(), &[(0, 20)]);
        c.add(5, 15);
        assert_eq!(c.intervals(), &[(0, 20)]);
        c.add(30, 40);
        assert_eq!(c.intervals(), &[(0, 20), (30, 40)]);
        assert!(c.covers(0, 20));
        assert!(!c.covers(0, 21));
        assert!(c.covers(30, 40));
    }

    #[test]
    fn coverage_reports_exactly_the_bytes_it_does_not_hold() {
        let c = Coverage::from_sorted(vec![(10, 20), (30, 40)]);
        assert_eq!(c.missing(0, 50), vec![(0, 10), (20, 30), (40, 50)]);
        assert_eq!(c.missing(12, 18), Vec::<(u64, u64)>::new());
        assert_eq!(c.missing(15, 35), vec![(20, 30)]);
        assert_eq!(c.missing(45, 50), vec![(45, 50)]);
    }
}
