// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts token` (DESIGN-claude-setup-token S6): its flags, every refusal
//! before a spawn, the accounts it accepts, and, with the setup-token stand-in
//! ([`crate::harness::setup_token::tests::World`]) and a scratch seated vault, a stored token,
//! a write a vault pass holds off, Ctrl-C during that wait, a token under a day, and `--remove`.
//! The stand-in's token is the sentinel no output, table or note may hold.

use super::*;
use crate::harness::align::Captured;
use crate::harness::cli::{AccountsCmd, Cmd, run_accounts};
use crate::harness::setup_token::tests::{World, token as sentinel};
use aterm_secrets::claude_token::{Check, Mismatch, Verified};
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// The verb's clock, and the fake standings' (2027-01-15).
pub(in crate::harness) const NOW: u64 = 1_800_000_000;

/// The roster's row with an email.
pub(in crate::harness) const EMAIL: &str = "person@example.com";

/// The lab key every test account is keyed by.
pub(in crate::harness) const KEY: &str = "ac-fedcba9876543210";

/// The roster: `work` (an email), `lent` (an email, held by m7), `bare` (no email).
const ROSTER: &str = "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \
                      \"person@example.com\"\n\n[[account]]\nlabel = \"lent\"\nclass = \
                      \"dedicated\"\ndir = \"/logins/lent\"\nemail = \"lent@example.com\"\n\
                      held_by = \"m7\"\n\n[[account]]\nlabel = \"bare\"\nclass = \
                      \"dedicated\"\ndir = \"/logins/bare\"\n";

pub(in crate::harness) fn keyer(_realm: &str, _email: &str) -> Option<String> {
    Some(KEY.to_owned())
}

pub(in crate::harness) fn key() -> Key {
    Key::parse(KEY).expect("a key")
}

/// A scratch world for the verb: its state, config (the roster beside it) and home.
pub(in crate::harness) struct Scratch {
    dir: aterm_tempfile::TempDir,
}

impl Scratch {
    pub(in crate::harness) fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-accounts-token-{tag}-"))
            .suffix(".noindex")
            .tempdir()
            .expect("scratch");
        let scratch = Self { dir };
        std::fs::create_dir_all(scratch.path().join("home")).expect("home");
        std::fs::write(scratch.path().join("accounts.toml"), ROSTER).expect("roster");
        scratch
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(in crate::harness) fn env(&self) -> Env {
        let tmp = self.path();
        Env {
            state: tmp.join("state"),
            aterm_state: Some(tmp.join("aterm-state")),
            cwd: tmp.join("work"),
            home: Some(tmp.join("home")),
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            xdg_config_home: None,
            codex_login: None,
            now: i64::try_from(NOW).unwrap(),
            sid: "s-test".to_owned(),
            sock: None,
            config: Some(tmp.join("aterm.toml")),
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        }
    }

    pub(in crate::harness) fn store(&self) -> Store {
        self.env().accounts_store_for(Cli::Claude).expect("a store")
    }

    fn table(&self) -> Table {
        read_table(&self.store()).expect("the table reads")
    }
}

/// A standing the fake vault answers with.
#[derive(Clone)]
pub(in crate::harness) enum Answer {
    Unknown,
    Absent,
    Found(Box<Standing>),
}

/// The vault, stood in for: a route, a standing, whether a write here still waits, and the
/// writes and removals it was asked for.
#[derive(Clone)]
pub(in crate::harness) struct FakeVault {
    route: WriteRoute,
    pub(in crate::harness) found: Answer,
    pub(in crate::harness) pending: bool,
    writes: Rc<RefCell<Vec<Meta>>>,
    removes: Rc<Cell<u32>>,
}

impl FakeVault {
    pub(in crate::harness) fn new(found: Answer) -> Self {
        Self {
            route: WriteRoute::Seated,
            found,
            pending: false,
            writes: Rc::default(),
            removes: Rc::default(),
        }
    }
}

impl TokenVault for FakeVault {
    fn route(&self, _by: Instant) -> Result<WriteRoute, String> {
        Ok(self.route)
    }
    fn standing(&self, _key: &Key, _by: Instant) -> Result<Lookup<Standing>, String> {
        Ok(match &self.found {
            Answer::Unknown => Lookup::Unknown,
            Answer::Absent => Lookup::Absent,
            Answer::Found(standing) => Lookup::Found((**standing).clone()),
        })
    }
    fn listed(&self) -> Result<Lookup<Vec<Standing>>, String> {
        Ok(match &self.found {
            Answer::Unknown => Lookup::Unknown,
            Answer::Absent => Lookup::Found(Vec::new()),
            Answer::Found(standing) => Lookup::Found(vec![(**standing).clone()]),
        })
    }
    fn waiting_here(&self, _key: &Key) -> Result<bool, String> {
        Ok(self.pending)
    }
    fn seat_here(&self) -> Result<Option<String>, String> {
        Ok(Some("m22".to_owned()))
    }
    fn write(
        &self,
        _key: &Key,
        token: &SecretBytes,
        meta: &Meta,
        _on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        assert_eq!(token.as_slice(), sentinel().as_bytes(), "the token made");
        self.writes.borrow_mut().push(meta.clone());
        Ok(MemberWritten::Posted)
    }
    fn remove(&self, _key: &Key, _by: Instant) -> Result<MemberUnset, String> {
        self.removes.set(self.removes.get() + 1);
        Ok(MemberUnset::Posted)
    }
    fn verify(
        &self,
        _key: &Key,
        _evidence: Verified,
        _on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        Ok(MemberWritten::Refused(
            "the fake vault verifies nothing".to_owned(),
        ))
    }
}

/// A standing of the account's token: `check`, ending at `expires_at`, verified or not, minted
/// here or on m7, handed to the removed Macs `exposed`.
pub(in crate::harness) fn standing(
    check: Check,
    expires_at: u64,
    verified: bool,
    here: bool,
    exposed: &[&str],
) -> Answer {
    Answer::Found(Box::new(Standing {
        key: key(),
        meta: Some(Meta {
            email: EMAIL.to_owned(),
            account_id: "c-0123456789ab".to_owned(),
            minted_at: NOW - 86_400,
            expires_at,
            expiry_from: ExpiryFrom::Printed,
            minted_on: if here { "m22" } else { "m7" }.to_owned(),
            cli: "2.1.318".to_owned(),
            verified: verified.then(|| Verified {
                on: "m7".to_owned(),
                at: NOW - 3_600,
                by: "windows".to_owned(),
                seven_day_resets_at: None,
                five_hour_resets_at: None,
            }),
        }),
        check,
        signer: Some(if here { "m22" } else { "m7" }.to_owned()),
        minted_here: here,
        exposed: exposed.iter().map(|m| (*m).to_owned()).collect(),
    }))
}

pub(in crate::harness) const A_YEAR: u64 = 365 * 86_400;

fn active() -> Answer {
    standing(Check::Readable, NOW + A_YEAR, true, false, &[])
}

