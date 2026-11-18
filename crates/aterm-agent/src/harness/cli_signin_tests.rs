// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `accounts renew --cli <id>`: a Kimi row renewed end to end by its sign-in's stand-in
//! (this test binary run in place as `kimi login --region global`, printing the recorded
//! transcript), a Codex row by its native loopback PKCE sign-in, and the machine's one sign-in at
//! a time.

// The tests gated `cfg(unix)` here start a manual sign-in or select a row that needs one,
// which only Unix queues (its private FIFO, `accounts::manual_signin_capability`).
#![cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "some helpers serve only the tests gated `cfg(unix)`"
    )
)]

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::time::Duration;

use super::*;
use crate::harness::login_drive::{self, Approval, Approver, Drove, Handoff, Request};

#[cfg(unix)]
#[test]
fn adding_a_native_account_requires_receipt_admission_before_any_directory_or_row() {
    use crate::harness::alab_roster::{
        self, Lab, LabAccount, ReceiptGuard, SourceGuard, SourceSnapshot, TestSourceOverride,
    };
    struct Refuse;
    impl SourceGuard for Refuse {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn same_source(&self, _: &dyn SourceGuard) -> bool {
            true
        }
        fn with_admitted(
            &self,
            _: Box<dyn FnOnce() -> Result<(), crate::harness::account_sync_error::SyncError> + '_>,
        ) -> Result<(), crate::harness::account_sync_error::SyncError> {
            Err("native add admission refused".into())
        }
    }
    let tmp = tmp_dir("native-add-receipt");
    let env = env_at(&tmp);
    let store = env.accounts_store();
    let machine = alab_roster::test_machine("m16");
    let pk = aterm_db::typed::keys::account_pk("claude", "native@example.test").expect("a key");
    let names = aterm_file::AccountNames {
        realm: "claude".into(),
        slug: "claude-native".into(),
        name: Some("claude native@example.test".into()),
        email: Some("native@example.test".into()),
        login: Some("native@example.test".into()),
        live: true,
    };
    let _source = TestSourceOverride::new(SourceSnapshot::Present(std::sync::Arc::new(Lab {
        me: Some(machine.clone()),
        names: [(pk.clone(), names)].into_iter().collect(),
        accounts: vec![LabAccount {
            pk,
            cli: Cli::Claude,
            vendor: Default::default(),
            label: "native".into(),
            legacy_label: None,
            email: "native@example.test".into(),
            identity: None,
            browser: None,
            joinable: true,
            preferred: vec![(machine, Some(1))],
        }],
        ..Lab::default()
    })))
    .with_guard(ReceiptGuard::new(Refuse));
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = run_accounts_add(
        Cli::Claude,
        &env,
        &FakeHost::new(&never_asked),
        "claude native@example.test",
        &Listing::default(),
        &mut out,
        &mut err,
    );
    assert_eq!(result, ExitCode::FAILURE);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("native add admission refused")
    );
    assert!(!env.accounts_roster_for(Cli::Claude).unwrap().exists());
    assert!(!store.login_dir("native").unwrap().exists());
    assert!(!store.sync_needs_recovery());
}

/// What the stand-in host runs for a sign-in.
enum Script {
    /// The Kimi stand-in, its `.stand-in` script (`<transcript> <exit>`).
    Kimi(&'static str),
    /// Codex's native loopback PKCE sign-in issuing a ChatGPT JWT for this email in `staging`.
    Codex(Option<&'static str>),
    /// No child: the sign-in ends so, on www.kimi.ai.
    Ends(login_drive::Outcome),
}

/// A window `accounts browser` opened: the browser, the profile and the page.
type Opened = Vec<(PathBuf, PathBuf, String)>;

/// A profile tidied after its window: the profile, and the origin and key its session was asked
/// after.
type Tidies = Vec<(PathBuf, Option<(String, String)>)>;

/// The pages handed over: each one's host and code.
type Handed = Vec<(Option<String>, Option<String>)>;

/// What runs inside an [`Script::Ends`] sign-in, after its plan and before its end is recorded.
type During = Box<dyn Fn(&Request<'_>)>;

struct SignInHost {
    script: Script,
    home: PathBuf,
    calls: Cell<usize>,
    handed: RefCell<Handed>,
    /// Whether stdin is a terminal.
    terminal: bool,
    /// Whether this runs over SSH.
    ssh: bool,
    /// Why no window opens at this desk ([`AccountsHost::no_window_desk`]), if one does not.
    no_desk: Option<&'static str>,
    /// The window browser installed, if any.
    window: Option<PathBuf>,
    /// What fetching the pinned window browser gives where none is installed, and how many
    /// fetches ran.
    fetched: (Result<PathBuf, String>, Cell<usize>),
    /// How each window ends, and whether the person signed in there (the vendor's session
    /// kept, which the pass after the window reads where it asks after one).
    ends: (crate::harness::browser_window::WindowEnd, bool),
    opened: RefCell<Opened>,
    /// The identity providers' cookies the pass after a window drops, or why it fails.
    tidy: Result<usize, String>,
    tidied: RefCell<Tidies>,
    /// The holds on the signals that end a window alive now ([`AccountsHost::hold_window_ends`]).
    holds: std::rc::Rc<Cell<i32>>,
    /// At the window: whether the profile carried its untidied mark.
    marked_at_window: Cell<Option<bool>>,
    /// At the pass after the window: the holds alive, and whether the mark was still there.
    at_tidy: Cell<Option<(i32, bool)>>,
    /// Run inside an [`Script::Ends`] sign-in ([`During`]).
    during: Option<During>,
    automatic: bool,
    cancel_on_queue: bool,
    cancelled: Cell<bool>,
    queued: RefCell<Vec<accounts::SigninPosition>>,
}

impl SignInHost {
    fn new(script: Script, home: &Path) -> SignInHost {
        SignInHost {
            script,
            home: home.to_path_buf(),
            calls: Cell::new(0),
            handed: RefCell::new(Vec::new()),
            terminal: false,
            ssh: false,
            no_desk: None,
            window: None,
            fetched: (
                Err(
                    "the host could not be resolved (no network?); nothing was installed"
                        .to_owned(),
                ),
                Cell::new(0),
            ),
            ends: (crate::harness::browser_window::WindowEnd::Done, true),
            opened: RefCell::new(Vec::new()),
            tidy: Ok(0),
            tidied: RefCell::new(Vec::new()),
            holds: std::rc::Rc::new(Cell::new(0)),
            marked_at_window: Cell::new(None),
            at_tidy: Cell::new(None),
            during: None,
            automatic: false,
            cancel_on_queue: false,
            cancelled: Cell::new(false),
            queued: RefCell::new(Vec::new()),
        }
    }

    /// A desk: one that opens a window, a terminal on stdin, no SSH, Chrome for Testing installed.
    fn desk(home: &Path) -> SignInHost {
        SignInHost {
            terminal: true,
            window: Some(PathBuf::from(
                "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for \
                 Testing",
            )),
            ..SignInHost::new(Script::Kimi("kimi 0"), home)
        }
    }
}

struct Approves<'a>(&'a RefCell<Handed>);

impl Approver for Approves<'_> {
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
        self.0.borrow_mut().push((
            login_drive::https_host(handoff.url),
            handoff.code.map(str::to_owned),
        ));
        Approval::Approved { page: false }
    }
}

impl AccountsHost for SignInHost {
    fn automatic_sign_in(&self) -> bool {
        self.automatic
    }
    fn work_cancelled(&self) -> bool {
        self.cancelled.get()
    }
    fn signin_queued(&self, position: accounts::SigninPosition) {
        self.queued.borrow_mut().push(position);
        if self.cancel_on_queue {
            self.cancelled.set(true);
        }
    }
    fn stdin_is_terminal(&self) -> bool {
        self.terminal
    }

    fn under_ssh(&self) -> bool {
        self.ssh
    }

    fn no_window_desk(&self) -> Option<&'static str> {
        self.no_desk
    }

    fn window_browser(&self) -> Option<PathBuf> {
        self.window.clone()
    }

    fn fetch_window_browser(&self) -> Result<PathBuf, String> {
        self.fetched.1.set(self.fetched.1.get() + 1);
        self.fetched.0.clone()
    }

    fn open_window(
        &self,
        browser: &Path,
        profile: &Path,
        url: &str,
    ) -> Result<crate::harness::browser_window::WindowEnd, String> {
        self.opened.borrow_mut().push((
            browser.to_path_buf(),
            profile.to_path_buf(),
            url.to_owned(),
        ));
        assert!(
            crate::harness::browser_window::profile_holder(profile).is_some(),
            "the profile is claimed for its window"
        );
        self.marked_at_window.set(Some(
            profile
                .join(crate::harness::browser_window::UNTIDIED_MARK)
                .is_file(),
        ));
        Ok(self.ends.0)
    }

    fn tidy_profile(
        &self,
        _browser: &Path,
        profile: &Path,
        session: Option<(&str, &str)>,
    ) -> Result<crate::harness::browser_window::Tidied, String> {
        self.tidied.borrow_mut().push((
            profile.to_path_buf(),
            session.map(|(o, k)| (o.to_owned(), k.to_owned())),
        ));
        self.at_tidy.set(Some((
            self.holds.get(),
            profile
                .join(crate::harness::browser_window::UNTIDIED_MARK)
                .is_file(),
        )));
        let dropped = self.tidy.clone()?;
        Ok(crate::harness::browser_window::Tidied {
            dropped,
            session: session.map(|_| self.ends.1),
        })
    }

    fn hold_window_ends(&self) -> WindowEnds {
        self.holds.set(self.holds.get() + 1);
        let holds = std::rc::Rc::clone(&self.holds);
        WindowEnds::noting(move || holds.set(holds.get() - 1))
    }

    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        panic!("another CLI's renewal runs no `claude` (asked under {login:?})");
    }

    fn unaskable(&self) -> Option<String> {
        None
    }

    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("not here".to_string())
    }

    fn now(&self) -> i64 {
        1_758_412_900
    }

    fn vendor_sign_in(&self, req: &Request<'_>) -> Drove {
        self.calls.set(self.calls.get() + 1);
        let mut approver = Approves(&self.handed);
        match &self.script {
            Script::Kimi(script) => {
                #[cfg(unix)]
                assert!(
                    req.recovery.is_some(),
                    "admitted Kimi plan lost its recovery identity"
                );
                std::fs::write(req.staging.join(".stand-in"), script).expect("script");
                let exe = std::env::current_exe().expect("test exe");
                let lead: Vec<OsString> = [
                    "harness::login_drive::tests::signin_stand_in",
                    "--exact",
                    "--nocapture",
                    "--",
                ]
                .map(OsString::from)
                .to_vec();
                let home = self.home.clone();
                login_drive::sign_in_with(
                    req,
                    (&exe, &lead),
                    None,
                    Some(Path::new(login_drive::SANDBOX_EXEC)),
                    &move |name| (name == "HOME").then(|| home.clone().into_os_string()),
                    &mut approver,
                    Duration::from_secs(60),
                )
            }
            Script::Ends(outcome) => {
                if let Some(during) = &self.during {
                    during(req);
                }
                Drove {
                    outcome: outcome.clone(),
                    host: Some("www.kimi.ai".to_owned()),
                }
            }
            Script::Codex(email) => crate::harness::codex_app_login::tests::mock_login_in_home(
                req.staging,
                *email,
                &mut approver,
                req.hosts,
                req.row.email.as_deref(),
                crate::harness::wake_deadline::WakeDeadline::after(Duration::from_secs(60)),
            ),
        }
    }
}

/// The word a step that signs `cli`'s roster row `label` in types for it, in the roster `env`
/// reads, with no lab ([`crate::harness::account_names::RowName`]).
fn step_word(env: &Env, cli: Cli, label: &str) -> String {
    crate::harness::account_names::tests::row_name_of(cli, env, label)
        .word
        .to_string()
}

/// `cli`'s roster row `label`, in the roster `env` reads, as a message names it.
fn shown_of(env: &Env, cli: Cli, label: &str) -> String {
    crate::harness::account_names::tests::row_name_of(cli, env, label).shown
}

fn renew(line: &str, env: &Env, host: &dyn AccountsHost) -> (String, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = match parsed(line) {
        Ok(Cmd::AccountsOf(of)) => run_accounts_of(&of, env, host, &mut out, &mut err),
        Ok(Cmd::Accounts(form)) => run_accounts(&form, env, host, &mut out, &mut err),
        other => panic!("{line} is no accounts command: {other:?}"),
    };
    (
        format!("{code:?}"),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn stagings(root: &Path, label: &str) -> Vec<String> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with(&format!("{label}+")))
                .collect()
        })
        .unwrap_or_default()
}

fn kimi_env(tag: &str) -> (Tmp, Env) {
    let tmp = tmp_dir(tag);
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Kimi,
        kimi_by_hand(
            "kimi-one",
            Some("kimi-one@example.com"),
            Some(crate::ai::connector::Region::Global),
        ),
    );
    (tmp, env)
}

/// Unix only, as the overlay staging home it signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_kimi_row_renews_through_its_own_sign_in_and_a_failed_one_preserves_its_binding() {
    let (tmp, env) = kimi_env("signin-kimi");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let logins_root = home.join(".aterm/logins/kimi");

    let failing = SignInHost::new(Script::Kimi("kimi 1"), &home);
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &failing,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    let prepared = store.load_logins(env.now).unwrap().0;
    let binding = prepared.get("kimi-one").unwrap();
    assert!(binding.renewal_generation > 0);
    assert_eq!(
        binding,
        &accounts::Binding {
            renewal_generation: binding.renewal_generation,
            ..Default::default()
        },
        "failed authentication leaves only its generation fence"
    );
    let (code, _, _) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &failing,
    );
    assert_eq!(code, exit(1));
    let after = store.load_logins(env.now).unwrap().0;
    let mut retained = after.clone();
    retained.workflows = prepared.workflows.clone();
    retained.workflow_clock = prepared.workflow_clock;
    assert_eq!(retained, prepared, "only workflow custody changes on retry");
    let raw = accounts::Roster::load_for(Cli::Kimi, &env.accounts_roster_for(Cli::Kimi).unwrap())
        .unwrap();
    let event = store
        .tail(20)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == accounts::Kind::SignIn)
        .unwrap();
    assert!(event.workflow.is_some());
    assert!(accounts::workflow_event_matches(
        raw.row("kimi-one").unwrap(),
        &after,
        &event
    ));
    assert!(
        stagings(&logins_root, "kimi-one").is_empty(),
        "staging removed"
    );
    let ledger = read(&store.ledger_path());
    assert!(
        ledger.contains("www.kimi.ai device exited-1") && ledger.contains("sign-in"),
        "{ledger}"
    );
    for said in [&ledger, &out, &err] {
        assert!(
            !said.contains("QXR4") && !said.contains("authorize_device"),
            "no code or URL is kept: {said}"
        );
    }

    let host = SignInHost::new(Script::Kimi("kimi 0"), &home);
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        *host.handed.borrow(),
        [(Some("www.kimi.ai".to_owned()), Some("QXR4-MZK7".to_owned()))],
        "the page and the printed code were handed over"
    );
    let staging = logins_root.join("kimi-one+1");
    assert!(staging.join("credentials/kimi-code.json").is_file());
    let logins = read(&tmp.path().join("state/accounts-kimi/logins.json"));
    assert!(
        logins.contains(&format!("\"dir\":\"{}\"", staging.display()))
            && logins.contains("\"proved_by\":\"unchecked\""),
        "{logins}"
    );
    assert!(
        out.contains(&format!(
            "{}: renewed",
            shown_of(&env, Cli::Kimi, "kimi-one")
        )) && !out.contains("QXR4"),
        "{out}"
    );
    let claude_logins = tmp.path().join("state/accounts/logins.json");
    assert!(
        !read(&claude_logins).contains("proved_by"),
        "Claude Code's store is not written"
    );
}

#[cfg(unix)]
#[test]
fn a_kimi_row_without_email_renews_through_its_device_flow() {
    let (tmp, env) = kimi_env("signin-kimi-no-email");
    let roster = env.accounts_roster_for(Cli::Kimi).expect("roster path");
    let text = read(&roster).replace("email = \"kimi-one@example.com\"\n", "");
    std::fs::write(roster, text).expect("email-less roster");
    let home = tmp.path().join("home");
    let host = SignInHost::new(Script::Kimi("kimi 0"), &home);
    let (code, out, err) = renew("accounts renew kimi-one --cli kimi", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        *host.handed.borrow(),
        [(Some("www.kimi.ai".to_owned()), Some("QXR4-MZK7".to_owned()))],
        "the unattended approver received the device page and code"
    );
    let logins = read(&tmp.path().join("state/accounts-kimi/logins.json"));
    assert!(
        logins.contains("kimi-one+1") && logins.contains("\"proved_by\":\"unchecked\""),
        "{logins}"
    );
}

