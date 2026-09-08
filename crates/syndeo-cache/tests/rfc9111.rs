//! RFC 9111 conformance suite.
//!
//! Each case names the section it comes from. The policy layer is pure, so these
//! run against a fixed clock with no store; the end-to-end cases at the bottom
//! drive the real index and blob store.

use http::HeaderMap;
use syndeo_cache::policy::{self, CacheOptions, Freshness, Storability, StoredMeta};
use syndeo_cache::{Cache, Lookup, StoreOutcome};
use std::sync::Arc;

/// A cache pinned to the suite's fixed clock.
fn cache_at(dir: &std::path::Path, now: u64) -> Cache {
    Cache::open(dir).unwrap().with_clock(Arc::new(move || now))
}

const NOW: u64 = 1_700_000_000;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.append(
            http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    h
}

fn date(secs: u64) -> String {
    syndeo_cache::headers::format_http_date(secs)
}

/// A response that arrived `age_secs` ago with the given headers.
fn stored(status: u16, pairs: &[(&str, &str)], age_secs: u64) -> StoredMeta {
    let mut all = vec![("date".to_string(), date(NOW - age_secs))];
    for (k, v) in pairs {
        all.push((k.to_string(), v.to_string()));
    }
    let refs: Vec<(&str, &str)> = all.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    StoredMeta {
        status,
        headers: headers(&refs),
        request_time: NOW - age_secs,
        response_time: NOW - age_secs,
    }
}

fn private() -> CacheOptions {
    CacheOptions::default()
}

fn shared() -> CacheOptions {
    CacheOptions {
        shared: true,
        ..CacheOptions::default()
    }
}

fn eval(req: &[(&str, &str)], meta: &StoredMeta, opts: &CacheOptions) -> Freshness {
    policy::evaluate(&headers(req), meta, NOW, opts)
}

fn is_fresh(f: &Freshness) -> bool {
    matches!(f, Freshness::Fresh { .. })
}

fn is_revalidate(f: &Freshness) -> bool {
    matches!(f, Freshness::Revalidate { .. })
}

// ---------------------------------------------------------------- §4.2 freshness

#[test]
fn s4_2_max_age_bounds_freshness() {
    let meta = stored(200, &[("cache-control", "max-age=100")], 50);
    assert!(is_fresh(&eval(&[], &meta, &private())));

    let meta = stored(200, &[("cache-control", "max-age=100")], 150);
    assert!(is_revalidate(&eval(&[], &meta, &private())));
}

#[test]
fn s4_2_1_max_age_beats_expires() {
    // Expires says long gone, max-age says fresh: max-age wins.
    let meta = stored(
        200,
        &[
            ("cache-control", "max-age=1000"),
            ("expires", &date(NOW - 500)),
        ],
        10,
    );
    assert!(is_fresh(&eval(&[], &meta, &private())));
}

#[test]
fn s4_2_1_s_maxage_applies_only_to_shared_caches() {
    let meta = stored(200, &[("cache-control", "max-age=10, s-maxage=1000")], 100);
    assert!(is_revalidate(&eval(&[], &meta, &private())), "private honours max-age");
    assert!(is_fresh(&eval(&[], &meta, &shared())), "shared honours s-maxage");
}

#[test]
fn s4_2_1_expires_is_relative_to_origin_date() {
    // Origin clock is 1000s ahead of ours; Expires must still be read against Date.
    let skew = 1000;
    let meta = StoredMeta {
        status: 200,
        headers: headers(&[
            ("date", &date(NOW + skew)),
            ("expires", &date(NOW + skew + 60)),
        ]),
        request_time: NOW - 10,
        response_time: NOW - 10,
    };
    assert!(is_fresh(&eval(&[], &meta, &private())));
}

#[test]
fn s4_2_1_unparsable_expires_means_already_stale() {
    let meta = stored(200, &[("expires", "0")], 1);
    assert!(is_revalidate(&eval(&[], &meta, &private())));
}

#[test]
fn s4_2_2_heuristic_freshness_is_a_tenth_of_the_last_modified_delta() {
    // Last modified 1000s before Date -> 100s of heuristic freshness.
    let meta = StoredMeta {
        status: 200,
        headers: headers(&[
            ("date", &date(NOW - 50)),
            ("last-modified", &date(NOW - 1050)),
        ]),
        request_time: NOW - 50,
        response_time: NOW - 50,
    };
    let (lifetime, heuristic) = policy::freshness_lifetime(&meta, &private());
    assert_eq!(lifetime, 100);
    assert!(heuristic);
    assert!(is_fresh(&eval(&[], &meta, &private())));
}