/// Where the host's vault is.
#[derive(Clone)]
pub(in crate::harness) enum VaultOf {
    Fake(FakeVault),
    Real(Paths),
    Contended(ContendedVault),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VaultRead {
    Route,
    Standing,
}

/// Delegate every operation to the real vault, observing the actual read boundary.
#[derive(Clone)]
pub(in crate::harness) struct ContendedVault {
    paths: Paths,
    before_read: Rc<dyn Fn(VaultRead, Instant) -> Instant>,
}

fn native_writer_is_busy(paths: &Paths) {
    let expected = aterm_secrets::client::config(paths)
        .unwrap()
        .expect("the configured fixture vault");
    let native = aterm_secrets::client::observe_materialization::<()>(paths, &expected, || {
        panic!("the held native writer admits no input read")
    })
    .unwrap();
    assert!(
        matches!(
            native,
            aterm_secrets::client::MaterializationObservation::Blocked(
                aterm_secrets::client::MaterializationBlocker::WriterBusy
            )
        ),
        "the actual native lock must be busy before releasing its writer"
    );
}

impl ContendedVault {
    fn real(&self) -> Vault {
        Vault(self.paths.clone())
    }
}

impl TokenVault for ContendedVault {
    fn route(&self, by: Instant) -> Result<WriteRoute, String> {
        let by = (self.before_read)(VaultRead::Route, by);
        self.real().route(by)
    }
    fn standing(&self, key: &Key, by: Instant) -> Result<Lookup<Standing>, String> {
        let by = (self.before_read)(VaultRead::Standing, by);
        self.real().standing(key, by)
    }
    fn listed(&self) -> Result<Lookup<Vec<Standing>>, String> {
        self.real().listed()
    }
    fn waiting_here(&self, key: &Key) -> Result<bool, String> {
        self.real().waiting_here(key)
    }
    fn seat_here(&self) -> Result<Option<String>, String> {
        self.real().seat_here()
    }
    fn write(
        &self,
        key: &Key,
        token: &SecretBytes,
        meta: &Meta,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        self.real().write(key, token, meta, on_busy)
    }
    fn remove(&self, key: &Key, by: Instant) -> Result<MemberUnset, String> {
        self.real().remove(key, by)
    }
    fn verify(
        &self,
        key: &Key,
        evidence: Verified,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        self.real().verify(key, evidence, on_busy)
    }
    fn post(&self) -> Result<bool, String> {
        self.real().post()
    }
}

/// The person at the terminal, stood in for: lets the stand-in go once it runs, tells it the
/// browser opened, and, where `stops`, presses Ctrl-C once the verb says it waits for the vault.
#[derive(Clone)]
pub(in crate::harness) struct TestPerson {
    home: PathBuf,
    said: Arc<Mutex<Vec<String>>>,
    stops: bool,
}

impl Person for TestPerson {
    fn say(&self, line: &str) {
        if line.contains("browser window opened") {
            std::fs::write(self.home.join("told"), "").expect("told");
        }
        self.said.lock().unwrap().push(line.to_owned());
    }

    fn paste(
        &self,
        _prompt: &str,
        cancelled: &AtomicBool,
    ) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(Instant::now() < until, "the paste was never cancelled");
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(None)
    }

    fn stopped(&self) -> bool {
        if self.home.join("pid").exists() && !self.home.join("go").exists() {
            std::fs::write(self.home.join("go"), "").expect("go");
        }
        self.stops
            && self
                .said
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("waiting for the vault"))
    }
}

/// The host the verb runs on: every token seam injected, nothing of this Mac's read.
pub(in crate::harness) struct Host {
    tty: bool,
    macos: bool,
    env: Vec<(OsString, OsString)>,
    vault: Option<VaultOf>,
    program: Option<Program>,
    pub(in crate::harness) asked_program: Cell<bool>,
    person: Option<TestPerson>,
    pub(in crate::harness) logins: Vec<SessionLogin>,
    write_wait: Option<Duration>,
    /// Whether another Mac's fleet drive typed into this run's session
    /// ([`AccountsHost::driven_over_the_fleet`]).
    fleet: bool,
}

impl Host {
    pub(in crate::harness) fn new(vault: VaultOf) -> Self {
        Self {
            tty: true,
            macos: true,
            env: Vec::new(),
            vault: Some(vault),
            program: None,
            asked_program: Cell::new(false),
            person: None,
            logins: Vec::new(),
            write_wait: None,
            fleet: false,
        }
    }

    /// The host of a run of the stand-in in `world`.
    fn minting(vault: VaultOf, world: &World) -> Self {
        Self {
            env: world.env(&[]),
            program: Some(World::program()),
            person: Some(TestPerson {
                home: world.home().to_path_buf(),
                said: Arc::default(),
                stops: false,
            }),
            ..Self::new(vault)
        }
    }

    fn said(&self) -> Vec<String> {
        self.person
            .as_ref()
            .map(|p| p.said.lock().unwrap().clone())
            .unwrap_or_default()
    }
}

impl AccountsHost for Host {
    fn stdin_is_terminal(&self) -> bool {
        self.tty
    }
    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        panic!("accounts token runs no `claude auth status` (under {login:?})")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _dir: Option<&str>) -> Result<i32, String> {
        panic!("accounts token runs no `claude auth login`")
    }
    fn now(&self) -> i64 {
        i64::try_from(NOW).unwrap()
    }
    fn on_macos(&self) -> bool {
        self.macos
    }
    fn verb_env(&self) -> Vec<(OsString, OsString)> {
        self.env.clone()
    }
    fn token_vault(&self) -> Option<Box<dyn TokenVault>> {
        Some(match self.vault.clone()? {
            VaultOf::Fake(fake) => Box::new(fake),
            VaultOf::Real(paths) => Box::new(Vault(paths)),
            VaultOf::Contended(vault) => Box::new(vault),
        })
    }
    fn setup_token_program(&self) -> Option<(Program, String)> {
        self.asked_program.set(true);
        Some((self.program.clone()?, "2.1.318".to_owned()))
    }
    fn token_person(&self) -> Option<Box<dyn Person>> {
        Some(Box::new(self.person.clone()?))
    }
    fn token_hang(&self) -> Duration {
        aterm_spec::wait::MUST_HAPPEN
    }
    fn token_write_wait(&self) -> Option<Duration> {
        self.write_wait
    }
    fn session_logins(&self, _env: &Env) -> Vec<SessionLogin> {
        self.logins.clone()
    }
    fn driven_over_the_fleet(&self) -> bool {
        self.fleet
    }
}

/// What a run said: success, stdout, stderr.
struct Ran {
    ok: bool,
    out: String,
    err: String,
}

