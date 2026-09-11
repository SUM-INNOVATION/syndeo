//! RFC 9111 conformance suite.
//!
//! Each case names the section it comes from. The policy layer is pure, so these
//! run against a fixed clock with no store; the end-to-end cases at the bottom
//! drive the real index and blob store.

use http::HeaderMap;
use std::sync::Arc;
use syndeo_cache::policy::{self, CacheOptions, Freshness, Storability, StoredMeta};
use syndeo_cache::{Cache, Lookup, StoreOutcome};

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
    assert!(
        is_revalidate(&eval(&[], &meta, &private())),
        "private honours max-age"
    );
    assert!(
        is_fresh(&eval(&[], &meta, &shared())),
        "shared honours s-maxage"
    );
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
    let meta = stored(
        200,
        &[("cache-control", "max-age=10, must-revalidate")],
        100,
    );
    // Even when the client explicitly accepts staleness.
    assert!(is_revalidate(&eval(
        &[("cache-control", "max-stale")],
        &meta,
        &private()
    )));
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
    let meta = stored(
        200,
        &[
            ("cache-control", "max-age=10, stale-if-error=600"),
            ("etag", "\"v1\""),
        ],
        100,
    );
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
        .store(
            "GET",
            url,
            &headers(&[("accept-encoding", "gzip")]),
            200,
            &resp,
            b"gzip body",
            NOW,
            NOW,
        )
        .unwrap();
    cache
        .store(
            "GET",
            url,
            &headers(&[("accept-encoding", "br")]),
            200,
            &resp,
            b"br body",
            NOW,
            NOW,
        )
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
        policy::storability(
            "GET",
            &headers(&[("cache-control", "no-store")]),
            &ok,
            10,
            &private()
        ),
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
    let meta = stored(
        200,
        &[("cache-control", "no-store, must-understand, max-age=60")],
        0,
    );
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
    let meta = stored(
        200,
        &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
        10,
    );
    assert!(is_revalidate(&eval(
        &[("cache-control", "no-cache")],
        &meta,
        &private()
    )));
}

#[test]
fn s5_2_1_request_max_age_can_be_stricter_than_the_response() {
    let meta = stored(
        200,
        &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
        100,
    );
    assert!(is_fresh(&eval(&[], &meta, &private())));
    assert!(is_revalidate(&eval(
        &[("cache-control", "max-age=50")],
        &meta,
        &private()
    )));
}

#[test]
fn s5_2_1_min_fresh_requires_remaining_lifetime() {
    let meta = stored(
        200,
        &[("cache-control", "max-age=100"), ("etag", "\"v1\"")],
        80,
    );
    assert!(is_fresh(&eval(
        &[("cache-control", "min-fresh=10")],
        &meta,
        &private()
    )));
    assert!(is_revalidate(&eval(
        &[("cache-control", "min-fresh=50")],
        &meta,
        &private()
    )));
}

#[test]
fn s5_2_1_max_stale_accepts_bounded_staleness() {
    let meta = stored(200, &[("cache-control", "max-age=10")], 60);
    assert!(matches!(
        eval(&[("cache-control", "max-stale=100")], &meta, &private()),
        Freshness::ServeStale { .. }
    ));
    assert!(is_revalidate(&eval(
        &[("cache-control", "max-stale=5")],
        &meta,
        &private()
    )));
}

// ------------------------------------------------------------ §5.2.2 no-cache

#[test]
fn s5_2_2_response_no_cache_forces_revalidation_every_time() {
    let meta = stored(
        200,
        &[
            ("cache-control", "no-cache, max-age=600"),
            ("etag", "\"v1\""),
        ],
        1,
    );
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
    assert_eq!(
        stored_headers["content-length"], "1234",
        "must not be overwritten"
    );
    assert!(
        !stored_headers.contains_key("connection"),
        "hop-by-hop dropped"
    );
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
    assert!(matches!(
        cache.lookup("GET", url, &HeaderMap::new()).unwrap(),
        Lookup::Fresh(_)
    ));

    cache.invalidate("POST", url).unwrap();
    assert!(matches!(
        cache.lookup("GET", url, &HeaderMap::new()).unwrap(),
        Lookup::Miss(_)
    ));
}

// ------------------------------------------------- pass-through, not mishandled