#[test]
fn s4_2_2_heuristic_freshness_is_capped() {
    let meta = StoredMeta {
        status: 200,
        headers: headers(&[
            ("date", &date(NOW)),
            // Ten years old: a tenth of that is a year, which we refuse.
            ("last-modified", &date(NOW - 10 * 365 * 86400)),
        ]),
        request_time: NOW,
        response_time: NOW,
    };
    let (lifetime, _) = policy::freshness_lifetime(&meta, &private());
    assert_eq!(lifetime, private().max_heuristic_lifetime);
}

#[test]
fn s4_2_3_age_accounts_for_upstream_age_and_request_delay() {
    // Upstream said the response was already 50s old, and it took 10s to reach us.
    let meta = StoredMeta {
        status: 200,
        headers: headers(&[("date", &date(NOW - 20)), ("age", "50")]),
        request_time: NOW - 30,
        response_time: NOW - 20,
    };
    // corrected_age = 50 + 10 = 60; resident = 20; total 80.
    assert_eq!(policy::current_age(&meta, NOW), 80);
}

#[test]
fn s4_2_3_apparent_age_covers_a_lying_upstream_age() {
    // No Age header, but Date is 500s in the past: apparent age is 500.
    let meta = StoredMeta {
        status: 200,
        headers: headers(&[("date", &date(NOW - 500))]),
        request_time: NOW,
        response_time: NOW,
    };
    assert_eq!(policy::current_age(&meta, NOW), 500);
}

#[test]
fn s4_2_4_must_revalidate_forbids_serving_stale() {
    let meta = stored(200, &[("cache-control", "max-age=10, must-revalidate")], 100);
    // Even when the client explicitly accepts staleness.
    assert!(is_revalidate(&eval(&[("cache-control", "max-stale")], &meta, &private())));
}

#[test]
fn s4_2_4_stale_while_revalidate_permits_serving_stale() {
    let meta = stored(
        200,
        &[("cache-control", "max-age=10, stale-while-revalidate=100")],
        50,
    );
    match eval(&[], &meta, &private()) {
        Freshness::ServeStale {
            refresh_in_background,
            ..
        } => assert!(refresh_in_background),
        other => panic!("expected serve-stale, got {other:?}"),
    }
}

#[test]
fn s4_2_4_stale_while_revalidate_window_is_bounded() {
    let meta = stored(
        200,
        &[("cache-control", "max-age=10, stale-while-revalidate=20")],
        200,
    );
    assert!(is_revalidate(&eval(&[], &meta, &private())));
}

#[test]
fn stale_if_error_is_reported_to_the_caller() {
    let meta = stored(200, &[("cache-control", "max-age=10, stale-if-error=600"), ("etag", "\"v1\"")], 100);
    match eval(&[], &meta, &private()) {
        Freshness::Revalidate { stale_if_error, .. } => assert_eq!(stale_if_error, Some(600)),
        other => panic!("expected revalidate, got {other:?}"),
    }
}

// ------------------------------------------------------------ §4.1 vary matching

#[test]
fn s4_1_vary_selects_the_matching_variant() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/asset";
    let resp = headers(&[
        ("cache-control", "max-age=600"),
        ("vary", "accept-encoding"),
        ("date", &date(NOW)),
    ]);

    cache
        .store("GET", url, &headers(&[("accept-encoding", "gzip")]), 200, &resp, b"gzip body", NOW, NOW)
        .unwrap();
    cache
        .store("GET", url, &headers(&[("accept-encoding", "br")]), 200, &resp, b"br body", NOW, NOW)
        .unwrap();

    for (encoding, expected) in [("gzip", &b"gzip body"[..]), ("br", &b"br body"[..])] {
        match cache
            .lookup("GET", url, &headers(&[("accept-encoding", encoding)]))
            .unwrap()
        {
            Lookup::Fresh(r) => assert_eq!(r.body, expected),
            other => panic!("expected a hit for {encoding}, got {other:?}"),
        }
    }

    // An encoding we never stored must miss rather than serve the wrong body.
    assert!(matches!(
        cache
            .lookup("GET", url, &headers(&[("accept-encoding", "zstd")]))
            .unwrap(),
        Lookup::Miss(_)
    ));
}