/// Runs `accounts token <name>` in `mode`, as `aterm harness` routes it, under the test keyer;
/// asserts nothing it printed holds the sentinel.
fn token_run(scratch: &Scratch, host: &Host, name: &str, mode: TokenMode) -> Ran {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let form = AccountsCmd::Token {
        name: name.to_owned(),
        mode,
    };
    let code = journal::with_keyer(keyer, || {
        run_accounts(&form, &scratch.env(), host, &mut out, &mut err)
    });
    let ran = Ran {
        ok: format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
        out: String::from_utf8_lossy(&out).into_owned(),
        err: String::from_utf8_lossy(&err).into_owned(),
    };
    let token = sentinel();
    for (what, text) in [("stdout", &ran.out), ("stderr", &ran.err)] {
        assert!(!text.contains(&token), "{what} holds the token");
        assert!(
            !text.contains(&token[..24]),
            "{what} holds part of the token"
        );
    }
    for line in host.said() {
        assert!(
            !line.contains(&token[..24]),
            "the person was told the token"
        );
    }
    ran
}

fn notes() -> Vec<journal::Note> {
    NOTED.with(|noted| std::mem::take(&mut *noted.borrow_mut()))
}

/// A scratch seated vault: m22, its server, its seat in the log.
pub(in crate::harness) struct Lab {
    _root: aterm_tempfile::TempDir,
    _server: aterm_secrets::server::Running,
    pub(in crate::harness) paths: Paths,
}

impl Lab {
    pub(in crate::harness) fn new() -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-accounts-token-vault-")
            .suffix(".noindex")
            .tempdir()
            .expect("the vault's root");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("server"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .expect("the server");
        let url = format!("http://{}/v1/test", server.addr);
        let paths = Paths::for_home(&root.path().join("m22-home"));
        aterm_secrets::client::create(&paths, &url, None, Some("m22")).expect("create");
        aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).expect("roster");
        aterm_secrets::client::seed_vault_genesis(&paths, &root.path().join("test.genesis"))
            .expect("genesis");
        aterm_secrets::client::admin_mirror_log(&paths, &[]).expect("the log");
        assert_eq!(
            aterm_secrets::client::write_route(&paths, Instant::now() + Duration::from_secs(60))
                .expect("the route"),
            WriteRoute::Seated,
            "PRECONDITION: m22 is seated in its log"
        );
        Self {
            _root: root,
            _server: server,
            paths,
        }
    }

    /// The account's token as the vault reads it, or why not.
    fn read(&self) -> Lookup<claude_token::TokenRead> {
        claude_token::read(&self.paths, &key(), aterm_spec::wait::MUST_HAPPEN).expect("a read")
    }

    fn holds_the_token(&self) -> bool {
        matches!(self.read(), Lookup::Found(read)
            if read.token().is_some_and(|t| t.as_slice() == sentinel().as_bytes()))
    }
}

/// THE VERB PARSES, FLAGS INCLUDED: a name and at most one of `--replace` and `--remove`, Claude
/// Code's alone.
#[test]
fn the_verb_parses_its_flags() {
    let parse = |line: &str| {
        let words: Vec<String> = line.split(' ').map(str::to_owned).collect();
        crate::harness::cli::parse(&words).map(|(cmd, _)| cmd)
    };
    for (line, mode) in [
        ("accounts token work", TokenMode::Make),
        ("accounts token work --replace", TokenMode::Replace),
        ("accounts token a@b.example --remove", TokenMode::Remove),
    ] {
        let Ok(Cmd::Accounts(AccountsCmd::Token { mode: parsed, .. })) = parse(line) else {
            panic!("{line} does not parse as the verb");
        };
        assert_eq!(parsed, mode, "{line}");
    }
    for refused in [
        "accounts token work --replace --remove",
        "accounts token",
        "accounts token a b",
        "accounts --replace",
        "accounts token work --cli codex",
    ] {
        assert!(parse(refused).is_err(), "{refused} parses");
    }
}

/// EACH REFUSAL COMES BEFORE ANY SPAWN: no terminal, a Mac not seated, a route
/// in the environment, a Mac other than macOS, and a bare run on a token that is active, soon,
/// exposed, unverified minted on another Mac, or a write here not yet in vault, each refuses
/// with its own words, and neither a program nor a write is asked for.
#[test]
fn every_refusal_comes_before_any_spawn() {
    let scratch = Scratch::new("refused");
    struct Case {
        name: &'static str,
        host: Host,
        says: &'static str,
    }
    let fake = |found: Answer| FakeVault::new(found);
    let not_seated = FakeVault {
        route: WriteRoute::NotSeated,
        ..fake(Answer::Absent)
    };
    let pending = FakeVault {
        pending: true,
        ..fake(Answer::Absent)
    };
    let cases = [
        Case {
            name: "no terminal",
            host: Host {
                tty: false,
                ..Host::new(VaultOf::Fake(fake(Answer::Absent)))
            },
            says: "a person at this Mac's terminal",
        },
        // A terminal another Mac's drive typed into is that Mac's, not a person's here.
        Case {
            name: "another Mac's drive",
            host: Host {
                fleet: true,
                ..Host::new(VaultOf::Fake(fake(Answer::Absent)))
            },
            says: "a tab another Mac's drive started or typed into",
        },
        Case {
            name: "not seated",
            host: Host::new(VaultOf::Fake(not_seated)),
            says: "not seated in the vault's log",
        },
        Case {
            name: "a route",
            host: Host {
                env: vec![("ANTHROPIC_BASE_URL".into(), "http://gw.example:1".into())],
                ..Host::new(VaultOf::Fake(fake(Answer::Absent)))
            },
            says: "routes Claude Code through `ANTHROPIC_BASE_URL`",
        },
        Case {
            name: "off macOS",
            host: Host {
                macos: false,
                ..Host::new(VaultOf::Fake(fake(Answer::Absent)))
            },
            says: "only on macOS",
        },
        Case {
            name: "unknown",
            host: Host::new(VaultOf::Fake(fake(Answer::Unknown))),
            says: "could not be read just now",
        },
        Case {
            name: "active",
            host: Host::new(VaultOf::Fake(fake(active()))),
            says: "is active until 2028-01-15",
        },
        Case {
            name: "soon",
            host: Host::new(VaultOf::Fake(fake(standing(
                Check::Readable,
                NOW + 10 * 86_400,
                true,
                false,
                &[],
            )))),
            says: "it ends within 30 days",
        },
        Case {
            name: "exposed",
            host: Host::new(VaultOf::Fake(fake(standing(
                Check::Readable,
                NOW + A_YEAR,
                true,
                false,
                &["m9"],
            )))),
            says: "exposed (removed m9) and valid",
        },
        Case {
            name: "unverified, minted on another Mac",
            host: Host::new(VaultOf::Fake(fake(standing(
                Check::Readable,
                NOW + A_YEAR,
                false,
                false,
                &[],
            )))),
            says: "made on m7, serving that Mac until it is verified",
        },
        Case {
            name: "unverified, minted here",
            host: Host::new(VaultOf::Fake(fake(standing(
                Check::Readable,
                NOW + A_YEAR,
                false,
                true,
                &[],
            )))),
            says: "unverified, serving this Mac",
        },
        Case {
            name: "a write here not yet in vault",
            host: Host::new(VaultOf::Fake(pending)),
            says: "made here is not yet in vault",
        },
    ];
    for case in cases {
        let VaultOf::Fake(vault) = case.host.vault.clone().unwrap() else {
            unreachable!()
        };
        let ran = token_run(&scratch, &case.host, "work", TokenMode::Make);
        assert!(!ran.ok, "{}: {}", case.name, ran.out);
        assert!(ran.err.contains(case.says), "{}: {}", case.name, ran.err);
        assert!(
            !case.host.asked_program.get(),
            "{}: a program was asked",
            case.name
        );
        assert!(vault.writes.borrow().is_empty(), "{}: a write", case.name);
    }
    assert!(scratch.table().rows.is_empty());
}

