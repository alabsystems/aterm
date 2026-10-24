// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `accounts renew --cli <id>`: a Kimi row renewed end to end by its sign-in's stand-in
//! (this test binary run in place as `kimi login --region global`, printing the recorded
//! transcript), a Codex row by its app server's replay, and the machine's one sign-in at a time.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::harness::login_drive::{self, Approval, Approver, Drove, Handoff, Request};

/// What the stand-in host runs for a sign-in.
enum Script {
    /// The Kimi stand-in, its `.stand-in` script (`<transcript> <exit>`).
    Kimi(&'static str),
    /// Codex's app server replaying this, its login file written into the staging home.
    AppServer(String),
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

struct SignInHost {
    script: Script,
    home: PathBuf,
    calls: Cell<usize>,
    handed: RefCell<Handed>,
    /// Whether stdin is a terminal.
    terminal: bool,
    /// Whether this runs over SSH.
    ssh: bool,
    /// The window browser installed, if any.
    window: Option<PathBuf>,
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
            window: None,
            ends: (crate::harness::browser_window::WindowEnd::Done, true),
            opened: RefCell::new(Vec::new()),
            tidy: Ok(0),
            tidied: RefCell::new(Vec::new()),
            holds: std::rc::Rc::new(Cell::new(0)),
            marked_at_window: Cell::new(None),
            at_tidy: Cell::new(None),
        }
    }

    /// A desk: a terminal on stdin, no SSH, Chrome for Testing installed.
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
    fn stdin_is_terminal(&self) -> bool {
        self.terminal
    }

    fn under_ssh(&self) -> bool {
        self.ssh
    }

    fn window_browser(&self) -> Option<PathBuf> {
        self.window.clone()
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
                    None,
                    &move |name| (name == "HOME").then(|| home.clone().into_os_string()),
                    &mut approver,
                    Duration::from_secs(60),
                )
            }
            Script::Ends(outcome) => Drove {
                outcome: outcome.clone(),
                host: Some("www.kimi.ai".to_owned()),
            },
            Script::AppServer(replay) => {
                std::fs::write(req.staging.join("auth.json"), "{}").expect("stand-in login");
                let server = crate::harness::codex_app_login::tests::serve(
                    replay.clone(),
                    Arc::new(Mutex::new(Vec::new())),
                );
                crate::harness::codex_app_login::login(
                    server,
                    &mut approver,
                    req.hosts,
                    req.row.email.as_deref(),
                    Instant::now() + Duration::from_secs(60),
                )
            }
        }
    }
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
    let (code, _, err) = run_line(
        "accounts add kimi-one --cli kimi --email kimi-one@example.com --region global",
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    (tmp, env)
}

#[test]
fn a_kimi_row_renews_through_its_own_sign_in_and_a_failed_one_changes_nothing() {
    let (tmp, env) = kimi_env("signin-kimi");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let logins_root = home.join(".aterm/logins/kimi");

    let failing = SignInHost::new(Script::Kimi("kimi 1"), &home);
    let before = read(&store.logins_file());
    let (code, out, err) = renew("accounts renew kimi-one --cli kimi", &env, &failing);
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(
        read(&store.logins_file()),
        before,
        "the binding is unchanged"
    );
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
    let (code, out, err) = renew("accounts renew kimi-one --cli kimi", &env, &host);
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
        out.contains("kimi-one: renewed") && !out.contains("QXR4"),
        "{out}"
    );
    let claude_logins = tmp.path().join("state/accounts/logins.json");
    assert!(
        !read(&claude_logins).contains("proved_by"),
        "Claude Code's store is not written"
    );
}