#[test]
fn s4_1_vary_star_is_never_stored() {
    let meta = stored(200, &[("cache-control", "max-age=600"), ("vary", "*")], 0);
    assert_eq!(
        policy::storability("GET", &HeaderMap::new(), &meta, 10, &private()),
        Storability::Reject("Vary: *")
    );
}

// -------------------------------------------------------------- §3 storability

#[test]
fn s3_no_store_is_honoured_on_both_sides() {
    let meta = stored(200, &[("cache-control", "no-store")], 0);
    assert!(matches!(
        policy::storability("GET", &HeaderMap::new(), &meta, 10, &private()),
        Storability::Reject(_)
    ));

    let ok = stored(200, &[("cache-control", "max-age=600")], 0);
    assert!(matches!(
        policy::storability("GET", &headers(&[("cache-control", "no-store")]), &ok, 10, &private()),
        Storability::Reject(_)
    ));
}

#[test]
fn s3_uncacheable_methods_are_not_stored() {
    let meta = stored(200, &[("cache-control", "max-age=600")], 0);
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        assert!(matches!(
            policy::storability(method, &HeaderMap::new(), &meta, 10, &private()),
            Storability::Reject(_)
        ));
    }
}

#[test]
fn s3_private_responses_are_refused_by_shared_caches_only() {
    let meta = stored(200, &[("cache-control", "private, max-age=600")], 0);
    assert_eq!(
        policy::storability("GET", &HeaderMap::new(), &meta, 10, &private()),
        Storability::Store
    );
    assert!(matches!(
        policy::storability("GET", &HeaderMap::new(), &meta, 10, &shared()),
        Storability::Reject(_)
    ));
}

#[test]
fn s3_5_authorized_requests_need_explicit_shared_permission() {
    let auth = headers(&[("authorization", "Bearer t")]);
    let plain = stored(200, &[("cache-control", "max-age=600")], 0);
    assert!(matches!(
        policy::storability("GET", &auth, &plain, 10, &shared()),
        Storability::Reject(_)
    ));

    let permitted = stored(200, &[("cache-control", "max-age=600, public")], 0);
    assert_eq!(
        policy::storability("GET", &auth, &permitted, 10, &shared()),
        Storability::Store
    );
    // A private cache is the user's own, so authorization is not a barrier.
    assert_eq!(
        policy::storability("GET", &auth, &plain, 10, &private()),
        Storability::Store
    );
}

#[test]
fn s3_statuses_outside_the_heuristic_set_need_explicit_freshness() {
    let bare = stored(500, &[("last-modified", &date(NOW - 10_000))], 0);
    assert!(matches!(
        policy::storability("GET", &HeaderMap::new(), &bare, 10, &private()),
        Storability::Reject(_)
    ));

    let explicit = stored(500, &[("cache-control", "max-age=60")], 0);
    assert_eq!(
        policy::storability("GET", &HeaderMap::new(), &explicit, 10, &private()),
        Storability::Store
    );
}

#[test]
fn s3_heuristically_cacheable_statuses_are_stored_without_directives() {
    for status in [200u16, 301, 404, 410] {
        let meta = stored(status, &[("last-modified", &date(NOW - 10_000))], 0);
        assert_eq!(
            policy::storability("GET", &HeaderMap::new(), &meta, 10, &private()),
            Storability::Store,
            "status {status}"
        );
    }
}

#[test]
fn s5_2_2_3_must_understand_overrides_no_store() {
    let meta = stored(200, &[("cache-control", "no-store, must-understand, max-age=60")], 0);
    assert_eq!(
        policy::storability("GET", &HeaderMap::new(), &meta, 10, &private()),
        Storability::Store
    );
}

#[test]
fn a_response_with_neither_freshness_nor_a_validator_is_pointless_to_store() {
    let meta = stored(200, &[], 0);
    assert!(matches!(
        policy::storability("GET", &HeaderMap::new(), &meta, 10, &private()),
        Storability::Reject(_)
    ));
}

// --------------------------------------------------- §5.2.1 request directives

#[test]
fn s5_2_1_request_no_cache_forces_revalidation() {
    let meta = stored(200, &[("cache-control", "max-age=600"), ("etag", "\"v1\"")], 10);
    assert!(is_revalidate(&eval(&[("cache-control", "no-cache")], &meta, &private())));
}

#[test]
fn s5_2_1_request_max_age_can_be_stricter_than_the_response() {
    let meta = stored(200, &[("cache-control", "max-age=600"), ("etag", "\"v1\"")], 100);
    assert!(is_fresh(&eval(&[], &meta, &private())));
    assert!(is_revalidate(&eval(&[("cache-control", "max-age=50")], &meta, &private())));
}