#[cfg(unix)]
#[test]
fn a_codex_row_is_proved_by_its_app_servers_account_and_another_account_is_refused() {
    let tmp = tmp_dir("signin-codex");
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Codex,
        accounts::Row {
            identity_email: Some("codex-one@example.com".to_owned()),
            ..by_hand("codex-one", Some("login-alias@example.com"))
        },
    );
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Codex).expect("codex's store");
    let logins_root = home.join(".aterm/logins/codex");

    let other = SignInHost::new(Script::Codex(Some("someone-else@example.com")), &home);
    let (code, out, err) = renew(
        "accounts renew codex-one@example.com --cli codex",
        &env,
        &other,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains(&format!(
            "{}: REFUSED: wrong account",
            shown_of(&env, Cli::Codex, "codex-one")
        )),
        "{out}"
    );
    assert!(
        stagings(&logins_root, "codex-one").len() == 1,
        "completed wrong-account login retained"
    );
    let logins = read(&store.logins_file());
    assert!(
        !logins.contains("codex-one+"),
        "the binding names no new home: {logins}"
    );

    let host = SignInHost::new(Script::Codex(Some("codex-one@example.com")), &home);
    let (code, out, err) = renew(
        "accounts renew codex-one@example.com --cli codex",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        *host.handed.borrow(),
        [(Some("auth.openai.com".to_owned()), None)]
    );
    let logins = read(&store.logins_file());
    assert!(
        logins.contains("codex-one+2") && logins.contains("\"proved_by\":\"status\""),
        "{logins}"
    );
    let ledger = read(&store.ledger_path());
    assert!(
        !ledger.contains("oauth/authorize") && !ledger.contains("auth_code_ok"),
        "{ledger}"
    );
}

#[cfg(unix)]
#[test]
fn unavailable_browser_records_codex_renewal_failure_without_a_person_task() {
    let tmp = tmp_dir("codex-browser-unavailable");
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Codex,
        by_hand("codex-one", Some("codex-one@example.com")),
    );
    let home = tmp.path().join("home");
    let host = SignInHost::new(
        Script::Ends(login_drive::Outcome::NotRun(
            "pinned browser fetch is unavailable".into(),
        )),
        &home,
    );
    let (code, out, err) = renew(
        "accounts renew codex-one@example.com --cli codex",
        &env,
        &host,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(!out.contains("NEEDS YOU"), "{out}");
    assert!(err.contains("pinned browser fetch is unavailable"), "{err}");
    let store = env.accounts_store_for(Cli::Codex).unwrap();
    let (logins, _) = store.load_logins(env.now).unwrap();
    assert!(
        logins
            .get("codex-one")
            .and_then(|binding| binding.sign_in_task)
            .is_none()
    );
    assert!(read(&store.ledger_path()).contains("browser not-run"));
}

/// Unix only, as the overlay staging home it signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_bounded_background_sign_in_reports_the_machine_held_without_starting() {
    let (tmp, env) = kimi_env("signin-lock");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let held = store
        .signin_within()
        .expect("the lock")
        .expect("free at first");
    let host = SignInHost {
        automatic: true,
        ..SignInHost::new(Script::Kimi("kimi 0"), &home)
    };
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains(&format!("{}: held", shown_of(&env, Cli::Kimi, "kimi-one")))
            && out.contains(accounts::SIGNIN_HELD),
        "{out}"
    );
    assert_eq!(host.calls.get(), 0, "nothing ran under another's lock");
    assert!(stagings(&home.join(".aterm/logins/kimi"), "kimi-one").is_empty());
    drop(held);
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(host.calls.get(), 1, "once it is free the renewal runs");
}