/// An email no session here is logged into and no row names is refused, and so is a row with no
/// email, both before any spawn.
#[test]
fn an_account_the_verb_cannot_name_is_refused() {
    let scratch = Scratch::new("unnamed");
    let host = Host::new(VaultOf::Fake(FakeVault::new(Answer::Absent)));
    let ran = token_run(&scratch, &host, "nobody@example.com", TokenMode::Make);
    assert!(
        ran.err.contains("no session on this Mac is logged into"),
        "{}",
        ran.err
    );
    let ran = token_run(&scratch, &host, "bare", TokenMode::Make);
    assert!(ran.err.contains("bare names no email"), "{}", ran.err);
    let ran = token_run(&scratch, &host, "missing", TokenMode::Make);
    assert!(
        ran.err.contains("no Claude Code row is labelled missing"),
        "{}",
        ran.err
    );
    assert!(!host.asked_program.get());
}

/// A HELD_BY ROW IS ACCEPTED when named on the verb, and the outcome names the Mac that holds it;
/// `--replace` passes an active token.
#[test]
fn a_held_row_is_accepted_and_the_outcome_names_its_holder() {
    let scratch = Scratch::new("held");
    let world = World::new("success", false);
    let fake = FakeVault::new(active());
    let host = Host::minting(VaultOf::Fake(fake.clone()), &world);
    let ran = token_run(&scratch, &host, "lent", TokenMode::Replace);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(
        ran.out
            .contains("the token serves lent on m7, the Mac that holds it"),
        "{}",
        ran.out
    );
    let writes = fake.writes.borrow();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].email, "lent@example.com");
    assert_eq!(writes[0].minted_on, "m22");
    let _ = notes();
}

/// A ROW-LESS EMAIL A SESSION HERE IS LOGGED INTO IS ACCEPTED, and its row records the account id
/// the session's login record names.
#[test]
fn a_row_less_email_a_session_here_is_on_is_accepted() {
    let scratch = Scratch::new("rowless");
    let world = World::new("success", false);
    let fake = FakeVault::new(Answer::Absent);
    let host = Host {
        logins: vec![SessionLogin {
            email: "solo@example.com".to_owned(),
            account_id: Some("c-0123456789ab".to_owned()),
            default: true,
        }],
        ..Host::minting(VaultOf::Fake(fake.clone()), &world)
    };
    let ran = token_run(&scratch, &host, "Solo@Example.com", TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert_eq!(fake.writes.borrow()[0].account_id, "c-0123456789ab");
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.label, None);
    assert_eq!(row.account_id.as_deref(), Some("c-0123456789ab"));
    let _ = notes();
}

/// A TOKEN THAT ENDS WITHIN A DAY STORES NOTHING: no write, no row, no note.
#[test]
fn a_token_under_a_day_stores_nothing() {
    let scratch = Scratch::new("short");
    let world = World::new("short", false);
    let fake = FakeVault::new(Answer::Absent);
    let host = Host::minting(VaultOf::Fake(fake.clone()), &world);
    let _ = notes();
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(!ran.ok);
    assert!(
        ran.err
            .contains("Claude Code made a token that ends within a day; nothing was stored"),
        "{}",
        ran.err
    );
    assert!(fake.writes.borrow().is_empty());
    assert!(scratch.table().rows.is_empty());
    assert!(notes().is_empty());
}

/// WITH THE STAND-IN AND A SCRATCH VAULT, THE TOKEN IS STORED: `claude_token::read` returns it,
/// the table's row holds every owner-named field (and nothing of the token), the journal holds
/// one value-free `token-minted` note, and a bare re-run is refused.
#[test]
fn a_minted_token_is_in_the_vault_and_its_row_holds_every_field() {
    let scratch = Scratch::new("stored");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host {
        logins: vec![SessionLogin {
            email: EMAIL.to_owned(),
            account_id: Some("c-0123456789ab".to_owned()),
            default: true,
        }],
        ..Host::minting(VaultOf::Real(lab.paths.clone()), &world)
    };
    let _ = notes();
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(
        ran.out.contains(
            "stored a Claude Code token for person@example.com in the vault, valid for 1 year"
        ),
        "{}",
        ran.out
    );
    assert!(lab.holds_the_token(), "the vault returns the token");

    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.key, KEY);
    assert_eq!(row.email, EMAIL);
    assert_eq!(row.account_id.as_deref(), Some("c-0123456789ab"));
    assert_eq!(row.label.as_deref(), Some("work"));
    assert_eq!(row.entry, key().entry());
    let minted_at = row.minted_at.expect("minted_at");
    assert_eq!(row.expires_at, Some(minted_at + A_YEAR - 43_200));
    assert_eq!(row.expiry_from.as_deref(), Some("printed"));
    assert_eq!(row.minted_on.as_deref(), Some("m22"));
    assert_eq!(row.signer.as_deref(), Some("m22"));
    assert_eq!(row.used_on, UsedOn::default());
    assert_eq!(row.vault, InVault::InVault);
    let bytes = std::fs::read(scratch.store().shared_dir().join(TABLE_FILE)).expect("the table");
    let token = sentinel();
    assert!(
        !bytes
            .windows(16)
            .any(|w| token.as_bytes().windows(16).any(|t| t == w)),
        "the table holds part of the token"
    );

    let noted = notes();
    assert_eq!(noted.len(), 1, "{noted:?}");
    assert_eq!(noted[0].kind, "token-minted");
    assert_eq!(noted[0].why, Some("printed"));
    assert_eq!(
        noted[0].account.map(|k| k.as_str().to_owned()).as_deref(),
        Some(KEY)
    );
    assert!(!format!("{noted:?}").contains(&token[..16]));

    let again = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(
        again.err.contains("unverified, serving this Mac"),
        "{}",
        again.err
    );
}