#[test]
fn client_conditionals_are_passed_through() {
    let meta = stored(200, &[("cache-control", "max-age=600")], 10);
    assert!(matches!(
        eval(&[("if-none-match", "\"v1\"")], &meta, &private()),
        Freshness::Unusable(_)
    ));
    assert!(matches!(
        eval(
            &[("if-modified-since", &date(NOW - 100))],
            &meta,
            &private()
        ),
        Freshness::Unusable(_)
    ));
}

#[test]
fn a_range_is_not_the_policy_layers_business() {
    // Resolving a range needs the stored length, which this layer does not have.
    // It answers "is the representation usable"; `Cache::lookup` answers "which
    // bytes of it", and a range it cannot satisfy becomes a miss there.
    let meta = stored(200, &[("cache-control", "max-age=600")], 10);
    assert!(is_fresh(&eval(
        &[("range", "bytes=0-99")],
        &meta,
        &private()
    )));
    assert!(is_fresh(&eval(
        &[("range", "bytes=0-99"), ("if-range", "\"v1\"")],
        &meta,
        &private()
    )));
}

// --------------------------------------------------------------- §4 HEAD and GET

/// A body long enough that a range of it is obviously not the whole thing.
fn ranged_body() -> Vec<u8> {
    (0..1000u32).map(|i| (i % 251) as u8).collect()
}

fn store_whole(cache: &Cache, url: &str, body: &[u8], extra: &[(&str, &str)]) {
    let mut pairs = vec![("cache-control", "max-age=600"), ("etag", "\"v1\"")];
    pairs.extend_from_slice(extra);
    let owned = date(NOW);
    pairs.push(("date", owned.as_str()));
    let len = body.len().to_string();
    pairs.push(("content-length", len.as_str()));
    let resp = headers(&pairs);
    cache
        .store("GET", url, &HeaderMap::new(), 200, &resp, body, NOW, NOW)
        .unwrap();
}

#[test]
fn s4_a_head_is_answered_from_the_stored_get_with_no_body() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/doc";
    let body = ranged_body();
    store_whole(&cache, url, &body, &[("content-type", "application/pdf")]);

    match cache.lookup("HEAD", url, &HeaderMap::new()).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.status, 200);
            assert!(response.body.is_empty(), "a HEAD carries no body");
            assert!(response.body_omitted);
            assert_eq!(response.headers["content-length"], "1000");
            assert_eq!(response.headers["content-type"], "application/pdf");
        }
        other => panic!("expected the stored GET to answer the HEAD, got {other:?}"),
    }

    // The reverse is not true: a GET is never answered from a HEAD.
    let other = "https://example.test/head-only";
    let resp = headers(&[
        ("cache-control", "max-age=600"),
        ("etag", "\"h1\""),
        ("content-length", "1000"),
        ("date", &date(NOW)),
    ]);
    cache
        .store("HEAD", other, &HeaderMap::new(), 200, &resp, b"", NOW, NOW)
        .unwrap();
    assert!(matches!(
        cache.lookup("HEAD", other, &HeaderMap::new()).unwrap(),
        Lookup::Fresh(_)
    ));
    assert!(matches!(
        cache.lookup("GET", other, &HeaderMap::new()).unwrap(),
        Lookup::Miss(_)
    ));
}

#[test]
fn s4_3_5_a_head_refreshes_the_stored_get_or_invalidates_it() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/thing";
    let body = ranged_body();
    store_whole(&cache, url, &body, &[("x-origin", "old")]);

    // Same validator: the HEAD's headers update the stored GET.
    let head = headers(&[
        ("cache-control", "max-age=600"),
        ("etag", "\"v1\""),
        ("content-length", "1000"),
        ("x-origin", "new"),
        ("date", &date(NOW)),
    ]);
    cache
        .store("HEAD", url, &HeaderMap::new(), 200, &head, b"", NOW, NOW)
        .unwrap();
    match cache.lookup("GET", url, &HeaderMap::new()).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.headers["x-origin"], "new");
            assert_eq!(response.body, body, "the body is untouched");
        }
        other => panic!("expected the GET to survive the HEAD, got {other:?}"),
    }

    // A different validator says the representation changed: the GET goes.
    let moved = headers(&[
        ("cache-control", "max-age=600"),
        ("etag", "\"v2\""),
        ("content-length", "2000"),
        ("date", &date(NOW)),
    ]);
    cache
        .store("HEAD", url, &HeaderMap::new(), 200, &moved, b"", NOW, NOW)
        .unwrap();
    assert!(
        matches!(
            cache.lookup("GET", url, &HeaderMap::new()).unwrap(),
            Lookup::Miss(_)
        ),
        "a HEAD that contradicts the stored GET invalidates it"
    );
}

