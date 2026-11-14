// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`Reader`] against `agh-fake`'s loopback GitHub (web, API and CDN origins) via the
//! platform's curl, with faults injected per route.

use super::*;
use agh_fake::{AssetSpec, FakeGithub, FakeOrigin, Fault, Match, ReleaseSpec, RepoSpec, token};

const OWNER: &str = "fixture-dev";
const REPO: &str = "app";
const SLUG: &str = "fixture-dev/app";
const LOGIN: &str = "fixture-dev-bot";
const BYTES: &[u8] = b"schema = 1\nroster_seq = 7\n";

/// A read's patience where the test expects it to deliver: a hang detector, never a budget.
const DELIVERS: Duration = Duration::from_secs(60);
/// A read's patience where every route fails by construction.
const FAILS: Duration = Duration::from_millis(300);

#[cfg(unix)]
#[test]
fn linked_delivery_is_refused_before_reading() {
    let dir = crate::test_scratch_dir("asset-file-kind");
    let target = dir.path().join("target");
    let body = dir.path().join("body");
    std::fs::write(&target, b"payload").unwrap();
    std::os::unix::fs::symlink(&target, &body).unwrap();
    assert!(read_capped(&body, 32).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"payload");
    std::fs::remove_file(&body).unwrap();
    let path = std::ffi::CString::new(body.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: path is NUL-terminated and remains live for the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(read_capped(&body, 32).is_err());
    std::fs::remove_file(&body).unwrap();
    std::fs::create_dir(&body).unwrap();
    assert!(read_capped(&body, 32).is_err());
    std::fs::remove_dir(&body).unwrap();
    std::fs::write(&body, b"payload").unwrap();
    assert_eq!(read_capped(&body, 7).unwrap(), b"payload");
    assert!(read_capped(&body, 6).is_err());
}

/// agh-fake holding the public `SLUG` with release `v1` (its latest) carrying `a.toml`.
fn fixture() -> FakeGithub {
    let fake = FakeGithub::start();
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    let id = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    fake.add_asset(id, AssetSpec::uploaded("a.toml", BYTES));
    fake
}

fn origins(fake: &FakeGithub) -> Origins {
    let p = fake.ports();
    Origins::loopback(p.api, p.uploads, p.web, p.cdn)
}

fn status(code: u16) -> Fault {
    Fault::Status {
        status: code,
        body: String::new(),
        headers: Vec::new(),
    }
}

/// The requests `fake` saw on `origin`.
fn on(fake: &FakeGithub, origin: FakeOrigin) -> Vec<agh_fake::Seen> {
    fake.seen()
        .into_iter()
        .filter(|s| s.origin == origin)
        .collect()
}

#[test]
fn a_web_route_answering_504_reads_the_asset_through_the_api_route() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let got = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16);
    assert_eq!(got.as_deref(), Ok(BYTES));
    let api: Vec<String> = on(&fake, FakeOrigin::Api)
        .into_iter()
        .map(|s| s.path_and_query)
        .collect();
    assert!(
        api.iter()
            .any(|p| p == "/repos/fixture-dev/app/releases/tags/v1"),
        "the release was read by its tag: {api:?}"
    );
    assert!(
        api.iter()
            .any(|p| p.starts_with("/repos/fixture-dev/app/releases/assets/")),
        "then the asset by its id: {api:?}"
    );
    assert_eq!(
        on(&fake, FakeOrigin::Cdn).len(),
        1,
        "one CDN read delivered it"
    );
}

#[test]
fn a_release_url_reads_as_its_asset_on_either_route_and_no_other_url_does() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let url = "https://github.com/fixture-dev/app/releases/download/v1/a.toml";
    assert_eq!(reader.bytes_at(url, 1 << 16).as_deref(), Ok(BYTES));
    assert_eq!(
        reader.api_requests(),
        2,
        "its listing, then the asset by id"
    );
    // Negative control: a URL off github.com's release route goes to its own host (here plain
    // http, refused before any request), never to the reader's routes.
    let asked = fake.seen().len();
    let other = reader.bytes_at(&url.replacen("https", "http", 1), 1 << 16);
    assert!(matches!(other, Err(AssetError::Refused(_))), "{other:?}");
    assert_eq!(fake.seen().len(), asked);
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let dest = dir.path().join("a.toml");
    reader.to_file_at(url, &dest, 1 << 16).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), BYTES);
    let asked = fake.seen().len();
    let other = reader.to_file_at(&url.replacen("https", "http", 1), &dest, 1 << 16);
    assert!(matches!(other, Err(AssetError::Refused(_))), "{other:?}");
    assert_eq!(fake.seen().len(), asked);
}