/// A SIGN-IN'S OWN RUN MAKES THE ACCOUNT'S TOKEN, ONCE: for an account with no token in use
/// here, the run that signed it in goes on to the bare verb, and the vault holds the token. An
/// account whose token stands is left alone with nothing said or spawned, as is every run the
/// verb itself would refuse without news: nobody at a terminal, a Mac other than macOS, a
/// routed terminal, a vault this Mac does not write, a write still waiting.
#[test]
fn a_sign_in_makes_the_accounts_token_only_where_none_is_in_use() {
    let after = |scratch: &Scratch, host: &Host| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = journal::with_keyer(keyer, || {
            after_sign_in(&scratch.env(), host, "work", &mut out, &mut err)
        });
        let said = String::from_utf8_lossy(&out).into_owned() + &String::from_utf8_lossy(&err);
        assert!(!said.contains(&sentinel()[..24]), "it printed the token");
        (code.map(|code| format!("{code:?}")), said)
    };
    let scratch = Scratch::new("after-sign-in");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    let (code, said) = after(&scratch, &host);
    assert_eq!(code, Some(format!("{:?}", ExitCode::SUCCESS)), "{said}");
    assert!(
        said.contains("work: it has no year-long token in use here"),
        "{said}"
    );
    assert!(
        said.contains("stored a Claude Code token for person@example.com"),
        "{said}"
    );
    assert!(lab.holds_the_token());

    // The token made is in use here: the next sign-in makes none and says nothing.
    host.asked_program.set(false);
    assert_eq!(after(&scratch, &host), (None, String::new()));
    assert!(!host.asked_program.get());

    let fake = |found: Answer| FakeVault::new(found);
    let absent = || VaultOf::Fake(fake(Answer::Absent));
    for (what, host) in [
        (
            "nobody at a terminal",
            Host {
                tty: false,
                ..Host::minting(absent(), &world)
            },
        ),
        (
            "not macOS",
            Host {
                macos: false,
                ..Host::minting(absent(), &world)
            },
        ),
        (
            "a routed terminal",
            Host {
                env: vec![("ANTHROPIC_BASE_URL".into(), "http://gw.example:1".into())],
                ..Host::minting(absent(), &world)
            },
        ),
        (
            "a vault this Mac does not write",
            Host::minting(
                VaultOf::Fake(FakeVault {
                    route: WriteRoute::NotSeated,
                    ..fake(Answer::Absent)
                }),
                &world,
            ),
        ),
        (
            "a write still waiting",
            Host::minting(
                VaultOf::Fake(FakeVault {
                    pending: true,
                    ..fake(Answer::Absent)
                }),
                &world,
            ),
        ),
        (
            "an active token",
            Host::minting(VaultOf::Fake(fake(active())), &world),
        ),
    ] {
        let scratch = Scratch::new("after-sign-in-none");
        assert_eq!(after(&scratch, &host), (None, String::new()), "{what}");
        assert!(!host.asked_program.get(), "{what}");
    }
}

/// A SIGN-IN MADE WITH NO BROWSER WINDOW IS OFFERED THE STEP, AND NOTHING RUNS: no browser
/// session is known to be the account's, so its run makes no token. Where the account's token
/// is due the step is said once, with nothing spawned and nothing in the vault; where the
/// bare verb would refuse without news (nobody at a terminal, a token in use) nothing is said.
#[test]
fn a_sign_in_with_no_browser_window_is_offered_the_token_step_and_makes_none() {
    let offer = |scratch: &Scratch, host: &Host| {
        let mut out = Vec::new();
        journal::with_keyer(keyer, || {
            offer_after_sign_in(&scratch.env(), host, "work", &mut out);
        });
        String::from_utf8(out).expect("UTF-8")
    };
    let scratch = Scratch::new("offer-after-sign-in");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    let said = offer(&scratch, &host);
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(
        said.starts_with("work: it has no year-long token in use here")
            && said.contains("`aterm harness accounts token work`"),
        "{said}"
    );
    assert!(!host.asked_program.get(), "the offer spawned Claude Code");
    assert!(!lab.holds_the_token(), "the offer made a token");

    let absent = || VaultOf::Fake(FakeVault::new(Answer::Absent));
    for (what, host) in [
        (
            "nobody at a terminal",
            Host {
                tty: false,
                ..Host::minting(absent(), &world)
            },
        ),
        (
            "an active token",
            Host::minting(VaultOf::Fake(FakeVault::new(active())), &world),
        ),
    ] {
        let scratch = Scratch::new("offer-after-sign-in-none");
        assert_eq!(offer(&scratch, &host), "", "{what}");
        assert!(!host.asked_program.get(), "{what}");
    }
}

