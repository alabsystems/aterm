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
}

/// The pages handed over: each one's host and code.
type Handed = Vec<(Option<String>, Option<String>)>;

struct SignInHost {
    script: Script,
    home: PathBuf,
    calls: Cell<usize>,
    handed: RefCell<Handed>,
}

impl SignInHost {
    fn new(script: Script, home: &Path) -> SignInHost {
        SignInHost {
            script,
            home: home.to_path_buf(),
            calls: Cell::new(0),
            handed: RefCell::new(Vec::new()),
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
        false
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
                    &move |name| (name == "HOME").then(|| home.clone().into_os_string()),
                    &mut approver,
                    Duration::from_secs(60),
                )
            }
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
    let Ok(Cmd::AccountsOf(of)) = parsed(line) else {
        panic!("{line} is another CLI's accounts command");
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_accounts_of(&of, env, host, &mut out, &mut err);
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