// ------------------------------------------------------------- §3.3, §14 ranges

#[test]
fn s14_a_stored_body_answers_a_range_without_touching_the_origin() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/video.mp4";
    let body = ranged_body();
    store_whole(&cache, url, &body, &[]);

    let request = headers(&[("range", "bytes=100-199")]);
    match cache.lookup("GET", url, &request).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.status, 206);
            assert_eq!(response.body, body[100..=199]);
            assert_eq!(response.headers["content-range"], "bytes 100-199/1000");
            assert_eq!(response.headers["content-length"], "100");
        }
        other => panic!("expected a range out of the store, got {other:?}"),
    }

    // A suffix range, and one that runs past the end, both resolve.
    let request = headers(&[("range", "bytes=-10")]);
    match cache.lookup("GET", url, &request).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.body, body[990..]);
            assert_eq!(response.headers["content-range"], "bytes 990-999/1000");
        }
        other => panic!("expected a suffix range, got {other:?}"),
    }

    // Multipart is passed through rather than approximated.
    let request = headers(&[("range", "bytes=0-9, 20-29")]);
    assert!(matches!(
        cache.lookup("GET", url, &request).unwrap(),
        Lookup::Miss(_)
    ));

    // An unsatisfiable range is ignored, and the whole body is served.
    let request = headers(&[("range", "bytes=5000-6000")]);
    match cache.lookup("GET", url, &request).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.status, 200);
            assert_eq!(response.body, body);
        }
        other => panic!("expected the whole body, got {other:?}"),
    }
}

#[test]
fn s13_1_5_if_range_is_checked_against_the_stored_validator() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/big.iso";
    let body = ranged_body();
    store_whole(&cache, url, &body, &[]);

    let matching = headers(&[("range", "bytes=0-9"), ("if-range", "\"v1\"")]);
    match cache.lookup("GET", url, &matching).unwrap() {
        Lookup::Fresh(response) => assert_eq!(response.status, 206),
        other => panic!("expected the range to be served, got {other:?}"),
    }

    // The client holds a piece of a different representation: our copy is no
    // use to it, so it goes to the origin for the whole thing.
    let stale = headers(&[("range", "bytes=0-9"), ("if-range", "\"v0\"")]);
    assert!(matches!(
        cache.lookup("GET", url, &stale).unwrap(),
        Lookup::Miss(_)
    ));

    // A weak tag may not be used for If-Range at all.
    let weak = headers(&[("range", "bytes=0-9"), ("if-range", "W/\"v1\"")]);
    assert!(matches!(
        cache.lookup("GET", url, &weak).unwrap(),
        Lookup::Miss(_)
    ));
}