#[test]
fn a_codex_row_is_proved_by_its_app_servers_account_and_another_account_is_refused() {
    let tmp = tmp_dir("signin-codex");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (code, _, err) = run_line(
        "accounts add codex-one --cli codex --email codex-one@example.com",
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Codex).expect("codex's store");
    let logins_root = home.join(".aterm/logins/codex");
    let replay = crate::harness::codex_app_login::tests::REPLAY;

    let other = SignInHost::new(
        Script::AppServer(replay.replace("codex-one@example.com", "someone-else@example.com")),
        &home,
    );
    let (code, out, err) = renew("accounts renew codex-one --cli codex", &env, &other);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(out.contains("codex-one: REFUSED: wrong account"), "{out}");
    assert!(
        stagings(&logins_root, "codex-one").is_empty(),
        "staging removed"
    );
    let logins = read(&store.logins_file());
    assert!(
        !logins.contains("codex-one+"),
        "the binding names no new home: {logins}"
    );

    let host = SignInHost::new(Script::AppServer(replay.to_owned()), &home);
    let (code, out, err) = renew("accounts renew codex-one --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(
        *host.handed.borrow(),
        [(Some("auth.openai.com".to_owned()), None)]
    );
    let logins = read(&store.logins_file());
    assert!(
        logins.contains("codex-one+1") && logins.contains("\"proved_by\":\"status\""),
        "{logins}"
    );
    let ledger = read(&store.ledger_path());
    assert!(
        !ledger.contains("oauth/authorize") && !ledger.contains("11111111"),
        "{ledger}"
    );
}

