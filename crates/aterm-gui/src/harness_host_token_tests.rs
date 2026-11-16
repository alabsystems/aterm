// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Claude Code session on its account's vault token, as this host attributes it
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.6–§5.8, slice S11): its exec-time
//! environment holds the descriptor variable, the marker naming its row's entry and the served
//! token's `minted_at`, and its walls, tap readings and tap claim count under the row; the tap
//! records its served facts in this Mac's token table, and on the minting Mac (a scratch seated
//! vault, m22) verification by windows: the basis is a reading of a session on the row's stored
//! login, never a token's, a new token is held off while that stored login works here until one
//! reports, a reading of a token whose windows differ is folded into no row, a session the
//! wrapper served another token than the vault's (it names the one it served) supplies nothing of
//! the vault's token's verification or refusal, one an earlier wrapper served counts by its birth
//! and compares nothing, a session on no token the vault holds for its row supplies nothing, a
//! reading taken while the vault is busy is read again, a lost login is recorded as the refusal of
//! the token the session runs on, the turn-end route move judges a session by the token it was
//! served; `/login`, the profile reads and the `/usage` probe leave it out. Every vault is a
//! scratch one, and the token an inert fixture.

use super::*;
use aterm_agent::harness::accounts_tokens::{self, Verdict, Windows};
use aterm_agent::harness::token_route::{self, DESCRIPTOR_ENV, MARKER_ENV, MINTED_ENV, TokenFact};
use aterm_agent::harness::{claims, journal};
use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Lookup, Meta, MintedBy};
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

/// The environment the wrapper leaves a session it served: the descriptor, the marker and the
/// served token's `minted_at`.
fn token_env(marker: &str, minted_at: u64) -> Vec<String> {
    vec![
        "HOME=/Users//example".to_owned(),
        format!("{DESCRIPTOR_ENV}=5"),
        format!("{MARKER_ENV}={marker}"),
        format!("{MINTED_ENV}={minted_at}"),
    ]
}

/// A scratch seated vault in `home`, as m22, holding the account's inert token, minted here:
/// what a `token` verb's mint leaves.
pub(super) struct Vault {
    _server: aterm_secrets::server::Running,
    paths: Paths,
    /// The account the token is made for.
    email: String,
    /// The vault's source, pinned to its signer, as a Mac it admits configures it.
    pinned: String,
    /// The route its mints name ([`Meta::minted_by`]).
    minted_by: Option<MintedBy>,
}

impl Vault {
    /// The vault, its token made at `minted_at`.
    fn minted_in(home: &Path, minted_at: u64) -> Self {
        Self::minted_for(home, minted_at, EMAIL)
    }

    /// The vault, its token for `email` made at `minted_at`.
    pub(super) fn minted_for(home: &Path, minted_at: u64, email: &str) -> Self {
        Self::made(home, minted_at, email, None)
    }

    /// The vault, its token for `email` made at `minted_at` by the route `minted_by`.
    fn made(home: &Path, minted_at: u64, email: &str, minted_by: Option<MintedBy>) -> Self {
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
        let created =
            aterm_secrets::client::create(&paths, &url, None, Some("m22")).expect("create");
        aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).expect("roster");
        aterm_secrets::client::seed_vault_genesis(&paths, &home.with_extension("genesis"))
            .expect("genesis");
        aterm_secrets::client::admin_mirror_log(&paths, &[]).expect("the log");
        let vault = Self {
            _server: server,
            paths,
            email: email.to_owned(),
            pinned: format!("{url}#{}", created.signer),
            minted_by,
        };
        vault.mint(minted_at);
        vault
    }

    /// The meta of the account's token made here at `minted_at`, not verified.
    fn meta(&self, minted_at: u64) -> Meta {
        Meta {
            email: self.email.clone(),
            account_id: "c-0123456789ab".to_owned(),
            minted_at,
            expires_at: minted_at + 365 * 86_400,
            expiry_from: ExpiryFrom::Printed,
            minted_on: "m22".to_owned(),
            cli: "2.1.318".to_owned(),
            minted_by: self.minted_by,
            verified: None,
        }
    }

    /// Writes the account's token as one made here at `minted_at` and not verified: the first
    /// mint, or a `--replace`.
    fn mint(&self, minted_at: u64) {
        let written = claude_token::write(
            &self.paths,
            &Key::parse(KEY).unwrap(),
            &aterm_secrets::SecretBytes::from(FIXTURE_TOKEN.as_bytes()),
            &self.meta(minted_at),
            OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        )
        .expect("the write");
        assert_eq!(written, MemberWritten::Posted);
    }

    /// Lets in the Mac at `home` as `label`: its seat bound by the vault's log, its vault
    /// configured from the same source, and the records synced there. Returns its vault's paths.
    fn admit(&self, home: &Path, label: &str) -> Paths {
        let paths = Paths::for_home(home);
        aterm_secrets::client::configure_log_member(&paths, &self.pinned, label)
            .expect("configured");
        let _ = aterm_secrets::client::machine_identity(&paths).expect("an identity");
        let device = aterm_secrets::commons::bootstrap::Device::open(&paths)
            .expect("the device")
            .expect("a device key");
        let store = aterm_secrets::client::load_records(&self.paths).expect("the records");
        let log = store.log().expect("a log");
        let aterm_secrets::membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
            panic!("a genesis");
        };
        let request = device
            .request(root.network, root.authority)
            .expect("a request");
        let admitted =
            aterm_secrets::client::admit_by_phrase(&self.paths, &request, [0x12; 32], label, 1_000)
                .expect("the admission");
        assert!(matches!(
            admitted,
            aterm_secrets::client::ByLog::Admitted(_)
        ));
        aterm_secrets::client::post_records_to_servers(&self.paths).expect("posted");
        aterm_secrets::client::sync(&paths, None).expect("the admitted Mac syncs");
        paths
    }

    fn verified(&self) -> bool {
        matches!(
            claude_token::standing(&self.paths, &Key::parse(KEY).unwrap()),
            Ok(Lookup::Found(standing)) if standing.verified()
        )
    }
}

/// The fixture token: inert, in the setup-token grammar.
const FIXTURE_TOKEN: &str = concat!("sk-ant-", "oat01-inert_fixture-token-0123456789");

/// How long before the world's session was born its vault's first token was made: the session
/// started on it.
const MINTED_BEFORE_BIRTH: u64 = 600;

/// Records the row a mint of the verb leaves in this Mac's token table for the token made at
/// `minted_at`: the row's label, the token's `minted_at`, in the vault, nothing compared yet.
fn record_mint(w: &TapWorld, minted_at: u64) {
    accounts_tokens::update_table(&accounts::Store::at(&w.state()), |table| {
        table.rows.insert(
            KEY.to_owned(),
            accounts_tokens::TokenRow {
                key: KEY.to_owned(),
                email: EMAIL.to_owned(),
                account_id: Some("c-0123456789ab".to_owned()),
                label: Some("main".to_owned()),
                entry: Key::parse(KEY).unwrap().entry(),
                minted_at: Some(minted_at),
                expires_at: Some(minted_at + 365 * 86_400),
                expiry_from: Some("printed".to_owned()),
                minted_on: Some("m22".to_owned()),
                signer: Some("m22".to_owned()),
                used_on: accounts_tokens::UsedOn::default(),
                vault: accounts_tokens::InVault::InVault,
                verified: None,
                verification: accounts_tokens::Verification::default(),
                removed_at: None,
            },
        );
    })
    .expect("the mint's row");
}

/// What the verb's mint leaves: the token made at `minted_at` in the vault, then its row here.
fn mint(w: &TapWorld, vault: &Vault, minted_at: u64) {
    vault.mint(minted_at);
    record_mint(w, minted_at);
}