#[test]
fn s5_2_1_min_fresh_requires_remaining_lifetime() {
    let meta = stored(200, &[("cache-control", "max-age=100"), ("etag", "\"v1\"")], 80);
    assert!(is_fresh(&eval(&[("cache-control", "min-fresh=10")], &meta, &private())));
    assert!(is_revalidate(&eval(&[("cache-control", "min-fresh=50")], &meta, &private())));
}

#[test]
fn s5_2_1_max_stale_accepts_bounded_staleness() {
    let meta = stored(200, &[("cache-control", "max-age=10")], 60);
    assert!(matches!(
        eval(&[("cache-control", "max-stale=100")], &meta, &private()),
        Freshness::ServeStale { .. }
    ));
    assert!(is_revalidate(&eval(&[("cache-control", "max-stale=5")], &meta, &private())));
}

// ------------------------------------------------------------ §5.2.2 no-cache

#[test]
fn s5_2_2_response_no_cache_forces_revalidation_every_time() {
    let meta = stored(200, &[("cache-control", "no-cache, max-age=600"), ("etag", "\"v1\"")], 1);
    assert!(is_revalidate(&eval(&[], &meta, &private())));
}

#[test]
fn s5_2_2_qualified_no_cache_only_suppresses_named_fields() {
    let meta = stored(
        200,
        &[
            ("cache-control", "max-age=600, no-cache=\"set-cookie\""),
            ("set-cookie", "session=abc"),
        ],
        10,
    );
    assert!(is_fresh(&eval(&[], &meta, &private())), "still usable");
    assert_eq!(policy::suppressed_fields(&meta), vec!["set-cookie"]);
}

// ------------------------------------------------------------ §4.3 validation

#[test]
fn s4_3_1_conditional_headers_are_built_from_the_stored_validators() {
    let meta = stored(
        200,
        &[
            ("cache-control", "max-age=1"),
            ("etag", "\"v1\""),
            ("last-modified", &date(NOW - 5000)),
        ],
        100,
    );
    let conditional = policy::conditional_headers(&meta);
    assert_eq!(conditional[0].0, http::header::IF_NONE_MATCH);
    assert_eq!(conditional[0].1, "\"v1\"");
    assert_eq!(conditional[1].0, http::header::IF_MODIFIED_SINCE);
}

#[test]
fn s4_3_4_a_304_updates_headers_but_not_content_metadata() {
    let mut stored_headers = headers(&[
        ("cache-control", "max-age=1"),
        ("etag", "\"v1\""),
        ("content-length", "1234"),
        ("x-origin", "old"),
    ]);
    let fresh = headers(&[
        ("cache-control", "max-age=600"),
        ("x-origin", "new"),
        ("content-length", "999"),
        ("connection", "keep-alive"),
    ]);
    policy::apply_304(&mut stored_headers, &fresh);

    assert_eq!(stored_headers["cache-control"], "max-age=600");
    assert_eq!(stored_headers["x-origin"], "new");
    assert_eq!(stored_headers["content-length"], "1234", "must not be overwritten");
    assert!(!stored_headers.contains_key("connection"), "hop-by-hop dropped");
    assert_eq!(stored_headers["etag"], "\"v1\"", "untouched fields survive");
}

// ------------------------------------------------------------ §4.4 invalidation

#[test]
fn s4_4_unsafe_methods_invalidate_the_target() {
    assert!(policy::invalidates("POST"));
    assert!(policy::invalidates("DELETE"));
    assert!(!policy::invalidates("GET"));
    assert!(!policy::invalidates("HEAD"));

    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/thing";
    let resp = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
    cache
        .store("GET", url, &HeaderMap::new(), 200, &resp, b"v1", NOW, NOW)
        .unwrap();
    assert!(matches!(cache.lookup("GET", url, &HeaderMap::new()).unwrap(), Lookup::Fresh(_)));

    cache.invalidate("POST", url).unwrap();
    assert!(matches!(cache.lookup("GET", url, &HeaderMap::new()).unwrap(), Lookup::Miss(_)));
}

// ------------------------------------------------- pass-through, not mishandled

#[test]
fn range_and_client_conditionals_are_passed_through() {
    let meta = stored(200, &[("cache-control", "max-age=600")], 10);
    assert!(matches!(
        eval(&[("range", "bytes=0-99")], &meta, &private()),
        Freshness::Unusable(_)
    ));
    assert!(matches!(
        eval(&[("if-none-match", "\"v1\"")], &meta, &private()),
        Freshness::Unusable(_)
    ));
}