#[cfg(unix)]
#[test]
fn a_codex_sign_ins_staging_home_keeps_only_auth_json_under_codex_auth_path_and_a_keyring_store_runs_none()
 {
    let tmp = tmp_dir("signin-codex-auth-path");
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Codex,
        by_hand("codex-one", Some("codex-one@example.com")),
    );
    let home = tmp.path().join("home");
    let host = SignInHost::new(Script::Codex(Some("codex-one@example.com")), &home);
    let (code, out, err) = renew(
        "accounts renew codex-one@example.com --cli codex",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let said = out
        .lines()
        .find(|l| l.contains("by hand with:"))
        .unwrap_or_default()
        .to_owned();
    // As the product joins it, a component at a time (`\` on Windows).
    let staging = home
        .join(".aterm")
        .join("logins")
        .join("codex")
        .join("codex-one+1");
    assert!(
        said.contains("CODEX_AUTH_PATH=")
            && said.contains(&staging.join("auth.json").display().to_string()),
        "{out}"
    );
    for unshared in ["sessions", "config.toml", "skills"] {
        assert!(
            std::fs::symlink_metadata(staging.join(unshared)).is_err(),
            "CODEX_AUTH_PATH staging creates no overlay links ({unshared})"
        );
    }
    assert!(
        staging.join("auth.json").is_file() && !staging.join("auth.json").is_symlink(),
        "the row's own login file is its own"
    );

    // Negative: with the login in the OS keyring, the sign-in would replace the machine's own
    // login, so nothing runs and no staging home is made.
    std::fs::create_dir_all(home.join(".codex")).expect("default codex dir");
    std::fs::write(
        home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .expect("config");
    let keyring = SignInHost::new(Script::Codex(Some("codex-one@example.com")), &home);
    let (code, out, err) = renew(
        "accounts renew codex-one@example.com --cli codex",
        &env,
        &keyring,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(err.contains("OS keyring"), "{err}");
    assert_eq!(keyring.calls.get(), 0);
    assert!(!home.join(".aterm/logins/codex/codex-one+2").exists());
}

/// WITH CODEX'S LOGIN IN THE OS KEYRING, THE ROW ON THIS MACHINE'S OWN LOGIN IS NOT TOLD ITS
/// SIGN-IN WOULD REPLACE THAT LOGIN INSTEAD OF MAKING ITS OWN: that login is the row's, so its
/// refusal says why the pool's staged sign-in cannot make it and what signs it in; a row of its
/// own keeps the refusal that its sign-in would replace this machine's own login. Neither runs a
/// sign-in or makes a staging home.
#[cfg(unix)]
#[test]
fn under_a_keyring_login_the_default_rows_renewal_refusal_is_its_own() {
    let tmp = tmp_dir("signin-codex-keyring-default");
    let mut env = env_at(&tmp);
    env.sock = None;
    let home = tmp.path().join("home");
    let work = home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work's home");
    let text = accounts::append_rows_for(
        Cli::Codex,
        "",
        &[
            accounts::Row {
                label: "main".into(),
                email: Some("main@example.com".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "work".into(),
                email: Some("work@example.com".into()),
                dir: Some(work.display().to_string()),
                ..accounts::Row::default()
            },
        ],
    )
    .expect("the rows admit");
    std::fs::write(env.accounts_roster_for(Cli::Codex).expect("a roster"), text).expect("roster");
    std::fs::create_dir_all(home.join(".codex")).expect("default codex dir");
    std::fs::write(
        home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .expect("config");
    for (label, own) in [("main", true), ("work", false)] {
        let host = SignInHost::new(Script::Codex(Some("main@example.com")), &home);
        let (code, out, err) = renew(
            &format!("accounts renew {label}@example.com --cli codex"),
            &env,
            &host,
        );
        assert_eq!(code, exit(1), "{label}: {out}{err}");
        let shown = shown_of(&env, Cli::Codex, label);
        assert!(
            err.contains(&keyring_staging(&shown, own)),
            "{label}: {err}"
        );
        assert!(
            !err.contains(&keyring_staging(&shown, !own)),
            "{label}: {err}"
        );
        assert_eq!(host.calls.get(), 0, "{label}: no sign-in ran");
        assert!(
            !home.join(format!(".aterm/logins/codex/{label}+1")).exists(),
            "{label}: no staging home"
        );
    }
}

/// Unix only, as the overlay staging home its failed Kimi renewal signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn ledger_accounts_reads_the_named_clis_ledger() {
    let (tmp, env) = kimi_env("ledger-cli");
    let home = tmp.path().join("home");
    let failing = SignInHost::new(Script::Kimi("kimi 1"), &home);
    let (code, _, _) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &failing,
    );
    assert_eq!(code, exit(1));
    assert_eq!(
        parsed("ledger accounts --cli kimi 5"),
        Ok(Cmd::Ledger {
            file: LedgerFile::Accounts(Cli::Kimi),
            count: 5,
            json: false,
        })
    );
    let (code, out, err) = run_line("ledger accounts --cli kimi", &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.contains(" sign-in ") && out.contains("exited-1"),
        "kimi's own ledger: {out}"
    );
    // The failed sign-in learned no key, so its row is named as the account the row held then,
    // never by its label or its address.
    assert!(
        !out.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .any(|word| word == "kimi-one"),
        "neither the row's label nor its address: {out}"
    );
    // Negative: Claude Code's ledger holds none of it, and `--cli` names a pool or nothing.
    let (code, out, _) = run_line("ledger accounts", &env);
    assert_eq!(code, exit(0));
    assert!(!out.contains("exited-1"), "{out}");
    assert!(parsed("ledger accounts --cli nope").is_err());
    assert!(parsed("ledger disk --cli kimi").is_err());
    assert!(parsed("ledger accounts --cli kimi --region global").is_err());
}

/// Off Unix, a renewal whose staging home is an overlay (Kimi's) runs no sign-in: aterm links an
/// overlay only on Unix, so the renewal says so, binds nothing and leaves no staging home.
#[cfg(not(unix))]
#[test]
#[ignore = "off Unix `accounts add` takes the manual sign-in queue, which only Unix has"]
fn a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links() {
    let (tmp, env) = kimi_env("signin-no-links");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let before = read(&store.logins_file());
    let host = SignInHost::new(Script::Kimi("kimi 0"), &home);
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    let said = format!("{out}{err}");
    assert_eq!(code, exit(1), "{said}");
    assert!(
        said.contains("its staging home could not be linked to")
            && said.contains("which aterm makes only on Unix")
            && said.contains("nothing was run"),
        "{said}"
    );
    assert_eq!(host.calls.get(), 0, "no sign-in ran");
    assert!(
        stagings(&home.join(".aterm").join("logins").join("kimi"), "kimi-one").is_empty(),
        "staging removed"
    );
    assert_eq!(
        read(&store.logins_file()),
        before,
        "the binding is unchanged"
    );
}

#[cfg(unix)]
#[test]
fn accounts_cli_all_lists_each_kept_pool_and_none_is_claudes_empty_report() {
    let tmp = tmp_dir("accounts-all");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (_, none, _) = run_line("accounts list", &env);
    let (_, all, _) = run_line("accounts --cli all list", &env);
    assert_eq!(all, none, "no pool kept: Claude Code's own report");
    listed_by_hand(
        &env,
        Cli::Kimi,
        kimi_by_hand(
            "kimi-one",
            Some("kimi-one@example.com"),
            Some(crate::ai::connector::Region::Global),
        ),
    );
    listed_by_hand(
        &env,
        Cli::Codex,
        by_hand("codex-one", Some("codex-one@example.com")),
    );
    let (code, out, err) = run_line("accounts --cli all list", &env);
    assert_eq!(code, exit(0), "{err}");
    let codex = out.find("cli: codex").expect(&out);
    let kimi = out.find("cli: kimi").expect(&out);
    assert!(codex < kimi && out.contains("codex-one") && out.contains("kimi-one"));
    assert!(
        !out.contains("cli: claude") && !out.contains("cli: grok"),
        "{out}"
    );
    let (code, json, _) = run_line("accounts --cli all list --json", &env);
    assert_eq!(code, exit(0));
    let doc: Value = aterm_json::from_str(json.trim()).expect("json");
    let pools = doc["pools"].as_array().expect("pools");
    assert_eq!(
        pools.iter().map(|p| p["cli"].as_str()).collect::<Vec<_>>(),
        [Some("codex"), Some("kimi")]
    );
    assert!(
        parsed("accounts --cli all add x").is_err(),
        "the report alone"
    );
}

/// A host whose Codex login reader names `account`.
struct PoolHost {
    account: Option<&'static str>,
}

impl AccountsHost for PoolHost {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        panic!("another CLI's pool runs no `claude` (asked under {login:?})");
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("not here".to_string())
    }
    fn now(&self) -> i64 {
        1_758_412_900
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        self.account.map(str::to_owned)
    }
}

#[cfg(unix)]
#[test]
fn a_codex_adopt_default_takes_the_email_its_app_server_names() {
    let tmp = tmp_dir("adopt-codex");
    let mut env = env_at(&tmp);
    env.sock = None;
    let host = PoolHost {
        account: Some("codex-one@example.com"),
    };
    let (code, out, err) = renew("accounts adopt-default --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let roster = read(&tmp.path().join("accounts-codex.toml"));
    let label =
        crate::harness::account_names::tests::added_label(Cli::Codex, "codex-one@example.com");
    assert!(
        roster.contains(&format!("label = \"{label}\""))
            && roster.contains("codex-one@example.com"),
        "{roster}"
    );
    assert!(
        out.contains("Codex's default login names the account "),
        "{out}"
    );

    // Negative: a Codex login reader that names no account, with no account named, writes
    // nothing. Every row adopt-default lists has an address, so another pool's parse needs an
    // account or --email.
    let other = tmp_dir("adopt-codex-none");
    let mut env = env_at(&other);
    env.sock = None;
    let silent = PoolHost { account: None };
    let (code, _, err) = renew("accounts adopt-default --cli codex", &env, &silent);
    assert_eq!(code, exit(1), "{err}");
    assert!(err.contains("--email"), "{err}");
    assert!(!other.path().join("accounts-codex.toml").exists());
    assert!(parsed("accounts adopt-default kimi sol@x.example --cli kimi --region global").is_ok());
    for (line, pool) in [
        (
            "accounts adopt-default --cli kimi --region global",
            "Kimi Code's",
        ),
        ("accounts adopt-default", "Claude Code's"),
    ] {
        assert!(parsed(line).is_err(), "{pool} needs an account or --email");
    }
}

/// The account `adopt-default` names is the identity Codex's default login must report, with or
/// without the alias `--email` its login signs in with: a login that reports another account, or
/// none, is refused and nothing is written.
#[cfg(unix)]
#[test]
fn codex_adopt_default_checks_an_explicit_canonical_identity() {
    let label = crate::harness::account_names::tests::added_label(Cli::Codex, "owner@example.test");
    for alias in ["", " --email alias@example.test"] {
        for report in [Some("owner@example.test"), Some("other@example.test"), None] {
            let tmp = tmp_dir("adopt-codex-identity");
            let env = env_at(&tmp);
            let host = PoolHost { account: report };
            let (code, out, err) = renew(
                &format!("accounts adopt-default oai owner@example.test --cli codex{alias}"),
                &env,
                &host,
            );
            let matched = report == Some("owner@example.test");
            assert_eq!(code, exit(if matched { 0 } else { 1 }), "{out}{err}");
            let path = tmp.path().join("accounts-codex.toml");
            assert_eq!(path.is_file(), matched);
            if matched {
                let roster = Roster::load_for(Cli::Codex, &path).unwrap();
                assert_eq!(
                    roster.row(&label).unwrap().identity_email.as_deref(),
                    report
                );
                let store = env.accounts_store_for(Cli::Codex).unwrap();
                assert!(store.load_logins(env.now).unwrap().0.get(&label).is_none());
            }
        }
    }
}

/// Whether `text` holds `word` as a word of its own (a roster label printed).
#[cfg(unix)]
fn prints_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
        .any(|token| token == word)
}

/// `adopt-default <account> --cli codex` completes the default login's row already on the roster
/// (the row with no `dir`, which the first `add` lists as `main`), as the form that names no
/// account does, where `auth.json` reports the account named: a row with no address takes the
/// login's, and a row that has it already is completed again, which changes nothing. Its receipt
/// names the account, never the row's label. CONTROL: where `auth.json` reports another account,
/// it is refused as such and the roster is as it was.
/// NEGATIVE CONTROL: with `complete_with` comparing the identity asked for with the row's stored
/// `identity_email` alone (`existing == Some(supplied)`), the row with no address is refused (exit
/// 1, not 0) (2026-11-16, by hand on SF-07H S6's review settle, then restored).
#[cfg(unix)]
#[test]
fn codex_adopt_default_naming_the_account_completes_the_default_row() {
    for original in [
        "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n",
        "[[account]]\nlabel = 'main'\nclass = 'dedicated'\nemail = 'owner@example.test'\n",
    ] {
        for report in ["owner@example.test", "other@example.test"] {
            let tmp = tmp_dir("codex-adopt-named-default");
            let env = env_at(&tmp);
            std::fs::create_dir_all(env.home.as_ref().unwrap().join(".codex")).unwrap();
            let path = tmp.path().join("accounts-codex.toml");
            std::fs::write(&path, original).unwrap();
            let host = PoolHost {
                account: Some(report),
            };
            let (code, out, err) = renew(
                "accounts adopt-default oai owner@example.test --cli codex",
                &env,
                &host,
            );
            if report == "owner@example.test" {
                assert_eq!(code, exit(0), "{original}: {out}{err}");
                assert!(
                    out.contains("completed this machine's own login as oai owner@example.test"),
                    "{original}: {out}"
                );
                assert!(!prints_word(&out, "main"), "{original}: {out}");
                let rows = Roster::load_for(Cli::Codex, &path).unwrap().rows;
                assert_eq!(rows.len(), 1, "{original}: no second row");
                assert_eq!(rows[0].label, "main");
                assert_eq!(rows[0].email.as_deref(), Some("owner@example.test"));
            } else {
                assert_eq!(code, exit(1), "CONTROL {original}: {out}{err}");
                assert!(err.contains("reports another account"), "CONTROL: {err}");
                assert_eq!(read(&path), original, "CONTROL");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn codex_stats_and_adoption_leave_non_file_credentials_untouched() {
    for configuration in [
        "cli_auth_credentials_store = 'keyring'",
        "cli_auth_credentials_store = 'auto'",
        "cli_auth_credentials_store = 'ephemeral'",
        "cli_auth_credentials_store = 'future'",
        "not TOML",
    ] {
        let tmp = tmp_dir("codex-non-file-stats");
        let env = env_at(&tmp);
        let host = PoolHost {
            account: Some("file-account@example.test"),
        };
        let path = tmp.path().join("accounts-codex.toml");
        let home = env.home.as_ref().unwrap().join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), configuration).unwrap();
        let (code, out, err) = renew("accounts adopt-default --cli codex", &env, &host);
        assert_eq!(code, exit(1), "{configuration}: {out}{err}");
        assert!(
            !path.exists(),
            "no earlier row can make this rejection pass"
        );
        std::fs::write(
            home.join("config.toml"),
            "cli_auth_credentials_store = 'file'",
        )
        .unwrap();
        let (code, out, err) = renew("accounts adopt-default --cli codex", &env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        let before = read(&path);
        std::fs::write(home.join("config.toml"), configuration).unwrap();
        let (code, out, err) = renew("accounts stats --cli codex", &env, &host);
        assert_eq!(code, exit(0), "{configuration}: {out}{err}");
        assert_eq!(read(&path), before);
    }
}

#[cfg(unix)]
#[test]
fn a_pools_stats_list_each_row_alike_as_text_and_json() {
    let (tmp, env) = kimi_env("stats-kimi");
    let host = PoolHost { account: None };
    let (text_code, text, err) = renew("accounts stats --cli kimi", &env, &host);
    let (json_code, json, _) = renew("accounts stats --cli kimi --json", &env, &host);
    assert_eq!(text_code, exit(0), "{err}");
    assert_eq!(json_code, text_code, "both forms exit alike");
    assert!(
        text.contains("Kimi Code CLI usage is read from its `/usage` panel")
            && text.contains("kimi kimi-one@example.com: not-enrolled")
            && text.contains("no reading yet"),
        "{text}"
    );
    let doc: Value = aterm_json::from_str(json.trim()).expect("json");
    let rows = doc["rows"].as_array().expect("rows");
    assert!(
        rows.iter()
            .any(|r| r["name"] == Value::from("kimi kimi-one@example.com")
                && r.get("label").is_none()),
        "each row by its account's name, never its label: {json}"
    );

    // Codex: the report asks no provider. It shows what a launch or a session last read, with its
    // age, and a row nobody read says so.
    listed_by_hand(
        &env,
        Cli::Codex,
        by_hand("codex-one", Some("codex-one@example.com")),
    );
    // `main`, the default home's row, is signed in here only while its login file is there.
    std::fs::create_dir_all(tmp.path().join("home/.codex")).expect("default home");
    std::fs::write(tmp.path().join("home/.codex/auth.json"), "{}").expect("default login");
    let reads = PoolHost { account: None };
    let (code, out, err) = renew("accounts stats --cli codex", &env, &reads);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        !out.contains("5h 0% left"),
        "CONTROL: nothing on record: {out}"
    );
    let aged = env.now - 7200;
    env.accounts_store_for(Cli::Codex)
        .expect("codex's store")
        .transact(env.now, &mut None, |state| {
            let mut events = accounts::note_sample(
                state,
                "main",
                accounts::Sample {
                    session_pct: Some(100),
                    session_reset: Some(env.now + 3600),
                    at: aged,
                    ..accounts::Sample::default()
                },
                aged,
            );
            events.extend(accounts::note_wall(
                state,
                "main",
                accounts::SESSION_LIMIT,
                Some(env.now + 3600),
                aged,
                aged,
            ));
            Ok(events)
        })
        .expect("a reading on record");
    let (code, out, err) = renew("accounts stats --cli codex", &env, &reads);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.contains("main (no address): limited")
            && out.contains("5h 0% left")
            && out.contains("read 2025-09-20T22:00:00Z")
            && out
                .contains("oai codex-one@example.com: not-enrolled · login idle · no reading yet"),
        "the reading stands with its age: {out}"
    );

    // Negative: with no home known, both forms exit 1.
    let mut homeless = env_at(&tmp);
    homeless.home = None;
    let (a, _, _) = renew("accounts stats --cli kimi", &homeless, &host);
    let (b, _, _) = renew("accounts stats --cli kimi --json", &homeless, &host);
    assert_eq!(
        (a.as_str(), b.as_str()),
        (exit(1).as_str(), exit(1).as_str())
    );
}

/// `ACCOUNTS BROWSER` opens the row's own profile (the one its headless approver signs in
/// through) at its region's sign-in page, and records only the page's host and how the window
/// ended. CONTROL: a mainland row opens kimi.com, and a window closed with nothing signed in
/// exits 1.
///
/// Unix only, as its row's `accounts add` queues for the machine's sign-in, which only Unix does
/// (this file's head); off Unix a run's own desk refuses `accounts browser` before its window
/// (`browser_window::no_window_desk_in`, which `headless_fetch`'s
/// `the_window_route_reads_the_desk_and_the_downloader` holds).
#[cfg(unix)]
#[test]
fn accounts_browser_opens_the_rows_own_profile_at_its_regions_page_and_records_no_secret() {
    let (tmp, env) = kimi_env("browser-kimi");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let host = SignInHost::desk(&home);
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    let browser = host.window.clone().expect("a window browser");
    assert_eq!(
        *host.opened.borrow(),
        [(browser.clone(), profile, "https://www.kimi.ai/".to_owned())]
    );
    assert!(
        out.contains("in Google Chrome for Testing")
            && out.contains("sign in there as kimi-one@example.com")
            && out.contains(&format!(
                "next: aterm harness accounts renew {} --cli kimi",
                step_word(&env, Cli::Kimi, "kimi-one")
            )),
        "{out}"
    );
    let ledger = read(&store.ledger_path());
    let line = ledger
        .lines()
        .find(|l| l.contains("window"))
        .unwrap_or_default();
    assert!(
        line.contains("sign-in") && line.contains("www.kimi.ai window done"),
        "{ledger}"
    );
    assert!(
        !ledger.contains('@') && !ledger.contains("https://"),
        "{ledger}"
    );
    assert_eq!(host.calls.get(), 0, "no CLI sign-in ran");

    listed_by_hand(
        &env,
        Cli::Kimi,
        kimi_by_hand(
            "kimi-cn",
            Some("kimi-cn@example.com"),
            Some(crate::ai::connector::Region::MainlandCn),
        ),
    );
    let (code, out, err) = renew(
        "accounts browser kimi-cn@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        host.opened.borrow().last().map(|o| o.2.as_str()),
        Some("https://www.kimi.com/")
    );
    let empty = SignInHost {
        ends: (crate::harness::browser_window::WindowEnd::Quit, false),
        ..SignInHost::desk(&tmp.path().join("other-home"))
    };
    let other_env = Env {
        home: Some(tmp.path().join("other-home")),
        state: tmp.path().join("other-state"),
        ..env_at(&tmp)
    };
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &other_env,
        &empty,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(out.contains("nothing was signed in"), "{out}");
}

/// `ACCOUNTS BROWSER` FETCHES ITS OWN WINDOW BROWSER where none is installed: the pinned Chrome for
/// Testing it fetched is the one the window opens in, and it says so first. NEGATIVE CONTROL: a
/// fetch that failed opens nothing, takes no sign-in lock and makes no profile, and says why with
/// what else works. CONTROL: with a window browser installed nothing is fetched; a row refused
/// for its own reasons (no email) fetches nothing either.
///
/// Unix only, as its row's `accounts add` queues for the machine's sign-in, which only Unix does
/// (this file's head); off Unix a run's own desk refuses `accounts browser` before its window
/// (`browser_window::no_window_desk_in`, which `headless_fetch`'s
/// `the_window_route_reads_the_desk_and_the_downloader` holds).
#[cfg(unix)]
#[test]
fn accounts_browser_fetches_its_own_window_browser_where_none_is_installed() {
    let (tmp, env) = kimi_env("browser-fetch");
    let home = tmp.path().join("home");
    let fetched = home.join(
        "Library/Caches/aterm/browser.noindex/chrome-0.0.0-mac-arm64-harness/chrome-mac-arm64/\
         Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    );
    let fetching = SignInHost {
        window: None,
        fetched: (Ok(fetched.clone()), Cell::new(0)),
        ..SignInHost::desk(&home)
    };
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &fetching,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(fetching.fetched.1.get(), 1);
    assert_eq!(
        fetching.opened.borrow().first().map(|o| o.0.clone()),
        Some(fetched.clone()),
        "the window opens in the fetched browser"
    );
    let says = out.find("aterm fetches the pinned Google Chrome for Testing");
    let opens = out.find(&format!(
        "opening the browser profile aterm keeps for {}",
        shown_of(&env, Cli::Kimi, "kimi-one")
    ));
    assert!(
        says.is_some() && says < opens && out.contains("held to its pinned size and sha256"),
        "{out}"
    );

    let other = tmp.path().join("other-home");
    let other_env = Env {
        home: Some(other.clone()),
        state: tmp.path().join("other-state"),
        ..env_at(&tmp)
    };
    let failing = SignInHost {
        window: None,
        ..SignInHost::desk(&other)
    };
    let other_store = other_env
        .accounts_store_for(Cli::Kimi)
        .expect("kimi's store");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &other_store.dir,
        &other_store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &other_env,
        &failing,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(failing.fetched.1.get(), 1);
    assert!(failing.opened.borrow().is_empty(), "nothing was opened");
    assert!(
        err.contains(
            "the pinned Google Chrome for Testing was not fetched: the host could not be \
             resolved (no network?)"
        ) && err.contains("Run this again to fetch it again")
            && err.contains("$ATERM_BROWSER")
            && err.ends_with("nothing was opened\n"),
        "{err}"
    );
    assert!(!profile.exists(), "no profile was made");
    let lock = other_store.signin_now().expect("lock");
    assert!(lock.is_some(), "the sign-in lock was let go");
    drop(lock);

    let installed = SignInHost::desk(&home);
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &installed,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        installed.fetched.1.get(),
        0,
        "CONTROL: one installed is opened"
    );
    assert!(!out.contains("fetches the pinned"), "{out}");
    listed_by_hand(
        &env,
        Cli::Kimi,
        kimi_by_hand(
            "kimi-bare",
            None,
            Some(crate::ai::connector::Region::Global),
        ),
    );
    let (code, _, err) = renew("accounts browser kimi-bare --cli kimi", &env, &fetching);
    assert_eq!(code, exit(1));
    assert!(err.contains("kimi-bare (no address) has no email"), "{err}");
    assert_eq!(fetching.fetched.1.get(), 1, "a refused row fetches nothing");
}

/// `ACCOUNTS BROWSER` STOPPED AS ITS FETCH PUT THE BUILD IN PLACE OPENS NOTHING: the fetch says
/// it was stopped once the pinned Chrome for Testing was in place (a Ctrl-C noted during the
/// rename), so no window opens, no sign-in lock is held and no profile is made, and it says the
/// build stays for the next run rather than that nothing was fetched. CONTROL: the same host's
/// fetch giving the build opens the window in it
/// (`accounts_browser_fetches_its_own_window_browser_where_none_is_installed`).
///
/// Unix only, as its row's `accounts add` queues for the machine's sign-in, which only Unix does
/// (this file's head); off Unix a run's own desk refuses `accounts browser` before its window
/// (`browser_window::no_window_desk_in`, which `headless_fetch`'s
/// `the_window_route_reads_the_desk_and_the_downloader` holds).
#[cfg(unix)]
#[test]
fn accounts_browser_stopped_as_its_fetch_placed_the_build_opens_nothing() {
    let (tmp, env) = kimi_env("browser-fetch-stopped");
    let home = tmp.path().join("home");
    let stopped = SignInHost {
        window: None,
        fetched: (
            Err(crate::harness::browser_window::STOPPED_IN_PLACE.to_owned()),
            Cell::new(0),
        ),
        ..SignInHost::desk(&home)
    };
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &stopped,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(stopped.fetched.1.get(), 1);
    assert!(stopped.opened.borrow().is_empty(), "nothing was opened");
    assert!(
        err.contains("stopped: the pinned Google Chrome for Testing was fetched and stays")
            && !err.contains("was not fetched")
            && err.ends_with("nothing was opened\n"),
        "{err}"
    );
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    assert!(!profile.exists(), "no profile was made");
    let lock = store.signin_now().expect("lock");
    assert!(lock.is_some(), "the sign-in lock was let go");
}

/// `ACCOUNTS BROWSER` HOLDS THE SIGNALS THAT END IT AND MARKS THE PROFILE until the pass after
/// the window has run: the window opens on a profile marked untidied, under one hold on the
/// signals that would end this process, which is still held through the pass after the window
/// and put back once the window is recorded; a pass that ran removes the mark. CONTROL: a pass
/// that failed leaves the mark, for the next headless launch on the profile to make up.
///
/// Unix only, as its row's `accounts add` queues for the machine's sign-in, which only Unix does
/// (this file's head); off Unix a run's own desk refuses `accounts browser` before its window
/// (`browser_window::no_window_desk_in`, which `headless_fetch`'s
/// `the_window_route_reads_the_desk_and_the_downloader` holds).
#[cfg(unix)]
#[test]
fn accounts_browser_holds_its_ends_through_the_pass_after_the_window_and_marks_it_until_then() {
    let (tmp, env) = kimi_env("browser-holds");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    let mark = profile.join(crate::harness::browser_window::UNTIDIED_MARK);
    let host = SignInHost::desk(&home);
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        host.marked_at_window.get(),
        Some(true),
        "marked before its window"
    );
    assert_eq!(
        host.at_tidy.get(),
        Some((1, true)),
        "one hold alive through the pass after the window, the mark still there"
    );
    assert_eq!(host.holds.get(), 0, "put back once recorded");
    assert!(!mark.exists(), "the pass ran, so the mark is gone");
    assert!(
        read(&store.ledger_path()).contains("www.kimi.ai window done"),
        "recorded"
    );

    let failing = SignInHost {
        tidy: Err("the headless browser did not start".to_owned()),
        ..SignInHost::desk(&home)
    };
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &failing,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(failing.at_tidy.get(), Some((1, true)));
    assert_eq!(failing.holds.get(), 0);
    assert!(
        mark.is_file(),
        "CONTROL: an untidied window leaves its mark"
    );
}

/// Run in place by [`a_window_hold_keeps_this_process_through_a_hangup_a_stop_and_a_quit`]
/// (`ATERM_TEST_WINDOW_ENDS` naming the file it writes): under a live [`WindowEnds`] hold it
/// raises SIGHUP, SIGTERM and SIGQUIT at itself, then opens the window's own wait inside that
/// hold, and writes whether the window's end was noted and what the window's wait first reads.
/// With `ATERM_TEST_WINDOW_ENDS_BARE` set it holds nothing and raises SIGHUP alone, at its
/// default action (no core signal, so no crash report). Anywhere else it is an empty test.
#[cfg(unix)]
#[test]
fn window_ends_stand_in() {
    use std::sync::atomic::Ordering;
    let Some(out) = std::env::var_os("ATERM_TEST_WINDOW_ENDS").map(PathBuf::from) else {
        return;
    };
    let bare = std::env::var_os("ATERM_TEST_WINDOW_ENDS_BARE").is_some();
    let held = (!bare).then(WindowEnds::hold);
    let raised: &[libc::c_int] = if bare {
        // SAFETY: SIGHUP's default action, set before it is raised.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_DFL) };
        &[libc::SIGHUP]
    } else {
        &[libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT]
    };
    for &signal in raised {
        #[expect(
            clippy::disallowed_methods,
            reason = "held, each signal runs a handler that only notes it; bare, SIGHUP alone at its \
                      default action, no core signal"
        )]
        // SAFETY: sends `signal` to this thread; a held one runs a handler that stores to an
        // atomic.
        unsafe {
            libc::raise(signal);
        }
    }
    let noted = WINDOW_INTERRUPTED.load(Ordering::SeqCst);
    let window = window_until_enter(|ended| ended());
    std::fs::write(&out, format!("survived noted={noted} window={window:?}")).expect("its line");
    drop(held);
    std::process::exit(0);
}

