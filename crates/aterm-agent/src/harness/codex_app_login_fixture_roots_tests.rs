// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native fixture's read ([`read_identified_login_with_fixture_roots`]) sends every request
//! of one read over its pinned roots: a loopback provider under a test CA answers its usage read
//! and the one presentation of a refresh token a lost login's decision makes
//! ([`crate::harness::codex_login_rotation`]). No other reader trusts that CA, and the fixture's
//! client reaches no issuer off this Mac.

use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_http::stream::{Connect, Deadline};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::*;

/// A throwaway CA in no trust store, a leaf under it for `127.0.0.1`, and the leaf's key
/// (`testdata/fixture-roots/README.md`).
const CA: &[u8] = include_bytes!("testdata/fixture-roots/ca.der");
const LEAF: &[u8] = include_bytes!("testdata/fixture-roots/leaf.der");
const LEAF_KEY: &[u8] = include_bytes!("testdata/fixture-roots/leaf-key.pkcs8.der");

const NOW: i64 = 1_790_000_000;
const EMAIL: &str = "owner@example.test";
const REFRESH: &str = "ended-refresh-token";

/// When a refusal made at [`NOW`] is settled ([`ROTATION_HANG`] later).
fn settled_at() -> i64 {
    NOW + i64::try_from(ROTATION_HANG.as_secs()).expect("seconds")
}

/// What a [`tls_provider`] saw.
struct Served {
    /// Each request a client sent, which it does only once it trusts the leaf.
    requests: Vec<RecordedHttp>,
    /// Connections that opened a handshake and sent no request: a client that did not trust
    /// the leaf.
    untrusted: usize,
}

impl Served {
    fn lines(&self) -> Vec<String> {
        self.requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.target))
            .collect()
    }
}