#[test]
fn the_latest_release_reads_through_the_api_route_when_its_web_route_fails() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/latest/"), status(502));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let got = reader.bytes(&Asset::latest(OWNER, REPO, "a.toml"), 1 << 16);
    assert_eq!(got.as_deref(), Ok(BYTES));
    assert!(
        on(&fake, FakeOrigin::Api)
            .iter()
            .any(|s| s.path_and_query == "/repos/fixture-dev/app/releases/latest"),
        "the API's latest release named the asset"
    );
}

#[test]
fn a_healthy_web_route_asks_the_api_nothing() {
    let fake = fixture();
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    for asset in [
        Asset::tagged(OWNER, REPO, "v1", "a.toml"),
        Asset::latest(OWNER, REPO, "a.toml"),
    ] {
        assert_eq!(
            reader.bytes(&asset, 1 << 16).as_deref(),
            Ok(BYTES),
            "{asset}"
        );
    }
    assert_eq!(
        on(&fake, FakeOrigin::Api).len(),
        0,
        "the metered route was never asked"
    );
}

#[test]
fn both_routes_failing_ends_naming_both_and_never_reads_as_absence() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    fake.fault_always(Match::api("/repos/fixture-dev/app/releases/"), status(503));
    let reader = Reader::loopback(origins(&fake), None, FAILS);
    let got = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16);
    let Err(AssetError::Unavailable { web, api }) = &got else {
        panic!("both routes failing is unavailability, never absence: {got:?}");
    };
    assert!(
        web.contains("/fixture-dev/app/releases/download/v1/a.toml") && web.contains("504"),
        "the web route's failure names its route and status: {web}"
    );
    assert!(
        api.contains("503"),
        "the API route's failure names its status: {api}"
    );
    let said = got.unwrap_err().to_string();
    assert!(
        said.contains("web route") && said.contains("API route"),
        "the message names both routes: {said}"
    );
}

#[test]
fn a_web_404_is_absent_and_asks_nothing_more() {
    let fake = fixture();
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    for asset in [
        Asset::tagged(OWNER, REPO, "v1", "missing.toml"),
        Asset::tagged(OWNER, REPO, "v9", "a.toml"),
        Asset::latest(OWNER, REPO, "missing.toml"),
    ] {
        fake.clear_seen();
        let got = reader.bytes(&asset, 1 << 16);
        assert!(
            matches!(&got, Err(e) if e.is_absent()),
            "{asset}: a 404 is absence: {got:?}"
        );
        assert_eq!(
            on(&fake, FakeOrigin::Api).len(),
            0,
            "{asset}: a 404 sends nothing to the metered route"
        );
        let web = on(&fake, FakeOrigin::Web);
        let first_hops = web
            .iter()
            .filter(|s| {
                !s.path_and_query.contains("/releases/download/")
                    || asset.release != Release::Latest
            })
            .count();
        assert_eq!(first_hops, 1, "{asset}: one web read, no storm: {web:?}");
    }
}

#[test]
fn the_api_route_answering_absent_ends_the_read_as_absent() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let got = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "missing.toml"), 1 << 16);
    assert!(
        matches!(&got, Err(AssetError::Absent(why)) if why.contains("lists no asset")),
        "the API's fresh read of the release decides absence: {got:?}"
    );
}

#[test]
fn a_file_reads_through_the_api_route_and_lands_whole() {
    let fake = fixture();
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let id = fake.add_release(SLUG, ReleaseSpec::published("v2", "2026-10-02T00:00:00Z"));
    fake.add_asset(id, AssetSpec::uploaded("app.tar.zst", &big));
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let dir = crate::test_scratch_dir("asset-file");
    let dest = dir.path().join("app.tar.zst");
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let got = reader.to_file(
        &Asset::tagged(OWNER, REPO, "v2", "app.tar.zst"),
        &dest,
        1 << 20,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(std::fs::read(&dest).unwrap(), big);
    assert!(
        !dir.path().join("app.tar.zst.part").exists(),
        "the partial was renamed"
    );
}

#[test]
fn a_spent_api_allowance_rests_the_api_leg_for_the_rest_of_the_read() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reset = aterm_time::unix_secs().unwrap() + 3600;
    fake.fault_always(
        Match::api("/repos/fixture-dev/app/releases/"),
        Fault::PrimaryExhausted { reset },
    );
    let reader = Reader::loopback(origins(&fake), None, FAILS);
    let got = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16);
    let Err(AssetError::Unavailable { api, .. }) = &got else {
        panic!("unavailable: {got:?}");
    };
    assert!(api.contains("403"), "the API's refusal is named: {api}");
    assert_eq!(
        on(&fake, FakeOrigin::Api).len(),
        1,
        "a spent allowance is asked once, not once a round"
    );
}