// ------------------------------------------------------------- end-to-end store

#[test]
fn end_to_end_store_hit_revalidate_and_dedupe() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let body = vec![b'x'; 4096];

    // Same body behind three different URLs: one blob, three entries.
    for i in 0..3 {
        let url = format!("https://example.test/copy{i}");
        let resp = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
        let outcome = cache
            .store("GET", &url, &HeaderMap::new(), 200, &resp, &body, NOW, NOW)
            .unwrap();
        match outcome {
            StoreOutcome::Stored { deduped, .. } => assert_eq!(deduped, i > 0),
            other => panic!("expected a store, got {other:?}"),
        }
    }

    let stats = cache.stats().unwrap();
    assert_eq!(stats.entries, 3);
    assert_eq!(stats.blobs, 1);
    assert!((stats.dedupe_ratio() - 3.0).abs() < 1e-9);

    // A stale entry with a validator asks for revalidation rather than missing.
    let url = "https://example.test/stale";
    let resp = headers(&[
        ("cache-control", "max-age=1"),
        ("etag", "\"v1\""),
        ("date", &date(NOW)),
    ]);
    cache
        .store("GET", url, &HeaderMap::new(), 200, &resp, b"old", NOW - 500, NOW - 500)
        .unwrap();

    let key = match cache.lookup("GET", url, &HeaderMap::new()).unwrap() {
        Lookup::Revalidate {
            response,
            conditional,
            ..
        } => {
            assert_eq!(conditional[0].1, "\"v1\"");
            response.key.clone()
        }
        other => panic!("expected revalidate, got {other:?}"),
    };

    // The origin says 304 with a longer lifetime: the entry becomes fresh again.
    let not_modified = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
    let refreshed = cache
        .record_not_modified(&key, &not_modified, NOW, NOW)
        .unwrap()
        .expect("entry still present");
    assert_eq!(refreshed.body, b"old");
    assert!(matches!(
        cache.lookup("GET", url, &HeaderMap::new()).unwrap(),
        Lookup::Fresh(_)
    ));
}

#[test]
fn a_peer_body_is_only_believed_against_an_independent_hash() {
    use syndeo_cache::{ContentId, Integrity, PeerProof};

    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let body = b"console.log('real');";
    let tampered = b"console.log('evil');";

    // Proof from a prior origin fetch.
    let proof = PeerProof::Content(ContentId::of(body));
    assert!(cache.accept_peer_body(&proof, body).is_ok());
    assert!(cache.accept_peer_body(&proof, tampered).is_err());

    // Proof from SRI in the markup.
    let integrity = Integrity::parse(
        &syndeo_cache::sri::Hash::compute(syndeo_cache::sri::Algorithm::Sha384, body).to_token(),
    )
    .unwrap();
    let proof = PeerProof::Integrity(integrity);
    assert!(cache.accept_peer_body(&proof, body).is_ok());
    assert!(cache.accept_peer_body(&proof, tampered).is_err());

    // No proof at all is not a proof.
    let empty = PeerProof::Integrity(Integrity::default());
    assert!(cache.accept_peer_body(&empty, body).is_err());

    let stats = cache.stats().unwrap();
    assert_eq!(stats.peer_accepted, 2);
    assert_eq!(stats.peer_rejected, 3);
}

#[test]
fn urls_are_normalized_before_keying() {
    assert_eq!(
        Cache::normalize_url("https://Example.test:443/a?b=1#frag"),
        Cache::normalize_url("https://example.test/a?b=1")
    );
    assert_ne!(
        Cache::normalize_url("https://example.test/a?b=1"),
        Cache::normalize_url("https://example.test/a?b=2")
    );
}

#[test]
fn dropping_the_last_reference_frees_the_blob() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let resp = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
    let body = vec![b'q'; 8192];

    cache
        .store("GET", "https://example.test/a", &HeaderMap::new(), 200, &resp, &body, NOW, NOW)
        .unwrap();
    cache
        .store("GET", "https://example.test/b", &HeaderMap::new(), 200, &resp, &body, NOW, NOW)
        .unwrap();

    let id = syndeo_cache::ContentId::of(&body);
    cache.purge("GET", "https://example.test/a").unwrap();
    assert!(cache.has_content(id), "still referenced by /b");

    cache.purge("GET", "https://example.test/b").unwrap();
    assert!(!cache.has_content(id), "last reference gone");
}