/// A LIVE HOLD ON A WINDOW'S ENDS KEEPS THIS PROCESS THROUGH A HANGUP, A STOP AND A QUIT: each
/// only notes the window's end, so `accounts browser` hangs its window up, tidies the profile and
/// records it when its terminal closes or it is told to stop, and one noted before the window
/// opens ends the window at once. CONTROL: without the hold the first of them, its terminal's
/// hangup, ends the stand-in before it writes a line.
#[cfg(unix)]
#[test]
fn a_window_hold_keeps_this_process_through_a_hangup_a_stop_and_a_quit() {
    use std::os::unix::process::ExitStatusExt as _;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let exe = std::env::current_exe().expect("test exe");
    let run = |name: &str, bare: bool| {
        let out = tmp.path().join(name);
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.args([
            "harness::cli::tests::signin::window_ends_stand_in",
            "--exact",
            "--nocapture",
        ])
        .env("ATERM_TEST_WINDOW_ENDS", &out)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
        if bare {
            cmd.env("ATERM_TEST_WINDOW_ENDS_BARE", "1");
        }
        let status = cmd.status().expect("the stand-in ran");
        (status, std::fs::read_to_string(&out).ok())
    };
    let (status, line) = run("held", false);
    assert!(status.success(), "{status:?}");
    assert_eq!(
        line.as_deref(),
        Some("survived noted=true window=Some(Interrupted)")
    );
    let (status, line) = run("bare", true);
    assert_eq!(status.signal(), Some(libc::SIGHUP), "CONTROL: {status:?}");
    assert_eq!(line, None, "CONTROL");
}

/// `ACCOUNTS BROWSER` opens nothing where its host's desk opens no window (a Linux session with no
/// display, [`AccountsHost::no_window_desk`]), without a terminal, over SSH, under another
/// sign-in's lock or on a profile another browser holds. The desk is refused first, before a
/// terminal or SSH is asked after, and none of the first three refusals fetches a browser where
/// none is installed. CONTROL: with all of that clear it opens one window, whatever display the
/// session running the test has. Unix alone: the held profile is planted as Chromium's
/// `SingletonLock` link, which only unix reads.
#[cfg(unix)]
#[test]
fn accounts_browser_refuses_off_a_desk_without_a_terminal_over_ssh_under_a_held_lock_or_an_open_profile()
 {
    let (tmp, env) = kimi_env("browser-refusals");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let line = "accounts browser kimi-one@example.com --cli kimi";
    // No browser installed, so a refusal that came after the fetch would count one.
    let no_desk = SignInHost {
        no_desk: crate::harness::browser_window::no_window_desk_in(true, true, &|_| None),
        terminal: false,
        ssh: true,
        window: None,
        ..SignInHost::desk(&home)
    };
    let no_terminal = SignInHost {
        terminal: false,
        window: None,
        ..SignInHost::desk(&home)
    };
    let over_ssh = SignInHost {
        ssh: true,
        window: None,
        ..SignInHost::desk(&home)
    };
    for (host, said) in [
        (&no_desk, "this session has no display"),
        (&no_terminal, "stdin is not a terminal"),
        (&over_ssh, "over SSH"),
    ] {
        let (code, out, err) = renew(line, &env, host);
        assert_eq!(code, exit(1), "{out}{err}");
        assert!(
            err.contains(said) && err.contains("nothing was opened"),
            "{err}"
        );
        assert!(host.opened.borrow().is_empty());
        assert_eq!(host.fetched.1.get(), 0, "{said}: nothing was fetched");
    }
    let held_by = SignInHost {
        cancel_on_queue: true,
        ..SignInHost::desk(&home)
    };
    let held = store.signin_within().expect("lock").expect("free");
    let (code, out, err) = renew(line, &env, &held_by);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(err.contains("cancelled"), "{err}");
    assert_eq!(
        held_by.queued.borrow().as_slice(),
        &[accounts::SigninPosition {
            ahead: 0,
            blocker: accounts::SigninBlocker::UnscheduledSignin,
        }]
    );
    assert!(held_by.opened.borrow().is_empty());
    drop(held);

    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    std::fs::create_dir_all(&profile).expect("profile");
    std::os::unix::fs::symlink("other-desk.example-4242", profile.join("SingletonLock"))
        .expect("lock");
    let open_elsewhere = SignInHost::desk(&home);
    let (code, out, err) = renew(line, &env, &open_elsewhere);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        err.contains(
            "its browser profile is locked by a browser of another machine (pid 4242 on \
             other-desk.example)"
        ) && err.contains("remove "),
        "{err}"
    );
    assert!(open_elsewhere.opened.borrow().is_empty());

    std::fs::remove_file(profile.join("SingletonLock")).expect("unlock");
    let clear = SignInHost::desk(&home);
    let (code, out, err) = renew(line, &env, &clear);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(clear.opened.borrow().len(), 1);
}

/// The file [`live_desk_stand_in`] writes its run's exit and standard error to.
#[cfg(target_os = "linux")]
const LIVE_DESK_ENV: &str = "ATERM_TEST_LIVE_DESK";

/// The live host's desk is this process's: a Linux session with no display opens nothing,
/// whatever display the machine running the test has. This test binary, run again with
/// `DISPLAY` and `WAYLAND_DISPLAY` removed ([`live_desk_stand_in`]), runs `accounts browser` on
/// [`LiveAccounts`] and is refused for having no display. CONTROL: with a `DISPLAY` named (one
/// nothing connects to) it passes the desk and is refused at the next step, its stdin, which is
/// no terminal.
#[cfg(target_os = "linux")]
#[test]
fn the_live_host_refuses_a_linux_session_with_no_display() {
    let tmp = tmp_dir("browser-live-desk");
    let exe = std::env::current_exe().expect("test exe");
    // The run gets a scratch home and an empty PATH, so a step past the desk (a window
    // browser found or fetched, the machine's sign-in lock) reads none of the person's.
    let home = tmp.path().join("home");
    let no_path = tmp.path().join("no-path");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&no_path).expect("no-path");
    let run = |name: &str, display: Option<&str>| {
        let said = tmp.path().join(name);
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.args([
            "harness::cli::tests::signin::live_desk_stand_in",
            "--exact",
            "--nocapture",
        ])
        .env(LIVE_DESK_ENV, &said)
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("PATH", &no_path)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
        if let Some(display) = display {
            cmd.env("DISPLAY", display);
        }
        let ran = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
            .expect("the stand-in ran");
        assert!(ran.status.success(), "{ran:?}");
        let said = read(&said);
        let (code, err) = said.split_once('\n').unwrap_or((said.as_str(), ""));
        (code.to_owned(), err.to_owned())
    };
    let headless = crate::harness::browser_window::no_window_desk_in(true, true, &|_| None)
        .expect("a Linux session with no display opens no window");
    let (code, err) = run("headless", None);
    assert_eq!(code, exit(1), "{err}");
    assert!(err.contains(headless), "{err}");
    let (code, err) = run("display", Some(":aterm-test-unconnected"));
    assert_eq!(code, exit(1), "CONTROL: {err}");
    assert!(
        !err.contains(headless) && err.contains("stdin is not a terminal"),
        "CONTROL: {err}"
    );
}

/// [`the_live_host_refuses_a_linux_session_with_no_display`]'s run: with [`LIVE_DESK_ENV`] set,
/// `accounts browser` of a scratch Kimi row on [`LiveAccounts`], its exit and standard error
/// written there. Anywhere else it is an empty test.
#[cfg(target_os = "linux")]
#[test]
fn live_desk_stand_in() {
    let Some(said) = std::env::var_os(LIVE_DESK_ENV).map(PathBuf::from) else {
        return;
    };
    let (_tmp, env) = kimi_env("browser-live-desk-run");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    // The row by its account's address: its label names nothing, as it has one.
    let code = run_accounts_browser(
        Cli::Kimi,
        &env,
        &LiveAccounts::default(),
        "kimi-one@example.com",
        &mut out,
        &mut err,
    );
    std::fs::write(
        &said,
        format!("{code:?}\n{}", String::from_utf8_lossy(&err)),
    )
    .expect("its run's words");
}

/// `ACCOUNTS BROWSER` opens nothing for a row with no email, a word that names no account (the
/// roster label of a row with an address among them), a Kimi row with no region (whose roster is
/// refused whole) or a machine with no window browser.
///
/// Each of these refusals comes before `accounts browser` asks its host's desk
/// ([`AccountsHost::no_window_desk`]), and off Unix a run's own desk refuses the row with none of
/// them before its window browser is looked for (`browser_window::no_window_desk_in`, which
/// `headless_fetch`'s `the_window_route_reads_the_desk_and_the_downloader` holds). Unix only, as
/// its rows' `accounts add` queues for the machine's sign-in, which only Unix does (this file's
/// head).
#[cfg(unix)]
#[test]
fn accounts_browser_refuses_no_email_no_region_no_window_browser_and_an_unknown_account() {
    let (tmp, env) = kimi_env("browser-rows");
    let home = tmp.path().join("home");
    listed_by_hand(&env, Cli::Kimi, kimi_by_hand("kimi-bare", None, None));
    let host = SignInHost::desk(&home);
    let (code, _, err) = renew("accounts browser kimi-bare --cli kimi", &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains("kimi-bare (no address) has no email"), "{err}");
    let (code, _, err) = renew("accounts browser nobody --cli kimi", &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains(&aterm_file::no_account("nobody")), "{err}");
    // A row with an address is named by its account, never by its label.
    let (code, _, err) = renew("accounts browser kimi-one --cli kimi", &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains(&aterm_file::no_account("kimi-one")), "{err}");
    let bare = SignInHost {
        window: None,
        ..SignInHost::desk(&home)
    };
    let (code, _, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &bare,
    );
    assert_eq!(code, exit(1));
    assert!(
        err.contains("no Chromium that opens a window is installed here")
            && err.contains("$ATERM_BROWSER"),
        "{err}"
    );
    assert!(host.opened.borrow().is_empty() && bare.opened.borrow().is_empty());

    let roster = tmp.path().join("accounts-kimi.toml");
    let mut text = read(&roster);
    text.push_str(
        "\n[[account]]\nlabel = \"kimi-nowhere\"\nclass = \"dedicated\"\n\
         email = \"kimi-nowhere@example.com\"\n",
    );
    std::fs::write(&roster, text).expect("roster");
    let (code, _, err) = renew(
        "accounts browser kimi-nowhere@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(1));
    assert!(err.contains("no region"), "{err}");
    assert!(host.opened.borrow().is_empty());
}

/// A POOL'S SIGN-IN VERBS TAKE AN ACCOUNT'S WORD AND REFUSE A ROW'S LABEL, UNDER `--cli`: Kimi
/// Code's row `m4-1`, whose address is `kai@x.example`, is reached by `accounts renew` from its
/// key, its provider word and address as two argv words, `kimi:<address>` and its address alone,
/// and each renewal names it by its name and types its key in the step it leaves (no lab slugs
/// it here); its label is refused by `renew` and by `login` with the resolver's no-account
/// words, and nothing runs. `login` from its key reaches the row (and stops, with no terminal).
/// Nothing either verb prints holds the label.
/// NEGATIVE CONTROL: with `run_accounts_renew_for` and `run_accounts_login_for` taking the row
/// labelled with the word first (`file_roster.row(word)`), `accounts renew m4-1 --cli kimi` runs
/// a sign-in and this fails on the label's refusal (2026-11-16, by hand, then restored).
#[cfg(unix)]
#[test]
fn a_pools_sign_in_verbs_take_each_account_word_and_refuse_a_rows_label() {
    let tmp = tmp_dir("signin-words-kimi");
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Kimi,
        kimi_by_hand(
            "m4-1",
            Some("kai@x.example"),
            Some(crate::ai::connector::Region::Global),
        ),
    );
    let home = tmp.path().join("home");
    let host = SignInHost::new(
        Script::Ends(login_drive::Outcome::NeedsYou(
            "no headless browser is installed here".to_owned(),
        )),
        &home,
    );
    let key = aterm_db::typed::keys::account_pk("kimi:global", "kai@x.example").expect("a key");
    let labelled = |text: &str| {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .any(|token| token == "m4-1")
    };
    for (at, word) in [
        key.as_str(),
        "kimi kai@x.example",
        "kimi:kai@x.example",
        "kai@x.example",
    ]
    .into_iter()
    .enumerate()
    {
        let (code, out, err) = renew(&format!("accounts renew {word} --cli kimi"), &env, &host);
        assert_eq!(code, exit(1), "`{word}`: {out}{err}");
        assert_eq!(
            host.calls.get(),
            at + 1,
            "`{word}` reached the row: {out}{err}"
        );
        assert!(
            out.contains("NEEDS YOU: approve the Kimi Code CLI sign-in for kimi kai@x.example")
                && out.contains(&format!("accounts login {key} --cli kimi")),
            "`{word}`: {out}"
        );
        assert!(!labelled(&out) && !labelled(&err), "`{word}`: {out}{err}");
    }
    let calls = host.calls.get();
    for verb in ["renew", "login"] {
        let (code, out, err) = renew(&format!("accounts {verb} m4-1 --cli kimi"), &env, &host);
        assert_eq!(code, exit(1), "{verb}: {out}{err}");
        assert!(
            err.contains(&aterm_file::no_account("m4-1")),
            "{verb}: {err}"
        );
        assert_eq!(host.calls.get(), calls, "{verb} of the label ran nothing");
    }
    let (code, out, err) = renew(&format!("accounts login {key} --cli kimi"), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(err.contains("not run: stdin is not a terminal"), "{err}");
    assert!(!err.contains(&aterm_file::no_account(&key)), "{err}");
    assert_eq!(
        host.calls.get(),
        calls,
        "login with no terminal ran nothing"
    );
}

/// A renewal whose page stops on the vendor's sign-in names `accounts browser` in its NEEDS YOU
/// line. CONTROL: a stop for any other reason keeps naming `accounts login`.
///
/// Unix only, as the overlay staging home it signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_renewal_finding_the_profile_signed_out_names_accounts_browser() {
    let (tmp, env) = kimi_env("browser-renew");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let signed_out = SignInHost::new(
        Script::Ends(login_drive::Outcome::NeedsSignIn(
            "the sign-in waits on `www.kimi.ai/code/authorize_device` for a person".to_owned(),
        )),
        &home,
    );
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &signed_out,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains(&format!(
            "NEEDS YOU: sign {} in once in its own browser profile at this machine's desk \
             (`aterm harness accounts browser {} --cli kimi` opens it); aterm's later \
             renewals approve its sign-in there by themselves",
            shown_of(&env, Cli::Kimi, "kimi-one"),
            step_word(&env, Cli::Kimi, "kimi-one")
        )),
        "{out}"
    );
    assert!(
        !out.contains("accounts renew"),
        "no renewal handed to a person: {out}"
    );
    assert!(!out.contains("accounts login"), "{out}");
    assert!(
        err.contains("it stopped on the vendor's sign-in page"),
        "{err}"
    );
    assert!(
        read(&store.ledger_path()).contains("www.kimi.ai device needs-sign-in"),
        "{}",
        read(&store.ledger_path())
    );

    let no_browser = SignInHost::new(
        Script::Ends(login_drive::Outcome::NeedsYou(
            "no headless browser is installed here".to_owned(),
        )),
        &home,
    );
    let (code, out, _) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &no_browser,
    );
    assert_eq!(code, exit(1));
    assert!(
        out.contains(&format!(
            "NEEDS YOU: approve the Kimi Code CLI sign-in for {}",
            shown_of(&env, Cli::Kimi, "kimi-one")
        )) && out.contains(&format!(
            "accounts login {} --cli kimi",
            step_word(&env, Cli::Kimi, "kimi-one")
        )) && !out.contains("accounts browser"),
        "{out}"
    );
}

