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
use crate::harness::setup_token::{OpenerRoute, OpenerWaiter};
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

/// The roster: `work` (an email), `m7-2` (an email, held by m7; its label no word of its
/// address, so a line that printed it is told apart from one that printed the address), `bare`
/// (no email).
const ROSTER: &str = "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \
                      \"person@example.com\"\n\n[[account]]\nlabel = \"m7-2\"\nclass = \
                      \"dedicated\"\ndir = \"/logins/lent\"\nemail = \"lent@example.com\"\n\
                      held_by = \"m7\"\n\n[[account]]\nlabel = \"bare\"\nclass = \
                      \"dedicated\"\ndir = \"/logins/bare\"\n";

pub(in crate::harness) fn keyer(_realm: &str, _email: &str) -> Option<String> {
    Some(KEY.to_owned())
}

pub(in crate::harness) fn key() -> Key {
    Key::parse(KEY).expect("a key")
}

/// How a message names the test account (`EMAIL`'s) with no lab: its name ([`Named::shown`]).
pub(in crate::harness) const SHOWN: &str = "claude person@example.com";

/// The word `word` (a slug or a key), as a step names an account by it.
pub(in crate::harness) fn word_of(word: &str) -> Word {
    Named {
        pk: word.to_owned(),
        row: None,
        names: aterm_file::AccountNames::default(),
    }
    .word()
}

/// The resolver's key of the test account (`EMAIL`'s): the word its steps name it by with no lab,
/// whatever keyer the journal has, and one the verb takes.
pub(in crate::harness) fn person_pk() -> String {
    account_names::names_of_login(Cli::Claude, None, EMAIL, None)
        .expect("a key")
        .0
}

/// The step that names the test account (`EMAIL`'s) with no lab, as a line quotes it.
pub(in crate::harness) fn work_step() -> String {
    format!("`aterm harness accounts token {}`", person_pk())
}

/// Whether `label` is a word of `text`: a run of the label grammar's bytes, as SF-07H's exit
/// test reads a printed label (`account_names`' tests).
pub(in crate::harness) fn says_label(text: &str, label: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .map(|word| word.trim_end_matches('.'))
        .any(|word| word == label)
}