#[test]
fn token_required_standing_waits_for_its_native_vault_writer() {
    for age in [Duration::from_secs(3), Duration::from_secs(61)] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let scratch = Scratch::new("standing-writer");
        let lab = Lab::new();
        let world = World::new("success", false);
        let first = Cell::new(true);
        let held = Rc::new(RefCell::new(None));
        let release = Rc::clone(&held);
        let observed = Rc::new(Cell::new(0));
        let blocked = Rc::clone(&observed);
        let paths = lab.paths.clone();
        let host = Host::minting(
            VaultOf::Contended(ContendedVault {
                paths: lab.paths.clone(),
                before_read: Rc::new(move |read, by| {
                    if read != VaultRead::Standing || !first.replace(false) {
                        return by;
                    }
                    // The real route preflight has finished before this writer takes custody.
                    *release.borrow_mut() =
                        Some(aterm_secrets::client::test_hold_state_lock(&paths).unwrap());
                    native_writer_is_busy(&paths);
                    let by = by - age;
                    if Instant::now() < by {
                        blocked.set(blocked.get() + 1);
                        drop(release.borrow_mut().take());
                    }
                    by
                }),
            }),
            &world,
        );
        let table = scratch.store().shared_dir().join(TABLE_FILE);
        let before = std::fs::read(&table).ok();
        let _ = notes();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = journal::with_keyer(keyer, || {
            run(
                &scratch.env(),
                &host,
                "work",
                TokenMode::Make,
                &mut out,
                &mut err,
            )
        });
        assert_eq!(held.borrow().is_some(), expired);
        assert_eq!(observed.get(), usize::from(!expired));
        drop(held.borrow_mut().take());
        assert_eq!(
            Vault(lab.paths.clone())
                .route(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
            WriteRoute::Seated
        );
        assert_eq!(host.asked_program.get(), !expired);
        let written = notes();
        if expired {
            assert_eq!(result, ExitCode::from(1));
            assert_eq!(std::fs::read(&table).ok(), before);
            assert!(written.is_empty());
            assert!(matches!(lab.read(), Lookup::Absent));
            assert!(!claude_token::waiting_here(&lab.paths, &key()).unwrap());
        } else {
            assert_eq!(
                result,
                ExitCode::SUCCESS,
                "{} {}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            );
            assert!(lab.holds_the_token());
            assert_eq!(
                scratch.table().rows.get(KEY).unwrap().vault,
                InVault::InVault
            );
            assert_eq!(written.len(), 1);
        }
    }
}

#[test]
fn token_preflight_waits_for_its_native_vault_writer() {
    for age in [Duration::from_secs(3), Duration::from_secs(61)] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let scratch = Scratch::new("preflight-writer");
        let lab = Lab::new();
        let world = World::new("success", false);
        let held = Rc::new(RefCell::new(Some(
            aterm_secrets::client::test_hold_state_lock(&lab.paths).unwrap(),
        )));
        let observed = Rc::new(Cell::new(0));
        let release = Rc::clone(&held);
        let blocked = Rc::clone(&observed);
        let paths = lab.paths.clone();
        let host = Host::minting(
            VaultOf::Contended(ContendedVault {
                paths: lab.paths.clone(),
                before_read: Rc::new(move |read, by| {
                    if read == VaultRead::Route && Instant::now() < by {
                        native_writer_is_busy(&paths);
                        assert!(release.borrow().is_some());
                        blocked.set(blocked.get() + 1);
                        drop(release.borrow_mut().take());
                    }
                    by
                }),
            }),
            &world,
        );
        let table = scratch.store().shared_dir().join(TABLE_FILE);
        let before = std::fs::read(&table).ok();
        let _ = notes();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = journal::with_keyer(keyer, || {
            run_with_clock(
                &scratch.env(),
                &host,
                "work",
                TokenMode::Make,
                &mut out,
                &mut err,
                || Instant::now() - age,
            )
        });
        assert_eq!(
            held.borrow().is_some(),
            expired,
            "only the actual native contention releases the writer"
        );
        drop(held.borrow_mut().take());
        assert_eq!(
            Vault(lab.paths.clone())
                .route(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
                .unwrap(),
            WriteRoute::Seated
        );
        assert_eq!(host.asked_program.get(), !expired);
        let written = notes();
        if expired {
            assert_eq!(code, ExitCode::from(1));
            assert_eq!(
                observed.get(),
                0,
                "the expired actual wait never claims remaining time"
            );
            assert!(String::from_utf8_lossy(&err).contains("vault state is busy"));
            assert_eq!(std::fs::read(&table).ok(), before);
            assert!(written.is_empty());
            assert!(!lab.holds_the_token());
            assert!(!claude_token::waiting_here(&lab.paths, &key()).unwrap());
        } else {
            assert_eq!(
                code,
                ExitCode::SUCCESS,
                "{} {}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            );
            assert!(observed.get() > 0);
            assert!(lab.holds_the_token());
            assert_eq!(
                scratch.table().rows.get(KEY).unwrap().vault,
                InVault::InVault
            );
            assert_eq!(written.len(), 1);
            assert_eq!(written[0].kind, "token-minted");
        }
        for text in [&out, &err] {
            assert!(!String::from_utf8_lossy(text).contains(&sentinel()[..24]));
        }
    }
}

/// A VAULT PASS HOLDING THE CLAIM ACROSS THE SUCCESS FRAME: the write waits, then lands in the
/// vault's waiting writes (`Waiting{LockDeadline}`, "not yet in vault"); a bare re-run is then
/// refused, and after the next pass the vault returns the token.
#[test]
fn a_write_a_pass_holds_off_lands_waiting_and_the_next_pass_stores_it() {
    let scratch = Scratch::new("waiting");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host {
        write_wait: Some(Duration::from_secs(1)),
        ..Host::minting(VaultOf::Real(lab.paths.clone()), &world)
    };
    let claim = aterm_secrets::client::test_hold_pass_claim(&lab.paths)
        .expect("the claim")
        .expect("no pass holds it");
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(ran.out.starts_with("not yet in vault: "), "{}", ran.out);
    assert!(
        ran.out.contains(waits_words(WaitFor::LockDeadline)),
        "{}",
        ran.out
    );
    assert!(
        host.said()
            .iter()
            .any(|l| l == "waiting for the vault (a vault pass is running)"),
        "{:?}",
        host.said()
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        row.vault,
        InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned())
    );
    assert!(row.vault.not_yet());
    assert!(claude_token::waiting_here(&lab.paths, &key()).expect("waiting"));
    assert!(!lab.holds_the_token(), "nothing is in the vault yet");

    let again = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(
        again.err.contains("made here is not yet in vault"),
        "{}",
        again.err
    );

    drop(claim);
    aterm_secrets::client::sync(&lab.paths, None).expect("the next pass");
    assert!(lab.holds_the_token(), "the next pass stored the token");
    let _ = notes();
}

/// CTRL-C DURING THAT WAIT LANDS THE WRITE IN THE WAITING WRITES AT ONCE, long before the hang
/// detector, and the token is kept for the next pass.
#[test]
fn ctrl_c_during_the_wait_lands_the_write_waiting() {
    let scratch = Scratch::new("ctrl-c");
    let lab = Lab::new();
    let world = World::new("success", false);
    let mut host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    if let Some(person) = &mut host.person {
        person.stops = true;
    }
    let claim = aterm_secrets::client::test_hold_pass_claim(&lab.paths)
        .expect("the claim")
        .expect("no pass holds it");
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(ran.out.starts_with("not yet in vault: "), "{}", ran.out);
    assert!(claude_token::waiting_here(&lab.paths, &key()).expect("waiting"));
    drop(claim);
    aterm_secrets::client::sync(&lab.paths, None).expect("the next pass");
    assert!(lab.holds_the_token());
    let _ = notes();
}

/// --REMOVE DELETES BOTH NAMES AND UPDATES THE ROW: the vault holds no entry of the account, the
/// row says `removed` with when, and one value-free `token-removed` note.
#[test]
fn remove_deletes_both_names_and_updates_the_row() {
    let scratch = Scratch::new("remove");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(lab.holds_the_token());
    let _ = notes();

    let removed = token_run(&scratch, &host, EMAIL, TokenMode::Remove);
    assert!(removed.ok, "{} {}", removed.out, removed.err);
    assert!(
        removed
            .out
            .starts_with("removed the Claude Code token for person@example.com from the vault"),
        "{}",
        removed.out
    );
    assert!(
        matches!(lab.read(), Lookup::Absent),
        "the vault holds neither name"
    );
    let Lookup::Found(listed) = claude_token::listed(&lab.paths).expect("listed") else {
        panic!("the vault could not be listed");
    };
    assert!(listed.is_empty(), "{listed:?}");
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.vault, InVault::Removed);
    assert_eq!(row.removed_at, Some(NOW));
    assert!(row.minted_at.is_some(), "the row keeps what it knew");
    let noted = notes();
    assert_eq!(
        noted.iter().map(|n| n.kind).collect::<Vec<_>>(),
        ["token-removed"]
    );

    let again = token_run(&scratch, &host, EMAIL, TokenMode::Remove);
    assert!(
        again.out.contains("the vault holds no Claude Code token"),
        "{}",
        again.out
    );
}