/// A RENEWAL'S STOP NAMES THE ROW BY THE WORD ITS OWN LINES TYPE: under a lab that slugs Kimi
/// Code's `kimi-one` (`kimi-one-x`), the sign-in a renewal hands its host carries that slug
/// ([`Request::word`]), the word its `NEEDS YOU:` line types for the row, so the headless
/// approver's stop on the vendor's sign-in page types it too (`headless_vendor_tests`'
/// `the_window_and_the_headless_approver_open_one_profile_per_row` holds the approver to the
/// word it is handed), never the account's key beside the slug in the same run.
/// NEGATIVE CONTROL: with `SignInPlan::drive` handing its host no word (`word: None`), this
/// fails on the word the request carries (2026-11-16, by hand, then restored).
///
/// Unix only, as the overlay staging home it signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_renewals_sign_in_carries_the_word_its_lines_type_for_the_row() {
    let (tmp, env) = kimi_env("browser-renew-word");
    let home = tmp.path().join("home");
    let email = "kimi-one@example.com";
    let realm = "kimi:global";
    let pk = aterm_db::typed::keys::account_pk(realm, email).expect("a key");
    let lab = crate::harness::alab_roster::Lab {
        names: [(
            pk,
            aterm_file::AccountNames {
                realm: realm.to_owned(),
                slug: "kimi-one-x".to_owned(),
                name: aterm_db::typed::keys::account_name(realm, email),
                email: Some(email.to_owned()),
                login: Some(email.to_owned()),
                live: true,
            },
        )]
        .into(),
        ..crate::harness::alab_roster::Lab::default()
    };
    let carried = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&carried);
    let host = SignInHost {
        during: Some(Box::new(move |req: &Request<'_>| {
            seen.borrow_mut()
                .push(req.word.map(|word| word.as_str().to_owned()));
        })),
        ..SignInHost::new(
            Script::Ends(login_drive::Outcome::NeedsSignIn(
                "the sign-in waits on `www.kimi.ai/code/authorize_device` for a person".to_owned(),
            )),
            &home,
        )
    };
    let (code, out, err) = crate::harness::alab_roster::with_lab(lab, || {
        renew("accounts renew kimi-one-x --cli kimi", &env, &host)
    });
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(*carried.borrow(), [Some("kimi-one-x".to_owned())]);
    assert!(
        out.contains("(`aterm harness accounts browser kimi-one-x --cli kimi` opens it)"),
        "{out}"
    );
}

/// A GROK ROW IS SIGNED IN BY `accounts login`, NEVER A RENEWAL: accounts.x.ai answers the
/// headless browser with a block page, so `add`'s `run:` step, `accounts --cli grok` and
/// `accounts stats --cli grok` for a row that needs a sign-in (before any renewal stopped for it:
/// the window's upkeep starts none), `browser`'s `next:` step and a renewal that found the
/// profile signed out (and the task it records) each name `accounts login <account> --cli grok`,
/// and none promises that a renewal approves the page. CONTROLS: a Kimi row's `add` names
/// `renew`, its report and stats name no sign-in step (aterm signs it in by itself), its
/// signed-out renewal names `accounts browser` then `renew`, and a stored browser task names
/// that on Kimi and the login on Grok.
#[cfg(unix)]
#[test]
fn a_grok_rows_next_steps_name_its_login_never_a_renewal() {
    let tmp = tmp_dir("grok-next-steps");
    let home = tmp.path().join("home");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (code, out, err) = run_line("accounts add xai grok-one@example.com --cli grok", &env);
    assert_eq!(code, exit(0), "{err}");
    let grok_one =
        crate::harness::account_names::tests::added_label(Cli::Grok, "grok-one@example.com");
    let kimi_one =
        crate::harness::account_names::tests::added_label(Cli::Kimi, "kimi-one@example.com");
    assert!(
        out.contains(&format!(
            "run: aterm harness accounts login {} --cli grok",
            step_word(&env, Cli::Grok, &grok_one)
        )),
        "{out}"
    );
    let (code, out, err) = run_line("accounts add kimi kimi-one@example.com --cli kimi", &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.contains(&format!(
            "run: aterm harness accounts renew {} --cli kimi",
            step_word(&env, Cli::Kimi, &kimi_one)
        )),
        "CONTROL: {out}"
    );
    let pool = PoolHost { account: None };
    for (cli, line) in [
        (Cli::Grok, "accounts --cli grok"),
        (Cli::Grok, "accounts stats --cli grok"),
        (Cli::Kimi, "accounts --cli kimi"),
        (Cli::Kimi, "accounts stats --cli kimi"),
    ] {
        let (code, out, err) = renew(line, &env, &pool);
        assert_eq!(code, exit(0), "{line}: {out}{err}");
        if cli == Cli::Grok {
            assert!(
                out.contains(&format!(
                    "sign-in needed: Run aterm harness accounts login {} --cli grok",
                    step_word(&env, Cli::Grok, &grok_one)
                )),
                "{line}: {out}"
            );
        } else {
            assert!(!out.contains("sign-in needed"), "CONTROL: {line}: {out}");
        }
        // The JSON listing and stats say the same: the step, and (the listing) that the row
        // itself renews by itself only where its CLI's does.
        let stats = line.contains("stats");
        let line = format!("{line} --json");
        let (code, out, err) = renew(&line, &env, &pool);
        assert_eq!(code, exit(0), "{line}: {out}{err}");
        let needed = if cli == Cli::Grok {
            format!(
                "\"sign_in_needed\":\"Run aterm harness accounts login {} --cli grok\"",
                step_word(&env, Cli::Grok, &grok_one)
            )
        } else {
            "\"sign_in_needed\":null".to_owned()
        };
        assert!(out.contains(&needed), "{line}: {out}");
        if !stats {
            let label = if cli == Cli::Grok {
                grok_one.as_str()
            } else {
                kimi_one.as_str()
            };
            let doc: Value = aterm_json::from_str(out.trim()).expect("json");
            let row = crate::harness::account_names::tests::report_row(cli, &env, label, &doc)
                .unwrap_or_else(|| panic!("{line}: no {label} row: {out}"));
            assert_eq!(
                row["renews"].as_bool(),
                Some(cli != Cli::Grok),
                "{line}: {label}'s own row: {out}"
            );
        }
    }

    let desk = SignInHost::desk(&home);
    let (code, out, err) = renew(
        "accounts browser grok-one@example.com --cli grok",
        &env,
        &desk,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(desk.opened.borrow().len(), 1, "one window");
    assert!(
        out.contains(&format!(
            "next: aterm harness accounts login {} --cli grok",
            step_word(&env, Cli::Grok, &grok_one)
        )) && !out.contains("accounts renew")
            && !out.contains("the renewal checks it")
            && !out.contains("headlessly"),
        "{out}"
    );

    let store = env.accounts_store_for(Cli::Grok).expect("grok's store");
    let signed_out = SignInHost::new(
        Script::Ends(login_drive::Outcome::NeedsSignIn(
            "the sign-in waits on `accounts.x.ai/oauth2/device` for a person".to_owned(),
        )),
        &home,
    );
    let (code, out, err) = renew(
        "accounts renew grok-one@example.com --cli grok",
        &env,
        &signed_out,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains(&format!(
            "NEEDS YOU: approve the Grok Build sign-in for {}: run `aterm harness accounts login \
             {} --cli grok`",
            shown_of(&env, Cli::Grok, &grok_one),
            step_word(&env, Cli::Grok, &grok_one)
        )) && !out.contains("accounts browser"),
        "{out}"
    );
    let task = |store: &accounts::Store, label: &str| {
        store
            .load_logins(env.now)
            .expect("logins")
            .0
            .get(label)
            .and_then(|b| b.sign_in_task)
            .map(|t| t.action)
    };
    assert_eq!(task(&store, &grok_one), Some(accounts::SignInAction::Login));

    // The Kimi CONTROL signs in to an overlay staging home, which aterm links only on Unix
    // (`a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links`).
    let kimi = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    if cfg!(unix) {
        let (code, out, _) = renew(
            "accounts renew kimi-one@example.com --cli kimi",
            &env,
            &signed_out,
        );
        assert_eq!(code, exit(1), "CONTROL: {out}");
        assert!(
            out.contains(&format!(
                "accounts browser {} --cli kimi",
                step_word(&env, Cli::Kimi, &kimi_one)
            )) && out.contains("aterm's later renewals approve its sign-in there by themselves"),
            "CONTROL: {out}"
        );
        assert_eq!(
            task(&kimi, &kimi_one),
            Some(accounts::SignInAction::Browser)
        );
    }

    // Each step names the row by its account's word, `accounts browser`'s too.
    let word =
        |cli, label| crate::harness::account_names::tests::row_name_of(cli, &env, label).word;
    assert_eq!(
        accounts::SignInAction::Browser.next_step(Cli::Grok, &word(Cli::Grok, &grok_one)),
        format!(
            "Run aterm harness accounts login {} --cli grok",
            word(Cli::Grok, &grok_one)
        )
    );
    assert_eq!(
        accounts::SignInAction::Browser.next_step(Cli::Kimi, &word(Cli::Kimi, &kimi_one)),
        format!(
            "Run aterm harness accounts browser {0} --cli kimi, then aterm harness accounts renew \
             {0} --cli kimi",
            word(Cli::Kimi, &kimi_one)
        ),
        "CONTROL"
    );
}

/// A renewal whose page met Cloudflare's bot check ends as a failure on
/// record naming Cloudflare, with the account's check, a durable human login task and no NEEDS
/// YOU prompt. A later renewal with nobody at the terminal runs nothing and names the person's
/// step; a person's renewal that fails for another reason replaces neither the task nor the
/// check.
///
/// Unix only, as the overlay staging home it signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_renewal_on_a_bot_check_records_a_quiet_human_login_task() {
    let (tmp, env) = kimi_env("bot-check-renew");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let checked = SignInHost::new(
        Script::Ends(login_drive::Outcome::BotCheck(
            "the sign-in page `www.kimi.ai/code/authorize_device` carries Cloudflare's bot check; \
             aterm pressed nothing there and never tries to pass it"
                .to_owned(),
        )),
        &home,
    );
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &checked,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(!out.contains("NEEDS YOU"), "{out}");
    assert!(
        err.contains(
            "the sign-in page `www.kimi.ai/code/authorize_device` carries Cloudflare's bot check"
        ) && !err.contains("person"),
        "{err}"
    );
    let ledger = read(&store.ledger_path());
    assert!(ledger.contains("www.kimi.ai device bot-check"), "{ledger}");
    let roster_path = tmp.path().join("accounts-kimi.toml");
    let records = store
        .read_account_records(Some(&roster_path), env.now)
        .unwrap();
    let tasks = accounts::sign_in_tasks(&records.raw_roster, &records.logins);
    assert_eq!(tasks, [("kimi-one".into(), accounts::SignInAction::Login)]);
    assert!(out.contains("human browser challenge recorded"), "{out}");
    assert!(
        out.contains(&format!(
            "accounts login {} --cli kimi",
            step_word(&env, Cli::Kimi, "kimi-one")
        )),
        "{out}"
    );
    let before = records.logins.get("kimi-one").cloned();
    assert!(
        before
            .as_ref()
            .is_some_and(|binding| binding.check_stop.is_some()),
        "the account's check is on record"
    );
    // With nobody at this terminal, the row the check stopped is not signed in again, on any
    // clock: the run names the person's step and changes nothing.
    let failed = SignInHost::new(Script::Ends(login_drive::Outcome::Failed), &home);
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &failed,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        err.contains("not run: its last sign-in stopped where only a person at this Mac can go on")
            && err.contains(&format!(
                "a person runs `aterm harness accounts login {} --cli kimi`",
                step_word(&env, Cli::Kimi, "kimi-one")
            )),
        "{err}"
    );
    assert_eq!(
        store.load_logins(env.now).unwrap().0.get("kimi-one"),
        before.as_ref()
    );
    // A person at this terminal runs it, and an unrelated failure replaces neither the task nor
    // the check.
    let failed = SignInHost {
        terminal: true,
        ..SignInHost::new(Script::Ends(login_drive::Outcome::Failed), &home)
    };
    let (_, out, _) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &failed,
    );
    assert!(!out.contains("human browser challenge"), "{out}");
    assert!(!out.contains("NEEDS YOU"), "{out}");
    let after = store.load_logins(env.now).unwrap().0;
    let after = after.get("kimi-one");
    assert_eq!(
        after.map(|binding| (binding.sign_in_task, binding.check_stop.clone())),
        before
            .as_ref()
            .map(|binding| (binding.sign_in_task, binding.check_stop.clone()))
    );

    let signed_out = SignInHost {
        terminal: true,
        ..SignInHost::new(
            Script::Ends(login_drive::Outcome::NeedsSignIn(
                "the sign-in waits on `www.kimi.ai/code/authorize_device` for a person".to_owned(),
            )),
            &home,
        )
    };
    let (_, out, _) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &signed_out,
    );
    assert!(out.contains("NEEDS YOU"), "CONTROL: {out}");
    assert!(
        read(&store.dir.join("logins.json")).contains("sign_in_task"),
        "CONTROL: a person's stop notes a task"
    );
}

/// A RENEWAL ON A BOT CHECK THAT COULD NOT GO ON RECORD STILL ENDS AS A BOT CHECK, which the
/// background holds as long as a stop for a person (design R-13), not as a plain failure that only
/// the 15-minute cooldown holds: with no line on record a restart has nothing to hold the row by,
/// so its end is all there is. Its record fails where the binding moved during the sign-in, or
/// where the ledger cannot be written. CONTROLS: a plain failure so unrecorded ends `Failed`, and
/// a bot check on record ends `BotCheck` with its line there. Each renewal is a person's at this
/// Mac's terminal, the one door the check an earlier one left on record does not hold
/// (`cli::person_at_terminal`).
#[cfg(unix)]
#[test]
fn an_unrecorded_bot_check_still_ends_as_a_bot_check() {
    let (tmp, env) = kimi_env("unrecorded-bot-check");
    let home = tmp.path().join("home");
    let roster_path = tmp.path().join("accounts-kimi.toml");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let roster = accounts::Roster::parse_for(Cli::Kimi, &read(&roster_path)).expect("the roster");
    let row = roster.row("kimi-one").expect("the row").clone();
    let bot_check = || {
        login_drive::Outcome::BotCheck(
            "the sign-in page `www.kimi.ai/code/authorize_device` carries Cloudflare's bot check; \
             aterm pressed nothing there and never tries to pass it"
                .to_owned(),
        )
    };
    let renewal = |outcome: login_drive::Outcome, during: Option<During>| {
        let host = SignInHost {
            during,
            terminal: true,
            ..SignInHost::new(Script::Ends(outcome), &home)
        };
        let logins = store.load_logins(env.now).expect("logins").0;
        renew_one_for(
            Cli::Kimi,
            0,
            &host,
            &store,
            (&row, &roster_path, &logins),
            (
                row.dir.as_deref(),
                crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, &row.label),
            ),
            &mut Vec::new(),
            &mut Vec::new(),
        )
    };
    // The binding moves under the sign-in: another login is bound to the row meanwhile.
    let moves = || -> Option<During> {
        Some(Box::new(|req: &Request<'_>| {
            let dir = req.staging.to_string_lossy().into_owned();
            req.store
                .transact_logins_within(
                    &accounts::LOCK_LADDER,
                    1_758_412_900,
                    &mut None,
                    |_, logins| {
                        logins.labels.insert(
                            "kimi-one".to_owned(),
                            accounts::Binding {
                                dir: Some(dir),
                                ..accounts::Binding::default()
                            },
                        );
                        Ok(Vec::new())
                    },
                )
                .expect("the binding moves");
        }))
    };
    let bot_checks_on_record = || read(&store.ledger_path()).matches(" bot-check").count();
    assert_eq!(
        renewal(bot_check(), moves()),
        Renewal::BotCheck,
        "binding moved"
    );
    assert_eq!(bot_checks_on_record(), 0, "the bot check is not on record");
    assert_eq!(
        renewal(login_drive::Outcome::Failed, moves()),
        Renewal::Failed,
        "CONTROL: a plain failure, unrecorded"
    );
    // Where the last sign-in staged its login: a renewal removes it only once its end is on
    // record, so a staging directory still there says the end never went on record.
    let staged = std::rc::Rc::new(RefCell::new(PathBuf::new()));
    let noting = || -> Option<During> {
        let staged = std::rc::Rc::clone(&staged);
        Some(Box::new(move |req: &Request<'_>| {
            *staged.borrow_mut() = req.staging.to_path_buf();
        }))
    };
    assert_eq!(renewal(bot_check(), noting()), Renewal::BotCheck, "CONTROL");
    assert_eq!(bot_checks_on_record(), 1, "CONTROL: its line is on record");
    assert!(
        !staged.borrow().exists(),
        "CONTROL: on record, its staging directory is removed"
    );

    // The ledger cannot be written: a directory stands where it is.
    let ledger = store.ledger_path();
    let unwritable = || -> Option<During> {
        let ledger = ledger.clone();
        let staged = std::rc::Rc::clone(&staged);
        Some(Box::new(move |req: &Request<'_>| {
            *staged.borrow_mut() = req.staging.to_path_buf();
            std::fs::rename(&ledger, ledger.with_extension("kept")).expect("the ledger moves");
            std::fs::create_dir_all(&ledger).expect("a directory at the ledger");
        }))
    };
    assert_eq!(
        renewal(bot_check(), unwritable()),
        Renewal::BotCheck,
        "ledger unwritable"
    );
    assert!(
        staged.borrow().is_dir(),
        "not on record, its staging directory is kept: {}",
        staged.borrow().display()
    );
}