/// [`provider`] under TLS, serving [`LEAF`]: it answers each request with
/// `answer(method, target)` until an empty connection ends it.
fn tls_provider(
    answer: impl Fn(&str, &str) -> (&'static str, Value) + Send + 'static,
) -> (u16, std::thread::JoinHandle<Served>) {
    aterm_http::tls::init_crypto();
    let crypto = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .expect("init_crypto installs a provider");
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto)
            .with_safe_default_protocol_versions()
            .expect("default protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(LEAF)],
                PrivateKeyDer::try_from(LEAF_KEY).expect("a PKCS#8 key"),
            )
            .expect("the leaf and its key pair"),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("TLS provider");
    let port = listener.local_addr().expect("address").port();
    let server = std::thread::spawn(move || {
        let mut served = Served {
            requests: Vec::new(),
            untrusted: 0,
        };
        while let Ok((mut tcp, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            let _ = tcp.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
            if !matches!(tcp.peek(&mut [0]), Ok(1)) {
                break;
            }
            let mut session =
                rustls::ServerConnection::new(Arc::clone(&config)).expect("TLS session");
            let mut stream = rustls::Stream::new(&mut session, &mut tcp);
            let Some(request) = read_http_request_from(&mut stream) else {
                served.untrusted += 1;
                continue;
            };
            let (status, body) = answer(&request.method, &request.target);
            send_http_json(&mut stream, status, &body);
            session.send_close_notify();
            let _ = session.write_tls(&mut tcp);
            served.requests.push(request);
        }
        served
    });
    (port, server)
}

/// Ends a [`tls_provider`] and returns what it saw.
fn end(port: u16, server: std::thread::JoinHandle<Served>) -> Served {
    let _ = TcpStream::connect(("127.0.0.1", port));
    server.join().expect("TLS provider")
}

/// The provider of a login whose refresh token simply ended: it refuses the access token on
/// every usage read, and the refresh token as expired.
fn lost_login(_method: &str, target: &str) -> (&'static str, Value) {
    match target {
        "/oauth/token" => (
            "401 Unauthorized",
            json!({"error":{"code":"refresh_token_expired"}}),
        ),
        _ => ("401 Unauthorized", json!({"error":"unauthorized"})),
    }
}

/// A login in `home` whose access token is good for an hour, signed in at `issuer`.
fn login_at(home: &Path, issuer: &str) -> StoredAuth {
    login_expiring(home, issuer, NOW + 3600)
}

/// A login in `home` signed in at `issuer` whose access token expires at `exp`.
fn login_expiring(home: &Path, issuer: &str, exp: i64) -> StoredAuth {
    let access = make_chatgpt_jwt(Some(EMAIL), "pro", exp);
    write_auth_doc(
        home,
        &json!({"auth_mode":"chatgpt", "tokens":{
            "id_token":access, "access_token":access, "refresh_token":REFRESH,
            "account_id":"acct_native_test",
            "oauth_provenance":{"issuer":issuer, "client_id":CLIENT_ID}
        }}),
    )
    .expect("login");
    load_stored_auth(home).expect("stored login")
}

/// The fixture's client: the test CA is its only root.
fn pinned(wait: Duration) -> Client {
    Client::new(Trust::Roots(vec![CA.to_vec()]), ProxyMode::Direct, wait)
}

/// The refusal the journal beside `home`'s login holds, as `(when, why)`.
fn recorded_refusal(home: &Path) -> Option<(Option<i64>, RefusalReason)> {
    match RefreshJournal::read(home).expect("a readable journal") {
        Some(RefreshJournal::Rejected { at, reason, .. }) => Some((at, reason)),
        _ => None,
    }
}

/// The host of a native fixture reads a lost login once: its usage is refused, so it presents
/// the login's refresh token to the login's own issuer, which the fixture serves under its
/// test CA beside the usage route. Both requests arrive, the token is presented once, and the
/// issuer's `refresh_token_expired` is the reason on record: pending now, and the settled
/// refusal a minute later that a move reads.
#[test]
fn a_fixture_read_presents_a_lost_logins_token_under_its_pinned_roots() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let config_home = tmp.path().join("codex");
    let user_home = tmp.path().join("user");
    std::fs::create_dir_all(&config_home).expect("configuration");
    let (port, server) = tls_provider(lost_login);
    let origin = format!("https://127.0.0.1:{port}");
    login_at(&home, &origin);
    std::fs::write(
        config_home.join("config.toml"),
        format!("chatgpt_base_url='{origin}'\n"),
    )
    .expect("route");

    let read = read_identified_login_with_fixture_roots(
        &home,
        &config_home,
        &user_home,
        aterm_spec::wait::MUST_HAPPEN,
        NOW,
        Some(EMAIL),
        vec![CA.to_vec()],
        crate::harness::provider_read::Use::fixture(
            crate::harness::provider_read::Read::CodexEndpoint,
            &home,
            None,
        ),
    );

    let served = end(port, server);
    assert_eq!(
        served.lines(),
        ["GET /api/codex/usage", "POST /oauth/token"],
        "the usage read and the refresh both reach the fixture's provider"
    );
    assert_eq!(served.untrusted, 0, "neither request refused the leaf");
    assert!(
        served.requests[1].body.contains(REFRESH),
        "the refresh presents the login's own token"
    );
    assert_eq!(
        read,
        IdentifiedRead::Unread,
        "a refusal just made is no sign-out yet"
    );
    assert_eq!(
        recorded_refusal(&home),
        Some((Some(NOW), RefusalReason::Expired))
    );
    assert!(
        matches!(
            refusal_standing(&home, Some(EMAIL), NOW),
            RefusalStanding::Pending { reason: RefusalReason::Expired, settles_at, .. }
                if settles_at == settled_at()
        ),
        "the host's own refusal is pending for a minute"
    );
    let settled = settled_refusal(&home, Some(EMAIL), settled_at());
    assert_eq!(settled, Some(RefusalReason::Expired));
    assert!(settled.is_some_and(RefusalReason::ends_ordinarily));
}

/// The read's other refresh, made before the usage read for an access token at or near its
/// expiry, goes over the fixture's client too: the token is presented first, once, with no
/// handshake that refused the leaf, and the issuer's refusal is on record as the 401 path's is.
#[test]
fn a_fixture_reads_early_refresh_goes_over_its_pinned_roots() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let config_home = tmp.path().join("codex");
    let user_home = tmp.path().join("user");
    std::fs::create_dir_all(&config_home).expect("configuration");
    let (port, server) = tls_provider(lost_login);
    let origin = format!("https://127.0.0.1:{port}");
    login_expiring(&home, &origin, NOW - 60);
    std::fs::write(
        config_home.join("config.toml"),
        format!("chatgpt_base_url='{origin}'\n"),
    )
    .expect("route");

    let _ = read_identified_login_with_fixture_roots(
        &home,
        &config_home,
        &user_home,
        aterm_spec::wait::MUST_HAPPEN,
        NOW,
        Some(EMAIL),
        vec![CA.to_vec()],
        crate::harness::provider_read::Use::fixture(
            crate::harness::provider_read::Read::CodexEndpoint,
            &home,
            None,
        ),
    );

    let served = end(port, server);
    let lines = served.lines();
    assert_eq!(
        served.untrusted, 0,
        "no request refused the leaf: {lines:?}"
    );
    assert_eq!(
        lines.first().map(String::as_str),
        Some("POST /oauth/token"),
        "the early refresh is the read's first request: {lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| *line == "POST /oauth/token")
            .count(),
        1,
        "the token is presented once: {lines:?}"
    );
    assert_eq!(
        recorded_refusal(&home),
        Some((Some(NOW), RefusalReason::Expired))
    );
}