/// The test account `work` as the verb resolves it with no lab: its row's label, name and word.
pub(super) fn work_account() -> Account {
    Account {
        email: EMAIL.to_owned(),
        label: Some("work".to_owned()),
        shown: SHOWN.to_owned(),
        word: word_of(&person_pk()),
        account_id: Some("c-0123456789ab".to_owned()),
        key: key(),
        holder: None,
    }
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

    pub(in crate::harness) fn path(&self) -> &Path {
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

    pub(in crate::harness) fn table(&self) -> Table {
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

/// The vault, stood in for: a route, a standing, whether a write here still waits, what a
/// verification answers (`None`: refused), what a write answers (`None`: posted), the standing
/// each read after the first answers (`None`: the same: what the vault holds once a capture ran),
/// the standing it answers once a write was asked (`None`: that one, or, where `lands`, one of the
/// token written, made from `found`'s), and the writes, standing reads and removals it was asked
/// for, and the key of each account a standing read or a removal was asked of (`asked`).
#[derive(Clone)]
pub(in crate::harness) struct FakeVault {
    route: WriteRoute,
    pub(in crate::harness) found: Answer,
    pub(in crate::harness) pending: bool,
    restamped: Option<MemberWritten>,
    written: Option<MemberWritten>,
    captured: Option<Answer>,
    landed: Option<Answer>,
    lands: bool,
    writes: Rc<RefCell<Vec<Meta>>>,
    reads: Rc<Cell<u32>>,
    removes: Rc<Cell<u32>>,
    pub(in crate::harness) asked: Rc<RefCell<Vec<String>>>,
}

impl FakeVault {
    pub(in crate::harness) fn new(found: Answer) -> Self {
        Self {
            route: WriteRoute::Seated,
            found,
            pending: false,
            restamped: None,
            written: None,
            captured: None,
            landed: None,
            lands: false,
            writes: Rc::default(),
            reads: Rc::default(),
            removes: Rc::default(),
            asked: Rc::default(),
        }
    }
}

impl TokenVault for FakeVault {
    fn route(&self, _by: Instant) -> Result<WriteRoute, String> {
        Ok(self.route)
    }
    fn standing(&self, key: &Key, _by: Instant) -> Result<Lookup<Standing>, String> {
        self.asked.borrow_mut().push(key.as_str().to_owned());
        let first = self.reads.get() == 0;
        self.reads.set(self.reads.get() + 1);
        let written = self.writes.borrow().last().cloned();
        let found = match (written, &self.landed, &self.found) {
            (Some(_), Some(landed), _) => landed,
            (Some(meta), None, Answer::Found(standing)) if self.lands => {
                return Ok(Lookup::Found(Standing {
                    meta: Some(meta),
                    ..(**standing).clone()
                }));
            }
            (None, _, _) if first => &self.found,
            (Some(_) | None, _, _) => self.captured.as_ref().unwrap_or(&self.found),
        };
        Ok(match found {
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
        Ok(self.written.clone().unwrap_or(MemberWritten::Posted))
    }
    fn remove(&self, key: &Key, _by: Instant) -> Result<MemberUnset, String> {
        self.asked.borrow_mut().push(key.as_str().to_owned());
        self.removes.set(self.removes.get() + 1);
        Ok(MemberUnset::Posted)
    }
    fn verify(
        &self,
        _key: &Key,
        _minted_at: u64,
        _evidence: Verified,
        _on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        Ok(self.restamped.clone().unwrap_or_else(|| {
            MemberWritten::Refused("the fake vault verifies nothing".to_owned())
        }))
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
            minted_by: None,
            verified: verified.then(|| Verified {
                on: "m7".to_owned(),
                at: NOW - 3_600,
                by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
                seven_day_resets_at: None,
                five_hour_resets_at: None,
            }),
        }),
        check,
        signer: Some(if here { "m22" } else { "m7" }.to_owned()),
        minted_here: here,
        exposed: exposed.iter().map(|m| (*m).to_owned()).collect(),
        refused_at: None,
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
        minted_at: u64,
        evidence: Verified,
        on_busy: OnBusy,
    ) -> Result<MemberWritten, String> {
        self.real().verify(key, minted_at, evidence, on_busy)
    }
    fn post(&self) -> Result<bool, String> {
        self.real().post()
    }
}

/// What the verb's sign-in says, heard for the test: lets the stand-in go once it runs, tells it
/// the window opened, and, where `stops`, presses Ctrl-C once the verb says it waits for the
/// vault.
#[derive(Clone)]
pub(in crate::harness) struct TestPerson {
    home: PathBuf,
    said: Arc<Mutex<Vec<String>>>,
    stops: bool,
}

impl TestPerson {
    fn say(&self, line: &str) {
        if line.contains("sign-in window opened") {
            std::fs::write(self.home.join("told"), "").expect("told");
        }
        self.said.lock().unwrap().push(line.to_owned());
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

/// The verb's sign-in in a test: aterm's own waiter on the window's files, heard by a
/// [`TestPerson`].
struct TestSignIn<'a> {
    person: TestPerson,
    waiter: OpenerWaiter<'a>,
}

impl SignIn for TestSignIn<'_> {
    fn say(&self, line: &str) {
        self.person.say(line);
    }

    fn code(&self, cancelled: &AtomicBool) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
        self.waiter.code(cancelled)
    }

    fn stopped(&self) -> bool {
        self.person.stopped()
    }

    fn failed(&self) -> Option<crate::harness::open_url::MetWord> {
        self.waiter.failed()
    }
}

/// The host the verb runs on: every token seam injected, nothing of this Mac's read.
pub(in crate::harness) struct Host {
    tty: bool,
    macos: bool,
    /// Whether this Mac's desk opens aterm's sign-in window (no SSH session).
    desk: bool,
    env: Vec<(OsString, OsString)>,
    vault: Option<VaultOf>,
    program: Option<Program>,
    pub(in crate::harness) asked_program: Cell<bool>,
    person: Option<TestPerson>,
    pub(in crate::harness) logins: Vec<SessionLogin>,
    /// This aterm's survey rows ([`AccountsHost::local_survey`]): none unless a test gives some.
    pub(in crate::harness) survey: Option<Vec<String>>,
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
            desk: true,
            env: Vec::new(),
            vault: Some(vault),
            program: None,
            asked_program: Cell::new(false),
            person: None,
            logins: Vec::new(),
            survey: None,
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
    fn opens_sign_in_window(&self) -> bool {
        crate::harness::cli::opens_sign_in_window_by(self.macos && self.desk, &|_| None, &|| {
            self.fleet
        })
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
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
    fn token_sign_in<'a>(&self, route: &'a OpenerRoute) -> Box<dyn SignIn + 'a> {
        let waiter = OpenerWaiter::new(route);
        match self.person.clone() {
            Some(person) => Box::new(TestSignIn { person, waiter }),
            None => Box::new(waiter),
        }
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
    fn local_survey(&self, _env: &Env) -> Option<Vec<String>> {
        self.survey.clone()
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
        Self::seated(None)
    }

    /// The scratch seated vault of a Mac whose home is `home` (its server and genesis kept in a
    /// scratch root of their own): a fixture's own home, read as `seat.home` is.
    pub(in crate::harness) fn in_home(home: &Path) -> Self {
        Self::seated(Some(home))
    }

    fn seated(home: Option<&Path>) -> Self {
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
        let paths =
            Paths::for_home(&home.map_or_else(|| root.path().join("m22-home"), Path::to_path_buf));
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
    for (line, mode, named) in [
        (
            "accounts token claude-ada-x",
            TokenMode::Make,
            "claude-ada-x",
        ),
        (
            "accounts token ac-0123456789abcdef --replace",
            TokenMode::Replace,
            "ac-0123456789abcdef",
        ),
        (
            "accounts token claude a@b.example --remove",
            TokenMode::Remove,
            "claude a@b.example",
        ),
        ("accounts token a@b.example", TokenMode::Make, "a@b.example"),
    ] {
        let Ok(Cmd::Accounts(AccountsCmd::Token { mode: parsed, name })) = parse(line) else {
            panic!("{line} does not parse as the verb");
        };
        assert_eq!((parsed, name.as_str()), (mode, named), "{line}");
    }
    for refused in [
        "accounts token claude-ada-x --replace --remove",
        "accounts token",
        "accounts token a b",
        "accounts token claude a@b.example claude-ada-x",
        "accounts --replace",
        "accounts token claude-ada-x --cli codex",
    ] {
        assert!(parse(refused).is_err(), "{refused} parses");
    }
    // Two accounts named: the refusal counts the words and repeats none, since a word may be an
    // address and the refusal reaches captured stderr.
    let Err(why) = parse("accounts token claude a@b.example claude-ada-x") else {
        unreachable!("refused above");
    };
    assert!(why.contains("not 2 words"), "{why}");
    assert!(
        !why.contains("a@b.example") && !why.contains("claude-ada-x"),
        "{why}"
    );
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
            name: "no desk for the window",
            host: Host {
                desk: false,
                ..Host::new(VaultOf::Fake(fake(Answer::Absent)))
            },
            says: "opens only at this Mac's own desktop",
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
            says: "made on m7, awaiting verification there",
        },
        Case {
            name: "unverified, minted here, with no row of it here",
            host: Host::new(VaultOf::Fake(fake(standing(
                Check::Readable,
                NOW + A_YEAR,
                false,
                true,
                &[],
            )))),
            says: "unverified (awaiting a reading of its stored login here)",
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
        let ran = token_run(&scratch, &case.host, EMAIL, TokenMode::Make);
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

/// AN ACCOUNT THE VERB CANNOT NAME IS REFUSED, before any spawn: an address no session here is
/// on and no row holds, a roster label (`work`'s, whose row has an address, and `bare`'s, whose
/// has none), and a word that names nothing; and an account only the lab names, which no session
/// here is on and no row here holds.
#[test]
fn an_account_the_verb_cannot_name_is_refused() {
    let scratch = Scratch::new("unnamed");
    let host = Host::new(VaultOf::Fake(FakeVault::new(Answer::Absent)));
    for word in ["nobody@example.com", "work", "bare", "missing"] {
        let ran = token_run(&scratch, &host, word, TokenMode::Make);
        assert!(!ran.ok, "{word}: {}", ran.out);
        assert!(
            ran.err.contains(&format!("no account is named `{word}`")),
            "{word}: {}",
            ran.err
        );
    }
    let mut lab = crate::harness::alab_roster::Lab::default();
    lab.names.insert(
        "ac-0123456789abcdef".to_owned(),
        aterm_file::AccountNames {
            realm: "claude".to_owned(),
            slug: "claude-ada-x".to_owned(),
            name: Some("claude ada@x.example".to_owned()),
            email: Some("ada@x.example".to_owned()),
            login: None,
            live: true,
        },
    );
    let ran = crate::harness::alab_roster::with_lab(lab, || {
        token_run(&scratch, &host, "claude-ada-x", TokenMode::Make)
    });
    assert!(
        ran.err.contains(
            "no session on this Mac is on ac-0123456789abcdef (claude-ada-x), and no roster row \
             here holds it"
        ),
        "{}",
        ran.err
    );
    assert!(!ran.err.contains("ada@x.example"), "{}", ran.err);
    // An account of another pool only: the verb is Claude Code's alone, so no `--cli` takes it.
    let mut lab = crate::harness::alab_roster::Lab::default();
    lab.names.insert(
        "ac-00000000000000e5".to_owned(),
        aterm_file::AccountNames {
            realm: "codex".to_owned(),
            slug: "oai-eve-v".to_owned(),
            name: Some("oai eve@v.example".to_owned()),
            email: Some("eve@v.example".to_owned()),
            login: None,
            live: true,
        },
    );
    for word in ["oai-eve-v", "oai:eve@v.example"] {
        let ran = crate::harness::alab_roster::with_lab(lab.clone(), || {
            token_run(&scratch, &host, word, TokenMode::Make)
        });
        assert!(
            ran.err.contains(&format!(
                "`{word}` is an OpenAI account: a token is made only for a Claude Code account"
            )),
            "{word}: {}",
            ran.err
        );
        assert!(!ran.err.contains("--cli"), "{word}: {}", ran.err);
    }
    assert!(!host.asked_program.get());
}

/// A HELD_BY ROW IS ACCEPTED when named on the verb, and the outcome names the Mac that holds it
/// and the account by its word (its key with no lab, its slug under one), never by the row's
/// label; `--replace` passes an active token.
/// NEGATIVE CONTROL (2026-11-16, by hand, then restored): with `held_note` naming the row by its
/// label, `the token serves m7-2 on m7` fails.
#[test]
fn a_held_row_is_accepted_and_the_outcome_names_its_holder() {
    let scratch = Scratch::new("held");
    let world = World::new("success", false);
    let (pk, _) =
        account_names::names_of_login(Cli::Claude, None, "lent@example.com", None).expect("a key");
    let mut lab = crate::harness::alab_roster::Lab::default();
    lab.names.insert(
        pk.clone(),
        aterm_file::AccountNames {
            realm: "claude".to_owned(),
            slug: "claude-lent-example".to_owned(),
            name: Some("claude lent@example.com".to_owned()),
            email: Some("lent@example.com".to_owned()),
            login: None,
            live: true,
        },
    );
    for (under, word) in [(None, pk.as_str()), (Some(&lab), "claude-lent-example")] {
        let fake = FakeVault::new(active());
        let host = Host::minting(VaultOf::Fake(fake.clone()), &world);
        let run = || {
            token_run(
                &scratch,
                &host,
                "claude lent@example.com",
                TokenMode::Replace,
            )
        };
        let ran = match under {
            Some(lab) => crate::harness::alab_roster::with_lab(lab.clone(), run),
            None => run(),
        };
        assert!(ran.ok, "{word}: {} {}", ran.out, ran.err);
        assert!(
            ran.out.contains(&format!(
                "the token serves {word} on m7, the Mac that holds it"
            )),
            "{}",
            ran.out
        );
        assert!(
            ran.out
                .contains("stored a Claude Code token for claude lent@example.com"),
            "{}",
            ran.out
        );
        for text in [&ran.out, &ran.err] {
            assert!(!says_label(text, "m7-2"), "the row's label: {text}");
        }
        let writes = fake.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].email, "lent@example.com");
        assert_eq!(writes[0].minted_on, "m22");
    }
    let _ = notes();
}

/// A ROW-LESS EMAIL A SESSION HERE IS LOGGED INTO IS ACCEPTED AS SPELLED, and its row records the
/// account id the session's login record names. The address is matched exactly, as every
/// account word is (`aterm_file::resolve`: no folded case): `Solo@Example.com` names no account,
/// where the verb before SF-07H folded its case. A decision, not an accident: the resolver is
/// the one rule every account verb and `aterm file` share, and the refusal lists nothing.
/// NEGATIVE CONTROL (2026-11-16, by hand, then restored): with the verb folding the word's case
/// before it resolves it, the folded spelling makes the token.
#[test]
fn a_row_less_email_a_session_here_is_on_is_accepted_exactly_as_spelled() {
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
    let folded = token_run(&scratch, &host, "Solo@Example.com", TokenMode::Make);
    assert!(!folded.ok, "{}", folded.out);
    assert!(
        folded
            .err
            .contains("no account is named `Solo@Example.com`"),
        "{}",
        folded.err
    );
    assert!(!host.asked_program.get(), "a folded case ran the verb");
    let ran = token_run(&scratch, &host, "solo@example.com", TokenMode::Make);
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
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
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
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(
        ran.out.contains(
            "stored a Claude Code token for claude person@example.com in the vault, valid for 1 \
             year"
        ),
        "{}",
        ran.out
    );
    assert!(lab.holds_the_token(), "the vault returns the token");
    for text in [&ran.out, &ran.err] {
        assert!(!says_label(text, "work"), "the row's label: {text}");
    }

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

    let again = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(
        again
            .err
            .contains("unverified (awaiting a reading of its stored login here)"),
        "{}",
        again.err
    );
}

/// A SIGN-IN'S OWN RUN MAKES THE ACCOUNT'S TOKEN, ONCE: for an account with no token in use
/// here, the run that signed it in goes on to the bare verb, and the vault holds the token. An
/// account whose token stands is left alone with nothing said or spawned, as is every run the
/// verb itself would refuse without news: no desk for the window, a Mac other than macOS, a
/// routed terminal, a vault this Mac does not write, a write still waiting.
#[test]
fn a_sign_in_makes_the_accounts_token_only_where_none_is_in_use() {
    let after = |scratch: &Scratch, host: &Host| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = journal::with_keyer(keyer, || {
            after_sign_in(
                &scratch.env(),
                host,
                "work",
                SignedIn::Window,
                (&mut out, &mut err),
                &mut |_| {},
            )
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
        said.contains(&format!(
            "{SHOWN}: it has no year-long token in use here, so this run makes one, as {} does",
            work_step()
        )),
        "{said}"
    );
    assert!(
        said.contains("stored a Claude Code token for claude person@example.com"),
        "{said}"
    );
    assert!(!says_label(&said, "work"), "the row's label: {said}");
    assert!(lab.holds_the_token());

    // The token made is in use here: the next sign-in makes none and says nothing.
    host.asked_program.set(false);
    assert_eq!(after(&scratch, &host), (None, String::new()));
    assert!(!host.asked_program.get());

    let fake = |found: Answer| FakeVault::new(found);
    let absent = || VaultOf::Fake(fake(Answer::Absent));
    for (what, host) in [
        (
            "no desk for the window",
            Host {
                desk: false,
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

/// A SIGN-IN'S OWN RUN NEVER REPLACES A TOKEN THE VAULT HOLDS (§4.6, §5.6): the run that signs an
/// account in, aterm's window renewal included, makes its token only where the vault holds none,
/// or only one whose end has passed. A token this Mac saw Claude Code refuse that aterm's window
/// made on another Mac (in use on every Mac, so one Mac's refusal is no other Mac's), one made
/// here at a terminal that it refused, one this Mac cannot open and one whose check failed are
/// each left alone, with nothing said, spawned or written: only a person replaces a token the
/// vault holds, and the bare verb itself refuses the first and the third. NEGATIVE CONTROL: an
/// ended token is replaced, its new one written once.
#[test]
fn a_sign_ins_own_run_never_replaces_a_token_the_vault_holds() {
    let world = World::new("success", false);
    let window_elsewhere = {
        let Answer::Found(mut found) = standing(Check::Readable, NOW + A_YEAR, false, false, &[])
        else {
            unreachable!("a standing");
        };
        if let Some(meta) = found.meta.as_mut() {
            meta.minted_by = Some(MintedBy::Window);
        }
        Answer::Found(found)
    };
    let refused = |found: Answer| match found {
        Answer::Found(standing) => Answer::Found(Box::new(Standing {
            refused_at: Some(NOW),
            ..*standing
        })),
        Answer::Absent | Answer::Unknown => unreachable!("a standing"),
    };
    let after = |scratch: &Scratch, fake: &FakeVault| {
        let host = Host::minting(VaultOf::Fake(fake.clone()), &world);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = journal::with_keyer(keyer, || {
            after_sign_in(
                &scratch.env(),
                &host,
                "work",
                SignedIn::Window,
                (&mut out, &mut err),
                &mut |_| {},
            )
        });
        let said = String::from_utf8_lossy(&out).into_owned() + &String::from_utf8_lossy(&err);
        assert!(!said.contains(&sentinel()[..24]), "it printed the token");
        (
            code.map(|code| format!("{code:?}")),
            said,
            host.asked_program.get(),
        )
    };
    for (what, found, state) in [
        (
            "rejected here, made in aterm's window on another Mac",
            refused(window_elsewhere),
            TokenState::Rejected { elsewhere: true },
        ),
        (
            "rejected here, made here at a terminal",
            refused(standing(Check::Readable, NOW + A_YEAR, false, true, &[])),
            TokenState::Rejected { elsewhere: false },
        ),
        (
            "unreadable here",
            standing(Check::Unreadable, NOW + A_YEAR, true, false, &[]),
            TokenState::Unreadable,
        ),
        (
            "mismatched",
            standing(
                Check::Mismatched(Mismatch::Value),
                NOW + A_YEAR,
                true,
                false,
                &[],
            ),
            TokenState::Mismatched(Mismatch::Value),
        ),
    ] {
        let scratch = Scratch::new("after-sign-in-held");
        let Answer::Found(held) = &found else {
            unreachable!("a standing");
        };
        let lookup = Lookup::Found((**held).clone());
        assert_eq!(
            TokenState::of(&lookup, scratch.table().rows.get(KEY), NOW),
            state,
            "{what}"
        );
        let fake = FakeVault::new(found);
        assert_eq!(
            after(&scratch, &fake),
            (None, String::new(), false),
            "{what}"
        );
        assert!(fake.writes.borrow().is_empty(), "{what}: a vault write");
    }

    // The verb typed bare refuses a token rejected here that other Macs use before anything
    // runs, and a `--replace` gets as far as the program.
    let scratch = Scratch::new("bare-run-rejected-elsewhere");
    let Answer::Found(held) = refused(active()) else {
        unreachable!("a standing");
    };
    let fake = FakeVault::new(Answer::Found(held));
    let host = Host::new(VaultOf::Fake(fake.clone()));
    let bare = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(
        !bare.ok
            && bare
                .err
                .contains("refused by Claude Code on this Mac, and in use on other Macs")
            && bare.err.contains("`--replace`"),
        "{}",
        bare.err
    );
    assert!(
        !host.asked_program.get(),
        "the bare run went past its refusal"
    );
    let replaced = token_run(&scratch, &host, EMAIL, TokenMode::Replace);
    assert!(
        replaced
            .err
            .contains("no Claude Code build is in atpkg's store"),
        "CONTROL: {}",
        replaced.err
    );
    assert!(fake.writes.borrow().is_empty());

    let scratch = Scratch::new("after-sign-in-ended");
    let fake = FakeVault::new(standing(Check::Readable, NOW - 1, true, false, &[]));
    let (code, said, asked) = after(&scratch, &fake);
    assert_eq!(
        code,
        Some(format!("{:?}", ExitCode::SUCCESS)),
        "CONTROL: {said}"
    );
    assert!(asked, "CONTROL: the run asked for the program");
    assert_eq!(fake.writes.borrow().len(), 1, "CONTROL: one write");
}

/// A BARE RUN IS REFUSED WHERE ONE MAC'S VIEW SAYS NOTHING OF THE OTHERS' (§4.6): a token this
/// Mac saw Claude Code refuse while other Macs use it, and one this Mac cannot open, are each
/// refused a bare run, with the `--replace` that makes a new one named. NEGATIVE CONTROL: a token
/// rejected here that this Mac alone uses, none, and an ended one are not refused.
#[test]
fn a_bare_run_is_refused_for_a_token_other_macs_may_use() {
    for (state, refused) in [
        (TokenState::Rejected { elsewhere: true }, true),
        (TokenState::Unreadable, true),
        (TokenState::Rejected { elsewhere: false }, false),
        (TokenState::None, false),
        (TokenState::Ended, false),
    ] {
        let said = state.bare_refusal(EMAIL, None, false);
        assert_eq!(said.is_some(), refused, "{state:?}: {said:?}");
        if let Some(said) = said {
            assert!(
                said.starts_with(&format!("a token for {EMAIL} is "))
                    && said.contains("`--replace`"),
                "{said}"
            );
        }
    }
}

/// A SIGN-IN MADE OUTSIDE THE SIGN-IN WINDOW IS OFFERED THE STEP, AND NOTHING RUNS: no browser
/// session is known to be the account's, so its run makes no token. Where the account's token
/// is due the step is said once, with nothing spawned and nothing in the vault; where the bare
/// verb would refuse without news (no desk for the window, a token in use) nothing is said. The
/// step's line shows the account's name to a person at this Mac's terminal and its word to a run
/// nobody is there for, and never the row's label.
#[test]
fn a_sign_in_outside_the_window_is_offered_the_token_step_and_makes_none() {
    let offer = |scratch: &Scratch, host: &Host| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ran = journal::with_keyer(keyer, || {
            after_sign_in(
                &scratch.env(),
                host,
                "work",
                SignedIn::Elsewhere,
                (&mut out, &mut err),
                &mut |_| {},
            )
        });
        assert!(ran.is_none() && err.is_empty(), "the offer ran the verb");
        String::from_utf8(out).expect("UTF-8")
    };
    let scratch = Scratch::new("offer-after-sign-in");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    let said = offer(&scratch, &host);
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(
        said.starts_with(&format!("{SHOWN}: it has no year-long token in use here"))
            && said.contains(&work_step()),
        "{said}"
    );
    assert!(!says_label(&said, "work"), "the row's label: {said}");
    assert!(!host.asked_program.get(), "the offer spawned Claude Code");
    assert!(!lab.holds_the_token(), "the offer made a token");
    // Nobody at this Mac's terminal (an agent's renewal): the line names the account by its word,
    // never its address, since a log or the fleet's relay of the run's output may keep it.
    let unattended = Host {
        tty: false,
        ..Host::minting(VaultOf::Real(lab.paths.clone()), &world)
    };
    let said = offer(&scratch, &unattended);
    assert!(
        said.starts_with(&format!(
            "{}: it has no year-long token in use here",
            person_pk()
        )) && said.contains(&work_step()),
        "{said}"
    );
    assert!(
        !said.contains(EMAIL),
        "nobody there, yet the address: {said}"
    );

    let absent = || VaultOf::Fake(FakeVault::new(Answer::Absent));
    for (what, host) in [
        (
            "no desk for the window",
            Host {
                desk: false,
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
                EMAIL,
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
        let pk = person_pk();
        let code = journal::with_keyer(keyer, || {
            run_with_clock(
                &scratch.env(),
                &host,
                Of::Word(&pk),
                TokenMode::Make,
                (Via::Desk, None),
                &mut out,
                &mut err,
                || Instant::now() - age,
                &mut |_| {},
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
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
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

    let again = token_run(&scratch, &host, EMAIL, TokenMode::Make);
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

/// A `--replace` WHOSE WRITE WAITS KEEPS THE VAULT'S TOKEN'S ROW (§4.6, §5.6): the vault holds the
/// account's token, found `differ` here; a `--replace` mint's write waits behind a vault pass, so
/// the account's row is the new token's, `waiting`, and the vault's token keeps its own row, its
/// verdict with it: it serves nowhere until the new token lands. NEGATIVE CONTROL: without that
/// kept row the vault's token would read as one with nothing compared, in use on its minting Mac.
#[test]
fn a_replace_whose_write_waits_keeps_the_vaults_tokens_row() {
    let scratch = Scratch::new("replace-waits");
    let lab = Lab::new();
    let old = Meta {
        email: EMAIL.to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at: NOW - 86_400,
        expires_at: NOW + A_YEAR,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    let written = claude_token::write(
        &lab.paths,
        &key(),
        &SecretBytes::from(crate::harness::setup_token::tests::other_token().as_bytes()),
        &old,
        OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the vault's token");
    assert_eq!(written, MemberWritten::Posted);
    let held = || claude_token::standing(&lab.paths, &key()).expect("a standing");
    let Lookup::Found(standing) = held() else {
        panic!("the vault holds the token");
    };
    let store = scratch.store();
    update_table(&store, |table| {
        let (row, _) = row_of_token(table, &standing, &old);
        row.label = Some("work".to_owned());
        row.verification.verdict = Some(Verdict::Differ);
    })
    .expect("the vault's token's row");
    let world = World::new("success", false);
    let host = Host {
        write_wait: Some(Duration::from_secs(1)),
        ..Host::minting(VaultOf::Real(lab.paths.clone()), &world)
    };
    let claim = aterm_secrets::client::test_hold_pass_claim(&lab.paths)
        .expect("the claim")
        .expect("no pass holds it");
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Replace);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(ran.out.starts_with("not yet in vault: "), "{}", ran.out);
    let found = held();
    assert!(
        matches!(&found, Lookup::Found(now) if now.meta.as_ref().map(|m| m.minted_at) == Some(old.minted_at)),
        "the vault still holds its token"
    );
    let table = scratch.table();
    assert!(table.rows[KEY].vault.not_yet(), "the new token's row");
    assert_ne!(table.rows[KEY].minted_at, Some(old.minted_at));
    assert_eq!(
        table.replaced.get(KEY).map(|kept| kept.minted_at),
        Some(Some(old.minted_at)),
        "the vault's token keeps its row"
    );
    let row = table.row_for(KEY, &found);
    assert_eq!(
        TokenState::of(&found, row, NOW).words(),
        "unverified (windows differ)"
    );
    assert!(!TokenState::of(&found, row, NOW).in_use());
    drop(claim);
    let _ = notes();
}

/// A `--replace` WHOSE WRITE LANDS AT ONCE KEEPS THE ROW OF THE TOKEN IT REPLACED (§5.1, §5.6):
/// the vault holds the account's token, made a day ago and found `differ` here, and a `--replace`
/// mint's write is posted, so a read of the vault once it returns names the new token. A tap that
/// read the vault's standing before the write landed judges the replaced token, and records its
/// reading against it after: that token's row is kept with its verdict, so it still serves
/// nowhere, the reading is kept apart ([`Due::Apart`]), and the account's row stays the new
/// token's. NEGATIVE CONTROL (applied to `mint`, then reverted): keeping the row of the token the
/// read after the write names, as before, keeps none, `left: None`, and that reading would be
/// compared with nothing and folded into the account's row (`Due::Nothing`).
#[test]
fn a_replace_whose_write_lands_keeps_the_row_of_the_token_it_replaced() {
    let scratch = Scratch::new("replace-lands");
    let lab = Lab::new();
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let old = Meta {
        email: EMAIL.to_owned(),
        account_id: "c-0123456789ab".to_owned(),
        minted_at: today - 86_400,
        expires_at: NOW + A_YEAR,
        expiry_from: ExpiryFrom::Printed,
        minted_on: "m22".to_owned(),
        cli: "2.1.318".to_owned(),
        minted_by: None,
        verified: None,
    };
    let written = claude_token::write(
        &lab.paths,
        &key(),
        &SecretBytes::from(crate::harness::setup_token::tests::other_token().as_bytes()),
        &old,
        OnBusy::Refuse(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("the vault's token");
    assert_eq!(written, MemberWritten::Posted);
    let held = || claude_token::standing(&lab.paths, &key()).expect("a standing");
    let Lookup::Found(before) = held() else {
        panic!("the vault holds the token");
    };
    let store = scratch.store();
    update_table(&store, |table| {
        let (row, _) = row_of_token(table, &before, &old);
        row.label = Some("work".to_owned());
        row.verification.verdict = Some(Verdict::Differ);
    })
    .expect("the vault's token's row");
    let world = World::new("success", false);
    let host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Replace);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(
        ran.out.starts_with("stored a Claude Code token"),
        "{}",
        ran.out
    );
    let Lookup::Found(landed) = held() else {
        panic!("the vault holds a token");
    };
    let new_at = landed.meta.as_ref().map(|meta| meta.minted_at);
    assert!(
        new_at.is_some_and(|at| at > old.minted_at),
        "PRECONDITION: the replacement landed"
    );
    let table = scratch.table();
    assert_eq!(table.rows[KEY].minted_at, new_at, "the new token's row");
    assert_eq!(
        table.replaced.get(KEY).map(|kept| kept.minted_at),
        Some(Some(old.minted_at)),
        "the replaced token keeps its row"
    );
    let found = Lookup::Found(before.clone());
    let state = TokenState::of(&found, table.row_for(KEY, &found), NOW);
    assert_eq!(state.words(), "unverified (windows differ)");
    assert!(!state.in_use(), "it still serves nowhere");
    let served = Served {
        label: "work".to_owned(),
        reading: Windows {
            week_reset: Some(i64::try_from(today).unwrap() + 3 * 86_400),
            session_reset: None,
        },
        others: Vec::new(),
        named: true,
    };
    assert_eq!(
        note_served(&store, &before, &served, today).expect("the table"),
        Due::Apart,
        "a reading judged against the replaced token is kept apart"
    );
    assert_eq!(
        scratch.table().rows[KEY].minted_at,
        new_at,
        "the account's row stays the new token's"
    );
    let _ = notes();
}

/// THE KEPT ROW FOLLOWS THE VAULT ONCE THE WRITE RETURNS (§5.1, §5.6): this Mac's first
/// `--replace` of the account's older token waited, so its row is the first token's and the older
/// token's row is kept. A second `--replace` reads the older token at its preflight; a vault pass
/// lands the first token and a session on it finds its windows different; the second's write
/// waits too. The row the mint keeps is that of the token the vault holds once its write
/// returned: the first token's, `differ`, which still serves nowhere. Where the first landed
/// during the second's capture and the second's write lands at once, the read after the write
/// names the second token, and the row kept is that of the token the vault held just before the
/// write: the first's again. CONTROL: where the read after a waiting write says nothing (the
/// vault could not be read just now), the row kept is that of the token the vault held before
/// the write, here the preflight's. NEGATIVE CONTROLS (each applied to `mint`, then reverted):
/// choosing the kept row by the preflight's token alone keeps the older token's row and drops the
/// first one's, `left: Some(<older>)`, and the first token would read `unverified (awaiting a
/// stored-login reading)` and serve this Mac again; for a landed write, choosing it by the token
/// the read after the write names keeps none, `left: None`, and choosing it by the preflight's
/// token rather than the read before the write keeps the older one's, `left: Some(<older>)`.
#[test]
fn the_kept_row_follows_the_vault_once_the_write_returns() {
    let Answer::Found(older) = standing(Check::Readable, NOW + A_YEAR, false, true, &[]) else {
        unreachable!()
    };
    let older_at = older.meta.as_ref().expect("a meta").minted_at;
    let mut first = (*older).clone();
    let first_at = older_at + 600;
    first.meta.as_mut().expect("a meta").minted_at = first_at;
    let account = work_account();
    let waits = || InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned());
    let second_run = |tag: &str, fake: FakeVault| {
        let scratch = Scratch::new(tag);
        let store = scratch.store();
        record(
            &store,
            &account,
            older.meta.as_ref().expect("a meta"),
            InVault::InVault,
            None,
        )
        .expect("the older token's row");
        record(
            &store,
            &account,
            first.meta.as_ref().expect("a meta"),
            waits(),
            Some(older_at),
        )
        .expect("the first replacement's row");
        // What the vault pass and a session on the first token did meanwhile: it landed, and its
        // windows were found `differ`.
        update_table(&store, |table| {
            table
                .rows
                .get_mut(KEY)
                .expect("the row")
                .verification
                .verdict = Some(Verdict::Differ);
        })
        .expect("its verdict");
        let world = World::new("success", false);
        let host = Host::minting(VaultOf::Fake(fake.clone()), &world);
        let ran = token_run(&scratch, &host, EMAIL, TokenMode::Replace);
        assert!(ran.ok, "{} {}", ran.out, ran.err);
        assert_eq!(fake.writes.borrow().len(), 1);
        let _ = notes();
        (scratch.table(), ran.out)
    };
    let waiting = |landed: Answer| FakeVault {
        written: Some(MemberWritten::Waiting(WaitFor::LockDeadline)),
        landed: Some(landed),
        ..FakeVault::new(Answer::Found(older.clone()))
    };

    let (table, out) = second_run(
        "kept-follows",
        waiting(Answer::Found(Box::new(first.clone()))),
    );
    assert!(out.starts_with("not yet in vault: "), "{out}");
    assert!(table.rows[KEY].vault.not_yet(), "the second token's row");
    assert!(
        table.rows[KEY]
            .minted_at
            .is_some_and(|at| at != first_at && at != older_at)
    );
    assert_eq!(
        table.replaced.get(KEY).and_then(|kept| kept.minted_at),
        Some(first_at),
        "the vault's token keeps its row"
    );
    let found = Lookup::Found(first.clone());
    let row = table.row_for(KEY, &found);
    let state = TokenState::of(&found, row, NOW);
    assert_eq!(state.words(), "unverified (windows differ)");
    assert!(!state.in_use(), "it still serves nowhere");

    let (table, out) = second_run(
        "kept-landed",
        FakeVault {
            captured: Some(Answer::Found(Box::new(first.clone()))),
            lands: true,
            ..FakeVault::new(Answer::Found(older.clone()))
        },
    );
    assert!(out.starts_with("stored a Claude Code token"), "{out}");
    assert_eq!(table.rows[KEY].vault, InVault::InVault);
    assert_eq!(
        table.replaced.get(KEY).and_then(|kept| kept.minted_at),
        Some(first_at),
        "the token the vault held just before the landed write keeps its row"
    );
    let row = table.row_for(KEY, &found);
    assert_eq!(
        TokenState::of(&found, row, NOW).words(),
        "unverified (windows differ)"
    );

    let (table, _) = second_run("kept-preflight", waiting(Answer::Unknown));
    assert_eq!(
        table.replaced.get(KEY).and_then(|kept| kept.minted_at),
        Some(older_at),
        "CONTROL: the token held before the write where the vault says nothing after it"
    );
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
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(ran.out.starts_with("not yet in vault: "), "{}", ran.out);
    assert!(claude_token::waiting_here(&lab.paths, &key()).expect("waiting"));
    drop(claim);
    aterm_secrets::client::sync(&lab.paths, None).expect("the next pass");
    assert!(lab.holds_the_token());
    let _ = notes();
}

/// --REMOVE DELETES BOTH NAMES AND UPDATES THE ROW: the vault holds no entry of the account, the
/// row says `removed` with when, and one value-free `token-removed` note. Removal opens no
/// window, so a person at the terminal over SSH, off macOS, runs it.
#[test]
fn remove_deletes_both_names_and_updates_the_row() {
    let scratch = Scratch::new("remove");
    let lab = Lab::new();
    let world = World::new("success", false);
    let host = Host::minting(VaultOf::Real(lab.paths.clone()), &world);
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(ran.ok, "{} {}", ran.out, ran.err);
    assert!(lab.holds_the_token());
    let _ = notes();

    let host = Host {
        desk: false,
        macos: false,
        ..Host::minting(VaultOf::Real(lab.paths.clone()), &world)
    };
    let removed = token_run(&scratch, &host, EMAIL, TokenMode::Remove);
    assert!(removed.ok, "{} {}", removed.out, removed.err);
    assert!(
        removed.out.starts_with(
            "removed the Claude Code token for claude person@example.com from the vault"
        ),
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

/// REMOVAL WITH NEITHER THE WINDOW NOR A PERSON'S TERMINAL IS REFUSED: no terminal over SSH, or
/// a tab another Mac's drive typed into, removes nothing.
#[test]
fn remove_without_window_or_terminal_is_refused() {
    let scratch = Scratch::new("remove-refused");
    for host in [
        Host {
            desk: false,
            tty: false,
            ..Host::new(VaultOf::Fake(FakeVault::new(active())))
        },
        Host {
            fleet: true,
            ..Host::new(VaultOf::Fake(FakeVault::new(active())))
        },
    ] {
        let ran = token_run(&scratch, &host, EMAIL, TokenMode::Remove);
        assert!(!ran.ok, "{}", ran.out);
        assert!(
            ran.err
                .contains("a token is removed at this Mac's own desktop"),
            "{}",
            ran.err
        );
    }
}

/// FOR EACH OFFER STATE, THE OFFERED STEP PASSES THE VERB'S PREFLIGHT: the step's words parse as
/// the verb, and the verb run in that state gets as far as asking for the program (none here): a
/// token rejected here that other Macs use is offered a `--replace`, one this Mac alone uses a
/// bare run. The states that offer nothing offer nothing, `unreadable here` among them.
#[test]
fn every_offered_step_passes_the_preflight() {
    let scratch = Scratch::new("offers");
    let refused = |found: Answer| match found {
        Answer::Found(standing) => Answer::Found(Box::new(Standing {
            refused_at: Some(NOW - 60),
            ..*standing
        })),
        Answer::Absent | Answer::Unknown => unreachable!(),
    };
    let offering = [
        ("none", Answer::Absent),
        (
            "ended",
            standing(Check::Readable, NOW - 1, true, false, &[]),
        ),
        ("rejected", refused(active())),
        (
            "rejected (made here at a terminal)",
            refused(standing(Check::Readable, NOW + A_YEAR, false, true, &[])),
        ),
        (
            "mismatched (draft)",
            standing(
                Check::Mismatched(Mismatch::Draft),
                NOW + A_YEAR,
                true,
                false,
                &[],
            ),
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
        ),
        (
            "soon",
            standing(Check::Readable, NOW + 86_400, true, false, &[]),
        ),
        (
            "exposed",
            standing(Check::Readable, NOW + A_YEAR, true, false, &["m9"]),
        ),
    ];
    for (name, found) in offering {
        let fake = FakeVault::new(found);
        let state = TokenState::of(
            &fake
                .standing(&key(), Instant::now() + STANDING_LOOK)
                .unwrap(),
            None,
            NOW,
        );
        assert_eq!(
            state.words().split(' ').next(),
            name.split(' ').next(),
            "{name}"
        );
        let step = state
            .offer(&word_of(&person_pk()), false)
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
            state.offer(&word_of(&person_pk()), true).is_none(),
            "{name}: offered while pending"
        );
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
        (
            "unreadable here",
            standing(Check::Unreadable, NOW + A_YEAR, true, false, &[]),
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
        assert_eq!(state.offer(&word_of(&person_pk()), false), None, "{name}");
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
            reading: Some(Windows {
                week_reset: Some(1_800_400_000),
                session_reset: Some(1_800_010_000),
            }),
            verdict: Some(Verdict::Shared),
            shared_with: BTreeSet::from(["other-0".to_owned(), "other-1".to_owned()]),
        },
        removed_at: Some(NOW + 6),
    };
    table.rows.insert(KEY.to_owned(), row.clone());
    table.replaced.insert(
        KEY.to_owned(),
        TokenRow {
            minted_at: Some(NOW - 86_400),
            vault: InVault::InVault,
            ..row.clone()
        },
    );
    table.offers.insert(
        KEY.to_owned(),
        Offered {
            pair: "none:-/ending:2027-01-15".to_owned(),
            at: NOW + 5,
        },
    );
    let text = table.to_json();
    let checks = table.checks_json();
    assert_eq!(
        Table::from_json(text.as_bytes()).and_then(|read| read.with_checks(checks.as_bytes())),
        Some(table.clone())
    );
    let bare = Table::from_json(text.as_bytes()).expect("a table");
    assert_eq!(
        (bare.rows[KEY].verification.clone(), bare.replaced.len()),
        (Verification::default(), 0),
        "`tokens.json` holds no check of a token"
    );
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
        TokenEvent::Differ,
        TokenEvent::Stopped,
    ] {
        let note = journal::token_note(event, journal::Key::new(KEY).unwrap(), NOW, None);
        assert!(journal::KINDS.contains(&note.kind), "{}", note.kind);
    }
}

/// A ROW KEPT WHILE THE BASIS WAS THE ROW'S USAGE `Sample` READS WITH NO VERIFICATION (§5.6): such
/// a row kept its basis and verdict in `tokens.json` under `verification`, reached against windows
/// that may be an earlier token's readings folded into the sample, so this build reads neither.
/// Its token awaits a stored-login reading, and a reading of the token showing that old basis's
/// windows decides nothing. A `differ` it kept is not kept either. CONTROL: the same basis and
/// verdict as this build keeps them, in [`CHECK_FILE`] under the row's key and `minted_at`, read
/// as they are: the token awaits a reading of itself, and that reading leaves the write due; the
/// `differ` stands.
#[test]
fn a_row_kept_while_the_basis_was_the_sample_reads_with_no_verification() {
    let Answer::Found(token) = standing(Check::Readable, NOW + A_YEAR, false, true, &[]) else {
        panic!("a standing");
    };
    let minted_at = token.meta.as_ref().expect("a meta").minted_at;
    let windows = Windows {
        week_reset: Some(1_800_200_000),
        session_reset: Some(1_800_003_600),
    };
    let check = |verdict: &str| {
        format!(
            r#"{{"basis_at":{minted_at},"basis":{{"seven_day_resets_at":1800200000,"five_hour_resets_at":1800003600}},"reading":null,"verdict":{verdict}}}"#
        )
    };
    let rows = |field: &str| {
        format!(
            r#"{{"schema":1,"rows":[{{"key":"{KEY}","email":"{EMAIL}","label":"work","entry":"{entry}","minted_at":{minted_at},"vault":"in-vault"{field}}}]}}"#,
            entry = key().entry(),
        )
    };
    for (under, verdict, why, due) in [
        (
            "verification",
            "null",
            Unverified::AwaitingBasis,
            Due::Nothing,
        ),
        (
            "verification",
            r#""differ""#,
            Unverified::AwaitingBasis,
            Due::Nothing,
        ),
        (CHECK_FILE, "null", Unverified::NoReadingYet, Due::Nothing),
        (
            CHECK_FILE,
            r#""differ""#,
            Unverified::WindowsDiffer,
            Due::Apart,
        ),
    ] {
        let case = format!("{under} {verdict}");
        let scratch = Scratch::new("sample-basis");
        let store = scratch.store();
        store.ensure().expect("the store");
        let (table, checks) = if under == CHECK_FILE {
            (
                rows(""),
                Some(format!(
                    r#"{{"schema":1,"checks":[{{"key":"{KEY}","minted_at":{minted_at},"{CHECK_KEY}":{}}}],"replaced":[]}}"#,
                    check(verdict)
                )),
            )
        } else {
            (
                rows(&format!(r#","verification":{}"#, check(verdict))),
                None,
            )
        };
        aterm_uds::private_file::write_private(&store.shared_dir(), TABLE_FILE, table.as_bytes())
            .expect("the kept table");
        if let Some(checks) = checks {
            aterm_uds::private_file::write_private(
                &store.shared_dir(),
                CHECK_FILE,
                checks.as_bytes(),
            )
            .expect("the kept checks");
        }
        assert_eq!(
            Unverified::of(scratch.table().rows.get(KEY), Some(minted_at), NOW),
            why,
            "{case}"
        );
        let served = Served {
            label: "work".to_owned(),
            reading: windows,
            others: Vec::new(),
            named: true,
        };
        let found = note_served(&store, &token, &served, NOW).expect("the table");
        if under == CHECK_FILE && verdict == "null" {
            assert!(
                matches!(found, Due::Verify(_)),
                "CONTROL: {case}: the basis compares, {found:?}"
            );
        } else {
            assert_eq!(found, due, "{case}");
        }
    }
}

/// A CHECK FILE NAMING A VERDICT THIS BUILD DOES NOT KNOW DOES NOT READ (§5.1): a later build that
/// made a new verdict word changed what the file means, whether the word is a check's or a kept
/// row's. The token made here is then held as one whose check file does not read is: its reading
/// is folded nowhere and compared with nothing, and the file is never written over. CONTROL: the
/// same file naming `differ` reads, and the reading is kept apart. NEGATIVE CONTROL (applied to
/// [`Verification::of_json`], then reverted): reading the unknown word as no verdict, as before,
/// reads the file, `left: Some(Table { .. })`, and the token would be compared again, its windows
/// the basis's, and its stamp written.
#[test]
fn a_check_file_naming_a_verdict_this_build_does_not_know_does_not_read() {
    let Answer::Found(token) = standing(Check::Readable, NOW + A_YEAR, false, true, &[]) else {
        panic!("a standing");
    };
    let minted_at = token.meta.as_ref().expect("a meta").minted_at;
    let check = |verdict: &str| {
        format!(
            r#"{{"basis_at":{minted_at},"basis":{{"seven_day_resets_at":1800200000,"five_hour_resets_at":1800003600}},"reading":null,"verdict":"{verdict}"}}"#
        )
    };
    let row = |at: u64, field: &str| {
        format!(
            r#"{{"key":"{KEY}","email":"{EMAIL}","label":"work","entry":"{entry}","minted_at":{at},"vault":"in-vault"{field}}}"#,
            entry = key().entry(),
        )
    };
    let table = format!(r#"{{"schema":1,"rows":[{}]}}"#, row(minted_at, ""));
    let checked = |verdict: &str| {
        format!(
            r#"{{"schema":1,"checks":[{{"key":"{KEY}","minted_at":{minted_at},"{CHECK_KEY}":{}}}],"replaced":[]}}"#,
            check(verdict)
        )
    };
    let kept = |verdict: &str| {
        format!(
            r#"{{"schema":1,"checks":[],"replaced":[{}]}}"#,
            row(
                minted_at - 600,
                &format!(r#","{CHECK_KEY}":{}"#, check(verdict))
            )
        )
    };
    let bare = Table::from_json(table.as_bytes()).expect("the table");
    for (of, file) in [
        ("a check's", checked("held")),
        ("a kept row's", kept("held")),
    ] {
        assert_eq!(bare.clone().with_checks(file.as_bytes()), None, "{of}");
    }
    for file in [checked("differ"), kept("differ")] {
        assert!(
            bare.clone().with_checks(file.as_bytes()).is_some(),
            "CONTROL"
        );
    }
    let windows = Windows {
        week_reset: Some(1_800_200_000),
        session_reset: Some(1_800_003_600),
    };
    let served = Served {
        label: "work".to_owned(),
        reading: windows,
        others: Vec::new(),
        named: true,
    };
    for (verdict, read) in [("held", false), ("differ", true)] {
        let scratch = Scratch::new("unknown-verdict");
        let store = scratch.store();
        store.ensure().expect("the store");
        let dir = store.shared_dir();
        aterm_uds::private_file::write_private(&dir, TABLE_FILE, table.as_bytes())
            .expect("the table");
        aterm_uds::private_file::write_private(&dir, CHECK_FILE, checked(verdict).as_bytes())
            .expect("the checks");
        let parts = read_parts(&store).expect("tokens.json reads");
        assert_eq!(parts.unchecked.is_none(), read, "{verdict}");
        let due = note_served(&store, &token, &served, NOW);
        if read {
            assert_eq!(due.expect("the table"), Due::Apart, "CONTROL");
        } else {
            assert!(due.is_err(), "{verdict}: {due:?}");
            assert_eq!(
                std::fs::read(dir.join(CHECK_FILE)).ok(),
                Some(checked(verdict).into_bytes()),
                "never written over"
            );
        }
    }
}

/// AN EARLIER BUILD'S REWRITE OF THE TABLE KEEPS EACH TOKEN'S CHECK (§5.1, §5.6): an earlier build
/// rewrites `tokens.json` whole on every `accounts` report, keeping only the fields it knows; this
/// build keeps a token's verdict and the kept row of the token the vault still holds in
/// [`CHECK_FILE`], which no earlier build opens. After such a rewrite the row's `differ` stands and
/// the kept row is still there; a row the earlier build moved to another token (its own mint)
/// reads with no check, since a check is its token's by `minted_at`. NEGATIVE CONTROL: the checks
/// kept in `tokens.json` (the layout this replaces) are gone after the rewrite.
#[test]
fn an_earlier_builds_rewrite_of_the_table_keeps_each_tokens_check() {
    let Answer::Found(token) = standing(Check::Readable, NOW + A_YEAR, false, true, &[]) else {
        panic!("a standing");
    };
    let minted_at = token.meta.as_ref().expect("a meta").minted_at;
    let scratch = Scratch::new("earlier-rewrite");
    let store = scratch.store();
    update_table(&store, |table| {
        let (row, _) = row_of_token(table, &token, token.meta.as_ref().expect("a meta"));
        row.label = Some("work".to_owned());
        row.verification.verdict = Some(Verdict::Differ);
        let mut kept = row.clone();
        kept.minted_at = Some(minted_at - 600);
        table.replaced.insert(KEY.to_owned(), kept);
    })
    .expect("the table");
    // What an earlier build writes back: its own fields of each row, nothing else.
    let earlier = |minted_at: u64| {
        format!(
            r#"{{"schema":1,"rows":[{{"key":"{KEY}","email":"{EMAIL}","label":"work","entry":"{entry}","minted_at":{minted_at},"vault":"in-vault","verification":{{"basis_at":null,"basis":null,"first_reading":null,"verdict":null}}}}],"offers":[],"posts":[]}}"#,
            entry = key().entry(),
        )
    };
    aterm_uds::private_file::write_private(
        &store.shared_dir(),
        TABLE_FILE,
        earlier(minted_at).as_bytes(),
    )
    .expect("the earlier build's rewrite");
    let table = scratch.table();
    assert_eq!(
        table.rows[KEY].verification.verdict,
        Some(Verdict::Differ),
        "the verdict stands"
    );
    assert_eq!(table.replaced[KEY].minted_at, Some(minted_at - 600));
    let found = Lookup::Found((*token).clone());
    assert_eq!(
        TokenState::of(&found, table.row_for(KEY, &found), NOW).words(),
        "unverified (windows differ)"
    );
    aterm_uds::private_file::write_private(
        &store.shared_dir(),
        TABLE_FILE,
        earlier(minted_at + 60).as_bytes(),
    )
    .expect("an earlier build's mint");
    assert_eq!(
        scratch.table().rows[KEY].verification,
        Verification::default(),
        "a check is its token's"
    );
}

/// AN EARLIER BUILD'S ROW OF THE VAULT'S TOKEN TAKES THAT TOKEN'S KEPT CHECK (§5.1, §5.6): this
/// Mac's `--replace` write waits, so the account's row is the new token's and the vault's token,
/// found `differ` here, is the kept row. An earlier build's rewrite follows the vault onto that
/// token, with none of its check: the row takes the kept row's verdict, and the token stays apart
/// (a refusal here is no row's: it is kept beside the vault state, which no rewrite of the table
/// touches). A row an earlier build moved to its own mint leaves the check of the
/// token it was of as that token's kept row, which keeps the token apart while the vault holds it.
/// NEGATIVE CONTROLS: read as the earlier build wrote it, with no check file, the vault's token
/// awaits a basis in both, and would serve on this Mac.
#[test]
fn an_earlier_builds_row_of_the_vaults_token_takes_its_kept_check() {
    let Answer::Found(token) = standing(Check::Readable, NOW + A_YEAR, false, true, &[]) else {
        panic!("a standing");
    };
    let meta = token.meta.clone().expect("a meta");
    let account = work_account();
    let earlier = |minted_at: u64| {
        format!(
            r#"{{"schema":1,"rows":[{{"key":"{KEY}","email":"{EMAIL}","label":"work","entry":"{entry}","minted_at":{minted_at},"vault":"in-vault","verification":{{"basis_at":null,"basis":null,"first_reading":null,"verdict":null}}}}],"offers":[],"posts":[]}}"#,
            entry = key().entry(),
        )
    };
    let found = Lookup::Found((*token).clone());
    let windows = Windows {
        week_reset: Some(1_800_200_000),
        session_reset: Some(1_800_003_600),
    };
    let served = Served {
        label: "work".to_owned(),
        reading: windows,
        others: Vec::new(),
        named: true,
    };
    for case in ["followed", "minted"] {
        let scratch = Scratch::new("earlier-row");
        let store = scratch.store();
        update_table(&store, |table| {
            let (row, _) = row_of_token(table, &token, &meta);
            row.label = Some("work".to_owned());
            row.verification.verdict = Some(Verdict::Differ);
        })
        .expect("the table");
        if case == "followed" {
            let new = Meta {
                minted_at: meta.minted_at + 600,
                ..meta.clone()
            };
            let waits = InVault::Waiting(waits_words(WaitFor::LockDeadline).to_owned());
            record(&store, &account, &new, waits, Some(meta.minted_at)).expect("the mint's row");
            assert_eq!(
                scratch.table().replaced[KEY].verification.verdict,
                Some(Verdict::Differ),
                "PRECONDITION: the vault's token is the kept row"
            );
        }
        let moved_to = if case == "followed" {
            meta.minted_at
        } else {
            meta.minted_at + 60
        };
        aterm_uds::private_file::write_private(
            &store.shared_dir(),
            TABLE_FILE,
            earlier(moved_to).as_bytes(),
        )
        .expect("the earlier build's rewrite");
        let table = scratch.table();
        assert_eq!(table.rows[KEY].minted_at, Some(moved_to), "{case}");
        let row = table.row_for(KEY, &found);
        assert_eq!(
            row.map(|row| row.verification.verdict),
            Some(Some(Verdict::Differ)),
            "{case}: the vault's token keeps its verdict"
        );
        if case == "followed" {
            assert!(table.replaced.is_empty(), "{case}: the kept row went");
        } else {
            assert_eq!(table.replaced[KEY].minted_at, Some(meta.minted_at));
            assert_eq!(
                TokenState::of(&found, row, NOW).words(),
                "unverified (windows differ)"
            );
        }
        assert!(!TokenState::of(&found, row, NOW).in_use(), "{case}");
        assert_eq!(
            note_served(&store, &token, &served, NOW).expect("the table"),
            Due::Apart,
            "{case}: its reading stays apart"
        );
        let unread = Table::from_json(earlier(moved_to).as_bytes()).expect("the earlier table");
        assert_eq!(
            Unverified::of(unread.row_for(KEY, &found), Some(meta.minted_at), NOW),
            Unverified::AwaitingBasis,
            "NEGATIVE CONTROL: {case}"
        );
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
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(
        ran.err.contains(&format!(
            "a token for {} is being made in another tab",
            person_pk()
        )),
        "{}",
        ran.err
    );
    assert!(!host.asked_program.get());
    drop(held);
    let ran = token_run(&scratch, &host, EMAIL, TokenMode::Make);
    assert!(host.asked_program.get(), "{}", ran.err);
}

/// A REFUSAL IS THIS MAC'S RECORD (DESIGN-claude-setup-token S10, §5.6): the first is kept beside
/// this Mac's vault state ([`aterm_secrets::claude_token::note_refused`]) and journals
/// `token-rejected` once; the vault is untouched; a standing that carries it reads `rejected`.
/// The table's row is made from the vault's meta on a Mac that kept none, so the report shows the
/// refused token's `verified`. A newer token is not the refused one: its own refusal is recorded
/// afresh, and the row follows it.
#[test]
fn a_refusal_marks_the_token_rejected_here_once() {
    let scratch = Scratch::new("rejected");
    let store = scratch.store();
    let paths = Paths::locate_for_home(&scratch.env().home.expect("a home"));
    let Answer::Found(standing) = active() else {
        panic!("a standing");
    };
    let _ = notes();
    assert!(note_rejected(&paths, &store, &standing, 401, NOW).expect("recorded"));
    assert!(
        paths.dir.join("claude-token-refusals.json").is_file(),
        "kept beside the vault state"
    );
    let row = scratch.table().rows.remove(KEY).expect("the row");
    assert_eq!(
        (row.minted_at, row.verified, row.vault, row.email.as_str()),
        (
            Some(NOW - 86_400),
            Some(NOW - 3_600),
            InVault::InVault,
            EMAIL
        )
    );
    assert!(!note_rejected(&paths, &store, &standing, 401, NOW + 60).expect("read"));
    assert_eq!(
        notes().iter().map(|n| n.kind).collect::<Vec<_>>(),
        ["token-rejected"]
    );
    let refused = Standing {
        refused_at: Some(NOW),
        ..(*standing).clone()
    };
    assert_eq!(
        TokenState::of(&Lookup::Found(refused), None, NOW + 60),
        TokenState::Rejected { elsewhere: true }
    );

    let mut newer = (*standing).clone();
    if let Some(meta) = newer.meta.as_mut() {
        meta.minted_at = NOW;
    }
    assert!(note_rejected(&paths, &store, &newer, 401, NOW + 120).expect("recorded"));
    assert_eq!(
        notes().iter().map(|n| n.kind).collect::<Vec<_>>(),
        ["token-rejected"]
    );
    assert_eq!(
        scratch.table().rows.get(KEY).and_then(|row| row.minted_at),
        Some(NOW),
        "the row follows the newer token"
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
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn the_reports_row_and_its_vault_tokens_section_agree_on_each_login() {
    let scratch = Scratch::new("login-agree");
    let logins = scratch.path().join("logins");
    for made in ["made", "ended", "token"] {
        std::fs::create_dir_all(logins.join(made)).unwrap();
        std::fs::write(logins.join(made).join(".claude.json"), "{}").unwrap();
    }
    let rows: Vec<accounts::Row> = ["made", "unmade", "ended", "token"]
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
            let mut events = Vec::new();
            for label in ["ended", "token"] {
                events.extend(accounts::note_auth(
                    state,
                    label,
                    accounts::Auth::SignedOut,
                    env.now,
                ));
            }
            Ok(events)
        })
        .unwrap();
    // The upkeep read `token`'s vault token in use here: its signed-out login is on its token.
    scratch
        .store()
        .note_tokens_here(
            Some(&[("token".to_owned(), u64::try_from(env.now).unwrap() + 86_400)].into()),
            env.now,
        )
        .unwrap();
    let host = Host::new(VaultOf::Fake(FakeVault::new(Answer::Absent)));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    // Each account its own lab key, as the lab gives them.
    let code = journal::with_keyer(
        |_, email| {
            let n = match email.split('@').next()? {
                "made" => 1,
                "unmade" => 2,
                "token" => 4,
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
    // Each row's line, by the account the report names it by.
    let row_line = |label: &str| {
        let name = format!("claude {label}@example.com ");
        rows.lines()
            .find(|line| line.starts_with(&name))
            .unwrap_or_else(|| panic!("{label}'s row: {out}"))
    };
    let said = |label: &str| {
        let row = row_line(label);
        let token = tokens
            .lines()
            .find(|line| line.starts_with(&format!("  claude {label}@example.com: ")))
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
    let row = row_line("unmade");
    assert!(row.contains("not-enrolled"), "{row}");
    assert_eq!(
        word_after(row, "login: "),
        accounts::LoginHere::SIGNED_OUT,
        "{row}"
    );
    let token = tokens
        .lines()
        .find(|line| line.starts_with("  claude unmade@example.com: "))
        .unwrap_or_else(|| panic!("unmade's vault tokens line: {out}"));
    assert!(token.contains("; no login here"), "unmade: {out}");
    let (row, in_row, in_tokens) = said("made");
    assert!(["ok", "idle"].contains(&in_row.as_str()), "{row}");
    assert_eq!(in_tokens, "ok", "NEGATIVE CONTROL: {out}");
    // A signed-out login whose row runs on its vault token here is on its token in the row, and
    // the vault tokens line says no signed-out word of it either.
    let row = row_line("token");
    assert_eq!(
        word_after(row, "login: "),
        accounts::LoginHere::ON_TOKEN,
        "{row}"
    );
    assert!(!row.contains("signed-out"), "{row}");
    let token = tokens
        .lines()
        .find(|line| line.starts_with("  claude token@example.com: "))
        .unwrap_or_else(|| panic!("token's vault tokens line: {out}"));
    assert!(
        !token.contains(accounts::LoginHere::SIGNED_OUT) && !token.contains("no login here"),
        "token: {out}"
    );
}

/// A ROW ON ITS VAULT TOKEN ASKS A PERSON FOR NO RENEWAL IN THE REPORT: `token`'s automatic
/// sign-in met its page, which left a renewal task for a person, and since then the upkeep read
/// its year token in use here. Renewal is off for it (`accounts renew token` runs nothing), so the
/// report's row says `on-token` with its token's end and no `sign-in needed:` step, and the JSON
/// row carries no sign-in task, no step, and no date to renew by, but the token's end.
/// NEGATIVE CONTROL: with no reading, the task's step and the task stand in both. Before the
/// report judged the step by the reading, it printed `login: on-token` beside `sign-in needed:
/// Run aterm harness accounts renew token in a terminal on this Mac`.
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "a launch starts on a vault token only on macOS"
)]
fn a_row_on_its_vault_token_asks_a_person_for_no_renewal_in_the_report() {
    let scratch = Scratch::new("on-token-step");
    let made = scratch.path().join("logins").join("token");
    std::fs::create_dir_all(&made).unwrap();
    std::fs::write(made.join(".claude.json"), "{}").unwrap();
    let roster_path = scratch.path().join("accounts.toml");
    let rows = [accounts::Row {
        label: "token".to_owned(),
        email: Some("token@example.com".to_owned()),
        dir: Some(made.to_str().unwrap().to_owned()),
        ..accounts::Row::default()
    }];
    std::fs::write(
        &roster_path,
        accounts::append_rows_for(Cli::Claude, "", &rows).unwrap(),
    )
    .unwrap();
    let env = scratch.env();
    let store = scratch.store();
    store
        .observe_admitted_workflow_for_test(&roster_path, "token", env.now, |_, state, logins| {
            accounts::note_sign_in_task(logins, "token", accounts::SignInAction::Renew, env.now);
            accounts::note_auth(state, "token", accounts::Auth::SignedOut, env.now)
        })
        .unwrap();
    let host = Host::new(VaultOf::Fake(FakeVault::new(Answer::Absent)));
    let report = |json: bool| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = journal::with_keyer(
            |_, _| Some(format!("ac-{:016x}", 4)),
            || {
                run_accounts(
                    &AccountsCmd::Report { json },
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
        out
    };
    let row_of = |out: &str| -> aterm_json::Value {
        let json: aterm_json::Value = aterm_json::from_str(out.trim()).expect("the report is JSON");
        json["accounts"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row["name"] == aterm_json::Value::from("claude token@example.com"))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("token's row: {out}"))
    };
    let step = &format!(
        "sign-in needed: Run aterm harness accounts renew {} in a terminal on this Mac",
        crate::harness::account_names::tests::row_name(Cli::Claude, &roster_path, "token").word
    );

    // CONTROL: signed out with no reading, the task's step stands.
    let text = report(false);
    assert!(text.contains(step), "{text}");
    let row = row_of(&report(true));
    assert!(row["sign_in_task"].is_object(), "{row}");
    assert!(row["sign_in_needed"].as_str().is_some(), "{row}");
    assert!(row["on_token_until"].is_null(), "{row}");

    let until = env.now + 86_400;
    store
        .note_tokens_here(
            Some(&[("token".to_owned(), u64::try_from(until).unwrap())].into()),
            env.now,
        )
        .unwrap();
    let text = report(false);
    let line = text
        .lines()
        .find(|line| line.contains("login: "))
        .unwrap_or_else(|| panic!("the login line: {text}"));
    assert!(
        line.contains(&format!(
            "login: {}, on its token until {}",
            accounts::LoginHere::ON_TOKEN,
            crate::harness::usage::rfc3339_utc(until)
        )),
        "{text}"
    );
    assert!(!line.contains("renew by"), "{text}");
    assert!(!text.contains("sign-in needed"), "{text}");
    let row = row_of(&report(true));
    assert_eq!(
        row["login"],
        aterm_json::Value::from(accounts::LoginHere::ON_TOKEN),
        "{row}"
    );
    assert!(row["sign_in_task"].is_null(), "{row}");
    assert!(row["sign_in_needed"].is_null(), "{row}");
    assert!(
        row["renew_by"].is_null() && row["due_from"].is_null(),
        "{row}"
    );
    assert_eq!(row["on_token_until"].as_i64(), Some(until), "{row}");
}

/// The host of a pass of the token step in aterm's sign-in window that no person started
/// (FL-136): the stand-in's host, with the window, a clock that moves, and a Claude Code build
/// that a test changes.
struct StepHost {
    token: Host,
    now: Cell<u64>,
    cli: RefCell<String>,
    /// The capture's hang detector ([`AccountsHost::token_hang`]).
    hang: Duration,
    /// Whether a person runs the verb at a terminal of their own
    /// ([`crate::harness::cli::person_at_terminal`]); nobody, unless a test says so.
    person: Cell<bool>,
}

impl AccountsHost for StepHost {
    fn stdin_is_terminal(&self) -> bool {
        self.person.get()
    }
    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        panic!("a token step runs no `claude auth status` (under {login:?})")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _dir: Option<&str>) -> Result<i32, String> {
        panic!("a token step runs no `claude auth login`")
    }
    fn now(&self) -> i64 {
        i64::try_from(self.now.get()).unwrap()
    }
    fn on_macos(&self) -> bool {
        true
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn opens_sign_in_window(&self) -> bool {
        true
    }
    fn verb_env(&self) -> Vec<(OsString, OsString)> {
        self.token.verb_env()
    }
    fn token_vault(&self) -> Option<Box<dyn TokenVault>> {
        self.token.token_vault()
    }
    fn setup_token_program(&self) -> Option<(Program, String)> {
        Some((World::program(), self.cli.borrow().clone()))
    }
    fn token_hang(&self) -> Duration {
        self.hang
    }
    fn session_logins(&self, env: &Env) -> Vec<SessionLogin> {
        self.token.session_logins(env)
    }
}

/// A TOKEN STEP A CHECK STOPPED LEAVES ONE RECORD AND WAITS FOR AN EVENT, NOT A CLOCK (plan
/// package FL-136; sitting design §3 and S1): a capture in aterm's sign-in window whose consent
/// page held Authorize behind a human check leaves one record in `tokens-steps.json` (its word,
/// when, and the builds that met it; no host, address, code or token byte) and one
/// `token-stopped` note, and posts no need. A pass a week later, or a month later, runs nothing,
/// and neither does one on a new Claude Code build: a build is no person at the check, so no
/// pass goes back to it; a person's own run goes at once, one that ends on a stop that holds
/// nothing (a browser that did not start) leaves the record as it was, and the capture that
/// makes the token clears it. The refusal says only a person's act tries it again. The pass is
/// this Mac's token upkeep's (plan package FL-138: `backfill_due`, then `backfill_run`, by
/// aterm's window with nobody there). The check is the account's too (`CheckedRow::hold_after`),
/// so the row is left to a person at every door with nobody there; once a login made for the row
/// has ended that, the step's own record alone holds every later pass.
/// NEGATIVE CONTROL: with `Stopped::again` giving a check the words of any other stop (the
/// branch's first form), the refusal promises a new build: `` with a new Claude Code or aterm
/// build ``; with `step_held` reading no record (`steps.get(..)` as `None`), or with
/// `Stopped::holds` letting a new build through a check's record (the branch's first form), the
/// pass on 2.1.319 runs, where the upkeep's record of the gap's run lets the new build through
/// too: `` a new Claude Code build went back to the check ``; with `mint` writing the record on
/// every end, the browser that did not start clears it: `` a stop that holds nothing cleared
/// the record ``.
#[test]
fn a_token_step_a_check_stopped_leaves_one_record_and_waits_for_an_event_not_a_clock() {
    use crate::harness::open_url::MetWord;
    use crate::harness::setup_token::tests::MODE_FILE;
    use crate::harness::token_steps::{self, STEPS_FILE};
    let scratch = Scratch::new("step-held");
    let lab = Lab::new();
    let world = World::new("window-human-check", false);
    std::fs::write(world.home().join("go"), b"go").unwrap();
    assert!(world.home().join(MODE_FILE).is_file());
    let host = StepHost {
        token: Host::minting(VaultOf::Real(lab.paths.clone()), &world),
        now: Cell::new(NOW),
        cli: RefCell::new("2.1.318".to_owned()),
        hang: aterm_spec::wait::MUST_HAPPEN,
        person: Cell::new(false),
    };
    let env = scratch.env();
    let store = scratch.store();
    let roster = accounts::Roster::parse(ROSTER).expect("the roster");
    let tasks = || {
        let logins = store
            .load_logins(i64::try_from(NOW).unwrap())
            .expect("the logins")
            .0;
        accounts::sign_in_tasks(&roster, &logins)
    };
    let tasks_before = tasks();
    // One pass nobody started: the step runs where it is due and no stop holds it.
    let window = NobodyRoutes {
        window: true,
        headless: false,
    };
    let pass = |host: &StepHost| -> Option<Ran> {
        journal::with_keyer(keyer, || {
            let Backfill::Due(finding) = backfill_due(&env, host, "work", false, window) else {
                return None;
            };
            assert_eq!(finding.gap, Gap::None);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = backfill_run(
                &env,
                host,
                "work",
                &finding,
                false,
                window,
                (&mut out, &mut err),
                &mut |_| {},
            )
            .expect("still due");
            Some(Ran {
                ok: format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
                out: String::from_utf8_lossy(&out).into_owned(),
                err: String::from_utf8_lossy(&err).into_owned(),
            })
        })
    };
    let record = || token_steps::read(&store).expect("the steps file reads");
    let _ = notes();

    let ran = pass(&host).expect("a step with no record is due");
    assert!(!ran.ok, "{}", ran.err);
    assert!(ran.err.contains("human check"), "{}", ran.err);
    assert!(
        ran.err.contains(
            "tried again only by a person at a terminal on this Mac, who meets the check in \
             aterm's sign-in window"
        ),
        "{}",
        ran.err
    );
    assert!(
        !ran.err.contains("with a new Claude Code or aterm build"),
        "a check's refusal promised a new build tries it again: {}",
        ran.err
    );
    assert_eq!(world.runs(), 1);
    let steps = record();
    assert_eq!(steps.len(), 1, "{steps:?}");
    let stopped = &steps[KEY];
    assert_eq!(stopped.word, MetWord::WindowHumanCheck);
    assert!(stopped.at >= NOW, "{stopped:?}");
    assert_eq!(stopped.cli, "2.1.318");
    let noted = notes();
    let kinds: Vec<_> = noted.iter().map(|note| (note.kind, note.why)).collect();
    assert_eq!(kinds, [("token-stopped", Some("human-check"))], "{noted:?}");
    // No need: no line for a person, no sign-in task, nothing beside the record.
    for text in [&ran.out, &ran.err] {
        assert!(!text.contains("NEEDS YOU"), "{text}");
    }
    assert_eq!(tasks(), tasks_before, "the stop posted a sign-in task");
    // The check is the account's too: every door with nobody there leaves the row to a person.
    let check = store
        .load_logins(i64::try_from(NOW).unwrap())
        .expect("the logins")
        .0
        .labels
        .get("work")
        .and_then(|binding| binding.check_stop.clone());
    assert!(
        check.is_some_and(|check| check.at >= i64::try_from(NOW).unwrap()),
        "the account's check was not recorded"
    );
    // A login made for the row ends the account's check, and only the step's own record holds
    // what follows: each control below is that record's alone.
    let mut logins = store
        .load_logins(i64::try_from(NOW).unwrap())
        .expect("the logins")
        .0;
    if let Some(binding) = logins.labels.get_mut("work") {
        binding.check_stop = None;
    }
    store.save_logins(&logins).expect("the login");

    // No clock: a week later, and a month later, nothing runs.
    for days in [1, 7, 30] {
        host.now.set(NOW + days * 86_400);
        assert!(
            pass(&host).is_none(),
            "a pass {days} days later ran the step"
        );
    }
    assert_eq!(world.runs(), 1, "a pass with no event reached the page");

    // A new Claude Code build is no person at the check: no pass goes back to it.
    host.cli.replace("2.1.319".to_owned());
    for days in [0, 60] {
        host.now.set(NOW + days * 86_400);
        assert!(
            pass(&host).is_none(),
            "a new Claude Code build went back to the check"
        );
    }
    assert_eq!(world.runs(), 1);
    assert_eq!(record()[KEY].cli, "2.1.318");
    assert!(notes().is_empty(), "a held pass noted something");

    // The record holds no host, address, code or token byte, and no reason line.
    let raw = std::fs::read_to_string(store.shared_dir().join(STEPS_FILE)).expect("the record");
    let token = sentinel();
    for held in [
        "://",
        "@",
        "claude.ai",
        "example.com",
        "test-code",
        "why",
        &token[..24],
    ] {
        assert!(!raw.contains(held), "the record holds {held:?}: {raw}");
    }

    // A person's own run at a terminal of their own goes at once, held or not. One that ends on
    // a stop that holds nothing leaves the record as it was, and the step held.
    let person_run = |mode: &str| {
        world.play(mode);
        let mut out = Vec::new();
        let mut err = Vec::new();
        host.person.set(true);
        let code = journal::with_keyer(keyer, || {
            run_accounts(
                &AccountsCmd::Token {
                    name: EMAIL.to_owned(),
                    mode: TokenMode::Make,
                },
                &env,
                &host,
                &mut out,
                &mut err,
            )
        });
        host.person.set(false);
        (code, out, err)
    };
    let (code, out, err) = person_run("window-browser-start");
    assert_ne!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::SUCCESS),
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    assert_eq!(world.runs(), 2);
    let steps = record();
    assert_eq!(
        steps.len(),
        1,
        "a stop that holds nothing cleared the record"
    );
    assert_eq!(steps[KEY].word, MetWord::WindowHumanCheck);
    assert_eq!(steps[KEY].cli, "2.1.318");
    assert!(
        !notes().iter().any(|note| note.kind == "token-stopped"),
        "a stop that holds nothing was noted as one"
    );
    assert!(pass(&host).is_none(), "the step is held no longer");

    // The capture that makes the token clears the record.
    let (code, out, err) = person_run("renew-window-token");
    let (out, err) = (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    );
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::SUCCESS),
        "{out}{err}"
    );
    assert!(!out.contains(&token[..24]) && !err.contains(&token[..24]));
    assert_eq!(world.runs(), 3);
    assert!(record().is_empty(), "the token's capture left the record");
    let kinds: Vec<_> = notes().iter().map(|note| note.kind).collect();
    assert_eq!(kinds, ["token-minted"]);
}

/// The account `work`'s check on record ([`accounts::CheckStop`]), by its second, read a day on
/// (a load caps every stamp at its own clock).
fn check_at(store: &accounts::Store) -> Option<i64> {
    store
        .load_logins(i64::try_from(NOW + 86_400).unwrap())
        .expect("the logins")
        .0
        .labels
        .get("work")
        .and_then(|binding| binding.check_stop.as_ref())
        .map(|check| check.at)
}

/// Ends `work`'s account check as a login made for the row does.
fn signed_in_again(store: &accounts::Store) {
    let mut logins = store
        .load_logins(i64::try_from(NOW).unwrap())
        .expect("the logins")
        .0;
    if let Some(binding) = logins.labels.get_mut("work") {
        binding.check_stop = None;
    }
    store.save_logins(&logins).expect("the login");
}

/// `accounts token person@example.com` as `aterm harness` routes it on `host`, under the test
/// keyer.
fn step_run(env: &Env, host: &StepHost, mode: TokenMode) -> Ran {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = journal::with_keyer(keyer, || {
        run_accounts(
            &AccountsCmd::Token {
                name: EMAIL.to_owned(),
                mode,
            },
            env,
            host,
            &mut out,
            &mut err,
        )
    });
    Ran {
        ok: format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
        out: String::from_utf8_lossy(&out).into_owned(),
        err: String::from_utf8_lossy(&err).into_owned(),
    }
}

/// WHO IS THERE DECIDES HOW THE TOKEN'S PAGE RUNS, NOT WHERE THE SIGN-IN RAN (sitting design §3):
/// a sign-in in aterm's window that nobody at a terminal started (this Mac's upkeep's, an
/// agent's) makes its token there with the window told nobody is there
/// (`ATERM_OPEN_URL_UNATTENDED`), so the consent page's human check ends that capture at once;
/// once a login made for the row has ended the account's check, the step's own record of the
/// check still holds the next such sign-in, whose step is offered with nothing run, and an
/// agent's run of the verb, which is told the person's step. A line of a sign-in nobody started
/// names the account by its word (its key, with no lab), never its address or its row's label.
/// CONTROL: a person's own sign-in in the window, at a terminal of theirs, makes the token
/// whatever the record says, in a window not told nobody is there, its lines naming the account
/// by its name, and the token made clears the record.
/// NEGATIVE CONTROL (2026-11-16, by hand, then restored): with `Account::named_for` showing the
/// name to every run, the window's intro line names `claude person@example.com`.
/// NEGATIVE CONTROL: with `after_sign_in` mapping every window sign-in to the person's route
/// (`SignedIn::Window => Via::Desk`, the merge's form), `` the window was not told nobody is
/// there ``; with it reading the check's record only after a headless sign-in, `` a sign-in
/// nobody started was not offered the step `` (the run's own read refuses it, so neither alone
/// lets the page load again: `` a sign-in nobody started went back to the check `` takes both);
/// with `run_with_clock` reading no check's record for a run nobody at a terminal started,
/// `` an agent's run went back to the check ``.
#[test]
fn who_is_there_decides_how_the_tokens_page_runs_not_where_the_sign_in_ran() {
    use crate::harness::open_url::MetWord;
    use crate::harness::token_steps;
    let scratch = Scratch::new("after-window-nobody");
    let lab = Lab::new();
    let world = World::new("window-human-check", false);
    let host = step_host(&lab, &world);
    let (env, store) = (scratch.env(), scratch.store());
    let after = |host: &StepHost| -> (Option<bool>, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = journal::with_keyer(keyer, || {
            after_sign_in(
                &env,
                host,
                "work",
                SignedIn::Window,
                (&mut out, &mut err),
                &mut |_| {},
            )
        });
        (
            code.map(|code| code == ExitCode::SUCCESS),
            String::from_utf8_lossy(&out).into_owned() + &String::from_utf8_lossy(&err),
        )
    };
    let told_nobody = || {
        std::fs::read_to_string(world.home().join("env"))
            .expect("the stand-in's env")
            .lines()
            .any(|line| line == "ATERM_OPEN_URL_UNATTENDED=1")
    };

    // The upkeep's sign-in in the window: its token's page meets the human check. Nobody is
    // there to read its lines, so they name the account by its word, never its address.
    let (code, said) = after(&host);
    assert_eq!(code, Some(false), "{said}");
    let intro = said.lines().next().unwrap_or_default();
    assert!(
        intro.starts_with(&format!(
            "{}: it has no year-long token in use here, so this run makes one",
            person_pk()
        )),
        "{said}"
    );
    assert!(
        !intro.contains(EMAIL),
        "nobody there, yet the address: {said}"
    );
    assert_eq!(world.runs(), 1);
    assert!(told_nobody(), "the window was not told nobody is there");
    assert_eq!(
        token_steps::read(&store).unwrap()[KEY].word,
        MetWord::WindowHumanCheck
    );
    assert!(check_at(&store).is_some(), "the account's check");

    // A login made for the row ends the account's check; the step's record still holds.
    signed_in_again(&store);
    world.play("renew-window-token");
    let (code, said) = after(&host);
    assert_eq!(
        code, None,
        "a sign-in nobody started was not offered the step: {said}"
    );
    assert!(
        said.contains(&format!("{} run by a person at this Mac", work_step())),
        "{said}"
    );
    assert!(
        said.starts_with(&format!(
            "{}: its token's consent page carried a check",
            person_pk()
        )),
        "{said}"
    );
    assert!(
        !said.contains(EMAIL),
        "nobody there, yet the address: {said}"
    );
    assert!(!says_label(&said, "work"), "the row's label: {said}");
    assert_eq!(
        world.runs(),
        1,
        "a sign-in nobody started went back to the check"
    );
    let ran = step_run(&env, &host, TokenMode::Make);
    assert_eq!(world.runs(), 1, "an agent's run went back to the check");
    assert!(!ran.ok, "{}", ran.out);
    assert!(
        ran.err
            .contains("tried again only by a person at a terminal on this Mac"),
        "{}",
        ran.err
    );

    // CONTROL: a person's own sign-in in the window makes it, in a window not told nobody is
    // there, and the token made clears the record; its lines show the account's name.
    host.person.set(true);
    let (code, said) = after(&host);
    host.person.set(false);
    assert_eq!(code, Some(true), "{said}");
    assert!(
        said.starts_with(&format!("{SHOWN}: it has no year-long token in use here")),
        "{said}"
    );
    assert_eq!(world.runs(), 2);
    assert!(!told_nobody(), "a person's window was told nobody is there");
    assert!(token_steps::read(&store).unwrap().is_empty());
}

/// ONE CHECK, ONE LIFETIME (sitting design §3): a token step's check is the account's too
/// (`CheckedRow::hold_after`), and the token a person's own run makes over the step's record,
/// having passed the very page that carried it, ends both: the step's record and the account's
/// check it wrote, so no door with nobody there holds the row for a check a person passed. A
/// check of the account met since (a sign-in's, later than the step's) is not that one and
/// stands. NEGATIVE CONTROL: with the token's success ending only the step's record (the merge's
/// form), `` the person's token left the account's check standing ``.
#[test]
fn a_token_a_person_made_over_a_checks_record_ends_the_accounts_check_too() {
    use crate::harness::token_steps;
    for later_check in [false, true] {
        let scratch = Scratch::new("step-check-ends");
        let lab = Lab::new();
        let world = World::new("window-human-check", false);
        let host = step_host(&lab, &world);
        let (env, store) = (scratch.env(), scratch.store());
        let window = NobodyRoutes {
            window: true,
            headless: false,
        };
        let Backfill::Due(finding) =
            journal::with_keyer(keyer, || backfill_due(&env, &host, "work", false, window))
        else {
            panic!("no token is due");
        };
        let code = journal::with_keyer(keyer, || {
            backfill_run(
                &env,
                &host,
                "work",
                &finding,
                false,
                window,
                (&mut Vec::new(), &mut Vec::new()),
                &mut |_| {},
            )
        });
        assert_ne!(code, Some(ExitCode::SUCCESS));
        let stopped = token_steps::read(&store).unwrap()[KEY].clone();
        let step_at = i64::try_from(stopped.at).unwrap();
        assert_eq!(check_at(&store), Some(step_at), "the account's check");
        let roster = env.accounts_roster_for(Cli::Claude).expect("the roster");
        if later_check {
            store
                .transact_pool_logins_within(
                    &[],
                    &roster,
                    step_at + 60,
                    &mut None,
                    |raw, _, logins| {
                        accounts::note_check_stop(logins, raw.row("work").unwrap(), step_at + 60);
                        Ok(Vec::new())
                    },
                )
                .unwrap();
            assert_eq!(check_at(&store), Some(step_at + 60), "the later check");
        }
        // The person's run, two minutes on.
        host.now.set(NOW + 120);
        world.play("renew-window-token");
        host.person.set(true);
        let ran = step_run(&env, &host, TokenMode::Make);
        host.person.set(false);
        assert!(ran.ok, "{}{}", ran.out, ran.err);
        assert!(token_steps::read(&store).unwrap().is_empty());
        let records = store
            .read_account_records(Some(&roster), i64::try_from(NOW + 120).unwrap())
            .unwrap();
        let held = crate::harness::accounts_sync::left_to_a_person(
            &store,
            records.raw_roster.row("work").unwrap(),
            &records.logins,
        );
        if later_check {
            assert_eq!(
                check_at(&store),
                Some(step_at + 60),
                "CONTROL: a later check of the account ended with the token's"
            );
            assert!(held);
        } else {
            assert_eq!(
                check_at(&store),
                None,
                "the person's token left the account's check standing"
            );
            assert!(
                !held,
                "the row is left to a person for a check a person passed"
            );
        }
    }
}

/// THE ONE HOLD READER FAILS CLOSED WITH NOBODY THERE (sitting design §3): where the account's
/// records do not read, what stands on them is unknown and a check may be among it, so this
/// Mac's upkeep finds no token due and an agent's run of the verb is refused, with nothing run.
/// CONTROL: a person's own run at a terminal asks nothing of the records and goes on.
/// NEGATIVE CONTROL: with `left_to_a_person_here` holding nothing on records it cannot read (the
/// merge's form), `` the upkeep found a token due on records it could not read ``.
#[test]
fn records_that_do_not_read_hold_the_token_step_with_nobody_there() {
    let scratch = Scratch::new("records-unread");
    let lab = Lab::new();
    let world = World::new("renew-window-token", false);
    let host = step_host(&lab, &world);
    let (env, store) = (scratch.env(), scratch.store());
    let window = NobodyRoutes {
        window: true,
        headless: false,
    };
    assert!(
        matches!(
            journal::with_keyer(keyer, || backfill_due(&env, &host, "work", false, window)),
            Backfill::Due(_)
        ),
        "CONTROL: due on records that read"
    );
    store.ensure().expect("the store");
    aterm_uds::private_file::write_private(&store.dir, accounts::LOGINS_FILE, b"{").unwrap();
    assert!(
        store
            .read_account_records(env.accounts_roster_for(Cli::Claude).as_deref(), 0)
            .is_err(),
        "the records still read"
    );
    assert!(
        matches!(
            journal::with_keyer(keyer, || backfill_due(&env, &host, "work", false, window)),
            Backfill::Nothing
        ),
        "the upkeep found a token due on records it could not read"
    );
    let ran = step_run(&env, &host, TokenMode::Make);
    assert!(!ran.ok, "{}", ran.out);
    assert_eq!(world.runs(), 0, "an agent's run reached the page");
}

/// A CHECK A TOKEN'S PAGE CARRIED THAT THE ACCOUNT'S RECORDS COULD NOT TAKE IS HANDED TO THE
/// CALLER (sitting design §3): where `accounts.lock` stays held past its ladder, the check is not
/// on the account, so the capture answers it for the upkeep to hold in its process until a pass
/// has written it (`AutoRenewGuard::unrecorded_checks`), and says so. CONTROL: with the lock
/// free, the check is on record and nothing is handed over.
/// NEGATIVE CONTROL: with `hold_after` writing only its line on a busy store (the merge's form),
/// `` the busy store's check was dropped ``.
#[test]
fn a_check_the_accounts_records_could_not_take_is_handed_to_the_caller() {
    use crate::harness::open_url::MetWord;
    use crate::harness::token_steps;
    let scratch = Scratch::new("check-busy");
    let (env, store) = (scratch.env(), scratch.store());
    store.ensure().expect("the store");
    let stopped = token_steps::Stopped::new(MetWord::WindowBotCheck, NOW, "2.1.318", "").unwrap();
    token_steps::put(&store, KEY, Some(&stopped)).unwrap();
    for busy in [true, false] {
        let checked = CheckedRow::before(&env, &store, (Some("work"), KEY), NOW).expect("the row");
        assert_eq!(checked.step_check_at, Some(NOW));
        let mut err = Vec::new();
        let handed = {
            let _held = busy.then(|| store.acquire().expect("the test holds accounts.lock"));
            checked.hold_after(&store, (KEY, "claude-work-x"), NOW, &mut err)
        };
        let err = String::from_utf8_lossy(&err).into_owned();
        if busy {
            let handed = handed.expect("the busy store's check was dropped");
            assert_eq!(handed.at, i64::try_from(NOW).unwrap());
            assert_eq!(handed.row.label, "work");
            assert!(
                err.contains("could not be recorded on the account"),
                "{err}"
            );
            assert_eq!(check_at(&store), None);
        } else {
            assert!(handed.is_none(), "CONTROL: {err}");
            assert!(err.is_empty(), "{err}");
            assert_eq!(check_at(&store), Some(i64::try_from(NOW).unwrap()));
        }
    }
}

/// A standing of the account's token made at `minted_at` by Claude Code `cli` to end at
/// `expires_at`, verified on m7.
fn made_by(minted_at: u64, expires_at: u64, cli: &str) -> Lookup<Standing> {
    let Answer::Found(mut found) = standing(Check::Readable, expires_at, true, false, &[]) else {
        unreachable!("a standing")
    };
    let meta = found.meta.as_mut().expect("its meta");
    meta.minted_at = minted_at;
    meta.cli = cli.to_owned();
    Lookup::Found(*found)
}

/// A SHORT-LIVED TOKEN IS NO GAP TO THE CLAUDE CODE BUILD THAT MADE IT, AND A TOKEN NEARS ITS END
/// BY ITS OWN END (plan package FL-138, [`SHORT_LIVED_SECS`]): a 30-day token Claude Code
/// 2.1.318 made, read with 20 days left on the account's home Mac, is no `soon` gap while a
/// capture would run 2.1.318 (or a build nobody knows) again, nor once it ended; it is one once a
/// capture would run 2.1.319, and its end is a gap to every Mac then. A year-long token in its
/// last 10 days is `soon` on the home Mac whatever the build and on no other Mac, unverified
/// (another Mac's window token) or handed to a removed Mac as much as verified; with more than
/// 30 days left it is no gap.
/// NEGATIVE CONTROL: with `gap_of` taking every `ended` reading for `Gap::Ended`, the 30-day
/// token's end is a gap to the build that made it (`left: Some(Ended)`); with
/// `SHORT_LIVED_SECS` three times `SOON_SECS`, a 61-day token is no gap at its end: `` a 61-day
/// token's end ``; with `gap_of` reading `soon` off [`TokenState::Soon`] alone, the unverified
/// token in its last 10 days is no gap: `` an unverified token's nearing end ``.
#[test]
fn a_short_lived_token_is_no_gap_to_the_build_that_made_it() {
    let vault = FakeVault::new(Answer::Absent);
    let gap = |state: &TokenState, found: &Lookup<Standing>, home: bool, cli: Option<&str>| {
        gap_of(state, found, &vault, (home, NOW, cli))
    };
    const DAY: u64 = 86_400;
    let (same, next) = (Some("2.1.318"), Some("2.1.319"));

    let month = made_by(NOW - 10 * DAY, NOW + 20 * DAY, "2.1.318");
    for cli in [same, Some(""), None] {
        assert_eq!(gap(&TokenState::Soon, &month, true, cli), None, "{cli:?}");
    }
    assert_eq!(gap(&TokenState::Soon, &month, true, next), Some(Gap::Soon));
    assert_eq!(gap(&TokenState::Soon, &month, false, next), None);
    let ended = made_by(NOW - 31 * DAY, NOW - DAY, "2.1.318");
    for home in [true, false] {
        assert_eq!(gap(&TokenState::Ended, &ended, home, same), None);
        assert_eq!(
            gap(&TokenState::Ended, &ended, home, next),
            Some(Gap::Ended)
        );
    }
    let longer = made_by(NOW - 62 * DAY, NOW - DAY, "2.1.318");
    assert_eq!(
        gap(&TokenState::Ended, &longer, false, same),
        Some(Gap::Ended),
        "a 61-day token's end"
    );

    let year = made_by(NOW - 355 * DAY, NOW + 10 * DAY, "2.1.318");
    let unverified = TokenState::Unverified {
        minted_here: false,
        minted_by: Some(MintedBy::Window),
        why: Unverified::NoReadingYet,
        minted_on: "m7".to_owned(),
        exposed: Vec::new(),
    };
    let exposed = TokenState::Exposed(vec!["m9".to_owned()]);
    for cli in [same, next] {
        assert_eq!(gap(&TokenState::Soon, &year, true, cli), Some(Gap::Soon));
    }
    assert_eq!(
        gap(&unverified, &year, true, same),
        Some(Gap::Soon),
        "an unverified token's nearing end"
    );
    assert_eq!(gap(&exposed, &year, true, same), Some(Gap::Soon));
    for state in [&TokenState::Soon, &unverified, &exposed] {
        assert_eq!(gap(state, &year, false, same), None, "{state:?}");
    }
    let fresh = made_by(NOW - DAY, NOW + A_YEAR, "2.1.318");
    for state in [&TokenState::Active, &unverified, &exposed] {
        assert_eq!(gap(state, &fresh, true, same), None, "{state:?}");
    }
}

/// This Mac's upkeep's pass of `work` by `routes` (`backfill_due`, then `backfill_run`), under
/// the test keyer: what it ran, `None` where nothing was due.
fn upkeep_pass(env: &Env, host: &StepHost, routes: NobodyRoutes) -> Option<Ran> {
    journal::with_keyer(keyer, || {
        let Backfill::Due(finding) = backfill_due(env, host, "work", false, routes) else {
            return None;
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = backfill_run(
            env,
            host,
            "work",
            &finding,
            false,
            routes,
            (&mut out, &mut err),
            &mut |_| {},
        )
        .expect("still due");
        Some(Ran {
            ok: code == ExitCode::SUCCESS,
            out: String::from_utf8_lossy(&out).into_owned(),
            err: String::from_utf8_lossy(&err).into_owned(),
        })
    })
}

/// An upkeep host on the stand-in `world`, on the scratch vault `lab`, at [`NOW`], running Claude
/// Code 2.1.318.
fn step_host(lab: &Lab, world: &World) -> StepHost {
    std::fs::write(world.home().join("go"), b"go").unwrap();
    StepHost {
        token: Host::minting(VaultOf::Real(lab.paths.clone()), world),
        now: Cell::new(NOW),
        cli: RefCell::new("2.1.318".to_owned()),
        hang: aterm_spec::wait::MUST_HAPPEN,
        person: Cell::new(false),
    }
}

/// AN UPKEEP RUN OPENS NO PAGE WHERE A STOP HELD THE STEP AFTER IT LOOKED, OR WHERE IT CANNOT
/// RECORD ITS RUN (plan package FL-138): between the upkeep's look (`backfill_due`) and its run,
/// another run of the account's step met a human check and left its record: the run, judging the
/// record again under the account's lock, refuses with nothing made, and Claude Code never runs.
/// With the record cleared, but `tokens-tried.json` not writable (its lock held), it refuses too,
/// before the capture. Once it can write, the same run makes the token, its record written before
/// the capture.
/// NEGATIVE CONTROL: with `run_with_clock` not reading `step_held` under the lock, the run opens
/// the page: `` the held step ran ``; with it writing no record before the capture, `` a run
/// this Mac could not record opened its page ``.
#[test]
fn an_upkeep_run_opens_no_page_where_a_stop_held_the_step_after_it_looked() {
    use crate::harness::open_url::MetWord;
    use crate::harness::token_steps::{self, Stopped};
    let scratch = Scratch::new("step-raced");
    let lab = Lab::new();
    let world = World::new("renew-window-token", false);
    let host = step_host(&lab, &world);
    let (env, store) = (scratch.env(), scratch.store());
    let window = NobodyRoutes {
        window: true,
        headless: false,
    };
    let run = |finding: &Finding| {
        journal::with_keyer(keyer, || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_with_clock(
                &env,
                &host,
                Of::Row("work"),
                finding.gap.mode(),
                (finding.via, Some(finding)),
                &mut out,
                &mut err,
                Instant::now,
                &mut |_| {},
            );
            (code, String::from_utf8_lossy(&err).into_owned())
        })
    };
    let Backfill::Due(finding) =
        journal::with_keyer(keyer, || backfill_due(&env, &host, "work", false, window))
    else {
        panic!("no token is due");
    };
    let stopped = Stopped::new(
        MetWord::WindowHumanCheck,
        NOW + 1,
        "2.1.318",
        &aterm_stamp::build_id(),
    )
    .expect("a stop that holds");
    token_steps::put(&store, KEY, Some(&stopped)).expect("the record");
    let (code, err) = run(&finding);
    assert_eq!(world.runs(), 0, "the held step ran");
    assert_ne!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        err.contains("held after this Mac's upkeep found it missing"),
        "{err}"
    );

    // The run's record goes before its capture: where it cannot be written (another holds
    // `tokens.lock` past its ladder), no page opens.
    token_steps::put(&store, KEY, None).expect("the record cleared");
    let table_lock =
        crate::harness::accounts::lock_at_within(&store.shared_dir().join(TABLE_LOCK), &[])
            .expect("the lock opens")
            .expect("the lock is free");
    let (code, err) = run(&finding);
    assert_eq!(
        world.runs(),
        0,
        "a run this Mac could not record opened its page"
    );
    assert_ne!(code, ExitCode::SUCCESS, "{err}");
    assert!(err.contains(token_steps::TRIED_FILE), "{err}");
    drop(table_lock);

    let (code, err) = run(&finding);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(world.runs(), 1);
    // The verb's own run leaves the record it wrote before the capture: `backfill_run` clears it.
    let tried = token_steps::read_tried(&store).expect("the record reads");
    assert_eq!(tried[KEY].gap, Gap::None, "{tried:?}");
    assert_eq!(tried[KEY].minted_at, None);
    assert!(tried[KEY].at >= NOW, "{tried:?}");
}

/// A GAP THE UPKEEP RAN IS HELD ON RECORD, WHATEVER ITS END AND ROUTE, UNTIL SOMETHING CHANGES
/// (plan package FL-138): a capture in aterm's window whose browser did not start (a stop that
/// holds nothing, so no stop record) leaves one record in `tokens-tried.json` (the gap's word, the
/// token's `minted_at`, when, and the builds; no host, address, code or token byte). A pass a
/// day later, or a month later, by the window or by the headless profile, runs nothing; a new
/// Claude Code build runs it once, and the next pass on that build nothing; a mailbox
/// configuration written after it runs it once more; the capture that makes the token clears
/// the record.
/// NEGATIVE CONTROL: with `step_held` reading no tried record (`tried.get(..)` as `None`), the
/// pass a day later runs: `` a pass 1 days later by the window ran the gap again ``.
#[test]
fn a_gap_the_upkeep_ran_is_held_on_record_whatever_its_end_and_route_until_something_changes() {
    use crate::harness::headless_oauth::MAILBOX_TOML_FILE;
    use crate::harness::token_steps::{self, TRIED_FILE};
    let scratch = Scratch::new("gap-tried");
    let lab = Lab::new();
    let world = World::new("window-browser-start", false);
    let host = step_host(&lab, &world);
    let (env, store) = (scratch.env(), scratch.store());
    let window = NobodyRoutes {
        window: true,
        headless: false,
    };
    let headless = NobodyRoutes {
        window: false,
        headless: true,
    };
    let tried = || token_steps::read_tried(&store).expect("the tried file reads");

    let ran = upkeep_pass(&env, &host, window).expect("a gap nobody ran is due");
    assert!(!ran.ok, "{}", ran.err);
    assert_eq!(world.runs(), 1);
    assert!(
        token_steps::read(&store)
            .expect("the steps file")
            .is_empty(),
        "a browser that did not start left a stop record"
    );
    let record = tried();
    assert_eq!(record.len(), 1, "{record:?}");
    assert_eq!(record[KEY].gap, Gap::None);
    assert_eq!(record[KEY].minted_at, None);
    assert_eq!(record[KEY].cli, "2.1.318");
    let at = record[KEY].at;

    for days in [1, 30] {
        host.now.set(NOW + days * 86_400);
        for (routes, by) in [(window, "the window"), (headless, "headless")] {
            assert!(
                upkeep_pass(&env, &host, routes).is_none(),
                "a pass {days} days later by {by} ran the gap again"
            );
        }
    }
    assert_eq!(world.runs(), 1, "a pass with no change reached the page");

    host.cli.replace("2.1.319".to_owned());
    let ran = upkeep_pass(&env, &host, window).expect("a new Claude Code build runs the gap");
    assert!(!ran.ok, "{}", ran.err);
    assert_eq!(world.runs(), 2);
    assert_eq!(tried()[KEY].cli, "2.1.319");
    assert!(upkeep_pass(&env, &host, headless).is_none());
    assert_eq!(world.runs(), 2, "the same build ran the gap twice");

    let at = tried()[KEY].at.max(at);
    let mailbox = store.dir.join(MAILBOX_TOML_FILE);
    std::fs::write(&mailbox, "").expect("the mailbox config");
    std::fs::File::options()
        .write(true)
        .open(&mailbox)
        .and_then(|file| file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(at + 60)))
        .expect("its time");
    host.now.set(at + 120);
    assert!(
        upkeep_pass(&env, &host, window).is_some(),
        "a mailbox configured after the run did not run the gap"
    );
    assert_eq!(world.runs(), 3);
    assert!(upkeep_pass(&env, &host, window).is_none());
    assert_eq!(world.runs(), 3);

    let raw = std::fs::read_to_string(store.shared_dir().join(TRIED_FILE)).expect("the record");
    let token = sentinel();
    for held in ["://", "@", "claude.ai", "example.com", "why", &token[..24]] {
        assert!(!raw.contains(held), "the record holds {held:?}: {raw}");
    }

    world.play("renew-window-token");
    host.cli.replace("2.1.320".to_owned());
    let ran = upkeep_pass(&env, &host, window).expect("a new build runs the gap");
    assert!(ran.ok, "{}{}", ran.out, ran.err);
    assert_eq!(world.runs(), 4);
    assert!(tried().is_empty(), "the token's capture left the record");
    assert!(upkeep_pass(&env, &host, window).is_none());
}

/// A HEADLESS LOGIN MAKES ITS TOKEN ON THE SAME PROFILE (plan package FL-137; sitting design
/// S5): after a headless sign-in binds a row whose headless profile a sign-in proved, the run
/// makes the row's token there. `claude setup-token` (the stand-in) hands its authorize request
/// to aterm's opener once, under the token's grant and never the window's; the request passes
/// `setup_token::admit_url` and Claude's pin; `Authorize` is pressed once, on a page that shows
/// the account; the callback reaches the waiting listener; and the token, which came back with
/// the stand-in's terminal closed, is in the vault, the step's record cleared. Before it, on
/// Cloudflare's check on the token's page, the login stays bound, no token is written, one stop
/// record is left, and nothing but the page's read was evaluated, with no input sent; on a
/// consent page that names no account, nothing is pressed and the record names that; on one
/// that carries a Turnstile, nothing is pressed, and its record holds the step until an event;
/// and a page that goes to its callback with nothing pressed has that callback handed to the
/// listener nowhere, with no token written. A check on the token's page is the account's check
/// too, with the login as it was. A check's record holds the next headless sign-in's run too,
/// though the login that sign-in made ended the account's check: nobody is at the profile to
/// answer one, so that run offers the step and opens nothing, until a person's act clears the
/// record. With no proven profile the step is offered and nothing runs.
/// NEGATIVE CONTROL: with `consent_step` pressing an enabled `Authorize` whatever `shows_email`
/// says, the unnamed page is pressed: `` the page that names nobody was pressed ``; with
/// `consent_step` reading no human check, the Turnstile page ends as no check: `` Turnstile: ``;
/// with `check_holds` holding nothing, the next headless sign-in goes back to the check: `` a
/// headless run went back to the check ``.
#[test]
fn a_headless_login_makes_its_token_on_the_same_profile() {
    use crate::harness::open_url::MetWord;
    use crate::harness::token_steps;
    let scratch = Scratch::new("headless-token");
    let world = World::new("headless-bot-check", false);
    std::fs::write(world.home().join("go"), b"go").unwrap();
    let vault = FakeVault::new(Answer::Absent);
    let writes = Rc::clone(&vault.writes);
    let host = StepHost {
        token: Host::minting(VaultOf::Fake(vault), &world),
        now: Cell::new(NOW),
        cli: RefCell::new("2.1.318".to_owned()),
        hang: aterm_spec::wait::MUST_HAPPEN,
        person: Cell::new(false),
    };
    let env = scratch.env();
    let store = scratch.store();
    // The row's login, as its headless sign-in just bound it.
    let mut logins = accounts::Logins::default();
    logins.labels.insert(
        "work".to_owned(),
        accounts::Binding {
            dir: Some("/logins/work".to_owned()),
            renewed_at: Some(i64::try_from(NOW).unwrap()),
            ..accounts::Binding::default()
        },
    );
    store.save_logins(&logins).expect("the login");
    let bound = || {
        store
            .load_logins(i64::try_from(NOW).unwrap())
            .expect("the logins")
            .0
            .labels
    };
    let bound_before = bound();
    assert!(bound_before.contains_key("work"));
    let after = || {
        journal::with_keyer(keyer, || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = after_sign_in(
                &env,
                &host,
                "work",
                SignedIn::Headless,
                (&mut out, &mut err),
                &mut |_| {},
            );
            let (out, err) = (
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
            );
            assert!(!out.contains(&sentinel()[..24]) && !err.contains(&sentinel()[..24]));
            (code.map(|code| format!("{code:?}")), out, err)
        })
    };
    let browsed = || std::fs::read_to_string(world.home().join("browser-log")).unwrap_or_default();
    let record = || token_steps::read(&store).expect("the steps file reads");
    let success = Some(format!("{:?}", ExitCode::SUCCESS));
    // The next sign-in of the row is a login made for it, which ends the account's check; the
    // token step's own record is left as it was.
    let signed_in_again = || {
        let mut logins = store
            .load_logins(i64::try_from(NOW).unwrap())
            .expect("the logins")
            .0;
        if let Some(binding) = logins.labels.get_mut("work") {
            binding.check_stop = None;
        }
        store.save_logins(&logins).expect("the login");
    };
    let _ = notes();

    // CONTROL: with no proven profile the step is offered, and nothing runs.
    let (code, out, err) = after();
    assert_eq!(code, None, "{out}{err}");
    assert!(out.contains(&work_step()), "{out}");
    assert_eq!(world.runs(), 0);

    // The profile the headless sign-in proved.
    let profile = crate::harness::headless_oauth::resolve_browser_profile_dir(
        &store.dir,
        Some("work"),
        Some(EMAIL),
    );
    std::fs::create_dir_all(&profile).expect("the proven profile");

    // CONTROL: Cloudflare's check on the token's page.
    let (code, out, err) = after();
    assert_ne!(code, success, "{out}{err}");
    assert!(
        out.contains("on the headless browser profile it just signed in on"),
        "{out}"
    );
    assert!(
        err.contains("headless browser profile") && err.contains("Cloudflare"),
        "{err}"
    );
    assert_eq!(world.runs(), 1);
    assert!(writes.borrow().is_empty(), "a token was written");
    // The login stays as it was, and the check is the account's too (`CheckedRow::hold_after`).
    let mut checked = bound();
    let check = checked
        .get_mut("work")
        .and_then(|binding| binding.check_stop.take());
    assert!(
        check.is_some_and(|check| check.at >= i64::try_from(NOW).unwrap()),
        "the account's check was not recorded"
    );
    assert_eq!(checked, bound_before, "the login moved");
    let steps = record();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[KEY].word, MetWord::BotCheck);
    let kinds: Vec<_> = notes().iter().map(|note| (note.kind, note.why)).collect();
    assert_eq!(kinds, [("token-stopped", Some("bot-check"))]);
    let log = browsed();
    assert!(log.lines().any(|method| method == "read"), "{log}");
    assert!(
        !log.lines()
            .any(|method| method == "act" || method.starts_with("Input.")),
        "a press or an input was sent at the check: {log}"
    );
    // The next headless sign-in makes no run at the check: the step is offered instead.
    let held = |what: &str| {
        let (code, out, err) = after();
        assert_eq!(code, None, "{what}: {out}{err}");
        assert!(
            out.contains("carried a check the last time"),
            "{what}: {out}"
        );
        assert!(out.contains(&work_step()), "{what}: {out}");
    };
    signed_in_again();
    held("Cloudflare");
    assert_eq!(world.runs(), 1, "a headless run went back to the check");
    // A person's act clears the record.
    token_steps::put(&store, KEY, None).expect("the record clears");

    // CONTROL: a consent page that names no address presses nothing.
    world.play("headless-unnamed");
    let (code, out, err) = after();
    let log = browsed();
    assert!(
        !log.lines().any(|method| method == "act"),
        "the page that names nobody was pressed: {log}"
    );
    assert_ne!(code, success, "{out}{err}");
    assert!(err.contains("did not show the account"), "{err}");
    assert_eq!(world.runs(), 2);
    assert!(writes.borrow().is_empty(), "a token was written");
    let steps = record();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[KEY].word, MetWord::ConsentUnproven);
    let kinds: Vec<_> = notes().iter().map(|note| (note.kind, note.why)).collect();
    assert_eq!(kinds, [("token-stopped", Some("consent-unproven"))]);

    // A Turnstile in the consent page: nothing is pressed, and its record holds the step for an
    // event, never a clock.
    world.play("headless-turnstile");
    let (code, out, err) = after();
    let log = browsed();
    assert!(
        !log.lines()
            .any(|method| method == "act" || method.starts_with("Input.")),
        "Turnstile: a press or an input was sent: {log}"
    );
    assert_ne!(code, success, "{out}{err}");
    assert!(err.contains("human check"), "Turnstile: {err}");
    assert_eq!(world.runs(), 3);
    assert!(writes.borrow().is_empty(), "a token was written");
    let steps = record();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[KEY].word, MetWord::HumanCheck);
    let kinds: Vec<_> = notes().iter().map(|note| (note.kind, note.why)).collect();
    assert_eq!(kinds, [("token-stopped", Some("human-check"))]);
    for days in [1, 30] {
        host.now.set(NOW + days * 86_400);
        assert_eq!(
            backfill_due(
                &env,
                &host,
                "work",
                false,
                NobodyRoutes {
                    window: false,
                    headless: true,
                },
            ),
            Backfill::Nothing,
            "the human check's step was due {days} days later"
        );
    }
    host.now.set(NOW);
    assert_eq!(world.runs(), 3);
    signed_in_again();
    held("Turnstile");
    assert_eq!(world.runs(), 3, "a headless run went back to the check");
    token_steps::put(&store, KEY, None).expect("the record clears");

    // A page that goes to its callback with nothing pressed: the listener has nothing.
    world.play("headless-callback");
    let (code, out, err) = after();
    let log = browsed();
    assert!(!log.lines().any(|method| method == "act"), "{log}");
    assert_ne!(code, success, "{out}{err}");
    assert!(err.contains("did not show the account"), "{err}");
    assert_eq!(world.runs(), 4);
    assert!(writes.borrow().is_empty(), "a token was written");
    assert!(
        !world.home().join("callback").exists(),
        "the listener took a callback with nothing pressed"
    );
    let steps = record();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[KEY].word, MetWord::ConsentUnproven);
    let kinds: Vec<_> = notes().iter().map(|note| (note.kind, note.why)).collect();
    assert_eq!(kinds, [("token-stopped", Some("consent-unproven"))]);

    // The consent page that shows the account.
    world.play("headless-token");
    let (code, out, err) = after();
    assert_eq!(code, success, "{out}{err}");
    assert!(
        out.contains("stored a Claude Code token for claude person@example.com"),
        "{out}"
    );
    assert_eq!(world.runs(), 5);
    let log = browsed();
    assert_eq!(
        log.lines()
            .filter(|method| *method == "Page.navigate")
            .count(),
        1,
        "the request reached the page once, past both checks: {log}"
    );
    assert_eq!(
        log.lines().filter(|method| *method == "act").count(),
        1,
        "{log}"
    );
    let callback = std::fs::read_to_string(world.home().join("callback")).expect("the callback");
    assert!(
        callback.starts_with("GET /callback?code=test-code&state=STANDINSTATE "),
        "{callback}"
    );
    let written = writes.borrow();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].email, EMAIL);
    // The headless route proved the account, so its token serves every Mac from mint.
    assert_eq!(written[0].minted_by, Some(MintedBy::Headless));
    drop(written);
    assert!(out.contains("it serves every Mac"), "{out}");
    assert!(record().is_empty(), "the token's capture left the record");
    let kinds: Vec<_> = notes().iter().map(|note| note.kind).collect();
    assert_eq!(kinds, ["token-minted"]);
    assert_eq!(bound(), bound_before, "the login moved");
}

/// A CAPTURE'S OWN HANG HOLDS NO STEP (FL-136): where the capture's hang detector runs out
/// before Claude Code showed an address to open (so aterm's sign-in window never opened), the
/// run says so and leaves no record and no `token-stopped` note: the hang is no page anyone met,
/// and nothing of it is the window's deadline. A record an earlier check left is kept as it was
/// by the hang of a person's own run over it.
/// The window's own deadline, which its opener records, still holds the step; the capture's
/// hang holds nothing.
/// NEGATIVE CONTROL: with `held_by` taking every hang on the window's route for its deadline,
/// the run leaves a `deadline` record: `` a capture's own hang held the step ``.
#[test]
fn a_captures_own_hang_holds_no_step() {
    use crate::harness::open_url::MetWord;
    use crate::harness::token_steps::{self, Stopped};
    let scratch = Scratch::new("step-hang");
    let lab = Lab::new();
    let world = World::new("silent", false);
    let host = StepHost {
        hang: Duration::ZERO,
        ..step_host(&lab, &world)
    };
    let env = scratch.env();
    let store = scratch.store();
    let make = || {
        journal::with_keyer(keyer, || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run(&env, &host, EMAIL, TokenMode::Make, &mut out, &mut err);
            Ran {
                ok: format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
                out: String::from_utf8_lossy(&out).into_owned(),
                err: String::from_utf8_lossy(&err).into_owned(),
            }
        })
    };
    let _ = notes();
    let ran = make();
    assert!(!ran.ok, "{}", ran.out);
    assert!(ran.err.contains("hang detector"), "{}", ran.err);
    assert!(
        token_steps::read(&store)
            .expect("the steps file reads")
            .is_empty(),
        "a capture's own hang held the step"
    );
    assert!(
        !notes().iter().any(|note| note.kind == "token-stopped"),
        "a capture's own hang was noted as a stop"
    );
    assert_eq!(world.runs(), 0, "the stand-in's page was reached");

    let earlier =
        Stopped::new(MetWord::WindowHumanCheck, NOW - 60, "2.1.318", "").expect("a record");
    token_steps::put(&store, KEY, Some(&earlier)).expect("the record");
    // A check's record holds every run nobody at a terminal started; a person's own runs the
    // capture over it.
    host.person.set(true);
    let ran = make();
    assert!(!ran.ok && ran.err.contains("hang detector"), "{}", ran.err);
    let steps = token_steps::read(&store).expect("the steps file reads");
    assert_eq!(steps.get(KEY), Some(&earlier), "{steps:?}");
    assert_eq!(steps.len(), 1, "{steps:?}");

    assert_eq!(held_by(&Err(setup_token::Error::Hang)), None);
    assert_eq!(
        held_by(&Err(setup_token::Error::Opener(MetWord::WindowDeadline))),
        Some(MetWord::WindowDeadline)
    );
    assert_eq!(
        held_by(&Err(setup_token::Error::Opener(MetWord::BrowserSignIn))),
        None
    );
}

/// A CODE PAGE'S STEP IS LET THROUGH ONCE THE VAULT DELIVERS THE ACCOUNT'S MAILBOX (FL-136): a
/// step a code page stopped where aterm's window read no mailbox of the account stays held until
/// the window reads one; the mailbox configurations are vault entries, read in place with no
/// file under `~/.secrets` written or dated, and one the vault delivers after the stop lets the
/// step through. A mailbox lets no other record through: the upkeep's record that it ran the
/// gap, beside the stop (the run that met the code page leaves both), keeps the gap held, since
/// the vault's entries date nothing; and a human check's step stays held.
/// NEGATIVE CONTROLS: with `step_held` passing no `mailbox_read` (`None` for every word), the
/// vault's mailbox is not seen: `` the vault's mailbox did not let the code page's step through ``;
/// with `Tried::holds` let through by `mailbox_read` as a code page's stop is, `` the vault's
/// mailbox let the upkeep's record of the run through ``.
#[cfg(unix)]
#[test]
fn a_code_pages_step_is_let_through_once_the_vault_delivers_the_accounts_mailbox() {
    use crate::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE;
    use crate::harness::open_url::MetWord;
    use crate::harness::token_steps::{self, Stopped, Tried};
    let scratch = Scratch::new("step-mailbox");
    let lab = Lab::new();
    let world = World::new("silent", false);
    let host = step_host(&lab, &world);
    let env = scratch.env();
    let store = scratch.store();
    let home = env.home.clone().expect("the scratch home");
    let window = NobodyRoutes {
        window: true,
        headless: false,
    };
    let found =
        || match journal::with_keyer(keyer, || backfill_due(&env, &host, "work", false, window)) {
            Backfill::Due(finding) => Some(finding),
            Backfill::NotSeated | Backfill::Nothing => None,
        };
    let due = || found().is_some();
    let finding = found().expect("CONTROL: a step with no record");
    let stopped = Stopped::new(MetWord::WindowCodeNoMailbox, NOW, "2.1.318", "").expect("a record");
    token_steps::put(&store, KEY, Some(&stopped)).expect("the record");
    assert!(!due(), "a code page's step was due with no mailbox");

    crate::vault_entry::put(
        &home,
        SECRETS_MAILBOX_TOML_FILE,
        "imap_url = \"imaps://imap.example.test/INBOX\"\nuser = \"person@example.com\"\n\
         password = \"a fixture's word\"\n",
    );
    assert!(
        !home.join(".secrets").exists(),
        "the vault's entry was written as a file"
    );
    assert_eq!(
        token_steps::route_at(&store, Some(&home), EMAIL),
        None,
        "a file dated the vault's entry"
    );
    assert!(
        due(),
        "the vault's mailbox did not let the code page's step through"
    );
    assert_eq!(world.runs(), 0, "a due step ran with no pass");

    // The upkeep's record of the run that met the code page, beside the stop.
    let ran = Tried::new(
        (finding.gap, finding.minted_at),
        NOW,
        "2.1.318",
        &aterm_stamp::build_id(),
    );
    token_steps::put_tried(&store, KEY, Some(&ran)).expect("the record");
    assert!(
        !due(),
        "the vault's mailbox let the upkeep's record of the run through"
    );
    token_steps::put_tried(&store, KEY, None).expect("the record cleared");
    assert!(due(), "CONTROL: the stop alone, the vault's mailbox read");

    let human = Stopped::new(MetWord::WindowHumanCheck, NOW, "2.1.318", "").expect("a record");
    token_steps::put(&store, KEY, Some(&human)).expect("the record");
    assert!(!due(), "a mailbox let a human check's step through");
}

/// THE UPKEEP'S TOKENS-HERE READING TAKES THE READING RAW TO SETTLE A SPLIT PAIR ONLY WHERE ITS
/// LISTING HOLDS ONE ([`rows_on_token_settled`]): a listing that holds none, here a vault that is
/// not there, settles nothing, so no settle poisons the window's accounts keeper's trace by the
/// clock, and the keeper's tokens-here duty over a vault that is not there can be kept. A listed
/// standing of a row the reading judges is judged at the reading raw all the same, split or not
/// ([`rows_on_token_of`], whose law is `served`'s
/// `the_tokens_here_reading_takes_the_reading_raw_only_to_judge_a_listed_standing`).
///
/// NEGATIVE CONTROL (2026-11-16, at the merge of the keeper with main's split settle):
/// `rows_on_token_settled` without its `holds_split(&listed) &&` failed "no split pair: the
/// reading is not taken raw", the trace poisoned for `a raw read of now`. Restored, the file
/// touched.
#[cfg(unix)]
#[test]
fn a_tokens_here_reading_takes_no_reading_raw_to_settle_where_no_pair_is_split() {
    use aterm_uds::trace::{self, Now, Unkept};
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let paths = Paths::for_home(dir.path());
    let store = Store::at(&dir.path().join("state"));
    let (read, traced) = trace::scope(|| {
        rows_on_token_settled(&paths, &store, &Roster::default(), Now::at(1_800_000_000))
    });
    assert_eq!(read, None, "no vault: no reading");
    assert!(
        !matches!(traced.unkept(), Some(Unkept::Poison(_))),
        "no split pair: the reading is not taken raw: {:?}",
        traced.unkept()
    );
}

#[path = "accounts_renew_token_tests.rs"]
mod renew_token;