/// `ACCOUNTS BROWSER` with no `--cli` serves Claude Code's pool: claude.ai's sign-in page, in the
/// profile its headless opener keys on. CONTROL: the record lands in Claude Code's own ledger.
///
/// Unix only, as its row's `accounts add` queues for the machine's sign-in, which only Unix does
/// (this file's head); off Unix a run's own desk refuses `accounts browser` before its window
/// (`browser_window::no_window_desk_in`, which `headless_fetch`'s
/// `the_window_route_reads_the_desk_and_the_downloader` holds).
#[cfg(unix)]
#[test]
fn accounts_browser_serves_claude_codes_pool() {
    let tmp = tmp_dir("browser-claude");
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Claude,
        by_hand("work-1", Some("work-1@example.com")),
    );
    let host = SignInHost::desk(&tmp.path().join("home"));
    let (code, out, err) = renew("accounts browser work-1@example.com", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let store = env.accounts_store();
    let profile = crate::harness::headless_oauth::resolve_browser_profile_dir(
        &store.dir,
        Some("work-1"),
        Some("work-1@example.com"),
    );
    assert_eq!(
        host.opened
            .borrow()
            .first()
            .map(|o| (o.1.clone(), o.2.clone())),
        Some((profile, "https://claude.ai/login".to_owned()))
    );
    assert!(
        out.contains(&format!(
            "next: aterm harness accounts renew {}   (",
            step_word(&env, Cli::Claude, "work-1")
        )) && out.contains("aterm cannot see from here whether claude.ai signed you in"),
        "{out}"
    );
    assert_eq!(
        *host.tidied.borrow(),
        [(host.opened.borrow()[0].1.clone(), None::<(String, String)>)],
        "no session key is asked after where the vendor's is not known"
    );
    assert!(
        read(&store.ledger_path()).contains("claude.ai window done session-unknown"),
        "{}",
        read(&store.ledger_path())
    );
}

/// `accounts browser` takes one account word, with or without `--cli`: its provider word and
/// address as two argv words are one name. Two words that are not one name, a flag it does not
/// take and none at all are refused, saying how many words it was given and never repeating them.
#[test]
fn accounts_browser_takes_one_account() {
    assert_eq!(
        parsed("accounts browser grok-1 --cli grok"),
        Ok(Cmd::AccountsOf(AccountsOf {
            cli: Cli::Grok,
            form: AccountsCmd::Browser {
                account: "grok-1".to_owned()
            },
            vendor: accounts::Vendor::default(),
        }))
    );
    assert_eq!(
        parsed("accounts browser claude ada@x.example"),
        Ok(Cmd::Accounts(AccountsCmd::Browser {
            account: "claude ada@x.example".to_owned()
        }))
    );
    for line in [
        "accounts browser",
        "accounts browser a b",
        "accounts browser a --json",
    ] {
        assert!(parsed(line).is_err(), "{line}");
    }
    let why = parsed("accounts browser ada@x.example bea@x.example").unwrap_err();
    assert!(
        why.starts_with("accounts browser takes one account: its slug, its key, or its provider word and address")
            && why.ends_with("not 2 words")
            && !why.contains("bea@"),
        "{why}"
    );
}

/// `ACCOUNTS BROWSER` judges the sign-in by the vendor's own session, never by a cookie store:
/// a profile a headless renewal already used (it has `Default/Cookies`) whose window closed with
/// nothing signed in exits 1, "nothing was signed in there", and names the window again. The
/// pass after the window asks kimi.ai's `access_token`, after the password manager was turned
/// off, and its dropped identity-provider cookies are counted. CONTROLS: the session kept exits 0
/// and names `renew`; a pass that could not run exits 1 and says the identity provider's sign-in
/// may be left.
///
/// Unix only, as its row's `accounts add` queues for the machine's sign-in, which only Unix does
/// (this file's head); off Unix a run's own desk refuses `accounts browser` before its window
/// (`browser_window::no_window_desk_in`, which `headless_fetch`'s
/// `the_window_route_reads_the_desk_and_the_downloader` holds).
#[cfg(unix)]
#[test]
fn accounts_browser_judges_the_sign_in_by_the_vendors_session_never_a_cookie_store() {
    use crate::harness::browser_window::WindowEnd;
    let (tmp, env) = kimi_env("browser-session");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    std::fs::create_dir_all(profile.join("Default")).expect("a used profile");
    std::fs::write(profile.join("Default/Cookies"), "used by a renewal").expect("cookies");
    let line = "accounts browser kimi-one@example.com --cli kimi";
    let empty = SignInHost {
        ends: (WindowEnd::Done, false),
        tidy: Ok(3),
        ..SignInHost::desk(&home)
    };
    let (code, out, err) = renew(line, &env, &empty);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains("3 identity providers' cookies were removed from it")
            && out.contains("www.kimi.ai keeps no session in it, so nothing was signed in there")
            && out.contains(&format!(
                "next: sign in there, then press Enter: aterm harness accounts browser {} --cli \
                 kimi",
                step_word(&env, Cli::Kimi, "kimi-one")
            ))
            && !out.contains("accounts renew"),
        "{out}"
    );
    assert!(
        out.contains("save no password there") && out.contains("removes the cookies of Google")
    );
    assert_eq!(
        *empty.tidied.borrow(),
        [(
            profile.clone(),
            Some(("https://www.kimi.ai".to_owned(), "access_token".to_owned()))
        )]
    );
    let prefs = read(&profile.join("Default/Preferences"));
    assert!(
        prefs.contains("\"credentials_enable_service\":false"),
        "{prefs}"
    );
    assert!(read(&store.ledger_path()).contains("www.kimi.ai window done no-session"));

    let kept = SignInHost::desk(&home);
    let (code, out, err) = renew(line, &env, &kept);
    assert_eq!(code, exit(0), "CONTROL: {out}{err}");
    assert!(
        out.contains(
            "it holds no identity provider's cookies; www.kimi.ai keeps its session in it"
        ) && out.contains(&format!(
            "next: aterm harness accounts renew {} --cli kimi",
            step_word(&env, Cli::Kimi, "kimi-one")
        )),
        "{out}"
    );

    let untidied = SignInHost {
        tidy: Err("the browser did not start".to_owned()),
        ..SignInHost::desk(&home)
    };
    let (code, out, err) = renew(line, &env, &untidied);
    assert_eq!(code, exit(1), "CONTROL: {out}{err}");
    assert!(
        err.contains("could not be opened again to remove the identity providers' cookies")
            && err.contains("a Google or other sign-in may be left in it")
            && err.contains(&format!(
                "run `aterm harness accounts browser {} --cli kimi` again",
                step_word(&env, Cli::Kimi, "kimi-one")
            )),
        "{err}"
    );
    assert!(!out.contains("next: aterm harness accounts renew"), "{out}");
    assert!(read(&store.ledger_path()).contains("www.kimi.ai window done untidied"));
}

/// `ACCOUNTS BROWSER` on a profile it makes marks it unproven, as a headless sign-in's fresh
/// profile is, until a sign-in there is kept: a window closed with nothing signed in leaves a
/// profile the sweep counts as signed in to nothing (`observe::sweep::browser_logins`) and no
/// renewal takes as proven. CONTROL: the session kept removes the mark.
/// NEGATIVE CONTROL: with the verb making the profile unmarked, `` assertion failed: …
/// is_file() ``.
#[cfg(unix)]
#[test]
fn accounts_browser_leaves_a_profile_it_made_unproven_until_a_sign_in_is_kept() {
    use crate::harness::{browser_window::WindowEnd, headless_oauth::PROFILE_UNPROVEN_MARK};
    let (tmp, env) = kimi_env("browser-unproven");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    assert!(!profile.exists());
    let line = "accounts browser kimi-one@example.com --cli kimi";
    let empty = SignInHost {
        ends: (WindowEnd::Done, false),
        ..SignInHost::desk(&home)
    };
    let (code, out, err) = renew(line, &env, &empty);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(profile.join(PROFILE_UNPROVEN_MARK).is_file());
    let kept = SignInHost::desk(&home);
    let (code, out, err) = renew(line, &env, &kept);
    assert_eq!(code, exit(0), "CONTROL: {out}{err}");
    assert!(!profile.join(PROFILE_UNPROVEN_MARK).exists());
}

/// `ACCOUNTS BROWSER` opens nothing on a profile another of aterm's launches holds (a headless
/// renewal's shell writes no `SingletonLock`), naming it by its pid. CONTROL: released, it opens.
#[cfg(unix)]
#[test]
fn accounts_browser_opens_nothing_on_a_profile_a_headless_sign_in_holds() {
    let (tmp, env) = kimi_env("browser-held");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let profile = crate::harness::headless_oauth::row_browser_profile(
        Cli::Kimi,
        &store.dir,
        &store.shared_dir(),
        "kimi-one",
        Some("kimi-one@example.com"),
    );
    std::fs::create_dir_all(&profile).expect("profile");
    let held = crate::harness::browser_window::ProfileLock::claim(&profile).expect("free");
    let host = SignInHost::desk(&home);
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        err.contains(&format!(
            "its browser profile is in use by another of aterm's sign-ins (pid {})",
            std::process::id()
        )) && err.contains("nothing was opened"),
        "{err}"
    );
    assert!(host.opened.borrow().is_empty());
    drop(held);
    let (code, out, err) = renew(
        "accounts browser kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "CONTROL: {out}{err}");
    assert_eq!(host.opened.borrow().len(), 1);
}

/// A renewal of another CLI that stopped where only a person can go on (its page waits on one,
/// a block page, its time ran out) is `NeedsPerson`, and one that met Cloudflare's bot check is
/// `BotCheck`, which the background's backoff keys on alike. CONTROL: a sign-in the CLI said
/// failed is a plain failure.
///
/// Unix only, as the overlay staging home its Kimi row signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_renewal_that_stopped_for_a_person_says_so() {
    let (tmp, env) = kimi_env("renew-needs-person");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let roster =
        accounts::Roster::parse_for(Cli::Kimi, &read(&tmp.path().join("accounts-kimi.toml")))
            .expect("kimi's roster");
    let row = roster.row("kimi-one").expect("the row").clone();
    let renewal = |outcome: login_drive::Outcome| {
        let host = SignInHost::new(Script::Ends(outcome), &home);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        renew_one_for(
            Cli::Kimi,
            0,
            &host,
            &store,
            (
                &row,
                &tmp.path().join("accounts-kimi.toml"),
                &store.load_logins(env.now).unwrap().0,
            ),
            (
                row.dir.as_deref(),
                crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, &row.label),
            ),
            &mut out,
            &mut err,
        )
    };
    for outcome in [
        login_drive::Outcome::NeedsSignIn("the sign-in waits on a person".to_owned()),
        login_drive::Outcome::NeedsYou("a bot-protection block page".to_owned()),
        login_drive::Outcome::Hung,
    ] {
        assert_eq!(
            renewal(outcome.clone()),
            Renewal::NeedsPerson,
            "{outcome:?}"
        );
    }
    assert_eq!(
        renewal(login_drive::Outcome::BotCheck(
            "the sign-in page `www.kimi.ai/code/authorize_device` carries Cloudflare's bot check; \
             aterm pressed nothing there and never tries to pass it"
                .to_owned()
        )),
        Renewal::BotCheck
    );
    assert_eq!(
        renewal(login_drive::Outcome::Failed),
        Renewal::Failed,
        "CONTROL"
    );
}