#[test]
fn only_the_api_leg_of_a_token_reader_carries_the_token() {
    let fake = fixture();
    let secret = token("asset");
    fake.add_token(&secret, LOGIN);
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(
        origins(&fake),
        Some(Token::new(secret.as_bytes()).unwrap()),
        DELIVERS,
    );
    let got = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16);
    assert_eq!(got.as_deref(), Ok(BYTES));
    let seen = fake.seen();
    assert!(
        seen.iter()
            .filter(|s| s.origin == FakeOrigin::Api)
            .all(|s| s.as_login.as_deref() == Some(LOGIN)),
        "the API leg speaks as the token: {seen:?}"
    );
    assert!(
        seen.iter()
            .filter(|s| s.origin != FakeOrigin::Api)
            .all(|s| !s.had_authorization),
        "the web route and the CDN never see it: {seen:?}"
    );
}

#[test]
fn an_anonymous_reader_sends_no_credential_anywhere() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    assert_eq!(
        reader
            .bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16)
            .as_deref(),
        Ok(BYTES)
    );
    assert!(fake.seen().iter().all(|s| !s.had_authorization));
}

#[test]
fn web_urls_name_their_asset_and_nothing_else() {
    assert_eq!(
        Asset::from_web_url(
            "https://github.com/alabsystems/aterm/releases/download/v0.174.0/aterm-machines.toml"
        ),
        Some(Asset::tagged(
            "alabsystems",
            "aterm",
            "v0.174.0",
            "aterm-machines.toml"
        ))
    );
    assert_eq!(
        Asset::from_web_url(
            "https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml"
        ),
        Some(Asset::latest("alabsystems", "aterm", "aterm-appcast.toml"))
    );
    for url in [
        "http://github.com/o/r/releases/download/t/n",
        "https://github.com.evil/o/r/releases/download/t/n",
        "https://github.com/o/r/releases/download/t/n/extra",
        "https://github.com/o/r/releases/download/t",
        "https://github.com/o/r/releases/download/../n",
        "https://github.com/o/r/releases/latest/n",
        "https://github.com/o/r/archive/t/n",
        "https://github.com/o/r/releases/download/t/n?x=1",
    ] {
        assert_eq!(Asset::from_web_url(url), None, "{url}");
    }
    let asset = Asset::tagged("o", "r", "t", "n");
    assert_eq!(
        asset.web_url(),
        "https://github.com/o/r/releases/download/t/n"
    );
    assert_eq!(Asset::from_web_url(&asset.web_url()), Some(asset));
    assert_eq!(
        Asset::in_slug("o/r", Release::Tag("t"), "n"),
        Some(asset),
        "a slug names the same asset"
    );
    for (slug, tag, name) in [
        ("o", "t", "n"),
        ("o/r/x", "t", "n"),
        ("o/", "t", "n"),
        ("/r", "t", "n"),
        ("o/..", "t", "n"),
        ("https://evil.com/a", "t", "n"),
        ("o/r", "v1?x", "n"),
        ("o/r", "", "n"),
        ("o/r", "t", "../n"),
    ] {
        assert_eq!(
            Asset::in_slug(slug, Release::Tag(tag), name),
            None,
            "{slug} {tag} {name}"
        );
    }
}

#[test]
fn an_unsafe_name_is_refused_before_any_request() {
    let fake = fixture();
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let got = reader.bytes(&Asset::tagged(OWNER, REPO, "..", "a.toml"), 1 << 16);
    assert!(matches!(got, Err(AssetError::Refused(_))), "{got:?}");
    assert!(fake.seen().is_empty());
}