#[test]
fn s3_3_partial_responses_are_stored_and_combined_rather_than_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/download.bin";
    let body = ranged_body();

    let partial = |first: usize, last: usize| {
        headers(&[
            ("cache-control", "max-age=600"),
            ("etag", "\"v1\""),
            ("content-range", &format!("bytes {first}-{last}/1000")),
            ("date", &date(NOW)),
        ])
    };

    // First half.
    let outcome = cache
        .store(
            "GET",
            url,
            &HeaderMap::new(),
            206,
            &partial(0, 499),
            &body[0..500],
            NOW,
            NOW,
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            StoreOutcome::StoredPartial {
                held: 500,
                complete_len: Some(1000)
            }
        ),
        "got {outcome:?}"
    );

    // A range inside what we hold is servable already.
    let request = headers(&[("range", "bytes=10-19")]);
    match cache.lookup("GET", url, &request).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.status, 206);
            assert_eq!(response.body, body[10..=19]);
        }
        other => panic!("expected the held range, got {other:?}"),
    }

    // One that is not is a miss, not a wrong answer.
    let beyond = headers(&[("range", "bytes=600-699")]);
    assert!(matches!(
        cache.lookup("GET", url, &beyond).unwrap(),
        Lookup::Miss(_)
    ));
    // And neither is the whole body, which we do not have.
    assert!(matches!(
        cache.lookup("GET", url, &HeaderMap::new()).unwrap(),
        Lookup::Miss(_)
    ));

    // An overlapping range adds only what is new, and does not replace.
    let outcome = cache
        .store(
            "GET",
            url,
            &HeaderMap::new(),
            206,
            &partial(400, 799),
            &body[400..800],
            NOW,
            NOW,
        )
        .unwrap();
    assert!(
        matches!(outcome, StoreOutcome::StoredPartial { held: 800, .. }),
        "got {outcome:?}"
    );
    let spanning = headers(&[("range", "bytes=450-550")]);
    match cache.lookup("GET", url, &spanning).unwrap() {
        Lookup::Fresh(response) => assert_eq!(response.body, body[450..=550]),
        other => panic!("expected a range spanning two stored runs, got {other:?}"),
    }

    // The last gap closes and the entry becomes an ordinary complete one.
    let outcome = cache
        .store(
            "GET",
            url,
            &HeaderMap::new(),
            206,
            &partial(800, 999),
            &body[800..1000],
            NOW,
            NOW,
        )
        .unwrap();
    assert!(
        matches!(outcome, StoreOutcome::Stored { .. }),
        "got {outcome:?}"
    );
    match cache.lookup("GET", url, &HeaderMap::new()).unwrap() {
        Lookup::Fresh(response) => {
            assert_eq!(response.status, 200);
            assert_eq!(
                response.body, body,
                "the runs reassemble into the original bytes"
            );
        }
        other => panic!("expected a complete body, got {other:?}"),
    }
}

#[test]
fn s3_3_ranges_from_a_different_representation_are_not_stitched_together() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/changing.bin";
    let first = vec![b'a'; 1000];
    let second = vec![b'b'; 1000];

    let partial = |etag: &str, f: usize, l: usize| {
        headers(&[
            ("cache-control", "max-age=600"),
            ("etag", etag),
            ("content-range", &format!("bytes {f}-{l}/1000")),
            ("date", &date(NOW)),
        ])
    };

    cache
        .store(
            "GET",
            url,
            &HeaderMap::new(),
            206,
            &partial("\"v1\"", 0, 499),
            &first[0..500],
            NOW,
            NOW,
        )
        .unwrap();
    // The file changed underneath us. Combining these would produce a body that
    // never existed, under a hash claiming it did.
    let outcome = cache
        .store(
            "GET",
            url,
            &HeaderMap::new(),
            206,
            &partial("\"v2\"", 500, 999),
            &second[500..1000],
            NOW,
            NOW,
        )
        .unwrap();
    assert!(
        matches!(outcome, StoreOutcome::StoredPartial { held: 500, .. }),
        "the older representation is dropped rather than merged: {outcome:?}"
    );

    let early = headers(&[("range", "bytes=0-9")]);
    assert!(
        matches!(cache.lookup("GET", url, &early).unwrap(), Lookup::Miss(_)),
        "the bytes from the old representation are gone"
    );
    let late = headers(&[("range", "bytes=500-509")]);
    match cache.lookup("GET", url, &late).unwrap() {
        Lookup::Fresh(response) => assert_eq!(response.body, &second[500..=509]),
        other => panic!("expected the new representation's range, got {other:?}"),
    }
}

#[test]
fn s3_3_multipart_ranges_are_passed_through_not_stored_as_one_body() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let multipart = headers(&[
        ("cache-control", "max-age=600"),
        ("etag", "\"v1\""),
        ("content-type", "multipart/byteranges; boundary=SEP"),
        ("content-range", "bytes 0-9/1000"),
        ("date", &date(NOW)),
    ]);
    let outcome = cache
        .store(
            "GET",
            "https://example.test/m",
            &HeaderMap::new(),
            206,
            &multipart,
            b"--SEP...",
            NOW,
            NOW,
        )
        .unwrap();
    assert_eq!(
        outcome,
        StoreOutcome::NotStored("multipart ranges are passed through")
    );

    // And a 206 that will not say which bytes it is, is not stored either.
    let vague = headers(&[
        ("cache-control", "max-age=600"),
        ("etag", "\"v1\""),
        ("date", &date(NOW)),
    ]);
    let outcome = cache
        .store(
            "GET",
            "https://example.test/v",
            &HeaderMap::new(),
            206,
            &vague,
            b"0123456789",
            NOW,
            NOW,
        )
        .unwrap();
    assert_eq!(
        outcome,
        StoreOutcome::NotStored("206 without a usable Content-Range")
    );
}