/// Unix only, as the overlay staging home its Kimi row signs in to is; off Unix,
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` holds the refusal.
#[cfg(unix)]
#[test]
fn a_background_renewal_keeps_its_human_step_when_both_output_streams_are_discarded() {
    let (tmp, env) = kimi_env("background-sign-in-task");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("store");
    let roster =
        accounts::Roster::parse_for(Cli::Kimi, &read(&tmp.path().join("accounts-kimi.toml")))
            .expect("roster");
    let row = roster.row("kimi-one").expect("row");
    let host = SignInHost::new(
        Script::Ends(login_drive::Outcome::NeedsSignIn(
            "private vendor text".into(),
        )),
        &home,
    );
    assert_eq!(
        renew_one_for(
            Cli::Kimi,
            0,
            &host,
            &store,
            (
                row,
                &tmp.path().join("accounts-kimi.toml"),
                &store.load_logins(env.now).unwrap().0
            ),
            (
                row.dir.as_deref(),
                crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, &row.label)
            ),
            &mut io::sink(),
            &mut io::sink()
        ),
        Renewal::NeedsPerson
    );
    let (logins, _) = env
        .accounts_store_for(Cli::Kimi)
        .expect("reopen")
        .load_logins(env.now)
        .expect("reload");
    assert_eq!(
        accounts::sign_in_tasks(&roster, &logins),
        [("kimi-one".into(), accounts::SignInAction::Browser)]
    );
    let (_, text, _) = run_line("accounts --cli kimi", &env);
    assert!(
        text.contains(&format!(
            "accounts browser {} --cli kimi",
            step_word(&env, Cli::Kimi, "kimi-one")
        )),
        "{text}"
    );
    let (_, json, _) = run_line("accounts --cli kimi --json", &env);
    assert!(
        json.contains("sign_in_task") && json.contains("browser"),
        "{json}"
    );
    assert!(!read(&store.logins_file()).contains("private vendor text"));
    let host = SignInHost::new(Script::Kimi("kimi 0"), &home);
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let (logins, _) = store.load_logins(env.now).expect("read success");
    assert!(accounts::sign_in_tasks(&roster, &logins).is_empty());
}

#[cfg(unix)]
#[test]
fn codex_signin_binds_auth_path_overlay_suffixes_reconcile_and_due_renewals_order_ready_rows_first()
{
    struct OrderHost {
        order: std::cell::RefCell<Vec<String>>,
    }
    impl AccountsHost for OrderHost {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            Err("unused".to_owned())
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            Err("unused".to_owned())
        }
        fn now(&self) -> i64 {
            1_758_412_900
        }
        fn vendor_sign_in(&self, req: &Request<'_>) -> Drove {
            self.order.borrow_mut().push(req.row.label.clone());
            if req.cli == Cli::Codex {
                let jwt = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
                    req.row.email.as_deref(),
                    "pro",
                    1_900_000_000,
                );
                let doc = format!(
                    r#"{{"auth_mode":"chatgpt","tokens":{{"id_token":"{jwt}","access_token":"{jwt}","refresh_token":"rt"}}}}"#
                );
                std::fs::write(req.staging.join("auth.json"), doc).expect("auth.json");
                Drove {
                    outcome: login_drive::Outcome::SignedIn {
                        email: req.row.email.clone(),
                        page: true,
                    },
                    host: Some("auth.openai.com".to_owned()),
                }
            } else {
                let cred = req.staging.join("credentials/kimi-code.json");
                std::fs::create_dir_all(cred.parent().expect("parent")).expect("cred dir");
                std::fs::write(cred, "{}").expect("kimi cred");
                Drove {
                    outcome: login_drive::Outcome::SignedIn {
                        email: None,
                        page: true,
                    },
                    host: Some("www.kimi.ai".to_owned()),
                }
            }
        }
    }

    let tmp = tmp_dir("signin-codex-auth-env");
    let mut env = env_at(&tmp);
    env.sock = None;
    let home = tmp.path().join("home");
    for (label, email) in [
        ("codex-one", "codex-one@example.com"),
        ("codex-two", "codex-two@example.com"),
    ] {
        listed_by_hand(&env, Cli::Codex, by_hand(label, Some(email)));
    }
    let host = OrderHost {
        order: std::cell::RefCell::new(Vec::new()),
    };
    let (code, out, err) = renew(
        "accounts renew codex-one@example.com --cli codex",
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let staging_one = home
        .join(".aterm")
        .join("logins")
        .join("codex")
        .join("codex-one+1");
    let store = env.accounts_store_for(Cli::Codex).expect("codex store");
    let (logins, _) = store.load_logins(env.now).expect("logins");
    let file_roster =
        accounts::Roster::parse_for(Cli::Codex, &read(&tmp.path().join("accounts-codex.toml")))
            .expect("roster");
    let bound = accounts::bound(&file_roster, &logins);
    let row_one = bound.row("codex-one").expect("codex-one");
    let env_one = crate::harness::accounts_pool::login_env(Cli::Codex, row_one, &home)
        .expect("codex-one LoginFile env resolves cleanly");
    assert_eq!(env_one.var, "CODEX_AUTH_PATH");
    assert_eq!(
        env_one.value,
        Some(staging_one.join("auth.json").display().to_string())
    );

    // The rest links overlay homes, which aterm makes only on Unix
    // (`a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links`).
    if !cfg!(unix) {
        return;
    }

    // `reconcile_staging_overlay_suffixes` adopts missing suffix files into `default_home` on an
    // overlay seam and never overwrites an existing default-home file.
    const SUFFIX_SHARED: &[crate::ai::connector::Shared] =
        &[crate::ai::connector::Shared::Suffix(".db")];
    let suffix_seam = crate::ai::connector::Seam::Overlay {
        var: "TEST_HOME",
        shared: SUFFIX_SHARED,
        markers: &[],
    };
    let default_overlay = home.join(".overlay-default");
    let staging_adopt = home.join(".aterm/logins/overlay/adopt");
    std::fs::create_dir_all(&staging_adopt).expect("adopt staging");
    std::fs::write(staging_adopt.join("auth.json"), "{}").expect("auth");
    std::fs::write(staging_adopt.join("state_5.db"), "initial-db").expect("initial db");
    std::fs::write(staging_adopt.join("state_5.db-wal"), "wal").expect("wal");
    reconcile_staging_overlay_suffixes(
        suffix_seam,
        &["auth.json"],
        &staging_adopt,
        &default_overlay,
    )
    .expect("reconcile adopt");
    assert_eq!(read(&default_overlay.join("state_5.db")), "initial-db");
    assert_eq!(read(&default_overlay.join("state_5.db-wal")), "wal");
    assert!(staging_adopt.join("state_5.db").is_symlink());
    assert!(!staging_adopt.join("state_5.db-wal").exists());

    std::fs::write(default_overlay.join("state_6.db"), "owner-state-6").expect("existing db");
    let staging_manual = home.join(".aterm/logins/overlay/manual");
    std::fs::create_dir_all(&staging_manual).expect("manual staging");
    std::fs::write(staging_manual.join("auth.json"), "{}").expect("auth");
    std::fs::write(staging_manual.join("state_6.db"), "transient").expect("transient");
    reconcile_staging_overlay_suffixes(
        suffix_seam,
        &["auth.json"],
        &staging_manual,
        &default_overlay,
    )
    .expect("reconcile existing");
    assert_eq!(read(&default_overlay.join("state_6.db")), "owner-state-6");
    assert!(staging_manual.join("state_6.db").is_symlink());

    // Due renewals order ready rows ahead of rows holding an unaddressed sign_in_task.
    let (_kimi_tmp, mut kimi_env) = kimi_env("due-order-sign-in-task");
    listed_by_hand(
        &kimi_env,
        Cli::Kimi,
        kimi_by_hand(
            "kimi-two",
            Some("kimi-two@example.com"),
            Some(crate::ai::connector::Region::Global),
        ),
    );
    let seed = OrderHost {
        order: std::cell::RefCell::new(Vec::new()),
    };
    let (code, out, err) = renew(
        "accounts renew kimi-one@example.com kimi-two@example.com --cli kimi",
        &kimi_env,
        &seed,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(*seed.order.borrow(), ["kimi-one", "kimi-two"]);
    kimi_env.now += 40 * 86_400;
    let kimi_store = kimi_env.accounts_store_for(Cli::Kimi).expect("kimi store");
    // Sessions here ran on both rows within the week: due logins this Mac uses
    // (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4).
    let used_here = |now: i64| {
        kimi_store
            .transact(now, &mut None, |state| {
                let mut events = Vec::new();
                for label in ["kimi-one", "kimi-two"] {
                    let sid = format!("s-{label}");
                    events.extend(accounts::note_session(state, &sid, label, None, now - 3600));
                    state.sessions.remove(&sid);
                }
                Ok(events)
            })
            .expect("sessions here ran on both rows");
    };
    used_here(kimi_env.now);
    let mut note = None;
    kimi_store
        .transact_pool_logins_within(
            &accounts::LOCK_LADDER,
            &kimi_env.accounts_roster_for(Cli::Kimi).unwrap(),
            kimi_env.now,
            &mut note,
            |_, _, logins| {
                accounts::admit_workflow(logins, "kimi-one")?;
                accounts::note_sign_in_task(
                    logins,
                    "kimi-one",
                    accounts::SignInAction::Browser,
                    kimi_env.now - 60,
                );
                Ok(Vec::new())
            },
        )
        .expect("record sign_in_task on kimi-one");
    // `--due` starts one account's sign-in and keeps the Mac's ten-minute spacing for the next
    // (§4.15 rule 4): it says so and leaves that row to a later pass.
    let due_pass = |env: &Env| {
        let host = OrderHost {
            order: std::cell::RefCell::new(Vec::new()),
        };
        let (code, out, err) = renew("accounts renew --cli kimi --due", env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        (host.order.into_inner(), out)
    };
    let (order, out) = due_pass(&kimi_env);
    assert_eq!(
        order,
        ["kimi-two"],
        "ready kimi-two runs ahead of kimi-one with an unaddressed sign_in_task"
    );
    assert!(
        out.contains(&format!(
            "{}: not run yet: ",
            shown_of(&kimi_env, Cli::Kimi, "kimi-one")
        )),
        "{out}"
    );
    kimi_env.now += crate::harness::sign_in_ladder::SPACING_S - 1;
    assert_eq!(
        due_pass(&kimi_env).0,
        Vec::<String>::new(),
        "inside the spacing"
    );
    kimi_env.now += 1;
    assert_eq!(due_pass(&kimi_env).0, ["kimi-one"], "ten minutes on");

    // Negative: once neither row has a sign_in_task, --due runs in roster order.
    kimi_env.now += 40 * 86_400;
    used_here(kimi_env.now);
    let (order, out) = due_pass(&kimi_env);
    assert_eq!(order, ["kimi-one"]);
    assert!(
        out.contains(&format!(
            "{}: not run yet: ",
            shown_of(&kimi_env, Cli::Kimi, "kimi-two")
        )),
        "{out}"
    );
}

/// NEGATIVE CONTROL: with the completion's receipt naming the row by its label (`completed this
/// machine's own login as main`), it fails `completed this machine's own login as oai
/// owner@example.test` (2026-11-16, by hand on SF-07H S6's review settle, then restored).
#[cfg(unix)]
#[test]
fn codex_default_completion_preserves_metadata_and_is_idempotent() {
    for inline in [false, true] {
        let tmp = tmp_dir("codex-complete");
        let env = env_at(&tmp);
        std::fs::create_dir_all(env.home.as_ref().unwrap().join(".codex")).unwrap();
        let original = if inline {
            "# keep this\naccount = [{ label = 'main', class = 'shared', identity_email = 'owner@example.test', browser = 'Work' }]\n"
        } else {
            "# keep this\n[[account]]\nlabel = 'main'\nclass = 'shared'\nidentity_email = 'owner@example.test'\nbrowser = 'Work' # keep that\n"
        };
        let path = tmp.path().join("accounts-codex.toml");
        std::fs::write(&path, original).unwrap();
        let host = PoolHost {
            account: Some("owner@example.test"),
        };
        let assert_renewal_metadata = |expected| {
            let before = read(&path);
            let (code, out, err) = renew("accounts --cli codex --json", &env, &host);
            assert_eq!(code, exit(0), "{out}{err}");
            let value: Value = aterm_json::from_str(out.trim()).unwrap();
            assert_eq!(value["accounts"][0]["renews"].as_bool(), Some(expected));
            assert_eq!(read(&path), before, "listing must remain read-only");
        };
        assert_renewal_metadata(false);
        for _ in 0..2 {
            let (code, out, err) = renew("accounts adopt-default --cli codex", &env, &host);
            assert_eq!(code, exit(0), "{out}{err}");
            // The receipt names the account the row completed is, never the row's label.
            assert!(
                out.contains("completed this machine's own login as oai owner@example.test"),
                "{out}"
            );
            assert!(!prints_word(&out, "main"), "{out}");
        }
        assert_renewal_metadata(true);
        let edited = read(&path);
        assert!(edited.contains("# keep this"));
        if !inline {
            assert!(edited.contains("# keep that"));
        }
        let mut expected = Roster::parse_for(Cli::Codex, original).unwrap().rows;
        expected[0].email = Some("owner@example.test".into());
        assert_eq!(
            Roster::parse_for(Cli::Codex, &edited).unwrap().rows,
            expected
        );
        assert!(
            !env.accounts_store_for(Cli::Codex)
                .unwrap()
                .logins_file()
                .exists()
        );
    }
}

#[test]
fn codex_default_completion_rechecks_identity_roster_and_source() {
    #[cfg(unix)]
    let races = ["identity", "roster", "source", "policy"];
    #[cfg(not(unix))]
    let races = ["identity", "roster", "policy"];
    for race in races {
        let tmp = tmp_dir("codex-complete-race");
        let env = env_at(&tmp);
        std::fs::create_dir_all(env.home.as_ref().unwrap().join(".codex")).unwrap();
        let path = tmp.path().join("accounts-codex.toml");
        let original = "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n";
        std::fs::write(&path, original).unwrap();
        let calls = Cell::new(0);
        let result = default_completion::complete(&env, None, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                if race == "roster" {
                    std::fs::write(&path, format!("{original}held_by = 'm2'\n")).unwrap();
                }
                #[cfg(unix)]
                if race == "source" {
                    crate::vault_entry::put(
                        env.home.as_ref().unwrap(),
                        "ai-accounts-codex.toml",
                        "[[account]]\nlabel = 'main'\nclass = 'dedicated'\nemail = 'other@example.test'\nmachines = ['m1']\n",
                    );
                }
                if race == "policy" {
                    aterm_uds::atomic_file::write(
                        env.config.as_ref().unwrap(),
                        ("[harness]\nlogins = 'off'\n").as_ref(),
                        0o600,
                    )
                    .unwrap();
                }
                if race == "identity" {
                    return Some("other@example.test".into());
                }
            }
            Some("owner@example.test".into())
        });
        assert!(result.is_err(), "{race}: {result:?}");
        assert!(!read(&path).contains("email ="), "{race}");
    }
}

#[test]
fn codex_default_completion_respects_holds_explicit_metadata_and_backend() {
    for extra in [
        "held_by = 'm2'",
        "email = 'alias@example.test'",
        "identity_email = 'other@example.test'",
    ] {
        let tmp = tmp_dir("codex-complete-preserve");
        let env = env_at(&tmp);
        std::fs::create_dir_all(env.home.as_ref().unwrap().join(".codex")).unwrap();
        let path = tmp.path().join("accounts-codex.toml");
        let original = format!("[[account]]\nlabel = 'main'\nclass = 'dedicated'\n{extra}\n");
        std::fs::write(&path, &original).unwrap();
        let host = PoolHost {
            account: Some("owner@example.test"),
        };
        let (code, _, _) = renew("accounts adopt-default --cli codex", &env, &host);
        assert_eq!(code, exit(1));
        assert_eq!(read(&path), original);
    }
}

#[test]
fn codex_default_completion_runs_in_init_and_upkeep_and_obeys_policy() {
    for (mode, policy) in [
        ("init", ""),
        ("upkeep", ""),
        ("init", "accounts = 'off'"),
        ("init", "logins = 'off'"),
        ("upkeep", "logins = 'off'"),
    ] {
        let tmp = tmp_dir("codex-complete-init");
        let env = env_at(&tmp);
        let home = env.home.as_ref().unwrap();
        let login = home.join(".codex");
        std::fs::create_dir_all(&login).unwrap();
        let jwt = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
            Some("owner@example.test"),
            "pro",
            1_900_000_000,
        );
        let auth = format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"id_token":"{jwt}","access_token":"{jwt}","refresh_token":"rt"}}}}"#
        );
        std::fs::write(login.join("auth.json"), &auth).unwrap();
        let path = tmp.path().join("accounts-codex.toml");
        std::fs::write(&path, "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n").unwrap();
        if !policy.is_empty() {
            aterm_uds::atomic_file::write(
                env.config.as_ref().unwrap(),
                (format!("[harness]\n{policy}\n")).as_ref(),
                0o600,
            )
            .unwrap();
        }
        if mode == "init" {
            let results = crate::harness::accounts_sync::sync_cli_accounts_for_init(
                home,
                &env.state,
                &tmp.path().join("accounts.toml"),
                env.now,
            );
            assert!(
                results.iter().all(|(_, result)| result.is_ok()),
                "{results:?}"
            );
        } else {
            let _ = crate::harness::accounts_sync::upkeep_live(
                home,
                &env.state,
                &tmp.path().join("accounts.toml"),
                aterm_uds::trace::Now::at(env.now),
                crate::harness::accounts_sync::UpkeepStep::Sync,
            );
        }
        let row = Roster::load_for(Cli::Codex, &path).unwrap().rows.remove(0);
        assert_eq!(
            row.email.as_deref(),
            policy.is_empty().then_some("owner@example.test")
        );
        assert_eq!(read(&login.join("auth.json")), auth);
    }
}

#[cfg(unix)]
#[test]
fn codex_default_completion_retains_alias_and_respects_native_auth_lock() {
    let tmp = tmp_dir("codex-complete-alias");
    let env = env_at(&tmp);
    let login = env.home.as_ref().unwrap().join(".codex");
    std::fs::create_dir_all(&login).unwrap();
    let path = tmp.path().join("accounts-codex.toml");
    let original = "[[account]]\nlabel = 'main'\nclass = 'dedicated'\nemail = 'alias@example.test'\nidentity_email = 'owner@example.test'\n";
    std::fs::write(&path, original).unwrap();
    let host = PoolHost {
        account: Some("owner@example.test"),
    };
    let (code, out, err) = renew("accounts adopt-default --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(read(&path), original);
    let lock = crate::harness::codex_app_login::AuthLock::try_take(&login.join("auth.json"))
        .unwrap()
        .unwrap();
    let (code, _, err) = renew("accounts adopt-default --cli codex", &env, &host);
    assert_eq!(code, exit(1), "{err}");
    assert!(err.contains("being changed"), "{err}");
    assert_eq!(read(&path), original);
    drop(lock);
}

#[test]
#[cfg(unix)]
fn codex_default_completion_does_not_write_linked_source_rosters() {
    for hard in [false, true] {
        let tmp = tmp_dir("codex-complete-linked");
        let env = env_at(&tmp);
        std::fs::create_dir_all(env.home.as_ref().unwrap().join(".codex")).unwrap();
        let original = "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n";
        let source = tmp.path().join("source.toml");
        let roster = tmp.path().join("accounts-codex.toml");
        std::fs::write(&source, original).unwrap();
        if hard {
            std::fs::hard_link(&source, &roster).unwrap();
        } else {
            std::os::unix::fs::symlink(&source, &roster).unwrap();
        }
        let result =
            default_completion::complete(&env, None, |_| Some("owner@example.test".into()));
        if hard {
            assert_eq!(result.unwrap(), Some("main".into()));
            assert!(read(&roster).contains("owner@example.test"));
        } else {
            assert!(result.unwrap_err().contains("linked roster"));
            assert_eq!(read(&roster), original);
        }
        assert_eq!(read(&source), original);
    }
}

#[test]
fn codex_default_completion_respects_signed_out_state() {
    let tmp = tmp_dir("codex-complete-signed-out");
    let env = env_at(&tmp);
    std::fs::create_dir_all(env.home.as_ref().unwrap().join(".codex")).unwrap();
    let path = tmp.path().join("accounts-codex.toml");
    let original = "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n";
    std::fs::write(&path, original).unwrap();
    let store = env.accounts_store_for(Cli::Codex).unwrap();
    std::fs::create_dir_all(&store.dir).unwrap();
    std::fs::write(env.home.as_ref().unwrap().join(".codex/auth.json"), "{}").unwrap();
    crate::ai::testing::current_codex_auth(
        &store,
        &path,
        "main",
        accounts::Auth::SignedOut,
        env.now,
    );
    let result = default_completion::complete(&env, None, |_| Some("owner@example.test".into()));
    assert!(matches!(result, Ok(None)), "{result:?}");
    assert_eq!(read(&path), original);
}

#[cfg(unix)]
#[test]
fn codex_default_completion_preserves_pool_holds_and_local_authority() {
    for warm in ["[]", "['m2']", "['m1']"] {
        let tmp = tmp_dir("codex-complete-pool");
        let env = env_at(&tmp);
        let home = env.home.as_ref().unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        aterm_uds::atomic_file::write(
            env.config.as_ref().unwrap(),
            ("[fleet]\nmachine = 'm1'\n").as_ref(),
            0o600,
        )
        .unwrap();
        let path = tmp.path().join("accounts-codex.toml");
        std::fs::write(&path, "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n").unwrap();
        let source = "ai-pool.toml";
        let raw = format!(
            "version = 1\n[[account]]\nprovider = 'codex'\nemail = 'owner@example.test'\nwarm = {warm}\n"
        );
        crate::vault_entry::put(home, source, &raw);
        let result =
            default_completion::complete(&env, None, |_| Some("owner@example.test".into()));
        assert!(result.is_ok(), "{warm}: {result:?}");
        let filled = Roster::load_for(Cli::Codex, &path).unwrap().rows[0]
            .email
            .is_some();
        assert_eq!(filled, warm == "['m1']", "{warm}");
        assert_eq!(delivered(home, source), raw);
    }
    for label in ["main", "another-machine"] {
        let tmp = tmp_dir("codex-complete-source-conflict");
        let env = env_at(&tmp);
        let home = env.home.as_ref().unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let path = tmp.path().join("accounts-codex.toml");
        std::fs::write(&path, "[[account]]\nlabel = 'main'\nclass = 'dedicated'\n").unwrap();
        let source = "ai-accounts-codex.toml";
        let raw = format!(
            "[[account]]\nlabel = '{label}'\nemail = 'other@example.test'\nmachines = ['m2']\n"
        );
        crate::vault_entry::put(home, source, &raw);
        let result =
            default_completion::complete(&env, None, |_| Some("owner@example.test".into()));
        assert_eq!(result.is_ok(), label != "main", "{result:?}");
        assert_eq!(delivered(home, source), raw);
    }
}

/// The text of the vault entry `name` under `home`, read in place.
#[cfg(unix)]
fn delivered(home: &Path, name: &str) -> String {
    crate::vault_entry::read_text(home, name, 4 << 20)
        .text()
        .expect("a delivered entry")
        .to_owned()
}

/// The sheet at `tmp`'s home whose one line gives `address`'s account to `computer`, in the column
/// `cells` fills, with this Mac named m16.
#[cfg(unix)]
fn sheet_of_other_macs(tmp: &Tmp, env: &Env, cells: &str, address: &str, computer: &str) {
    let config = env.config.clone().expect("a config path");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    aterm_uds::atomic_file::write(&config, b"[fleet]\nmachine = \"m16\"\n", 0o600).unwrap();
    crate::vault_entry::put(
        &tmp.path().join("home"),
        crate::harness::accounts_sync::ACCOUNTS_CSV_FILE,
        format!(
            "m,email,computer,,claude,Open AI,xai,kimi,google,meta,Payment,notes\n\
             ,{address},{computer},,{cells},,,,\n"
        ),
    );
}

/// A host with nobody at it whose sign-ins each stop at a bot-check, recording the rows they were
/// for.
struct BotChecked(RefCell<Vec<String>>);

impl AccountsHost for BotChecked {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        Err("unused".to_owned())
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("unused".to_owned())
    }
    fn now(&self) -> i64 {
        1_758_412_900
    }
    fn vendor_sign_in(&self, req: &Request<'_>) -> Drove {
        self.0.borrow_mut().push(req.row.label.clone());
        Drove {
            outcome: login_drive::Outcome::BotCheck("a stand-in's check".to_owned()),
            host: Some("www.kimi.ai".to_owned()),
        }
    }
}