/// A [`TapWorld`] whose roster is [`ROSTER`] and whose seat's home holds the scratch vault, its
/// token made [`MINTED_BEFORE_BIRTH`] before the world's session was born, with the row its mint
/// left in this Mac's token table.
fn token_world() -> (Arc<TapWorld>, Vault) {
    let w = TapWorld::new();
    std::fs::write(w.roster_path(), ROSTER).expect("roster");
    let home = w.tmp.path().join("home");
    std::fs::create_dir_all(home.join(".secrets")).expect("home");
    let minted_at = born(&w) - MINTED_BEFORE_BIRTH;
    let vault = Vault::minted_in(&home, minted_at);
    record_mint(&w, minted_at);
    (w, vault)
}

/// The default login's record under the seat's home, naming [`EMAIL`]: the row's stored login is
/// on this Mac.
fn stored_login_here(w: &TapWorld) {
    std::fs::write(
        w.tmp.path().join("home/.claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{EMAIL}"}}}}"#),
    )
    .expect("the login record");
}

/// The token route's judgment of the vault's token for `main` now
/// ([`token_route::standing_judged`]): this Mac's table and accounts state, the roster and the
/// seat's home.
fn judged(w: &TapWorld, vault: &Vault) -> TokenFact {
    judged_at(w, &vault.paths)
}

/// [`judged`] by the vault at `paths`.
fn judged_at(w: &TapWorld, paths: &Paths) -> TokenFact {
    let key = Key::parse(KEY).unwrap();
    let roster = accounts::Roster::load(&w.roster_path()).expect("the roster");
    let found = claude_token::standing(paths, &key).expect("a standing");
    token_route::standing_judged(
        &found,
        paths,
        &accounts::Store::at(&w.state()),
        Some(&roster),
        &key,
        &token_route::Account {
            label: Some("main".to_owned()),
            email: EMAIL.to_owned(),
        },
        u64::try_from(w.clock()).unwrap(),
    )
}

/// The windows a status line [`TapWorld::tap`] writes shows: the row's own account's.
fn rows_windows(w: &TapWorld) -> Windows {
    Windows {
        week_reset: Some(w.born + 4 * 3600 + 86_400),
        session_reset: Some(w.born + 4 * 3600),
    }
}

/// The windows a status line [`theirs`] writes shows: another account's.
fn their_windows(w: &TapWorld) -> Windows {
    Windows {
        week_reset: Some(w.born + 4 * 3600 + 2 * 86_400),
        session_reset: Some(w.born + 4 * 3600 + 1800),
    }
}

/// When the world's session was born.
fn born(w: &TapWorld) -> u64 {
    u64::try_from(w.born).unwrap()
}

/// The `minted_at` of the world's first token ([`token_world`]).
fn first(w: &TapWorld) -> u64 {
    born(w) - MINTED_BEFORE_BIRTH
}

/// Puts the worker's agent on its row's vault token, the one made at `minted_at`, as the wrapper
/// serves it: its snapshot's route and its exec-time environment.
fn on_token(w: &TapWorld, idle: &WorkerIdle, minted_at: u64) {
    let mut env = w.env.lock().unwrap();
    env.retain(|kv| {
        !token_route::TOKEN_STATE_ENV
            .iter()
            .chain([&MINTED_ENV])
            .any(|name| kv.split_once('=').is_some_and(|(k, _)| k == *name))
    });
    env.extend(token_env(&entry().name(), minted_at));
    drop(env);
    if let Some(snap) = idle.kept.lock().unwrap().snapshot.as_mut() {
        snap.route = accounts::AuthRoute::VaultToken { entry: entry() };
    }
}

/// Puts the worker's agent back on its row's stored login, as the route move's relaunch does.
fn on_stored(w: &TapWorld, idle: &WorkerIdle) {
    *w.env.lock().unwrap() = vec!["HOME=/Users//example".to_owned()];
    if let Some(snap) = idle.kept.lock().unwrap().snapshot.as_mut() {
        snap.route = accounts::AuthRoute::Stored;
    }
}

/// A status line of another account: its five-hour window `pct` used, and both windows' resets
/// later than the ones [`TapWorld::tap`] names.
fn theirs(w: &TapWorld, api_ms: u64, pct: u32) -> String {
    let (session, week) = (w.born + 4 * 3600 + 1800, w.born + 4 * 3600 + 2 * 86_400);
    format!(
        r#"{{"cost":{{"total_api_duration_ms":{api_ms}}},"rate_limits":{{"five_hour":{{"used_percentage":{pct},"resets_at":{session}}},"seven_day":{{"used_percentage":61,"resets_at":{week}}}}}}}"#
    )
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
            homes: None,
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
        read(&token_env(&entry().name(), 1)),
        ("wall", Some("main".to_owned()))
    );
    assert_eq!(
        read(&token_env("aterm-claude-token-ac-0000000000000001", 1)),
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
/// SERVES, BUT NEVER AS ITS BASIS: on the minting Mac, a session on its row's token with no
/// stored-login reading before it has its readings recorded under `main`, its tap claim on the
/// row's account written, and its first and last served in the token table; no basis is recorded
/// from them, so its token stays `unverified (awaiting a stored-login reading)` and nothing writes
/// `verified`, though the row's sample now holds the token's own windows.
#[cfg(unix)]
#[test]
fn the_tap_credits_a_token_sessions_reading_and_claim_to_its_row() {
    let (w, vault) = token_world();
    // The pool file a fleet Mac holds: what turns account claims on.
    aterm_agent::vault_entry::put(&w.tmp.path().join("home"), "ai-pool.toml", "version = 1\n");
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    on_token(&w, &idle, first(&w));
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
    assert_eq!(row.verification.basis, None, "a token reading is no basis");
    assert_eq!(row.verification.verdict, Some(Verdict::NoBasis));

    w.later(60);
    assert_eq!(step(20, 44), Some("reading recorded"));
    let row = table(&w).rows.remove(KEY).expect("the token's row");
    assert_eq!(row.used_on.first_served_at, Some(now));
    assert_eq!(row.used_on.last_served_at, Some(now + 60));
    assert_eq!(
        row.verification.basis, None,
        "the row's sample, now the token's own windows, is no basis either"
    );
    assert_eq!(row.verification.verdict, Some(Verdict::NoBasis));
    assert_eq!(row.verified, None);
    assert!(!vault.verified(), "nothing compared the token with itself");
}

/// A NEW TOKEN IS HELD UNTIL ITS STORED LOGIN REPORTS, THEN EQUAL WINDOWS VERIFY IT (§5.6): with
/// the row's stored login on this Mac, the token its mint left is held (`awaiting-basis`): a launch
/// stays on the stored login. That session's reading, credited to the row, is the basis, and the
/// hold lifts; a session on the token then reads the same windows, the comparison finds them
/// equal, and the scratch vault's meta says `verified`. CONTROL: with no stored login here the
/// same token is not held, so the row is never left with no route.
#[cfg(unix)]
#[test]
fn a_new_token_is_held_until_its_stored_login_reports_then_equal_windows_verify_it() {
    let (w, vault) = token_world();
    assert_eq!(
        judged(&w, &vault),
        TokenFact::InUse,
        "CONTROL: no stored login here, nothing to hold it for"
    );
    stored_login_here(&w);
    assert_eq!(
        judged(&w, &vault),
        TokenFact::AwaitingBasis,
        "held: a launch stays on the stored login"
    );
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    w.tap(10, 42, None);
    let stored = journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    assert_eq!(
        stored,
        Some("reading recorded"),
        "the stored login's reading"
    );
    let row = table(&w).rows.remove(KEY).expect("the token's row");
    assert_eq!(
        row.verification.basis,
        Some((u64::try_from(w.clock()).unwrap(), rows_windows(&w))),
        "the stored login's reading is the basis"
    );
    assert_eq!(row.verification.verdict, None, "nothing compared yet");
    assert_eq!(judged(&w, &vault), TokenFact::InUse, "the hold lifts");

    on_token(&w, &idle, first(&w));
    w.later(30);
    w.tap(20, 43, None);
    let step = journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    assert_eq!(step, Some("reading recorded"));
    let row = table(&w).rows.remove(KEY).expect("the token's row");
    assert_eq!(row.verification.verdict, Some(Verdict::Equal));
    assert!(row.verified.is_some(), "the minting Mac wrote it");
    assert!(vault.verified(), "the vault's meta says verified");
    assert_eq!(judged(&w, &vault), TokenFact::InUse);
    assert_eq!(
        w.sample_of("main").and_then(|s| s.session_pct),
        Some(43),
        "the token's reading is the row's"
    );
}

/// A STORED-LOGIN READING IS A BASIS ONLY AS FAR AS THE FOLD CREDITS IT: a session on the row's
/// stored login reports the row's windows, recorded as the basis. Its login record then names
/// another `accountUuid` for the same email (the account made again, or another signed in to the
/// home), so the next reading, other windows, is held back while the row adopts the new key
/// ([`accounts::Account::adopting`]), and it replaces nothing of the basis. A session whose login
/// record names an account no row lists is attributed to no row, and records nothing either.
/// CONTROL: the same reading under the unchanged key is credited to the row and, the newer
/// stored-login reading, replaces the basis.
#[cfg(unix)]
#[test]
fn a_stored_login_reading_the_fold_does_not_credit_is_no_basis() {
    for next in ["held back", "unlisted", "credited"] {
        let (w, _vault) = token_world();
        let record = |email: &str, uuid: &str| {
            std::fs::write(
                w.tmp.path().join("home/.claude.json"),
                format!(
                    r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"{uuid}"}}}}"#
                ),
            )
            .expect("the login record");
        };
        record(EMAIL, "00000000-0000-4000-8000-000000000001");
        // The session's own home, whose login record the tap reads.
        *w.env.lock().unwrap() = vec![format!("HOME={}", w.tmp.path().join("home").display())];
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
        w.tap(10, 42, None);
        assert_eq!(
            step(),
            Some("reading recorded"),
            "the stored login's reading"
        );
        let basis = |w: &TapWorld| table(w).rows[KEY].verification.basis.map(|(_, b)| b);
        assert_eq!(basis(&w), Some(rows_windows(&w)), "PRECONDITION: its basis");

        match next {
            "held back" => record(EMAIL, "00000000-0000-4000-8000-000000000002"),
            "unlisted" => record(
                "someone-else@example.com",
                "00000000-0000-4000-8000-000000000003",
            ),
            _ => {}
        }
        w.later(30);
        w.tap_text(&theirs(&w, 20, 8), None, None);
        let said = step();
        if next == "credited" {
            assert_eq!(said, Some("reading recorded"), "CONTROL");
            assert_eq!(
                basis(&w),
                Some(their_windows(&w)),
                "CONTROL: the newer credited reading is the basis"
            );
        } else {
            assert_ne!(
                w.sample_of("main").and_then(|s| s.session_pct),
                Some(8),
                "PRECONDITION: {next}: the fold credited nothing of the reading to the row \
                 ({said:?})"
            );
            assert_eq!(
                basis(&w),
                Some(rows_windows(&w)),
                "{next}: a reading the fold did not credit to the row is no basis"
            );
        }
    }
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
    on_token(&w, &token, first(&w));
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