/// FOR EACH OFFER STATE, THE OFFERED STEP PASSES THE VERB'S PREFLIGHT: the step's words parse as
/// the verb, and the verb run in that state gets as far as asking for the program (none here). The
/// states that offer nothing offer nothing.
#[test]
fn every_offered_step_passes_the_preflight() {
    let scratch = Scratch::new("offers");
    let rejected_row = |store: &Store| {
        update_table(store, |table| {
            table.rows.insert(
                KEY.to_owned(),
                TokenRow {
                    key: KEY.to_owned(),
                    email: EMAIL.to_owned(),
                    account_id: None,
                    label: Some("work".to_owned()),
                    entry: key().entry(),
                    minted_at: Some(NOW - 86_400),
                    expires_at: Some(NOW + A_YEAR),
                    expiry_from: Some("printed".to_owned()),
                    minted_on: Some("m7".to_owned()),
                    signer: Some("m7".to_owned()),
                    used_on: UsedOn::default(),
                    vault: InVault::InVault,
                    verified: Some(NOW - 3_600),
                    verification: Verification::default(),
                    rejected_at: Some(NOW - 60),
                    removed_at: None,
                },
            );
        })
        .expect("the row");
    };
    let offering = [
        ("none", Answer::Absent, false),
        (
            "ended",
            standing(Check::Readable, NOW - 1, true, false, &[]),
            false,
        ),
        ("rejected", active(), true),
        (
            "mismatched (draft)",
            standing(
                Check::Mismatched(Mismatch::Draft),
                NOW + A_YEAR,
                true,
                false,
                &[],
            ),
            false,
        ),
        (
            "mismatched (value)",
            standing(
                Check::Mismatched(Mismatch::Value),
                NOW + A_YEAR,
                true,
                false,
                &[],
            ),
            false,
        ),
        (
            "unreadable here",
            standing(Check::Unreadable, NOW + A_YEAR, true, false, &[]),
            false,
        ),
        (
            "soon",
            standing(Check::Readable, NOW + 86_400, true, false, &[]),
            false,
        ),
        (
            "exposed",
            standing(Check::Readable, NOW + A_YEAR, true, false, &["m9"]),
            false,
        ),
    ];
    for (name, found, rejected) in offering {
        let store = scratch.store();
        if rejected {
            rejected_row(&store);
        }
        let fake = FakeVault::new(found);
        let row = read_table(&store).unwrap().rows.remove(KEY);
        let state = TokenState::of(
            &fake
                .standing(&key(), Instant::now() + STANDING_LOOK)
                .unwrap(),
            row.as_ref(),
            NOW,
        );
        assert_eq!(
            state.words().split(' ').next(),
            name.split(' ').next(),
            "{name}"
        );
        let step = state
            .offer("work", false)
            .unwrap_or_else(|| panic!("{name} offers"));
        let line = step.to_string();
        let words: Vec<String> = line.split(' ').skip(2).map(str::to_owned).collect();
        let Ok((Cmd::Accounts(form @ AccountsCmd::Token { .. }), _)) =
            crate::harness::cli::parse(&words)
        else {
            panic!("{name}: `{line}` does not parse as the verb");
        };
        let host = Host::new(VaultOf::Fake(fake.clone()));
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = journal::with_keyer(keyer, || {
            run_accounts(&form, &scratch.env(), &host, &mut out, &mut err)
        });
        let err = String::from_utf8_lossy(&err);
        assert!(
            host.asked_program.get(),
            "{name}: `{line}` was refused: {err}"
        );
        assert!(
            err.contains("no Claude Code build is in atpkg's store"),
            "{name}: {err}"
        );
        assert!(
            state.offer("work", true).is_none(),
            "{name}: offered while pending"
        );
        if rejected {
            update_table(&store, |table| {
                table.rows.clear();
            })
            .unwrap();
        }
    }
    for (name, found) in [
        ("unknown", Answer::Unknown),
        ("active", active()),
        (
            "unverified",
            standing(Check::Readable, NOW + A_YEAR, false, true, &[]),
        ),
        (
            "unverified elsewhere",
            standing(Check::Readable, NOW + A_YEAR, false, false, &[]),
        ),
    ] {
        let fake = FakeVault::new(found);
        let state = TokenState::of(
            &fake
                .standing(&key(), Instant::now() + STANDING_LOOK)
                .unwrap(),
            None,
            NOW,
        );
        assert_eq!(state.offer("work", false), None, "{name}");
    }
}

/// THE TABLE ROUND-TRIPS every field, and a row whose key is not a lab key is left out.
#[test]
fn the_table_round_trips() {
    let mut table = Table::default();
    let row = TokenRow {
        key: KEY.to_owned(),
        email: EMAIL.to_owned(),
        account_id: Some("c-0123456789ab".to_owned()),
        label: Some("work".to_owned()),
        entry: key().entry(),
        minted_at: Some(NOW),
        expires_at: Some(NOW + A_YEAR),
        expiry_from: Some("assumed".to_owned()),
        minted_on: Some("m22".to_owned()),
        signer: Some("m22".to_owned()),
        used_on: UsedOn {
            first_served_at: Some(NOW + 1),
            last_served_at: Some(NOW + 2),
            peers: vec!["m7".to_owned()],
            peers_read_at: Some(NOW + 3),
        },
        vault: InVault::Overtaken("m7".to_owned()),
        verified: Some(NOW + 4),
        verification: Verification {
            basis: Some((
                NOW + 1,
                Windows {
                    week_reset: Some(1_800_400_000),
                    session_reset: None,
                },
            )),
            first_reading: Some(Windows {
                week_reset: Some(1_800_400_000),
                session_reset: Some(1_800_010_000),
            }),
            verdict: Some(Verdict::Equal),
        },
        rejected_at: None,
        removed_at: Some(NOW + 6),
    };
    table.rows.insert(KEY.to_owned(), row.clone());
    table.offers.insert(
        KEY.to_owned(),
        Offered {
            pair: "none:-/ending:2027-01-15".to_owned(),
            at: NOW + 5,
        },
    );
    let text = table.to_json();
    assert_eq!(Table::from_json(text.as_bytes()), Some(table));
    let foreign = text.replace(KEY, "not-a-key");
    assert_eq!(Table::from_json(foreign.as_bytes()), Some(Table::default()));
    assert_eq!(Table::from_json(b"{\"schema\":2,\"rows\":[]}"), None);
    for event in [
        TokenEvent::Minted,
        TokenEvent::Served,
        TokenEvent::Verified,
        TokenEvent::Rejected,
        TokenEvent::Fallback,
        TokenEvent::Offered,
        TokenEvent::Removed,
    ] {
        let note = journal::token_note(event, journal::Key::new(KEY).unwrap(), NOW, None);
        assert!(journal::KINDS.contains(&note.kind), "{}", note.kind);
    }
}

/// THE ACCOUNT'S LOCK HOLDS OFF A SECOND TAB: while another run holds `tokens/<key>.lock`, the
/// verb is refused before the vault is looked at; once it is let go, the same run passes it.
#[test]
fn a_second_tab_on_the_account_is_refused() {
    let scratch = Scratch::new("second-tab");
    let store = scratch.store();
    let tokens = store.shared_dir().join(TOKENS_DIR);
    store.ensure().expect("the store");
    aterm_types::fs_restricted::ensure_private_dir(&tokens).expect("the tokens folder");
    let held = accounts::lock_at_within(&tokens.join(format!("{KEY}.lock")), &[])
        .expect("the lock opens")
        .expect("no tab holds it");
    let host = Host::new(VaultOf::Fake(FakeVault::new(Answer::Absent)));
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(
        ran.err
            .contains("a token for person@example.com is being made in another tab"),
        "{}",
        ran.err
    );
    assert!(!host.asked_program.get());
    drop(held);
    let ran = token_run(&scratch, &host, "work", TokenMode::Make);
    assert!(host.asked_program.get(), "{}", ran.err);
}