#[test]
fn the_latest_tag_comes_from_the_web_redirect_and_asks_the_api_nothing() {
    let fake = fixture();
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    assert_eq!(
        reader.latest_tag(OWNER, REPO, "a.toml").as_deref(),
        Ok("v1")
    );
    assert_eq!(reader.api_requests(), 0);
    assert_eq!(on(&fake, FakeOrigin::Api).len(), 0);
}

#[test]
fn a_failing_latest_redirect_reads_the_tag_from_the_api_and_its_assets_reuse_that_listing() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    assert_eq!(
        reader.latest_tag(OWNER, REPO, "a.toml").as_deref(),
        Ok("v1")
    );
    assert_eq!(
        reader
            .bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16)
            .as_deref(),
        Ok(BYTES)
    );
    let listings = on(&fake, FakeOrigin::Api)
        .into_iter()
        .filter(|s| !s.path_and_query.contains("/releases/assets/"))
        .count();
    assert_eq!(
        listings, 1,
        "the latest release's listing served its tag's read too"
    );
    assert_eq!(reader.api_requests(), 2, "one listing and one asset");
}

#[test]
fn a_latest_release_without_the_name_names_its_tag_and_the_tag_read_says_absent() {
    let fake = fixture();
    for failing_web in [false, true] {
        if failing_web {
            fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
        }
        let reader = Reader::loopback(origins(&fake), None, DELIVERS);
        assert_eq!(
            reader.latest_tag(OWNER, REPO, "missing.toml").as_deref(),
            Ok("v1"),
            "web failing: {failing_web}"
        );
        let got = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "missing.toml"), 1 << 16);
        assert!(matches!(&got, Err(e) if e.is_absent()), "{got:?}");
    }
    let empty = FakeGithub::start();
    empty.add_repo(SLUG, RepoSpec::public());
    let reader = Reader::loopback(origins(&empty), None, DELIVERS);
    let got = reader.latest_tag(OWNER, REPO, "a.toml");
    assert!(
        matches!(&got, Err(e) if e.is_absent()),
        "no release at all: {got:?}"
    );
}