/// A REPLACEMENT IS COMPARED ONLY WITH A STORED-LOGIN READING, NEVER WITH WHAT AN EARLIER TOKEN
/// FOLDED INTO THE ROW: a first token made for `main` while signed in as another account (B) is
/// served with no stored-login reading yet, so its readings are compared with nothing and folded
/// into the row, whose sample now shows B's windows. A `--replace` signed in as B again makes a
/// second token; a session on it reads B's windows, not verified either (no stored-login reading
/// since it was made). A session on the row's stored login then reports the row's own windows,
/// recorded as the basis, which compares the second token's kept B reading at once: `differ`, and
/// its next B reading is folded into nothing, never verified. CONTROL: a second token whose
/// reading shows the row's own windows is verified at that stored-login reading. NEGATIVE
/// CONTROL: the row's sample after the first token's fold, taken as the second token's basis (the
/// rule this replaces), finds B's reading equal and would have verified another account's token.
#[cfg(unix)]
#[test]
fn a_replacement_is_compared_only_with_a_stored_login_reading() {
    for second_is_the_rows in [false, true] {
        let (w, vault) = token_world();
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
        on_token(&w, &idle, first(&w));
        w.tap_text(&theirs(&w, 10, 7), None, None);
        assert_eq!(step(), Some("reading recorded"), "compared with nothing");
        let row = table(&w).rows.remove(KEY).expect("the first token's row");
        assert_eq!(row.verification.basis, None);
        assert_eq!(row.verification.verdict, Some(Verdict::NoBasis));
        assert!(!vault.verified());
        let folded = w.sample_of("main").expect("the row's sample");
        assert_eq!(
            folded.week_reset,
            their_windows(&w).week_reset,
            "the first token's reading is the row's"
        );

        let second = born(&w) - MINTED_BEFORE_BIRTH / 2;
        mint(&w, &vault, second);
        on_token(&w, &idle, second);
        w.later(30);
        if second_is_the_rows {
            w.tap(20, 43, None);
        } else {
            w.tap_text(&theirs(&w, 20, 8), None, None);
        }
        // The fold finds nothing new in the row's own windows here (`reading: nothing new`); the
        // token's table records the reading either way.
        assert!(step().is_some(), "compared with nothing");
        let row = table(&w).rows.remove(KEY).expect("the second token's row");
        assert_eq!(row.minted_at, Some(second));
        assert_eq!(row.verification.basis, None);
        assert_eq!(row.verification.verdict, Some(Verdict::NoBasis));
        assert!(!vault.verified(), "the second token is not verified yet");
        assert_eq!(
            Verdict::of(
                Some(&Windows::current(&folded, w.clock())),
                &their_windows(&w),
                &[],
                w.clock()
            ),
            Verdict::Equal,
            "NEGATIVE CONTROL: the folded sample as a basis verifies B's token"
        );

        on_stored(&w, &idle);
        w.later(30);
        w.tap(30, 44, None);
        assert!(step().is_some(), "the stored login's reading");
        let row = table(&w).rows.remove(KEY).expect("the second token's row");
        assert_eq!(
            row.verification.basis.map(|(_, basis)| basis),
            Some(rows_windows(&w)),
            "the stored login's windows are the basis"
        );
        assert_eq!(
            (row.verification.verdict, vault.verified()),
            if second_is_the_rows {
                (Some(Verdict::Equal), true)
            } else {
                (Some(Verdict::Differ), false)
            },
            "the second token's kept reading is compared with the basis at once"
        );

        on_token(&w, &idle, second);
        w.later(30);
        if second_is_the_rows {
            w.tap(40, 45, None);
            assert!(step().is_some(), "the row's own windows");
            let row = table(&w).rows.remove(KEY).expect("the second token's row");
            assert_eq!(row.verification.verdict, Some(Verdict::Equal));
            assert!(
                vault.verified(),
                "CONTROL: the row's account's token is verified"
            );
        } else {
            let kept = w.sample_of("main");
            w.tap_text(&theirs(&w, 40, 9), None, None);
            assert_eq!(step(), Some(TAP_APART), "B's windows");
            assert_eq!(
                w.sample_of("main"),
                kept,
                "nothing of B's reading is folded into the row"
            );
            let row = table(&w).rows.remove(KEY).expect("the second token's row");
            assert_eq!(row.verification.verdict, Some(Verdict::Differ));
            assert!(!vault.verified(), "another account's token is not verified");
            assert_eq!(
                accounts_tokens::TokenState::of(
                    &claude_token::standing(&vault.paths, &Key::parse(KEY).unwrap()).unwrap(),
                    Some(&row),
                    u64::try_from(w.clock()).unwrap(),
                )
                .words(),
                "unverified (windows differ)"
            );
            assert_eq!(judged(&w, &vault), TokenFact::Differ);
        }
    }
}