/// ANOTHER MAC'S ACCOUNT IS A PERSON'S TO BORROW (`ATERM-KI-1105-9`): where the sheet gives
/// `kimi-one`'s account to m22 alone, the report says whose it is and the way to borrow it, stops
/// counting it among the rows on their way so that it offers listing another, and that way still
/// runs. `CONTROL`: the sheet naming this Mac.
#[cfg(unix)]
#[test]
fn a_person_can_borrow_another_macs_account_and_the_report_says_whose_it_is() {
    let (tmp, env) = kimi_env("other-macs-person");
    let default_login = env
        .home
        .as_deref()
        .unwrap()
        .join(Cli::Kimi.accounts().default_home)
        .join("credentials/kimi-code.json");
    std::fs::create_dir_all(default_login.parent().unwrap()).unwrap();
    std::fs::write(&default_login, "{}").unwrap();
    let sheet = |computer: &str| {
        sheet_of_other_macs(&tmp, &env, ",,,global", "kimi-one@example.com", computer);
    };
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi store");
    let mut note = None;
    store
        .transact_logins(env.now, &mut note, |state, _| {
            let _ = accounts::note_wall(
                state,
                ACCOUNTS_OWN_LABEL,
                "weekly",
                Some(env.now + 86_400),
                env.now,
                env.now,
            );
            Ok(Vec::new())
        })
        .unwrap();
    let host = || BotChecked(RefCell::default());

    sheet("m16");
    let (_, report, _) = renew("accounts --cli kimi", &env, &host());
    assert!(
        !report.contains("gives it to") && !report.contains("list another"),
        "CONTROL: {report}"
    );

    sheet("m22");
    let (_, report, _) = renew("accounts --cli kimi", &env, &host());
    assert!(
        report.contains("this pool's source gives it to m22")
            && report.contains(&format!(
                "accounts renew {}",
                step_word(&env, Cli::Kimi, "kimi-one")
            ))
            && report.contains("list another"),
        "{report}"
    );

    // A session here that runs on it, its login signed out, is this Mac's use of it: the
    // session door signs it in here, so the report neither says nothing does nor offers another.
    store
        .transact(env.now, &mut None, |state| {
            let mut events = accounts::note_session(state, "s-1", "kimi-one", None, env.now);
            events.extend(accounts::note_auth(
                state,
                "kimi-one",
                accounts::Auth::SignedOut,
                env.now,
            ));
            Ok(events)
        })
        .unwrap();
    let (_, report, _) = renew("accounts --cli kimi", &env, &host());
    assert!(
        !report.contains("gives it to m22") && !report.contains("list another"),
        "a session here runs on it: {report}"
    );

    let named = host();
    let _ = renew(
        "accounts renew kimi-one@example.com --cli kimi",
        &env,
        &named,
    );
    assert_eq!(*named.0.borrow(), ["kimi-one"], "a person named the row");
}

/// No report asks anybody to sign in another Mac's account (`ATERM-KI-1105-9`): Grok Build's pool,
/// whose rows a person signs in, asks for its own and not for one the sheet gives m22, in the
/// report, its JSON, and the stats. `CONTROL`: the same row while the sheet names this Mac.
#[cfg(unix)]
#[test]
fn no_report_asks_for_a_sign_in_of_another_macs_account() {
    let tmp = tmp_dir("other-macs-report");
    let mut env = env_at(&tmp);
    env.sock = None;
    listed_by_hand(
        &env,
        Cli::Grok,
        by_hand("grok-one", Some("grok-one@example.com")),
    );
    let sheet = |computer: &str| {
        sheet_of_other_macs(&tmp, &env, ",,1,", "grok-one@example.com", computer);
    };
    let asks = &format!(
        "sign-in needed: Run aterm harness accounts login {}",
        step_word(&env, Cli::Grok, "grok-one")
    );
    let needed = |json: &str| {
        let doc: Value = aterm_json::from_str(json).unwrap();
        let row =
            crate::harness::account_names::tests::report_row(Cli::Grok, &env, "grok-one", &doc)
                .cloned()
                .unwrap();
        (row["sign_in_needed"].clone(), row["belongs_to"].clone())
    };

    sheet("m16");
    let (_, report, _) = run_line("accounts --cli grok", &env);
    assert!(report.contains(asks), "CONTROL: {report}");
    let (_, stats, _) = run_line("accounts stats --cli grok", &env);
    assert!(stats.contains(asks), "CONTROL: {stats}");
    let (_, json, _) = run_line("accounts --cli grok --json", &env);
    let (asked, whose) = needed(&json);
    assert_ne!(asked, Value::Null, "CONTROL: {json}");
    assert_eq!(whose, Value::Null, "CONTROL: {json}");

    sheet("m22");
    let (_, report, _) = run_line("accounts --cli grok", &env);
    assert!(!report.contains(asks), "{report}");
    assert!(report.contains("gives it to m22"), "{report}");
    let (_, stats, _) = run_line("accounts stats --cli grok", &env);
    assert!(!stats.contains(asks), "{stats}");
    let (_, json, _) = run_line("accounts --cli grok --json", &env);
    let (asked, whose) = needed(&json);
    assert_eq!(asked, Value::Null, "{json}");
    assert_eq!(whose, Value::from("m22"), "{json}");
}

/// Set, [`a_test_host_holds_no_end_signal_through_any_account_verb`] is the child its own run
/// started, which runs the account verbs ([`account_verbs_read_sigterm`]) and writes what their
/// host read to the file this names, instead of starting another child.
#[cfg(unix)]
const VERB_DISPOSITION_ENV: &str = "ATERM_TEST_ACCOUNT_VERB_DISPOSITION";

/// A test host whose stand-ins read this process's SIGTERM action in the middle of an account
/// verb: each poll of its work's cancellation and each read of the Codex default login's identity
/// notes how many of its own holds ([`AccountsHost::hold_window_ends`]) are alive and what
/// SIGTERM does now ([`sigterm_action`]), then cancels (or reads no identity), so the verb runs
/// nothing past it.
#[cfg(unix)]
#[derive(Default)]
struct DispositionHost {
    holds: std::rc::Rc<Cell<i32>>,
    read: RefCell<std::collections::BTreeSet<String>>,
}

#[cfg(unix)]
impl DispositionHost {
    fn note(&self) {
        self.read.borrow_mut().insert(format!(
            "held={} sigterm={}",
            self.holds.get(),
            sigterm_action()
        ));
    }
}

#[cfg(unix)]
impl AccountsHost for DispositionHost {
    fn work_cancelled(&self) -> bool {
        self.note();
        true
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        self.note();
        None
    }
    fn hold_window_ends(&self) -> WindowEnds {
        self.holds.set(self.holds.get() + 1);
        let holds = std::rc::Rc::clone(&self.holds);
        WindowEnds::noting(move || holds.set(holds.get() - 1))
    }
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        panic!("no account verb here runs `claude` (asked under {login:?})");
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("not here".to_owned())
    }
    fn now(&self) -> i64 {
        1_758_412_900
    }
}

/// What SIGTERM does in this process now: `default`, `ignored` or `handled`.
#[cfg(unix)]
fn sigterm_action() -> &'static str {
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask, no flags).
    let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action changes nothing; SIGTERM's current one is read into `now`.
    if unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut now) } != 0 {
        return "unread";
    }
    match now.sa_sigaction {
        libc::SIG_DFL => "default",
        libc::SIG_IGN => "ignored",
        _ => "handled",
    }
}

/// The child's run ([`VERB_DISPOSITION_ENV`]): `harness agy login`, `accounts add`, `accounts
/// adopt-default`, `accounts adopt-default --cli codex` (the hold [`run_accounts_of`] takes
/// itself) and `signin --cli grok`, each on a fresh [`DispositionHost`] in one scratch world
/// that reaches the step its host is read at, one line each (`<verb>: <readings>`, a tab, then
/// its exit and standard error), then SIGTERM's action under the hold of the host the CLI's `run`
/// passes ([`LiveAccounts`], which in this build holds none, so the tests that drive the verbs
/// through `run` hold none either), then the CONTROL line: SIGTERM's action under a live hold.
#[cfg(unix)]
fn account_verbs_read_sigterm(said: &Path) {
    let tmp = tmp_dir("verb-disposition-child");
    let mut env = env_at(&tmp);
    // `signin --cli grok` tells no session of its need.
    env.sid.clear();
    env.sock = None;
    let home = env.home.clone().expect("a home");
    // `harness agy login` signs in with the managed agy.
    let shim = versions_layout(&env)
        .expect("a layout")
        .shim(&atpkg::store::ToolName::new("agy").expect("agy"));
    std::fs::create_dir_all(shim.parent().expect("its bin")).expect("bin");
    std::fs::write(&shim, "").expect("a managed agy");
    // `accounts adopt-default --cli codex` completes the default row from the default login.
    std::fs::create_dir_all(home.join(".codex")).expect("codex home");
    std::fs::write(
        tmp.path().join("accounts-codex.toml"),
        "[[account]]\nlabel = 'main'\nclass = 'shared'\n",
    )
    .expect("codex roster");
    let codex = match parsed("accounts adopt-default oai adopted@example.test --cli codex") {
        Ok(Cmd::AccountsOf(of)) => of,
        other => panic!("not another pool's adopt-default: {other:?}"),
    };
    let row = |label: &str, email: Option<&str>| accounts::Row {
        label: label.into(),
        email: email.map(str::to_owned),
        ..accounts::Row::default()
    };
    type Verb<'a> = &'a dyn Fn(&dyn AccountsHost, &mut Vec<u8>, &mut Vec<u8>) -> ExitCode;
    let verbs: [(&str, Verb<'_>); 5] = [
        ("agy login", &|host, out, _| {
            run_agy_login(&env, host, "primary", true, out)
        }),
        ("accounts add", &|host, out, err| {
            run_accounts_add(
                Cli::Claude,
                &env,
                host,
                "claude added@example.test",
                &Listing::default(),
                out,
                err,
            )
        }),
        ("accounts adopt-default", &|host, out, err| {
            let asked = row("adopted", None);
            run_accounts_adopt(Cli::Claude, &env, host, &asked, None, out, err)
        }),
        ("accounts adopt-default --cli codex", &|host, out, err| {
            run_accounts_of(&codex, &env, host, out, err)
        }),
        ("signin --cli grok", &|host, out, err| {
            run_signin(&env, host, false, true, out, err)
        }),
    ];
    let mut lines = Vec::new();
    for (name, verb) in verbs {
        let host = DispositionHost::default();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = verb(&host, &mut out, &mut err);
        let read = host.read.borrow();
        let read = if read.is_empty() {
            "nothing read".to_owned()
        } else {
            read.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        lines.push(format!(
            "{name}: {read}\t{code:?} {}{}",
            crate::harness::one_line(&String::from_utf8_lossy(&out), 200),
            crate::harness::one_line(&String::from_utf8_lossy(&err), 200)
        ));
    }
    let cli = LiveAccounts::default().hold_window_ends();
    lines.push(format!("LiveAccounts: sigterm={}", sigterm_action()));
    drop(cli);
    let live = WindowEnds::hold();
    let control = sigterm_action();
    drop(live);
    lines.push(format!("CONTROL a live hold: sigterm={control}"));
    std::fs::write(said, lines.join("\n")).expect("its lines");
}

/// A TEST HOST HOLDS NO END SIGNAL THROUGH ANY ACCOUNT VERB: each of them takes its hold on the
/// signals that end a window ([`WindowEnds`]) from its host, so on a test's host a stop sent to
/// the test binary (verify's SIGTERM at its first red) ends it at once, instead of being noted as
/// the person cancelling every sign-in test running beside it. This test binary, run again as its
/// own child with this test alone on one thread ([`VERB_DISPOSITION_ENV`]), so no live hold
/// another test takes meanwhile can change what it reads, runs each verb on a
/// [`DispositionHost`], which reads SIGTERM's action inside the verb: inside its host's one hold,
/// at its default action; and so does the host the CLI's `run` passes in this build. CONTROL:
/// under a live hold the same read finds a handler. NEGATIVE CONTROL: with `run_signin` taking
/// `WindowEnds::hold()` itself, its line reads `held=0 sigterm=handled`; with `LiveAccounts`
/// holding in this build too, its line reads `sigterm=handled`.
#[cfg(unix)]
#[test]
fn a_test_host_holds_no_end_signal_through_any_account_verb() {
    if let Some(said) = std::env::var_os(VERB_DISPOSITION_ENV) {
        account_verbs_read_sigterm(Path::new(&said));
        return;
    }
    let tmp = tmp_dir("verb-disposition");
    let said = tmp.path().join("said");
    let exe = std::env::current_exe().expect("test exe");
    let name = format!(
        "{}::a_test_host_holds_no_end_signal_through_any_account_verb",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.args(["--exact", &name, "--test-threads=1"])
        .env(VERB_DISPOSITION_ENV, &said)
        // Every signal starts at its default action, whatever this binary's other tests hold now.
        .default_signals()
        // The Grok sign-in reads this process's environment: no login is named there, and no
        // session is asked to show its need.
        .env_remove("ATERM_SESSION_ID");
    for name in crate::harness::grok_signin::CREDENTIAL_ENV
        .into_iter()
        .chain([
            crate::harness::grok_signin::LOGIN_FILE_ENV,
            crate::harness::grok_signin::HOME_ENV,
        ])
    {
        cmd.env_remove(name);
    }
    let ran = aterm_uds::deadline::output_while_process_progressing(
        &mut cmd,
        aterm_spec::wait::MUST_HAPPEN,
        "the account verbs' child",
    )
    .expect("the child ran");
    let told = format!(
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(
        ran.status.success(),
        "the child failed ({:?}):\n{told}",
        ran.status
    );
    assert!(told.contains("1 passed"), "the child ran no test:\n{told}");
    let said = std::fs::read_to_string(&said)
        .unwrap_or_else(|why| panic!("the child wrote no readings ({why}):\n{told}"));
    let heads: Vec<&str> = said
        .lines()
        .map(|line| line.split('\t').next().unwrap_or(line))
        .collect();
    assert_eq!(
        heads,
        [
            "agy login: held=1 sigterm=default",
            "accounts add: held=1 sigterm=default",
            "accounts adopt-default: held=1 sigterm=default",
            "accounts adopt-default --cli codex: held=1 sigterm=default",
            "signin --cli grok: held=1 sigterm=default",
            "LiveAccounts: sigterm=default",
            "CONTROL a live hold: sigterm=handled",
        ],
        "{said}\n{told}"
    );
}