/// A REFUSAL IS THIS MAC'S RECORD (DESIGN-claude-setup-token S10, §5.6): the first records
/// `rejected_at` (a row made from the vault's meta where this Mac kept none) and journals
/// `token-rejected` once; the token then reads `rejected` here; the row's stored-login state and
/// the vault are untouched. A newer token in the vault clears the old refusal, and its own is
/// recorded afresh.
#[test]
fn a_refusal_marks_the_token_rejected_here_once() {
    let scratch = Scratch::new("rejected");
    let store = scratch.store();
    let Answer::Found(standing) = active() else {
        panic!("a standing");
    };
    let _ = notes();
    assert!(note_rejected(&store, &standing, NOW).expect("recorded"));
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(row.rejected_at, Some(NOW));
    assert_eq!(row.vault, InVault::InVault);
    assert_eq!(row.removed_at, None);
    assert_eq!(row.email, EMAIL);
    let found = Lookup::Found((*standing).clone());
    assert_eq!(
        TokenState::of(&found, scratch.table().rows.get(KEY), NOW + 60),
        TokenState::Rejected
    );
    assert!(!note_rejected(&store, &standing, NOW + 60).expect("read"));
    assert_eq!(
        scratch.table().rows.get(KEY).and_then(|r| r.rejected_at),
        Some(NOW),
        "the first refusal stands"
    );
    assert_eq!(
        notes().iter().map(|n| n.kind).collect::<Vec<_>>(),
        ["token-rejected"]
    );

    let mut newer = (*standing).clone();
    if let Some(meta) = newer.meta.as_mut() {
        meta.minted_at = NOW;
    }
    assert_eq!(
        TokenState::of(
            &Lookup::Found(newer.clone()),
            scratch.table().rows.get(KEY),
            NOW + 60
        ),
        TokenState::of(&Lookup::Found(newer.clone()), None, NOW + 60),
        "a newer token is not the refused one"
    );
    assert!(note_rejected(&store, &newer, NOW + 120).expect("recorded"));
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        (row.minted_at, row.rejected_at),
        (Some(NOW), Some(NOW + 120))
    );
}

#[path = "accounts_tokens_served_tests.rs"]
mod served;

/// THE REPORT'S ROW AND ITS VAULT TOKENS SECTION SAY ONE THING OF A ROW'S LOGIN (the report once
/// printed `not-enrolled` beside `login: idle` for a row and `login ok` for the same row under
/// vault tokens). Both read [`accounts::login_here`] over the report's own rows: a row whose
/// login ended says `signed-out` in both places; one whose login was never made on this Mac is
/// `signed-out` in the row and `no login here` under vault tokens, and neither says a signed-in
/// word; a row signed in here says a signed-in word in both. Negative control: the row signed in
/// here is the one neither place calls signed out.
#[test]
fn the_reports_row_and_its_vault_tokens_section_agree_on_each_login() {
    let scratch = Scratch::new("login-agree");
    let logins = scratch.path().join("logins");
    for made in ["made", "ended"] {
        std::fs::create_dir_all(logins.join(made)).unwrap();
        std::fs::write(logins.join(made).join(".claude.json"), "{}").unwrap();
    }
    let rows: Vec<accounts::Row> = ["made", "unmade", "ended"]
        .iter()
        .map(|label| accounts::Row {
            label: (*label).to_owned(),
            email: Some(format!("{label}@example.com")),
            dir: Some(logins.join(label).to_str().unwrap().to_owned()),
            ..accounts::Row::default()
        })
        .collect();
    std::fs::write(
        scratch.path().join("accounts.toml"),
        accounts::append_rows_for(Cli::Claude, "", &rows).unwrap(),
    )
    .unwrap();
    let env = scratch.env();
    scratch
        .store()
        .transact_within(&accounts::LOCK_LADDER, env.now, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "ended",
                accounts::Auth::SignedOut,
                env.now,
            ))
        })
        .unwrap();
    let host = Host::new(VaultOf::Fake(FakeVault::new(Answer::Absent)));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    // Each account its own lab key, as the lab gives them.
    let code = journal::with_keyer(
        |_, email| {
            let n = match email.split('@').next()? {
                "made" => 1,
                "unmade" => 2,
                _ => 3,
            };
            Some(format!("ac-{n:016x}"))
        },
        || {
            run_accounts(
                &AccountsCmd::Report { json: false },
                &env,
                &host,
                &mut out,
                &mut err,
            )
        },
    );
    let (out, err) = (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    );
    assert_eq!(code, std::process::ExitCode::SUCCESS, "{out}{err}");
    let (rows, tokens) = out
        .split_once("vault tokens:\n")
        .unwrap_or_else(|| panic!("the report ends with its vault tokens section: {out}"));
    let word_after = |line: &str, lead: &str| {
        let rest = &line[line
            .find(lead)
            .unwrap_or_else(|| panic!("{lead:?} in {line:?}"))..];
        rest[lead.len()..]
            .split([',', ';', ' ', '\n'])
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let said = |label: &str| {
        let row = rows
            .lines()
            .find(|line| line.split_whitespace().next() == Some(label))
            .unwrap_or_else(|| panic!("{label}'s row: {out}"));
        let token = tokens
            .lines()
            .find(|line| line.starts_with(&format!("  {label} (")))
            .unwrap_or_else(|| panic!("{label}'s vault tokens line: {out}"));
        (
            row.to_owned(),
            word_after(row, "login: "),
            word_after(token, "; login "),
        )
    };
    // A login that ended here is signed out in both places.
    let (row, in_row, in_tokens) = said("ended");
    assert!(row.contains("signed-out"), "{row}");
    assert_eq!(in_row, accounts::LoginHere::SIGNED_OUT, "{row}");
    assert_eq!(in_tokens, in_row, "ended: {out}");
    // A login never made here: the row says it is not enrolled and signed out, and the vault
    // tokens line that this Mac holds no login of it; neither says a signed-in word.
    let row = rows
        .lines()
        .find(|line| line.split_whitespace().next() == Some("unmade"))
        .unwrap_or_else(|| panic!("unmade's row: {out}"));
    assert!(row.contains("not-enrolled"), "{row}");
    assert_eq!(
        word_after(row, "login: "),
        accounts::LoginHere::SIGNED_OUT,
        "{row}"
    );
    let token = tokens
        .lines()
        .find(|line| line.starts_with("  unmade ("))
        .unwrap_or_else(|| panic!("unmade's vault tokens line: {out}"));
    assert!(token.contains("; no login here"), "unmade: {out}");
    let (row, in_row, in_tokens) = said("made");
    assert!(["ok", "idle"].contains(&in_row.as_str()), "{row}");
    assert_eq!(in_tokens, "ok", "NEGATIVE CONTROL: {out}");
}