/// A SESSION ON ANOTHER TOKEN THAN THE VAULT'S SUPPLIES NOTHING OF ITS VERIFICATION: a
/// stored-login reading gives the basis and a session on the row's token verifies it; the token
/// is then replaced, and the session, still running on the one it started on, reads again. Its
/// marker names the entry alone, but the wrapper named the token it served beside it: the reading
/// is taken as neither the new token's nor the row's (`TAP_OTHER_TOKEN`), the new token's row
/// records nothing of it, and nothing writes `verified`.
#[cfg(unix)]
#[test]
fn a_session_on_an_earlier_token_supplies_nothing_of_the_new_ones_verification() {
    let (w, vault) = token_world();
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    w.tap(10, 42, None);
    assert_eq!(step(), Some("reading recorded"), "the stored login");
    on_token(&w, &idle, first(&w));
    w.later(30);
    w.tap(20, 43, None);
    assert_eq!(step(), Some("reading recorded"));
    assert!(vault.verified(), "the token the session started on");
    assert_eq!(table(&w).rows[KEY].minted_at, Some(first(&w)));

    let replaced = u64::try_from(w.clock()).unwrap();
    mint(&w, &vault, replaced);
    assert!(
        !vault.verified(),
        "PRECONDITION: the new token is not verified"
    );
    w.later(30);
    w.tap(30, 44, None);
    assert_eq!(step(), Some(TAP_OTHER_TOKEN));
    assert!(
        !vault.verified(),
        "a session on the earlier token verified the new one"
    );
    let row = table(&w).rows.remove(KEY).expect("the new token's row");
    assert_eq!(row.minted_at, Some(replaced));
    assert_eq!(
        (row.used_on.first_served_at, row.verification),
        (None, accounts_tokens::Verification::default()),
        "no served fact of the new token"
    );
    assert_eq!(
        w.sample_of("main").and_then(|s| s.session_pct),
        Some(43),
        "and nothing of the reading is the row's"
    );
}

/// A SESSION THE WRAPPER SERVED BEFORE THE NEW TOKEN REACHED THE VAULT IS NOT ON IT, HOWEVER LATE
/// IT WAS BORN (§5.6): a `--replace` stamps the new token's `minted_at` at its capture, and the
/// vault holds it only once its write lands; a launch in between is served the token the vault
/// still holds. That session was born after the new token's `minted_at`, so its birth would take
/// it for one on the new token; it names the token it was served, so its reading, the row's own
/// windows, verifies nothing and is folded into nothing. A session an earlier aterm's wrapper
/// served names no token, so its birth alone says which it runs on: it began after the new token
/// was made, so its reading counts for the new token and the row, but compares nothing and
/// verifies nothing. CONTROL: a session served the new token, its windows the same, verifies it.
#[cfg(unix)]
#[test]
fn a_session_served_before_the_new_token_reached_the_vault_is_not_on_it() {
    for served in ["the earlier token", "no token named", "the vault's token"] {
        let (w, vault) = token_world();
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
        let new = born(&w) - 1;
        assert!(
            born(&w) > new && new > first(&w),
            "PRECONDITION: {served}: born after the new token's minted_at, on the earlier one"
        );
        mint(&w, &vault, new);
        w.tap(10, 42, None);
        assert_eq!(
            step(),
            Some("reading recorded"),
            "{served}: the stored login"
        );
        assert_eq!(
            table(&w).rows[KEY]
                .verification
                .basis
                .map(|(_, basis)| basis),
            Some(rows_windows(&w)),
            "PRECONDITION: {served}: the new token's basis"
        );
        match served {
            "the earlier token" => on_token(&w, &idle, first(&w)),
            "no token named" => {
                on_token(&w, &idle, first(&w));
                w.env
                    .lock()
                    .unwrap()
                    .retain(|kv| !kv.starts_with(&format!("{MINTED_ENV}=")));
            }
            _ => on_token(&w, &idle, new),
        }
        w.later(30);
        w.tap(20, 43, None);
        let now = u64::try_from(w.clock()).unwrap();
        match served {
            "the vault's token" => {
                assert_eq!(step(), Some("reading recorded"), "CONTROL");
                assert_eq!(
                    table(&w).rows[KEY].verification.verdict,
                    Some(Verdict::Equal)
                );
                assert!(
                    vault.verified(),
                    "CONTROL: the token the session was served"
                );
            }
            "no token named" => {
                assert_eq!(step(), Some("reading recorded"), "{served}");
                let row = table(&w).rows.remove(KEY).expect("the new token's row");
                assert_eq!(
                    (row.used_on.first_served_at, row.verification.verdict),
                    (Some(now), None),
                    "{served}: served, and nothing compared"
                );
                assert!(!vault.verified(), "{served}: the new token is not verified");
                assert_eq!(
                    w.sample_of("main").and_then(|s| s.session_pct),
                    Some(43),
                    "{served}: the reading is the row's"
                );
            }
            _ => {
                assert_eq!(step(), Some(TAP_OTHER_TOKEN), "{served}");
                assert!(!vault.verified(), "{served}: the new token is not verified");
                let row = table(&w).rows.remove(KEY).expect("the new token's row");
                assert_eq!(
                    (row.used_on.first_served_at, row.verification.verdict),
                    (None, None),
                    "{served}: no served fact or verdict of the new token"
                );
                assert_eq!(
                    w.sample_of("main").and_then(|s| s.session_pct),
                    Some(42),
                    "{served}: nothing of the reading is the row's"
                );
            }
        }
    }
}

/// A REFUSAL IS THE VAULT'S TOKEN'S ONLY WHERE THE SESSION RUNS ON IT (§5.6): a session on the
/// row's token loses its login; the wrapper served it the token the vault held when it started,
/// since replaced, so its refusal is recorded of nothing here and the vault's token is still
/// judged as before; a wrapper that named no token, of a session begun before the vault's token
/// was made, records nothing either; nor does a session on the vault's token whose newest API
/// error answered `403`, Cloudflare's block page, which Claude Code writes as a lost login
/// though the token works. CONTROL: a session served the vault's token records the refusal where
/// its newest API error answered `401`, and so does one an earlier wrapper served that began
/// after the vault's token was made; the token then reads `rejected` here.
#[cfg(unix)]
#[test]
fn a_refusal_is_the_vaults_tokens_only_where_the_session_was_served_it() {
    for (served, status) in [
        ("the earlier token", Some(token_route::REFUSED_STATUS)),
        (
            "no token named, begun before it",
            Some(token_route::REFUSED_STATUS),
        ),
        (
            "no token named, begun after it",
            Some(token_route::REFUSED_STATUS),
        ),
        ("the vault's token", Some(token_route::REFUSED_STATUS)),
        ("the vault's token", Some(CLOUDFLARE_STATUS)),
        ("the vault's token", None),
    ] {
        let case = format!("{served}, status {status:?}");
        let (w, vault) = token_world();
        let new = if served == "no token named, begun after it" {
            born(&w) - 1
        } else {
            u64::try_from(w.clock()).unwrap()
        };
        mint(&w, &vault, new);
        let minted = match served {
            "the earlier token" => Some(first(&w)),
            "the vault's token" => Some(new),
            _ => None,
        };
        let mut env = token_env(&entry().name(), minted.unwrap_or(new));
        if minted.is_none() {
            env.retain(|kv| !kv.starts_with(&format!("{MINTED_ENV}=")));
        }
        let roster = accounts::Roster::load(&w.roster_path()).expect("the roster");
        let recorded = journal::with_keyer(keyer, || {
            let who = accounts::ProcessLogin::of_exec_env(&env).expect("an env");
            let token = token_route::ServedToken::of(
                env.iter().filter_map(|kv| kv.split_once('=')),
                Some(born(&w)),
            );
            assert_eq!(
                token,
                minted.map_or(
                    token_route::ServedToken::Unnamed(Some(born(&w))),
                    token_route::ServedToken::Named
                ),
                "{case}"
            );
            token_route::token_refused(
                &w.tmp.path().join("home"),
                &vault.paths,
                &accounts::Store::at(&w.state()),
                Some(&roster),
                (&who, token),
                status,
                u64::try_from(w.clock()).unwrap(),
            )
        });
        assert!(table(&w).rows.contains_key(KEY), "the new token's row");
        if matches!(
            served,
            "the vault's token" | "no token named, begun after it"
        ) && status == Some(token_route::REFUSED_STATUS)
        {
            assert_eq!(recorded, Some(true), "CONTROL: {case}");
            assert_eq!(judged(&w, &vault), TokenFact::Rejected, "CONTROL: {case}");
        } else {
            assert_eq!(recorded, Some(false), "{case}");
            assert_eq!(
                judged(&w, &vault),
                TokenFact::InUse,
                "{case}: the vault's token is judged as before"
            );
        }
    }
}

