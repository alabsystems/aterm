// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Claude Code session on its account's vault token, as this host attributes it
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.6–§5.8, slice S11): its exec-time
//! environment holds the descriptor variable and the marker naming its row's entry, and its
//! walls, tap readings and tap claim count under the row; the tap records its served facts in
//! this Mac's token table, and on the minting Mac (a scratch seated vault, m22) verification by
//! windows; `/login`, the profile reads and the `/usage` probe leave it out. Every vault is a
//! scratch one, and the token an inert fixture.

use super::*;
use aterm_agent::harness::accounts_tokens::{self, Verdict, Windows};
use aterm_agent::harness::token_route::{DESCRIPTOR_ENV, MARKER_ENV};
use aterm_agent::harness::{claims, journal};
use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Lookup, Meta};
use aterm_secrets::client::{MemberWritten, OnBusy, Paths};

/// The lab key every fixture account is keyed by.
const KEY: &str = "ac-fedcba9876543210";

/// The row's claims email.
const EMAIL: &str = "person@example.com";

/// A roster of one row that names [`EMAIL`].
const ROSTER: &str =
    "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"person@example.com\"\n";

fn keyer(_realm: &str, _email: &str) -> Option<String> {
    Some(KEY.to_owned())
}

fn entry() -> accounts::TokenEntry {
    accounts::TokenEntry::parse(&format!("aterm-claude-token-{KEY}")).expect("an entry")
}

/// The environment the wrapper leaves a session it served: the descriptor and the marker.
fn token_env(marker: &str) -> Vec<String> {
    vec![
        "HOME=/Users//example".to_owned(),
        format!("{DESCRIPTOR_ENV}=5"),
        format!("{MARKER_ENV}={marker}"),
    ]
}

/// A scratch seated vault in `home`, as m22, holding the account's inert token, minted here at
/// `now`: what a `token` verb's mint leaves.
struct Vault {
    _server: aterm_secrets::server::Running,
    paths: Paths,
}

impl Vault {
    fn minted_in(home: &Path, now: u64) -> Self {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: home.with_extension("server"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .expect("the server");
        let url = format!("http://{}/v1/test", server.addr);
        let paths = Paths::for_home(home);
        aterm_secrets::client::create(&paths, &url, None, Some("m22")).expect("create");
        aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).expect("roster");
        aterm_secrets::client::seed_vault_genesis(&paths, &home.with_extension("genesis"))
            .expect("genesis");
        aterm_secrets::client::admin_mirror_log(&paths, &[]).expect("the log");
        let meta = Meta {
            email: EMAIL.to_owned(),
            account_id: "c-0123456789ab".to_owned(),
            minted_at: now - 60,
            expires_at: now + 365 * 86_400,
            expiry_from: ExpiryFrom::Printed,
            minted_on: "m22".to_owned(),
            cli: "2.1.318".to_owned(),
            verified: None,
        };
        let token = concat!("sk-ant-", "oat01-inert_fixture-token-0123456789");
        let written = claude_token::write(
            &paths,
            &Key::parse(KEY).unwrap(),
            &aterm_secrets::SecretBytes::from(token.as_bytes()),
            &meta,
            OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        )
        .expect("the write");
        assert_eq!(written, MemberWritten::Posted);
        Self {
            _server: server,
            paths,
        }
    }

    fn verified(&self) -> bool {
        matches!(
            claude_token::standing(&self.paths, &Key::parse(KEY).unwrap()),
            Ok(Lookup::Found(standing)) if standing.verified()
        )
    }
}

/// A [`TapWorld`] whose roster is [`ROSTER`] and whose seat's home holds the scratch vault.
fn token_world() -> (Arc<TapWorld>, Vault) {
    let w = TapWorld::new();
    std::fs::write(w.roster_path(), ROSTER).expect("roster");
    let home = w.tmp.path().join("home");
    std::fs::create_dir_all(home.join(".secrets")).expect("home");
    let vault = Vault::minted_in(&home, u64::try_from(w.clock()).unwrap());
    (w, vault)
}

/// Puts the worker's agent on its row's vault token: its snapshot's route and its exec-time
/// environment.
fn on_token(w: &TapWorld, idle: &WorkerIdle) {
    w.env.lock().unwrap().extend(token_env(&entry().name()));
    if let Some(snap) = idle.kept.lock().unwrap().snapshot.as_mut() {
        snap.route = accounts::AuthRoute::VaultToken { entry: entry() };
    }
}

fn table(w: &TapWorld) -> accounts_tokens::Table {
    accounts_tokens::read_table(&accounts::Store::at(&w.state())).expect("the table reads")
}

/// A claims table that keeps what it is written, for the tap's claim.
struct Kept {
    rows: Mutex<Vec<claims::Row>>,
}