#[test]
fn a_second_sign_in_waits_for_the_machines_lock_then_reports_it_held() {
    let (tmp, env) = kimi_env("signin-lock");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let held = store
        .signin_within()
        .expect("the lock")
        .expect("free at first");
    let host = SignInHost::new(Script::Kimi("kimi 0"), &home);
    let (code, out, err) = renew("accounts renew kimi-one --cli kimi", &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains("kimi-one: held") && out.contains(accounts::SIGNIN_HELD),
        "{out}"
    );
    assert_eq!(host.calls.get(), 0, "nothing ran under another's lock");
    assert!(stagings(&home.join(".aterm/logins/kimi"), "kimi-one").is_empty());
    drop(held);
    let (code, out, err) = renew("accounts renew kimi-one --cli kimi", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(host.calls.get(), 1, "once it is free the renewal runs");
}

#[test]
fn a_codex_sign_ins_staging_home_is_an_overlay_before_its_child_runs_and_a_keyring_store_runs_none()
{
    let tmp = tmp_dir("signin-codex-overlay");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (code, _, err) = run_line(
        "accounts add codex-one --cli codex --email codex-one@example.com",
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    let home = tmp.path().join("home");
    let replay = crate::harness::codex_app_login::tests::REPLAY;
    let host = SignInHost::new(Script::AppServer(replay.to_owned()), &home);
    let (code, out, err) = renew("accounts renew codex-one --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let said = out
        .lines()
        .find(|l| l.contains("by hand with:"))
        .unwrap_or_default()
        .to_owned();
    assert!(said.contains("CODEX_HOME="), "{out}");
    let staging = home.join(".aterm/logins/codex/codex-one+1");
    for shared in ["sessions", "config.toml", "skills"] {
        assert!(
            std::fs::symlink_metadata(staging.join(shared))
                .is_ok_and(|m| m.file_type().is_symlink()),
            "printed {said:?}, and {shared} links to the default home"
        );
    }
    assert!(
        staging.join("auth.json").is_file() && !staging.join("auth.json").is_symlink(),
        "the row's own login is its own"
    );

    // Negative: with the login in the OS keyring, the sign-in would replace the machine's own
    // login, so nothing runs and no staging home is made.
    std::fs::write(
        home.join(".codex/config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )
    .expect("config");
    let keyring = SignInHost::new(Script::AppServer(replay.to_owned()), &home);
    let (code, out, err) = renew("accounts renew codex-one --cli codex", &env, &keyring);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(err.contains("OS keyring"), "{err}");
    assert_eq!(keyring.calls.get(), 0);
    assert!(!home.join(".aterm/logins/codex/codex-one+2").exists());
}

#[test]
fn ledger_accounts_reads_the_named_clis_ledger() {
    let (tmp, env) = kimi_env("ledger-cli");
    let home = tmp.path().join("home");
    let failing = SignInHost::new(Script::Kimi("kimi 1"), &home);
    let (code, _, _) = renew("accounts renew kimi-one --cli kimi", &env, &failing);
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
        out.contains("kimi-one") && out.contains("exited-1"),
        "kimi's own ledger: {out}"
    );
    // Negative: Claude Code's ledger holds none of it, and `--cli` names a pool or nothing.
    let (code, out, _) = run_line("ledger accounts", &env);
    assert_eq!(code, exit(0));
    assert!(!out.contains("kimi-one"), "{out}");
    assert!(parsed("ledger accounts --cli nope").is_err());
    assert!(parsed("ledger disk --cli kimi").is_err());
    assert!(parsed("ledger accounts --cli kimi --region global").is_err());
}

#[test]
fn accounts_cli_all_lists_each_kept_pool_and_none_is_claudes_empty_report() {
    let tmp = tmp_dir("accounts-all");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (_, none, _) = run_line("accounts list", &env);
    let (_, all, _) = run_line("accounts --cli all list", &env);
    assert_eq!(all, none, "no pool kept: Claude Code's own report");
    for line in [
        "accounts add kimi-one --cli kimi --email kimi-one@example.com --region global",
        "accounts add codex-one --cli codex --email codex-one@example.com",
    ] {
        let (code, _, err) = run_line(line, &env);
        assert_eq!(code, exit(0), "{err}");
    }
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

/// A host whose Codex app server names `account` and whose usage reader is `usage`.
struct PoolHost {
    account: Option<&'static str>,
    usage: Option<crate::harness::accounts_pool::ReadUsage<'static>>,
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
    fn usage_reader(&self) -> Option<crate::harness::accounts_pool::ReadUsage<'static>> {
        self.usage
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        self.account.map(str::to_owned)
    }
}

#[test]
fn a_codex_adopt_default_takes_the_email_its_app_server_names() {
    let tmp = tmp_dir("adopt-codex");
    let mut env = env_at(&tmp);
    env.sock = None;
    let host = PoolHost {
        account: Some("codex-one@example.com"),
        usage: None,
    };
    let (code, out, err) = renew("accounts adopt-default codex-1 --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let roster = read(&tmp.path().join("accounts-codex.toml"));
    assert!(
        roster.contains("label = \"codex-1\"") && roster.contains("codex-one@example.com"),
        "{roster}"
    );

    // Negative: an app server that names no account writes nothing, and another CLI still
    // needs --email.
    let other = tmp_dir("adopt-codex-none");
    let mut env = env_at(&other);
    env.sock = None;
    let silent = PoolHost {
        account: None,
        usage: None,
    };
    let (code, _, err) = renew("accounts adopt-default codex-1 --cli codex", &env, &silent);
    assert_eq!(code, exit(1), "{err}");
    assert!(err.contains("--email"), "{err}");
    assert!(!other.path().join("accounts-codex.toml").exists());
    assert!(parsed("accounts adopt-default kimi-1 --cli kimi --region global").is_err());
    assert!(
        parsed("accounts adopt-default main").is_err(),
        "Claude Code's needs it"
    );
}

fn full_main(cli: Cli, home: &Path, now: i64) -> Option<crate::harness::codex_app_login::Usage> {
    (cli == Cli::Codex && !home.to_string_lossy().contains("logins")).then(|| {
        crate::harness::codex_app_login::Usage {
            sample: accounts::Sample {
                session_pct: Some(100),
                session_reset: Some(now + 3600),
                at: now,
                ..accounts::Sample::default()
            },
            wall: Some((accounts::SESSION_LIMIT, Some(now + 3600))),
        }
    })
}

#[test]
fn a_pools_stats_list_each_row_alike_as_text_and_json() {
    let (tmp, env) = kimi_env("stats-kimi");
    let host = PoolHost {
        account: None,
        usage: None,
    };
    let (text_code, text, err) = renew("accounts stats --cli kimi", &env, &host);
    let (json_code, json, _) = renew("accounts stats --cli kimi --json", &env, &host);
    assert_eq!(text_code, exit(0), "{err}");
    assert_eq!(json_code, text_code, "both forms exit alike");
    assert!(
        text.contains("aterm reads no usage of Kimi Code")
            && text.contains("kimi-one: not-enrolled")
            && text.contains("no reading yet"),
        "{text}"
    );
    let doc: Value = aterm_json::from_str(json.trim()).expect("json");
    let rows = doc["rows"].as_array().expect("rows");
    assert!(rows.iter().any(|r| r["label"] == Value::from("kimi-one")));

    // Codex: each row's usage is read in its own home and recorded, so `main` reads limited.
    let (code, _, err) = run_line(
        "accounts add codex-one --cli codex --email codex-one@example.com",
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    let reads = PoolHost {
        account: None,
        usage: Some(&full_main),
    };
    std::fs::create_dir_all(tmp.path().join("home/.codex")).expect("default home");
    let (code, out, err) = renew("accounts stats --cli codex", &env, &reads);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.contains("main: limited") && out.contains("5h 100%"),
        "{out}"
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
#[test]
fn accounts_browser_opens_the_rows_own_profile_at_its_regions_page_and_records_no_secret() {
    let (tmp, env) = kimi_env("browser-kimi");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let host = SignInHost::desk(&home);
    let (code, out, err) = renew("accounts browser kimi-one --cli kimi", &env, &host);
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
            && out.contains("next: aterm harness accounts renew kimi-one --cli kimi"),
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

    let (code, _, err) = run_line(
        "accounts add kimi-cn --cli kimi --email kimi-cn@example.com --region mainland-cn",
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    let (code, out, err) = renew("accounts browser kimi-cn --cli kimi", &env, &host);
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
    let (code, out, err) = renew("accounts browser kimi-one --cli kimi", &other_env, &empty);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(out.contains("nothing was signed in"), "{out}");
}

/// `ACCOUNTS BROWSER` HOLDS THE SIGNALS THAT END IT AND MARKS THE PROFILE until the pass after
/// the window has run: the window opens on a profile marked untidied, under one hold on the
/// signals that would end this process, which is still held through the pass after the window
/// and put back once the window is recorded; a pass that ran removes the mark. CONTROL: a pass
/// that failed leaves the mark, for the next headless launch on the profile to make up.
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
    let (code, out, err) = renew("accounts browser kimi-one --cli kimi", &env, &host);
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
    let (code, out, err) = renew("accounts browser kimi-one --cli kimi", &env, &failing);
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

/// `ACCOUNTS BROWSER` opens nothing without a terminal, over SSH, under another sign-in's lock or
/// on a profile another browser holds. CONTROL: with all of that clear it opens one window.
#[test]
fn accounts_browser_refuses_without_a_terminal_over_ssh_under_a_held_lock_or_an_open_profile() {
    let (tmp, env) = kimi_env("browser-refusals");
    let home = tmp.path().join("home");
    let store = env.accounts_store_for(Cli::Kimi).expect("kimi's store");
    let line = "accounts browser kimi-one --cli kimi";
    let no_terminal = SignInHost {
        terminal: false,
        ..SignInHost::desk(&home)
    };
    let over_ssh = SignInHost {
        ssh: true,
        ..SignInHost::desk(&home)
    };
    for (host, said) in [
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
    }
    let held_by = SignInHost::desk(&home);
    let held = store.signin_within().expect("lock").expect("free");
    let (code, out, err) = renew(line, &env, &held_by);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(err.contains("kimi-one: held"), "{err}");
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

/// `ACCOUNTS BROWSER` opens nothing for a row with no email, an unknown label, a Kimi row with no
/// region (whose roster is refused whole) or a machine with no window browser.
#[test]
fn accounts_browser_refuses_no_email_no_region_no_window_browser_and_an_unknown_label() {
    let (tmp, env) = kimi_env("browser-rows");
    let home = tmp.path().join("home");
    let (code, _, err) = run_line("accounts add kimi-bare --cli kimi", &env);
    assert_eq!(code, exit(0), "{err}");
    let host = SignInHost::desk(&home);
    let (code, _, err) = renew("accounts browser kimi-bare --cli kimi", &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains("`kimi-bare` has no email"), "{err}");
    let (code, _, err) = renew("accounts browser nobody --cli kimi", &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains("`nobody` is not in the roster"), "{err}");
    let bare = SignInHost {
        window: None,
        ..SignInHost::desk(&home)
    };
    let (code, _, err) = renew("accounts browser kimi-one --cli kimi", &env, &bare);
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
    let (code, _, err) = renew("accounts browser kimi-nowhere --cli kimi", &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains("no region"), "{err}");
    assert!(host.opened.borrow().is_empty());
}

/// A renewal whose page stops on the vendor's sign-in names `accounts browser` in its NEEDS YOU
/// line. CONTROL: a stop for any other reason keeps naming `accounts login`.
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
    let (code, out, err) = renew("accounts renew kimi-one --cli kimi", &env, &signed_out);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(
        out.contains(
            "NEEDS YOU: sign `kimi-one` in once in its own browser profile: run `aterm harness \
             accounts browser kimi-one --cli kimi` at this machine's desk, then `aterm harness \
             accounts renew kimi-one --cli kimi`"
        ),
        "{out}"
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
    let (code, out, _) = renew("accounts renew kimi-one --cli kimi", &env, &no_browser);
    assert_eq!(code, exit(1));
    assert!(
        out.contains("NEEDS YOU: approve the Kimi Code CLI sign-in for kimi-one")
            && out.contains("accounts login kimi-one --cli kimi")
            && !out.contains("accounts browser"),
        "{out}"
    );
}

/// `ACCOUNTS BROWSER` with no `--cli` serves Claude Code's pool: claude.ai's sign-in page, in the
/// profile its headless opener keys on. CONTROL: the record lands in Claude Code's own ledger.
#[test]
fn accounts_browser_serves_claude_codes_pool() {
    let tmp = tmp_dir("browser-claude");
    let mut env = env_at(&tmp);
    env.sock = None;
    let (code, _, err) = run_line("accounts add work-1 --email work-1@example.com", &env);
    assert_eq!(code, exit(0), "{err}");
    let host = SignInHost::desk(&tmp.path().join("home"));
    let (code, out, err) = renew("accounts browser work-1", &env, &host);
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
        out.contains("next: aterm harness accounts renew work-1   (")
            && out.contains("aterm cannot see from here whether claude.ai signed you in"),
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

/// `accounts browser` takes one label, with or without `--cli`.
#[test]
fn accounts_browser_takes_one_label() {
    assert_eq!(
        parsed("accounts browser grok-1 --cli grok"),
        Ok(Cmd::AccountsOf(AccountsOf {
            cli: Cli::Grok,
            form: AccountsCmd::Browser {
                label: "grok-1".to_owned()
            },
            vendor: accounts::Vendor::default(),
        }))
    );
    assert_eq!(
        parsed("accounts browser work-1"),
        Ok(Cmd::Accounts(AccountsCmd::Browser {
            label: "work-1".to_owned()
        }))
    );
    for line in [
        "accounts browser",
        "accounts browser a b",
        "accounts browser a --json",
    ] {
        assert!(parsed(line).is_err(), "{line}");
    }
    assert!(parsed("accounts browser").unwrap_err().contains(
        "accounts browser takes one label: `aterm harness accounts browser <label> [--cli <id>]`"
    ));
}

/// `ACCOUNTS BROWSER` judges the sign-in by the vendor's own session, never by a cookie store:
/// a profile a headless renewal already used (it has `Default/Cookies`) whose window closed with
/// nothing signed in exits 1, "nothing was signed in there", and names the window again. The
/// pass after the window asks kimi.ai's `access_token`, after the password manager was turned
/// off, and its dropped identity-provider cookies are counted. CONTROLS: the session kept exits 0
/// and names `renew`; a pass that could not run exits 1 and says the identity provider's sign-in
/// may be left.
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
    let line = "accounts browser kimi-one --cli kimi";
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
            && out
                .contains("next: sign in there, then press Enter: aterm harness accounts browser")
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
        ) && out.contains("next: aterm harness accounts renew kimi-one --cli kimi"),
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
            && err.contains("accounts browser kimi-one --cli kimi"),
        "{err}"
    );
    assert!(!out.contains("next: aterm harness accounts renew"), "{out}");
    assert!(read(&store.ledger_path()).contains("www.kimi.ai window done untidied"));
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
    let (code, out, err) = renew("accounts browser kimi-one --cli kimi", &env, &host);
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
    let (code, out, err) = renew("accounts browser kimi-one --cli kimi", &env, &host);
    assert_eq!(code, exit(0), "CONTROL: {out}{err}");
    assert_eq!(host.opened.borrow().len(), 1);
}

/// A renewal of another CLI that stopped where only a person can go on (its page waits on one,
/// a block page, its time ran out) is `NeedsPerson`, which the background's backoff keys on.
/// CONTROL: a sign-in the CLI said failed is a plain failure.
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
        renew_one_for(Cli::Kimi, 0, &host, &store, &row, None, &mut out, &mut err)
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
        renewal(login_drive::Outcome::Failed),
        Renewal::Failed,
        "CONTROL"
    );
}