/// The status Cloudflare's block page answers, which Claude Code writes as `authentication_failed`
/// (`Please run /login`) though the credential works.
const CLOUDFLARE_STATUS: u16 = 403;

/// Writes the world's conversation's transcript under the seat's home ending on the lost-login
/// notice Claude Code writes, answered `status` (`None`: a notice that names none): what
/// [`upgrade_drive::named_api_error_status`] reads of the session.
fn lost_login_notice(w: &TapWorld, status: Option<u16>) {
    let project = w.tmp.path().join("home/.claude/projects/p");
    std::fs::create_dir_all(&project).expect("its project");
    let status = status.map_or(String::new(), |s| format!(r#","apiErrorStatus":{s}"#));
    std::fs::write(
        project.join("0badf00d-1111-2222-3333-444455556666.jsonl"),
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"go on\"}}}}\n\
             {{\"type\":\"assistant\",\"isSidechain\":false,\"isApiErrorMessage\":true,\
             \"error\":\"authentication_failed\"{status},\"message\":{{\"model\":\"<synthetic>\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Please run /login\"}}]}}}}\n"
        ),
    )
    .expect("its conversation's transcript");
}

/// THE HOST RECORDS A LOST LOGIN AS THE REFUSAL OF THE TOKEN ITS SESSION RUNS ON (§5.6): a
/// session on the row's token hits Claude Code's sign-in wall, or its login ends; the host's
/// account step reads what the wrapper served it from its exec-time environment and its birth,
/// so where that is the vault's token the refusal is recorded and the token reads `rejected`
/// here, and where the session runs on a token since replaced nothing is, nor where an earlier
/// wrapper's session (it names no token) began before the vault's token was made. NEGATIVE
/// CONTROL: the step judging the session by anything but what it was served, its birth included,
/// records the refusal of the wrong token, or of none.
#[cfg(unix)]
#[test]
fn the_host_records_a_lost_login_as_the_refusal_of_the_token_its_session_runs_on() {
    let wall = AccountPoint::Wall {
        kind: alab_policy::vocab::WallKind::Auth,
    };
    for (served, point, status) in [
        ("the vault's token", wall, token_route::REFUSED_STATUS),
        (
            "the vault's token",
            AccountPoint::LoginEnded,
            token_route::REFUSED_STATUS,
        ),
        (
            "no token named, begun after it",
            AccountPoint::LoginEnded,
            token_route::REFUSED_STATUS,
        ),
        (
            "no token named, begun before it",
            AccountPoint::LoginEnded,
            token_route::REFUSED_STATUS,
        ),
        ("the earlier token", wall, token_route::REFUSED_STATUS),
        ("the vault's token", wall, CLOUDFLARE_STATUS),
    ] {
        let case = format!("{served} at {point:?}, status {status}");
        let (w, vault) = token_world();
        lost_login_notice(&w, Some(status));
        let new = if served == "no token named, begun before it" {
            born(&w) + 1
        } else {
            born(&w) - 1
        };
        mint(&w, &vault, new);
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        match served {
            "the vault's token" => on_token(&w, &idle, new),
            "the earlier token" => on_token(&w, &idle, first(&w)),
            _ => {
                on_token(&w, &idle, first(&w));
                w.env
                    .lock()
                    .unwrap()
                    .retain(|kv| !kv.starts_with(&format!("{MINTED_ENV}=")));
            }
        }
        let _ = journal::with_keyer(keyer, || idle.account(point));
        assert!(table(&w).rows.contains_key(KEY), "the new token's row");
        if matches!(
            served,
            "the earlier token" | "no token named, begun before it"
        ) || status != token_route::REFUSED_STATUS
        {
            assert_eq!(judged(&w, &vault), TokenFact::InUse, "{case}");
        } else {
            assert_eq!(judged(&w, &vault), TokenFact::Rejected, "{case}");
        }
    }
}

/// A TOKEN READING TAKEN WHILE THE VAULT OR THE TOKEN TABLE CANNOT BE READ IS RECORDED NOWHERE AND
/// READ AGAIN (§5.6): a session on the row's token reports while a vault apply holds the vault's
/// state, so whose token it runs on is not known: the step drops it (`TAP_TOKEN_UNREAD`), the
/// row's record and the token's row keep nothing of it, and the next step, the vault read again,
/// folds that same reading. A token table that cannot be read drops the fold the same way.
/// NEGATIVE CONTROLS: a busy vault taken for a session on the stored login, or a table error
/// passed over, folds the reading into the row at once, though the token it runs on may be one
/// whose windows differ.
#[cfg(unix)]
#[test]
fn a_token_reading_taken_while_the_vault_or_its_table_cannot_be_read_is_read_again() {
    let (w, vault) = token_world();
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    on_token(&w, &idle, first(&w));
    let held =
        aterm_secrets::client::test_hold_state_lock(&vault.paths).expect("a pass holds the state");
    w.tap(10, 42, None);
    assert_eq!(step(), Some(TAP_TOKEN_UNREAD));
    assert_eq!(w.sample_of("main"), None, "nothing of it is the row's");
    assert_eq!(
        table(&w).rows[KEY].used_on.first_served_at,
        None,
        "nor the token's"
    );
    drop(held);
    assert_eq!(
        step(),
        Some("reading recorded"),
        "the same reading, read again"
    );
    assert_eq!(w.sample_of("main").and_then(|s| s.session_pct), Some(42));
    assert_eq!(
        table(&w).rows[KEY].used_on.first_served_at,
        Some(u64::try_from(w.clock()).unwrap())
    );

    // A token table that cannot be read is no reading's served facts or comparison: the fold is
    // dropped, nothing of the reading is the row's, and the next step, the table whole again,
    // folds it.
    let file = accounts::Store::at(&w.state())
        .shared_dir()
        .join(accounts_tokens::TABLE_FILE);
    let whole = std::fs::read(&file).expect("the table");
    std::fs::write(&file, b"not a table").expect("a broken table");
    w.later(60);
    w.tap(20, 44, None);
    assert_eq!(step(), Some("reading dropped: not recorded"));
    assert_eq!(
        w.sample_of("main").and_then(|s| s.session_pct),
        Some(42),
        "nothing of it is the row's"
    );
    std::fs::write(&file, whole).expect("the table again");
    assert_eq!(
        step(),
        Some("reading recorded"),
        "the same reading, read again"
    );
    assert_eq!(w.sample_of("main").and_then(|s| s.session_pct), Some(44));
}

/// THE TURN-END ROUTE MOVE JUDGES A TOKEN SESSION BY THE TOKEN IT WAS SERVED (§5.6): a session the
/// wrapper served the vault's token, in use here, is offered nothing; one served a token a
/// `--replace` has since replaced is offered the vault's new token, in use here, which a relaunch
/// serves. Nothing moves off macOS. NEGATIVE CONTROL: judged by anything but what it was served,
/// the session on the vault's own token is offered a relaunch onto that same token at every turn
/// end.
#[cfg(unix)]
#[test]
fn the_route_move_judges_a_token_session_by_the_token_it_was_served() {
    for served in ["the vault's token", "a replaced token"] {
        let (w, vault) = token_world();
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        if served == "a replaced token" {
            mint(&w, &vault, u64::try_from(w.clock()).unwrap());
        }
        on_token(&w, &idle, first(&w));
        assert_eq!(
            judged(&w, &vault),
            TokenFact::InUse,
            "PRECONDITION: {served}: the vault's token is in use here"
        );
        let moves = served == "a replaced token" && cfg!(target_os = "macos");
        assert_eq!(
            journal::with_keyer(keyer, || idle.route_move()),
            moves.then_some(RouteTo::Token),
            "{served}"
        );
    }
}

/// A READING OF A SESSION ON NO TOKEN THE VAULT HOLDS FOR ITS ROW IS FOLDED NOWHERE (§5.6): a
/// session keeps running on its row's token after `--remove` took it out of the vault, and a
/// session an earlier wrapper served (it names no token) began before the vault's token was made,
/// so it runs on an earlier one. Either may be another account's token: its reading is the row's
/// in nothing (no sample, no tap claim) and no served fact of the vault's token. CONTROL: a
/// session served the vault's token has its reading recorded and claimed.
#[cfg(unix)]
#[test]
fn a_reading_of_a_session_on_no_token_the_vault_holds_is_folded_nowhere() {
    for served in [
        "a removed token",
        "no token named, begun before it",
        "the vault's token",
    ] {
        let (w, vault) = token_world();
        // The pool file a fleet Mac holds: what turns account claims on.
        aterm_agent::vault_entry::put(&w.tmp.path().join("home"), "ai-pool.toml", "version = 1\n");
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        let kept = Arc::new(Kept {
            rows: Mutex::default(),
        });
        let key = Key::parse(KEY).unwrap();
        match served {
            "a removed token" => {
                claude_token::remove(
                    &vault.paths,
                    &key,
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                )
                .expect("the removal");
                assert!(
                    matches!(
                        claude_token::standing(&vault.paths, &key),
                        Ok(Lookup::Absent)
                    ),
                    "PRECONDITION: the vault holds no token of the row"
                );
            }
            "no token named, begun before it" => mint(&w, &vault, born(&w) + 1),
            _ => {}
        }
        on_token(&w, &idle, first(&w));
        if served == "no token named, begun before it" {
            w.env
                .lock()
                .unwrap()
                .retain(|kv| !kv.starts_with(&format!("{MINTED_ENV}=")));
        }
        w.tap(10, 42, None);
        let step = journal::with_keyer(keyer, || {
            claims::with_table(kept.clone(), || idle.tap_step(TapAt::Point))
        });
        let claimed = kept
            .rows
            .lock()
            .unwrap()
            .iter()
            .any(|row| !row.claims.is_empty());
        if served == "the vault's token" {
            assert_eq!(step, Some("reading recorded"), "CONTROL");
            assert_eq!(
                w.sample_of("main").and_then(|s| s.session_pct),
                Some(42),
                "CONTROL"
            );
            assert!(claimed, "CONTROL: the tap claimed the row's account");
        } else {
            assert_eq!(step, Some(TAP_OTHER_TOKEN), "{served}");
            assert_eq!(
                w.sample_of("main"),
                None,
                "{served}: nothing of it is the row's"
            );
            assert!(!claimed, "{served}: no claim");
            assert_eq!(
                table(&w).rows[KEY].used_on.first_served_at,
                None,
                "{served}: no served fact of the vault's token"
            );
        }
    }
}

/// The account's token as the vault at `paths` reads it.
fn standing_at(paths: &Paths) -> claude_token::Standing {
    match claude_token::standing(paths, &Key::parse(KEY).unwrap()).expect("a standing") {
        Lookup::Found(standing) => standing,
        Lookup::Absent | Lookup::Unknown => panic!("the vault holds the token"),
    }
}

/// Whether the vault at `paths` returns exactly the fixture token.
fn returns_fixture(paths: &Paths) -> bool {
    match claude_token::read(paths, &Key::parse(KEY).unwrap(), Duration::from_secs(60)) {
        Ok(Lookup::Found(read)) => read
            .token()
            .is_some_and(|token| token.as_slice() == FIXTURE_TOKEN.as_bytes()),
        _ => false,
    }
}

/// A status line of the row's account whose five-hour window resets at `session` and seven-day
/// window at `week`.
fn line(api_ms: u64, pct: u32, session: i64, week: i64) -> String {
    format!(
        r#"{{"cost":{{"total_api_duration_ms":{api_ms}}},"rate_limits":{{"five_hour":{{"used_percentage":{pct},"resets_at":{session}}},"seven_day":{{"used_percentage":30,"resets_at":{week}}}}}}}"#
    )
}

/// Whether a file under `dir` holds a run of sixteen bytes of the fixture token.
fn holds_fixture(dir: &Path) -> bool {
    let run = &FIXTURE_TOKEN.as_bytes()[FIXTURE_TOKEN.len() - 16..];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            holds_fixture(&path)
        } else {
            std::fs::read(&path).is_ok_and(|bytes| bytes.windows(run.len()).any(|w| w == run))
        }
    })
}