impl claims::Table for Kept {
    fn read(&self, _: &Path) -> Result<claims::Tabled, String> {
        let mut book = claims::Book::default();
        for row in self.rows.lock().unwrap().iter().cloned() {
            book.insert(row);
        }
        Ok(claims::Tabled {
            me: "aa01".to_owned(),
            book,
            writable: Ok(()),
            clock: None,
            skewed: Vec::new(),
            held: Vec::new(),
        })
    }

    fn write(&self, _: &Path, row: &claims::Row) -> Result<(), String> {
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|kept| kept.machine != row.machine);
        rows.push(row.clone());
        Ok(())
    }
}

/// THE WALL RECORDER CREDITS A TOKEN SESSION'S WALL TO ITS ROW: an exec-time environment with
/// the descriptor and the marker naming the row's own entry records the wall for `main`, as a
/// stored login's does. A marker naming another account's entry is off the row's credentials
/// (`not-stored`), and so is a descriptor with no marker.
#[test]
fn the_wall_recorder_credits_a_token_sessions_wall_to_its_row() {
    use aterm_agent::harness::upgrade::StandingWall;
    const NOW: i64 = 1_790_000_000;
    let roster = accounts::Roster {
        rows: vec![accounts::Row {
            label: "main".to_owned(),
            class: accounts::Class::Preferred,
            email: Some(EMAIL.to_owned()),
            ..accounts::Row::default()
        }],
    };
    let wall = StandingWall {
        at: u64::try_from(NOW - 10).unwrap(),
        until: Some(u64::try_from(NOW + 3600).unwrap()),
        window: Some("five_hour".to_owned()),
        credits: None,
    };
    let read = |env: &[String]| -> (&'static str, Option<String>) {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("aterm-hh-token-wall-")
            .tempdir()
            .expect("temp dir");
        let store = accounts::Store::at(&tmp.path().join("state"));
        let read = WallRead {
            env,
            settled: true,
            wall: Some(wall.clone()),
            screen_wall: None,
            started: Some(NOW - 100),
        };
        journal::with_keyer(keyer, || {
            account_wall_of(&store, &roster, "s-tok", &read, NOW)
        })
    };
    assert_eq!(
        read(&token_env(&entry().name())),
        ("wall", Some("main".to_owned()))
    );
    assert_eq!(
        read(&token_env("aterm-claude-token-ac-0000000000000001")),
        ("not-stored", None),
        "another account's entry"
    );
    assert_eq!(
        read(&[format!("{DESCRIPTOR_ENV}=5")]),
        ("not-stored", None),
        "a descriptor with no marker"
    );
    assert_eq!(
        read(&["HOME=/Users//example".to_owned()]),
        ("wall", Some("main".to_owned())),
        "CONTROL: the stored login"
    );
}

/// THE TAP CREDITS A TOKEN SESSION'S READING TO ITS ROW AND CLAIMS IT, AND RECORDS WHERE IT
/// SERVES: on the minting Mac, a session on its row's token with no stored-login reading before
/// it has its readings recorded under `main`, its tap claim on the row's account written, and its
/// first and last served in the token table; with no basis (no stored-login windows before the
/// fold) its token stays `unverified (no reading yet)` and nothing writes `verified`, though the
/// row's sample now holds the token's own windows.
#[cfg(unix)]
#[test]
fn the_tap_credits_a_token_sessions_reading_and_claim_to_its_row() {
    let (w, vault) = token_world();
    // The pool file a fleet Mac holds: what turns account claims on.
    std::fs::write(
        w.tmp.path().join("home/.secrets/ai-pool.toml"),
        "version = 1\n",
    )
    .expect("the pool");
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    on_token(&w, &idle);
    let kept = Arc::new(Kept {
        rows: Mutex::default(),
    });
    let step = |api_ms: u64, pct: u32| {
        w.tap(api_ms, pct, None);
        journal::with_keyer(keyer, || {
            claims::with_table(kept.clone(), || idle.tap_step(TapAt::Point))
        })
    };
    assert_eq!(step(10, 42), Some("reading recorded"));
    let sample = w.sample_of("main").expect("the row's sample");
    assert_eq!(sample.session_pct, Some(42), "credited to the row");
    let claimed = kept.rows.lock().unwrap().clone();
    assert!(
        claimed
            .iter()
            .flat_map(|row| &row.claims)
            .any(|claim| claim.key == claims::LeaseKey::new("claude", EMAIL).unwrap()),
        "the tap claimed the row's account: {claimed:?}"
    );
    let now = u64::try_from(w.clock()).unwrap();
    let row = table(&w).rows.remove(KEY).expect("the token's row");
    assert_eq!(row.label.as_deref(), Some("main"));
    assert_eq!(row.used_on.first_served_at, Some(now));
    assert_eq!(row.used_on.last_served_at, Some(now));
    assert_eq!(
        row.verification.basis.map(|(_, basis)| basis),
        Some(Windows::default()),
        "no stored-login windows before the fold"
    );
    assert_eq!(row.verification.verdict, Some(Verdict::NoBasis));

    w.later(60);
    assert_eq!(step(20, 44), Some("reading recorded"));
    let row = table(&w).rows.remove(KEY).expect("the token's row");
    assert_eq!(row.used_on.first_served_at, Some(now));
    assert_eq!(row.used_on.last_served_at, Some(now + 60));
    assert_eq!(row.verification.verdict, Some(Verdict::NoBasis));
    assert_eq!(row.verified, None);
    assert!(!vault.verified(), "nothing compared the token with itself");
}