/// CONTROL: the same provider and login read over the platform's trust store, as every reader
/// outside a fixture does. The CA is in no trust store, so the usage read and the refresh each
/// end at the handshake: no request arrives, nothing of the token is sent and nothing is
/// recorded. The same refresh over the fixture's client is the one the issuer answers.
#[test]
fn the_platform_transport_does_not_trust_a_fixtures_roots() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let (port, server) = tls_provider(lost_login);
    let origin = format!("https://127.0.0.1:{port}");
    let stored = login_at(&home, &origin);
    let usage_url = format!("{origin}/api/codex/usage");
    let fallback = format!("{DEFAULT_ISSUER}/oauth/token");
    let unpresented = |standing: RefusalStanding| {
        matches!(standing, RefusalStanding::Unpresented { digest }
            if digest == refresh_token_digest(REFRESH))
    };

    assert_eq!(
        read_identified_login_with(
            &home,
            &usage_url,
            &fallback,
            aterm_spec::wait::MUST_HAPPEN,
            NOW,
            Some(EMAIL),
        ),
        IdentifiedRead::Unread
    );
    assert!(matches!(
        refresh_outcome(
            &home,
            &stored,
            &fallback,
            aterm_spec::wait::MUST_HAPPEN,
            NOW,
            Some(EMAIL),
            None,
        ),
        Refreshed::Failed
    ));
    let served = end(port, server);
    assert!(
        served.requests.is_empty(),
        "no request reaches the provider: {:?}",
        served.lines()
    );
    assert_eq!(
        served.untrusted, 2,
        "the usage read and the refresh each refuse the leaf"
    );
    assert_eq!(recorded_refusal(&home), None);
    assert!(
        RefreshJournal::read(&home).expect("journal").is_none(),
        "a refresh that sent nothing leaves no attempt"
    );
    assert!(unpresented(refusal_standing(&home, Some(EMAIL), NOW)));

    let (port, server) = tls_provider(lost_login);
    let stored = login_at(&home, &format!("https://127.0.0.1:{port}"));
    assert!(matches!(
        refresh_outcome(
            &home,
            &stored,
            &fallback,
            aterm_spec::wait::MUST_HAPPEN,
            NOW,
            Some(EMAIL),
            Some(&pinned(aterm_spec::wait::MUST_HAPPEN)),
        ),
        Refreshed::Rejected
    ));
    let served = end(port, server);
    assert_eq!(served.lines(), ["POST /oauth/token"]);
    assert_eq!(served.untrusted, 0);
    assert_eq!(
        recorded_refusal(&home),
        Some((Some(NOW), RefusalReason::Expired))
    );
}

/// Counts the connections a client opens, and opens none off this Mac.
#[derive(Debug, Default)]
struct Dialed(AtomicUsize);

impl Connect for Dialed {
    fn connect(&self, host: &str, port: u16, _: Deadline) -> std::io::Result<TcpStream> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if host != "127.0.0.1" {
            return Err(std::io::Error::other("a test dials loopback alone"));
        }
        TcpStream::connect((host, port))
    }
}

/// A fixture's pinned roots are trust in a loopback issuer alone. A login whose issuer is
/// anywhere else is sent nothing over the fixture's client: no connection is opened, no attempt
/// is journaled and its token stays unpresented.
/// CONTROL: the same client does open one connection to a loopback issuer, which answers it.
#[test]
fn a_fixture_refresh_reaches_no_issuer_off_this_mac() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("login");
    let dialed = Arc::new(Dialed::default());
    let client = pinned(aterm_spec::wait::MUST_HAPPEN)
        .with_connector(Arc::clone(&dialed) as Arc<dyn Connect>);
    let fallback = format!("{DEFAULT_ISSUER}/oauth/token");
    let refresh = |stored: &StoredAuth| {
        refresh_outcome(
            &home,
            stored,
            &fallback,
            aterm_spec::wait::MUST_HAPPEN,
            NOW,
            Some(EMAIL),
            Some(&client),
        )
    };

    let elsewhere = login_at(&home, "https://auth.example.test");
    assert!(matches!(refresh(&elsewhere), Refreshed::Failed));
    assert_eq!(dialed.0.load(Ordering::SeqCst), 0, "nothing was dialed");
    assert!(RefreshJournal::read(&home).expect("journal").is_none());
    assert!(matches!(
        refusal_standing(&home, Some(EMAIL), NOW),
        RefusalStanding::Unpresented { .. }
    ));

    let (port, server) = tls_provider(lost_login);
    let here = login_at(&home, &format!("https://127.0.0.1:{port}"));
    assert!(matches!(refresh(&here), Refreshed::Rejected));
    assert_eq!(dialed.0.load(Ordering::SeqCst), 1);
    assert_eq!(end(port, server).lines(), ["POST /oauth/token"]);
}