/// Two seats of one scratch vault: m22, at `m22-home`, which minted the account's token in
/// aterm's window [`MINTED_BEFORE_BIRTH`] before the world's session was born, and this Mac, the
/// seat's home, which m22 let in as `m12`: the token reached it through the vault alone, so this
/// Mac's token table holds no row of it.
struct Peer {
    m22: Vault,
    here: Paths,
}

fn peer_world() -> (Arc<TapWorld>, Peer) {
    let w = TapWorld::new();
    std::fs::write(w.roster_path(), ROSTER).expect("roster");
    let home = w.tmp.path().join("home");
    std::fs::create_dir_all(home.join(".secrets")).expect("home");
    let m22_home = w.tmp.path().join("m22-home");
    std::fs::create_dir_all(m22_home.join(".secrets")).expect("m22's home");
    let m22 = Vault::made(&m22_home, first(&w), EMAIL, Some(MintedBy::Window));
    let here = m22.admit(&home, "m12");
    (w, Peer { m22, here })
}

/// A MAC HOLDING THE ACCOUNT'S STORED LOGIN VERIFIES A TOKEN ANOTHER MAC MINTED, AND REFUTES ONE
/// OF ANOTHER ACCOUNT (§5.6): m22 minted the account's token in aterm's window, so it is in use on
/// every Mac from mint, and this Mac, m12, holds no row of it. A session on the row's stored login
/// here reports the row's windows: the tap looks for the account's vault token though no row of
/// this Mac's table awaits one, and records the reading as its basis. A session on the token then
/// reads the same windows: they are equal, and this Mac writes `verified`, naming m12, which m22
/// reads, the token `active` there. A token of another account (its sessions read another
/// account's windows) is refuted here alone: `differ` stops it on this Mac (`token-differ`
/// journaled once, nothing written to the vault), and m22, which compared nothing, still uses it.
/// Nothing of the token's value reaches the journal or this Mac's state. NEGATIVE CONTROL
/// (2026-11-14, by hand, then restored with cp from a copy): with `basis_standing`'s look cut
/// where no row awaits, the stored-login reading makes no row of the token and records no basis.
#[cfg(unix)]
#[test]
fn a_mac_holding_the_accounts_stored_login_verifies_a_token_another_mac_minted_and_refutes_one_of_another_account()
 {
    for of_another in [false, true] {
        let (w, peer) = peer_world();
        let minted = standing_at(&peer.here);
        assert!(
            !minted.minted_here
                && minted.signer.as_deref() == Some("m22")
                && minted.serves_here()
                && !minted.verified(),
            "PRECONDITION: m22's window-made token, unverified, in use here: {minted:?}"
        );
        assert!(table(&w).rows.is_empty(), "PRECONDITION: no row of it here");
        stored_login_here(&w);
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
        let ((), notes) = journal::capture(|| {
            w.tap(10, 42, None);
            assert_eq!(
                step(),
                Some("reading recorded"),
                "the stored login's reading"
            );
            let row = table(&w)
                .rows
                .remove(KEY)
                .expect("the reading made the token's row");
            assert_eq!(row.minted_at, Some(first(&w)));
            assert_eq!(
                row.verification.basis,
                Some((u64::try_from(w.clock()).unwrap(), rows_windows(&w))),
                "the stored login's reading is the basis of m22's token"
            );

            on_token(&w, &idle, first(&w));
            w.later(30);
            if of_another {
                w.tap_text(&theirs(&w, 20, 8), None, None);
            } else {
                w.tap(20, 43, None);
            }
            let said = step();
            let now = u64::try_from(w.clock()).unwrap();
            let row = table(&w).rows.remove(KEY).expect("the token's row");
            aterm_secrets::client::sync(&peer.m22.paths, None).expect("m22 syncs");
            let theirs = standing_at(&peer.m22.paths);
            if of_another {
                assert_eq!(said, Some(TAP_APART), "another account's windows");
                assert_eq!(row.verification.verdict, Some(Verdict::Differ));
                assert_eq!(
                    judged_at(&w, &peer.here),
                    TokenFact::Differ,
                    "not in use here"
                );
                assert!(!standing_at(&peer.here).verified(), "nothing written");
                assert!(!theirs.verified(), "nor read on m22");
                assert!(
                    accounts_tokens::TokenState::of(&Lookup::Found(theirs), None, now).in_use(),
                    "m22 still uses it"
                );
            } else {
                assert_eq!(said, Some("reading recorded"));
                assert_eq!(row.verification.verdict, Some(Verdict::Equal));
                assert!(row.verified.is_some(), "this Mac wrote it");
                let on = |standing: &claude_token::Standing| {
                    standing
                        .meta
                        .as_ref()
                        .and_then(Meta::verification)
                        .map(|verified| verified.on.clone())
                };
                assert_eq!(on(&standing_at(&peer.here)).as_deref(), Some("m12"));
                assert_eq!(on(&theirs).as_deref(), Some("m12"), "m22 reads it");
                assert_eq!(
                    accounts_tokens::TokenState::of(&Lookup::Found(theirs), None, now),
                    accounts_tokens::TokenState::Active,
                    "active on m22"
                );
                assert_eq!(judged_at(&w, &peer.here), TokenFact::InUse);
            }
        });
        let differs = notes.iter().filter(|n| n.kind == "token-differ").count();
        assert_eq!(differs, usize::from(of_another), "token-differ: {notes:?}");
        assert!(
            !format!("{notes:?}").contains("inert_fixture"),
            "nothing of the value is journaled"
        );
        assert!(!holds_fixture(&w.state()), "nor in this Mac's state");
    }
}