/// EQUAL WINDOWS ON THE MINTING MAC WRITE `verified`: the row's stored login read its windows
/// first (a session on it), then a session on the row's token reads the same windows; the basis
/// recorded before the token's reading was folded is the stored login's, the comparison finds them
/// equal, and the scratch vault's meta now says `verified`.
#[cfg(unix)]
#[test]
fn equal_windows_after_a_stored_reading_verify_the_token() {
    let (w, vault) = token_world();
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    w.tap(10, 42, None);
    let stored = journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    assert_eq!(
        stored,
        Some("reading recorded"),
        "the stored login's reading"
    );
    let sample = w.sample_of("main").expect("the stored login's sample");
    let basis = Windows::current(&sample, w.clock());
    assert!(basis.week_reset.is_some() && basis.session_reset.is_some());
    assert!(table(&w).rows.is_empty(), "a stored login serves no token");

    on_token(&w, &idle);
    w.later(30);
    w.tap(20, 43, None);
    let step = journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    assert_eq!(step, Some("reading recorded"));
    let row = table(&w).rows.remove(KEY).expect("the token's row");
    assert_eq!(
        row.verification.basis.map(|(_, b)| b),
        Some(basis),
        "the basis is the stored login's"
    );
    assert_eq!(row.verification.verdict, Some(Verdict::Equal));
    assert!(row.verified.is_some(), "the minting Mac wrote it");
    assert!(vault.verified(), "the vault's meta says verified");
    assert_eq!(
        w.sample_of("main").and_then(|s| s.session_pct),
        Some(43),
        "the token's reading is the row's"
    );
}

/// `/login`, THE PROFILE READS AND THE `/usage` PROBE LEAVE A TOKEN SESSION OUT: its login pin is
/// [`LoginPin::Token`](aterm_agent::supervise::policy::login::LoginPin::Token), so the loop types
/// no `/login` there; the probe's claim refuses it (`not-a-stored-login`) and a claim already won
/// takes no read authority; its login account (a profile read) is none. CONTROL: the same session
/// on its stored login.
#[cfg(unix)]
#[test]
fn login_the_profile_reads_and_the_usage_probe_leave_a_token_session_out() {
    use aterm_agent::supervise::policy::login::LoginPin;
    let (w, _vault) = token_world();
    let mut cfg = tap_on();
    cfg.set("accounts", "early").expect("a mode");
    let stored = w.worker(&cfg, tapped_argv(), true);
    assert_ne!(stored.login_pin(), LoginPin::Token, "CONTROL");
    let probing = |idle: &WorkerIdle| {
        let who = journal::with_keyer(keyer, || {
            accounts::ProcessLogin::of_exec_env(&w.env.lock().unwrap()).expect("an env")
        });
        idle.kept.lock().unwrap().account.probing = Some(Probing {
            at: w.clock(),
            label: "main".to_owned(),
            pid: TAP_PID,
            start: String::new(),
            row: accounts::Row::default(),
            who,
            roster: w.roster_path(),
            state: w.state(),
        });
        idle.take_provider_read_account()
    };
    assert!(
        probing(&stored).is_some(),
        "CONTROL: a stored login's claim"
    );
    let claimed = journal::with_keyer(keyer, || stored.probe_claimed().err());
    assert_ne!(claimed, Some(NoProbe::NotJudged), "CONTROL");

    let token = w.worker(&cfg, tapped_argv(), true);
    on_token(&w, &token);
    assert_eq!(token.login_pin(), LoginPin::Token);
    assert!(
        probing(&token).is_none(),
        "no read authority for a token session"
    );
    assert_eq!(
        journal::with_keyer(keyer, || token.probe_claimed().err()),
        Some(NoProbe::NotJudged)
    );
    assert_eq!(token.login_account(), None, "no profile read");
}