// -------------------------------------------------------- eviction and garbage

#[test]
fn filling_past_the_budget_evicts_by_the_configured_policy() {
    use syndeo_cache::{CacheOptions, Eviction};

    let dir = tempfile::tempdir().unwrap();
    let options = CacheOptions {
        // Small enough that a handful of bodies passes it.
        capacity_bytes: Some(40_000),
        evict_to_fraction: 0.5,
        eviction: Eviction::LeastRecentlyUsed,
        ..CacheOptions::default()
    };
    let cache = Cache::with_options(dir.path(), options)
        .unwrap()
        .with_clock(Arc::new(|| NOW));

    // Incompressible, so the on-disk size is the body size.
    let body = |seed: u8| -> Vec<u8> {
        let mut state = seed as u64 + 1;
        (0..10_000)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    };

    for i in 0..8u8 {
        let url = format!("https://example.test/{i}");
        let resp = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
        cache
            .store(
                "GET",
                &url,
                &HeaderMap::new(),
                200,
                &resp,
                &body(i),
                NOW,
                NOW,
            )
            .unwrap();
    }

    let stats = cache.stats().unwrap();
    assert!(stats.evictions > 0, "nothing was evicted");
    assert!(
        stats.on_disk_bytes <= 40_000,
        "still over budget at {} bytes",
        stats.on_disk_bytes
    );
    assert!(
        stats.entries < 8,
        "every entry survived a budget it exceeded"
    );
}

#[test]
fn a_shared_body_survives_until_its_last_referring_entry_is_evicted() {
    use syndeo_cache::CacheOptions;

    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::with_options(
        dir.path(),
        CacheOptions {
            capacity_bytes: None,
            ..CacheOptions::default()
        },
    )
    .unwrap()
    .with_clock(Arc::new(|| NOW));

    let body = vec![b'z'; 4096];
    let id = syndeo_cache::ContentId::of(&body);
    for name in ["a", "b", "c"] {
        let resp = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
        cache
            .store(
                "GET",
                &format!("https://example.test/{name}"),
                &HeaderMap::new(),
                200,
                &resp,
                &body,
                NOW,
                NOW,
            )
            .unwrap();
    }

    cache.purge("GET", "https://example.test/a").unwrap();
    assert!(cache.has_content(id));
    cache.purge("GET", "https://example.test/b").unwrap();
    assert!(cache.has_content(id), "one entry still refers to it");
    cache.purge("GET", "https://example.test/c").unwrap();
    assert!(!cache.has_content(id), "the last reference has gone");
}

#[test]
fn the_integrity_index_does_not_grow_across_store_purge_and_collect() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_at(dir.path(), NOW);
    let url = "https://example.test/asset.js";

    let cycle = |n: usize| {
        let resp = headers(&[("cache-control", "max-age=600"), ("date", &date(NOW))]);
        let body = format!("console.log({n});");
        cache
            .store(
                "GET",
                url,
                &HeaderMap::new(),
                200,
                &resp,
                body.as_bytes(),
                NOW,
                NOW,
            )
            .unwrap();
        cache.purge("GET", url).unwrap();
        cache.collect_garbage().unwrap();
    };

    cycle(0);
    let after_one = cache.stats().unwrap().sri_rows;
    for n in 1..6 {
        cycle(n);
    }
    assert_eq!(
        cache.stats().unwrap().sri_rows,
        after_one,
        "integrity rows accumulated for bodies that are no longer stored"
    );
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
        .store(
            "GET",
            url,
            &HeaderMap::new(),
            200,
            &resp,
            b"old",
            NOW - 500,
            NOW - 500,
        )
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
        .store(
            "GET",
            "https://example.test/a",
            &HeaderMap::new(),
            200,
            &resp,
            &body,
            NOW,
            NOW,
        )
        .unwrap();
    cache
        .store(
            "GET",
            "https://example.test/b",
            &HeaderMap::new(),
            200,
            &resp,
            &body,
            NOW,
            NOW,
        )
        .unwrap();

    let id = syndeo_cache::ContentId::of(&body);
    cache.purge("GET", "https://example.test/a").unwrap();
    assert!(cache.has_content(id), "still referenced by /b");

    cache.purge("GET", "https://example.test/b").unwrap();
    assert!(!cache.has_content(id), "last reference gone");
}