/// A PEER MAC COMPARES NOTHING WITHOUT A CURRENT READING OF THE ACCOUNT'S STORED LOGIN (§5.6): on
/// m12, which holds no row of m22's token, a session on the token reads the row's own windows
/// twice. With no stored login of the account here, no reading is ever a basis, so neither
/// compares anything and nothing is written to the vault. With a stored-login reading whose week
/// ends before the token's reading, that basis compares nothing either. CONTROL: the exit test
/// above, with a current basis.
#[cfg(unix)]
#[test]
fn a_peer_mac_compares_nothing_without_a_current_stored_login_reading() {
    for case in ["no stored login", "an ended week"] {
        let (w, peer) = peer_world();
        let idle = w.worker(&tap_on(), tapped_argv(), true);
        let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
        if case == "an ended week" {
            stored_login_here(&w);
            let now = w.clock();
            w.tap_text(&line(10, 42, now + 30, now + 60), None, None);
            assert_eq!(step(), Some("reading recorded"), "{case}");
            assert!(
                table(&w).rows[KEY].verification.basis.is_some(),
                "{case}: PRECONDITION: a basis"
            );
            w.later(120);
        }
        on_token(&w, &idle, first(&w));
        for api_ms in [20, 30] {
            let now = w.clock();
            w.tap_text(&line(api_ms, 43, now + 3_600, now + 5 * 86_400), None, None);
            assert!(step().is_some(), "{case}: the token's reading");
            w.later(30);
        }
        let row = table(&w).rows.remove(KEY).expect("the token's row");
        assert_eq!(
            row.verification.verdict,
            Some(Verdict::NoBasis),
            "{case}: compared with nothing"
        );
        if case == "no stored login" {
            assert_eq!(
                row.verification.basis, None,
                "a token's reading is no basis"
            );
        }
        assert!(
            !standing_at(&peer.here).verified(),
            "{case}: nothing written"
        );
        assert!(row.verified.is_none(), "{case}");
    }
}