#[test]
fn a_stopped_read_ends_its_transfer_and_leaves_no_partial() {
    const HANG_DETECTOR: Duration = Duration::from_secs(60);
    let fake = fixture();
    fake.fault_always(
        Match::web("/fixture-dev/app/releases/"),
        Fault::Hang(Duration::from_secs(600)),
    );
    let dir = crate::test_scratch_dir("asset-stop");
    let dest = dir.path().join("a.toml");
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let stop = AtomicBool::new(false);
    let got = std::thread::scope(|scope| {
        scope.spawn(|| {
            let began = Instant::now();
            while on(&fake, FakeOrigin::Web).is_empty() {
                assert!(
                    began.elapsed() < HANG_DETECTOR,
                    "the read never asked the web route"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            stop.store(true, Ordering::Release);
        });
        reader.to_file_until(
            &Asset::tagged(OWNER, REPO, "v1", "a.toml"),
            &dest,
            1 << 16,
            &stop,
        )
    });
    fake.end_hangs();
    assert_eq!(got, Err(AssetError::Stopped));
    assert!(!dir.path().join("a.toml.part").exists() && !dest.exists());
    assert_eq!(
        on(&fake, FakeOrigin::Api).len(),
        0,
        "a stopped read asks nothing more"
    );
}

#[test]
fn a_reader_waits_out_an_outage_once_and_its_later_reads_try_each_route_once() {
    let fake = fixture();
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    fake.fault_always(Match::api("/repos/fixture-dev/app/releases/"), status(503));
    let reader = Reader::loopback(origins(&fake), None, FAILS);
    let asset = Asset::tagged(OWNER, REPO, "v1", "a.toml");
    let began = Instant::now();
    assert!(matches!(
        reader.bytes(&asset, 1 << 16),
        Err(AssetError::Unavailable { .. })
    ));
    assert!(
        began.elapsed() >= FAILS,
        "the first read waited out the patience"
    );
    fake.clear_seen();
    assert!(matches!(
        reader.bytes(&asset, 1 << 16),
        Err(AssetError::Unavailable { .. })
    ));
    assert_eq!(
        (
            on(&fake, FakeOrigin::Web).len(),
            on(&fake, FakeOrigin::Api).len()
        ),
        (1, 1),
        "a later read of a reader whose patience is spent tries each route once"
    );
}

#[test]
fn reads_of_one_release_at_once_ask_the_api_for_its_listing_once() {
    const NAMES: [&str; 6] = ["b0", "b1", "b2", "b3", "b4", "b5"];
    let fake = FakeGithub::start();
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    let id = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    for name in NAMES {
        fake.add_asset(id, AssetSpec::uploaded(name, name.as_bytes()));
    }
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    std::thread::scope(|scope| {
        let reads: Vec<_> = NAMES
            .iter()
            .map(|name| {
                let reader = &reader;
                scope.spawn(move || reader.bytes(&Asset::tagged(OWNER, REPO, "v1", name), 64))
            })
            .collect();
        for (read, name) in reads.into_iter().zip(NAMES) {
            assert_eq!(
                read.join().expect("read thread").as_deref(),
                Ok(name.as_bytes())
            );
        }
    });
    let listings = on(&fake, FakeOrigin::Api)
        .into_iter()
        .filter(|s| s.path_and_query == "/repos/fixture-dev/app/releases/tags/v1")
        .count();
    assert_eq!(listings, 1, "one listing served every read of the release");
}

#[test]
fn an_api_allowance_of_one_release_lists_one_release_and_its_other_assets_stay_free() {
    let fake = FakeGithub::start();
    fake.add_repo(SLUG, RepoSpec::public().push(LOGIN));
    let v1 = fake.add_release(SLUG, ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"));
    fake.add_asset(v1, AssetSpec::uploaded("a.toml", BYTES));
    fake.add_asset(v1, AssetSpec::uploaded("c.toml", BYTES));
    let v2 = fake.add_release(SLUG, ReleaseSpec::published("v2", "2026-11-01T00:00:00Z"));
    fake.add_asset(v2, AssetSpec::uploaded("a.toml", BYTES));
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    let listings = |fake: &FakeGithub| {
        on(fake, FakeOrigin::Api)
            .into_iter()
            .filter(|s| s.path_and_query.contains("/releases/tags/"))
            .count()
    };

    let reader = Reader::loopback(origins(&fake), None, DELIVERS).api_releases(1);
    let first = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16);
    assert_eq!(first.as_deref(), Ok(BYTES), "the first release is served");
    let free = reader.bytes(&Asset::tagged(OWNER, REPO, "v1", "c.toml"), 1 << 16);
    assert_eq!(
        free.as_deref(),
        Ok(BYTES),
        "a listed release's other asset is free"
    );
    assert_eq!(listings(&fake), 1, "one release listed");

    let quick = Reader::loopback(origins(&fake), None, FAILS).api_releases(1);
    assert_eq!(
        quick
            .bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16)
            .as_deref(),
        Ok(BYTES)
    );
    fake.clear_seen();
    let spent = quick.bytes(&Asset::tagged(OWNER, REPO, "v2", "a.toml"), 1 << 16);
    let why = spent
        .expect_err("a second release is past the allowance")
        .to_string();
    assert!(why.contains("allowance of 1 release(s) is spent"), "{why}");
    assert!(
        why.contains("504"),
        "the web route's failure is named too: {why}"
    );
    assert_eq!(listings(&fake), 0, "the spent allowance lists nothing");
    assert!(
        !on(&fake, FakeOrigin::Web).is_empty(),
        "the web route was still tried"
    );

    // Negative control: unbounded, the same second release is listed and read.
    let unbounded = Reader::loopback(origins(&fake), None, DELIVERS);
    assert_eq!(
        unbounded
            .bytes(&Asset::tagged(OWNER, REPO, "v2", "a.toml"), 1 << 16)
            .as_deref(),
        Ok(BYTES)
    );
    assert_eq!(listings(&fake), 1);
}

/// The releases listing is one API request naming each release's tag, flags and assets, newest
/// first and no more than asked; a draft is listed only to a token that may push; and a listed
/// release's asset read through the API route asks for no listing of its own.
#[test]
fn the_releases_listing_is_one_request_and_its_releases_need_no_listing_of_their_own() {
    let fake = fixture();
    let v2 = fake.add_release(
        SLUG,
        ReleaseSpec::published("v2", "2026-11-01T00:00:00Z").prerelease(true),
    );
    fake.add_asset(v2, AssetSpec::uploaded("b.toml", BYTES));
    fake.add_release(SLUG, ReleaseSpec::draft("v3", "2026-11-02T00:00:00Z"));
    let listed = |fake: &FakeGithub| {
        on(fake, FakeOrigin::Api)
            .into_iter()
            .filter(|s| {
                s.path_and_query
                    .starts_with("/repos/fixture-dev/app/releases?")
            })
            .count()
    };

    let reader = Reader::loopback(origins(&fake), None, DELIVERS);
    let releases = reader.releases(OWNER, REPO, 30, 1).unwrap();
    assert_eq!(
        releases,
        [
            ListedRelease {
                tag: "v2".into(),
                draft: false,
                prerelease: true,
                assets: vec!["b.toml".into()],
            },
            ListedRelease {
                tag: "v1".into(),
                draft: false,
                prerelease: false,
                assets: vec!["a.toml".into()],
            },
        ],
        "newest first; the draft is not listed to an anonymous reader"
    );
    assert_eq!(reader.api_requests(), 1);
    assert_eq!(listed(&fake), 1);
    assert!(
        on(&fake, FakeOrigin::Api)
            .iter()
            .any(|s| s.path_and_query == "/repos/fixture-dev/app/releases?per_page=30&page=1"),
        "one page of the asked size"
    );

    // A listed release's asset, its web route failing, costs the asset's request alone.
    fake.fault_always(Match::web("/fixture-dev/app/releases/"), status(504));
    fake.clear_seen();
    assert_eq!(
        reader
            .bytes(&Asset::tagged(OWNER, REPO, "v1", "a.toml"), 1 << 16)
            .as_deref(),
        Ok(BYTES)
    );
    assert!(
        !on(&fake, FakeOrigin::Api)
            .iter()
            .any(|s| s.path_and_query.contains("/releases/tags/")),
        "the listing served the asset's id"
    );
    assert_eq!(reader.api_requests(), 2, "the listing and the asset");

    // A page of one is the newest release alone; its second page the one before it, and its
    // third the end of the listing. Each page is its own request; page 0 asks for nothing.
    let one = Reader::loopback(origins(&fake), None, DELIVERS);
    let page = |n| {
        one.releases(OWNER, REPO, 1, n)
            .unwrap()
            .into_iter()
            .map(|r| r.tag)
            .collect::<Vec<_>>()
    };
    assert_eq!(page(1), ["v2"]);
    assert_eq!(page(2), ["v1"]);
    assert_eq!(page(3), Vec::<String>::new());
    assert_eq!(one.api_requests(), 3);
    assert!(matches!(
        one.releases(OWNER, REPO, 1, 0),
        Err(AssetError::Refused(_))
    ));
    assert_eq!(one.api_requests(), 3);

    // A token that may push is listed the draft too.
    let secret = token("listing");
    fake.add_token(&secret, LOGIN);
    let pusher = Reader::loopback(
        origins(&fake),
        Some(Token::new(secret.as_bytes()).unwrap()),
        DELIVERS,
    );
    let all = pusher.releases(OWNER, REPO, 30, 1).unwrap();
    assert_eq!(
        all.iter()
            .map(|r| (r.tag.as_str(), r.draft))
            .collect::<Vec<_>>(),
        [("v3", true), ("v2", false), ("v1", false)]
    );

    // An unknown repository is absent; an unsafe name is refused before any request.
    assert!(
        reader
            .releases(OWNER, "missing", 30, 1)
            .unwrap_err()
            .is_absent()
    );
    fake.clear_seen();
    assert!(matches!(
        reader.releases(OWNER, "a/b", 30, 1),
        Err(AssetError::Refused(_))
    ));
    assert!(fake.seen().is_empty());
}

/// The listing has the API route alone: when that try reaches no host (here nothing listens on
/// the API's port), the listing is unavailable at once, after that one try, however much
/// patience the reader has left, where waiting for a web leg it does not have kept it trying.
#[test]
fn a_listing_whose_api_host_is_unreachable_ends_after_one_try() {
    let fake = fixture();
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let p = fake.ports();
    let reader = Reader::loopback(
        Origins::loopback(closed, p.uploads, p.web, p.cdn),
        None,
        DELIVERS,
    );
    let error = reader.releases(OWNER, REPO, 30, 1).unwrap_err();
    assert!(
        matches!(&error, AssetError::Unavailable { web, .. } if web.contains("lists no releases")),
        "{error:?}"
    );
    assert_eq!(reader.api_requests(), 1, "one try, never a retry");
}