/// m22's meta of its token made at [`first`], refuted, as a later build writes a refutation: of a
/// later schema (2), which no build of schema 1 parses, so every Mac of this build reads a pair
/// beside it `mismatched (value)`.
fn refuted_meta(w: &TapWorld, peer: &Peer) -> String {
    let json = peer.m22.meta(first(w)).to_json();
    assert!(json.contains(r#""schema":1"#), "PRECONDITION: {json}");
    let at = u64::try_from(w.clock()).unwrap();
    json.replacen(r#""schema":1"#, r#""schema":2"#, 1).replacen(
        '{',
        &format!(
            r#"{{"refuted":{{"on":"m22","at":{at},"seven_day_resets_at":{},"five_hour_resets_at":null}},"#,
            born(w) + 4 * 3600 + 2 * 86_400
        ),
        1,
    )
}

/// Moves the world's clock, where it must, into this Mac's turn to settle a pair that it and m22
/// split just now: the turns go to the pair's writers in seat order, each
/// [`claude_token::SETTLE_TURN_SECS`] long from each Mac's first sighting of the split
/// ([`claude_token::settle`]), so both Macs see it first, at the world's clock now, writing
/// nothing; where m22's seat comes first this Mac's turn is the second, and the clock moves half a
/// turn into it.
fn into_this_macs_turn(w: &TapWorld, peer: &Peer) {
    let key = Key::parse(KEY).unwrap();
    for paths in [&peer.here, &peer.m22.paths] {
        claude_token::test_sight_split(
            paths,
            &key,
            u64::try_from(w.clock()).unwrap(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("sees the split");
    }
    let records = aterm_secrets::client::load_records(&peer.here).expect("the records");
    let seats = aterm_secrets::client::seats_ever(records.log().expect("a log"));
    let seat = |label: &str| {
        seats
            .iter()
            .find(|(_, _, named)| named == label)
            .map(|(seat, _, _)| *seat)
            .expect("a seat")
    };
    if seat("m22") < seat("m12") {
        w.later(i64::try_from(claude_token::SETTLE_TURN_SECS * 3 / 2).unwrap());
    }
}

/// TWO MACS' CONCURRENT VERIFY AND REFUTE ARE SETTLED AT ONE TICK, WITH NO NEW MINT (§5.6): this
/// Mac, m12, verifies m22's token by its stored login's windows while m22, before reading that
/// verification, writes the pair again refuted, as a later build's refutation is ([`refuted_meta`]).
/// Once each Mac holds both, the pair reads `mismatched (draft)` on both, whatever schema its meta
/// is of. In this Mac's turn to settle it ([`into_this_macs_turn`]), m22's settle writes nothing,
/// and this Mac's next token reading settles it in that tick: one draft, the token m22 minted
/// beside the refutation, which both Macs read `mismatched (value)`, so neither serves it, and
/// this Mac's verification is not written over the refutation (`aterm-secrets`' settle tests pin
/// the bytes kept).
#[cfg(unix)]
#[test]
fn a_concurrent_verify_and_refute_are_settled_at_one_tick_with_no_new_mint() {
    use aterm_secrets::claude_token::{Check, Mismatch, Settled};
    let (w, peer) = peer_world();
    stored_login_here(&w);
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    w.tap(10, 42, None);
    assert_eq!(step(), Some("reading recorded"), "the basis");

    let key = Key::parse(KEY).unwrap();
    let refuted = refuted_meta(&w, &peer);
    aterm_secrets::client::test_put_everyone_draft(
        &peer.m22.paths,
        &[
            (&key.entry(), FIXTURE_TOKEN.as_bytes()),
            (&key.meta_entry(), refuted.as_bytes()),
        ],
    )
    .expect("m22's refutation");
    on_token(&w, &idle, first(&w));
    w.later(30);
    w.tap(20, 43, None);
    assert_eq!(step(), Some("reading recorded"));
    assert!(
        standing_at(&peer.here).verified(),
        "PRECONDITION: this Mac verified it, not yet holding m22's refutation"
    );
    for paths in [&peer.here, &peer.m22.paths] {
        aterm_secrets::client::sync(paths, None).expect("syncs");
        assert_eq!(
            standing_at(paths).check,
            Check::Mismatched(Mismatch::Draft),
            "the two drafts split the pair"
        );
    }
    w.later(30);
    into_this_macs_turn(&w, &peer);
    assert_eq!(
        claude_token::settle(
            &peer.m22.paths,
            &key,
            u64::try_from(w.clock()).unwrap(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .expect("m22's settle"),
        Settled::Theirs("m12".to_owned()),
        "m22's turn it is not"
    );

    w.tap(30, 44, None);
    assert!(step().is_some(), "the next token reading");
    for paths in [&peer.here, &peer.m22.paths] {
        aterm_secrets::client::sync(paths, None).expect("syncs");
        let settled = standing_at(paths);
        assert_eq!(
            settled.check,
            Check::Mismatched(Mismatch::Value),
            "one tick settles it, to the refutation"
        );
        assert!(!settled.verified(), "the refutation stands: unverified");
        assert!(!returns_fixture(paths), "served nowhere");
    }
}

/// A SPLIT PAIR WHOSE WINNING META THIS BUILD DOES NOT READ IS SETTLED AT A READING ON ITS TOKEN
/// (§5.6): m22 and this Mac, m12, each write the pair again at once, refuted as a later build
/// refutes it ([`refuted_meta`], of schema 2), neither having read the other's. Once each Mac holds
/// both, the pair reads `mismatched (draft)`, and its meta, the register's winner, does not parse
/// here, so the standing names no token, and a session on the token reads as one on another token.
/// In this Mac's turn ([`into_this_macs_turn`]), the next reading of that session settles the pair
/// anyway: the settle goes before the reading is judged for its token. Both Macs then read one
/// draft, `mismatched (value)`, served nowhere. In the test above, the pair's winning meta is m22's
/// refutation or this Mac's verification, whichever op the log orders first. Before this fix, the
/// tap judged the token first, and about two runs in five that test failed: the reading was taken
/// as one of another token and settled nothing. Here neither meta parses, so that order fails
/// every run, as this test did before the fix.
#[cfg(unix)]
#[test]
fn a_split_pair_whose_winning_meta_does_not_parse_is_settled_at_a_reading_on_its_token() {
    use aterm_secrets::claude_token::{Check, Mismatch, Settled};
    let (w, peer) = peer_world();
    stored_login_here(&w);
    let idle = w.worker(&tap_on(), tapped_argv(), true);
    let step = || journal::with_keyer(keyer, || idle.tap_step(TapAt::Point));
    w.tap(10, 42, None);
    assert_eq!(step(), Some("reading recorded"), "the basis");

    let key = Key::parse(KEY).unwrap();
    let theirs = refuted_meta(&w, &peer);
    w.later(1);
    let ours = refuted_meta(&w, &peer).replacen(r#""on":"m22""#, r#""on":"m12""#, 1);
    assert_ne!(theirs, ours, "PRECONDITION: two values of the meta");
    for (paths, meta) in [(&peer.m22.paths, &theirs), (&peer.here, &ours)] {
        aterm_secrets::client::test_put_everyone_draft(
            paths,
            &[
                (&key.entry(), FIXTURE_TOKEN.as_bytes()),
                (&key.meta_entry(), meta.as_bytes()),
            ],
        )
        .expect("a refutation");
    }
    for paths in [&peer.here, &peer.m22.paths] {
        aterm_secrets::client::sync(paths, None).expect("syncs");
        let split = standing_at(paths);
        assert_eq!(
            split.check,
            Check::Mismatched(Mismatch::Draft),
            "the two drafts split the pair"
        );
        assert!(
            split.meta.is_none(),
            "PRECONDITION: its winning meta does not parse here"
        );
    }
    w.later(30);
    into_this_macs_turn(&w, &peer);
    assert_eq!(
        claude_token::settle(
            &peer.m22.paths,
            &key,
            u64::try_from(w.clock()).unwrap(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .expect("m22's settle"),
        Settled::Theirs("m12".to_owned()),
        "m22's turn it is not"
    );

    on_token(&w, &idle, first(&w));
    w.tap(30, 44, None);
    assert_eq!(
        step(),
        Some(TAP_OTHER_TOKEN),
        "the reading, read before its settle, names no token of the vault's"
    );
    for paths in [&peer.here, &peer.m22.paths] {
        aterm_secrets::client::sync(paths, None).expect("syncs");
        let settled = standing_at(paths);
        assert_eq!(
            settled.check,
            Check::Mismatched(Mismatch::Value),
            "the reading settles it, to one draft"
        );
        assert!(!returns_fixture(paths), "served nowhere");
    }
}

/// A SPLIT PAIR IS SETTLED AT THE ACCOUNTS UPKEEP'S PASS, WITH NO SESSION OF ITS ACCOUNT ON ANY
/// MAC (§5.6): this Mac, m12, and m22 verify m22's token at once, neither having read the other's
/// verification, so once each holds both the pair reads `mismatched (draft)` on both and no Mac
/// serves it. No session of the account runs anywhere, so no reading's tap settles it. In this
/// Mac's turn ([`into_this_macs_turn`]), m22's pass writes nothing, and this Mac's pass
/// ([`accounts_tokens::rows_on_token_settled`]) settles it and lists the vault again, so the row
/// reads as on its token at that same pass; both Macs then read one verified draft and return the
/// token. CONTROL: a read that settles nothing ([`accounts_tokens::rows_on_token_read`]) leaves
/// the pair split and the row off its token.
#[cfg(unix)]
#[test]
fn a_split_pair_is_settled_at_the_accounts_upkeep_with_no_session_anywhere() {
    use aterm_secrets::claude_token::{BY_WINDOWS, Check, Mismatch, Verified};
    let (w, peer) = peer_world();
    let key = Key::parse(KEY).unwrap();
    let now = u64::try_from(w.clock()).unwrap();
    for (paths, on) in [(&peer.here, "m12"), (&peer.m22.paths, "m22")] {
        let evidence = Verified {
            on: on.to_owned(),
            at: now,
            by: BY_WINDOWS.to_owned(),
            seven_day_resets_at: Some(born(&w) + 4 * 3600 + 86_400),
            five_hour_resets_at: Some(born(&w) + 4 * 3600),
        };
        assert_eq!(
            claude_token::verify(
                paths,
                &key,
                first(&w),
                evidence,
                OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            )
            .expect("a verification"),
            MemberWritten::Posted,
            "{on}'s verification"
        );
    }
    let split = || {
        for paths in [&peer.here, &peer.m22.paths] {
            aterm_secrets::client::sync(paths, None).expect("syncs");
            assert_eq!(
                standing_at(paths).check,
                Check::Mismatched(Mismatch::Draft),
                "the two verifications split the pair"
            );
        }
    };
    split();
    into_this_macs_turn(&w, &peer);
    let at = w.clock();
    let roster = accounts::Roster::load(&w.roster_path()).expect("the roster");
    let store = accounts::Store::at(&w.state());
    assert_eq!(
        journal::with_keyer(keyer, || {
            accounts_tokens::rows_on_token_read(&peer.here, &store, &roster, at)
        }),
        Some(std::collections::BTreeMap::new()),
        "CONTROL: a read that settles nothing: the row is off its token"
    );
    split();
    let m22_listed = claude_token::listed(&peer.m22.paths).expect("m22's listing");
    assert!(
        !accounts_tokens::settle_splits(
            &accounts_tokens::Vault(peer.m22.paths.clone()),
            &m22_listed,
            u64::try_from(at).unwrap()
        ),
        "m22's pass: not its turn"
    );
    let on_token = journal::with_keyer(keyer, || {
        accounts_tokens::rows_on_token_settled(&peer.here, &store, &roster, at)
    })
    .expect("the listing was read");
    assert_eq!(
        on_token.get("main"),
        Some(&(first(&w) + 365 * 86_400)),
        "the pass that settled the pair reads the row on its token"
    );
    for paths in [&peer.here, &peer.m22.paths] {
        aterm_secrets::client::sync(paths, None).expect("syncs");
        let settled = standing_at(paths);
        assert_eq!(settled.check, Check::Readable, "settled: {settled:?}");
        assert!(settled.verified(), "a verification stands");
        assert!(returns_fixture(paths), "the same token, served");
    }
}
