// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm harness accounts renew` and `adopt-default`.

use std::cell::RefCell;

use super::*;

const OWNER: &str = "owner@example.com";
const WORK: &str = "w2@example.com";

/// This machine's own login (renewable, a browser profile of its own), a
/// renewable second account, and one with no email.
fn roster_text(work_dir: &str) -> String {
    let dir = |d: &str| aterm_toml::Value::from(d).to_string();
    format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"{OWNER}\"\nbrowser = \
         \"Claude main\"\n\n[[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \
         {}\nemail = \"{WORK}\"\n\n[[account]]\nlabel = \"plain\"\nclass = \"shared\"\ndir = \
         {}\n",
        dir(work_dir),
        dir(&format!("{work_dir}-plain"))
    )
}

fn renew_env(tag: &str) -> (Tmp, Env) {
    let tmp = tmp_dir(tag);
    let env = env_at(&tmp);
    let work = tmp.path().join("logins-own").join("work-2");
    std::fs::create_dir_all(&work).expect("mkdir");
    std::fs::write(work.join("a-login-file"), "{}").expect("write");
    std::fs::write(
        env.accounts_roster().expect("a config dir"),
        roster_text(work.to_str().expect("utf-8")),
    )
    .expect("write");
    (tmp, env)
}

/// A SCRIPTED MACHINE for `renew`: its sign-in "signs in" by writing the
/// home's `.claude.json` with the account it is told to (`signs_in_as`, the
/// address the browser holds — `None`: the address it was hinted with), and
/// exits `flow`; its check answers what THAT HOME's `.claude.json` says, as
/// the vendor's does.
struct RenewHost {
    tty: bool,
    ssh_session: bool,
    exe: Option<PathBuf>,
    flow: i32,
    signs_in_as: Option<&'static str>,
    obstruct_checkpoint: bool,
    ssh_exit: i32,
    flows: RefCell<Vec<(String, String, RenewBrowser)>>,
    checked: RefCell<Vec<String>>,
    sshed: RefCell<Vec<(String, Vec<String>)>>,
}

impl RenewHost {
    fn new() -> RenewHost {
        RenewHost {
            tty: true,
            ssh_session: false,
            exe: Some(PathBuf::from("/opt/aterm/bin/aterm")),
            flow: 0,
            signs_in_as: None,
            obstruct_checkpoint: false,
            ssh_exit: 0,
            flows: RefCell::new(Vec::new()),
            checked: RefCell::new(Vec::new()),
            sshed: RefCell::new(Vec::new()),
        }
    }
}

/// What the vendor's `auth status --json` prints for a home whose
/// `.claude.json` holds `email`: the keys the check reads, and the
/// organization's, which it must drop.
fn status_for(email: Option<&str>) -> Captured {
    match email {
        Some(email) => Captured {
            stdout: format!(
                r#"{{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"{email}","orgId":"o-1","orgName":"Example Org","subscriptionType":"max"}}"#
            )
            .into_bytes(),
            code: Some(0),
        },
        None => Captured {
            stdout: br#"{"loggedIn":false,"authMethod":"none"}"#.to_vec(),
            code: Some(1),
        },
    }
}

impl AccountsHost for RenewHost {
    fn stdin_is_terminal(&self) -> bool {
        self.tty
    }

    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        panic!("a renewal asks no login outside its staging home (asked under {login:?})");
    }

    fn unaskable(&self) -> Option<String> {
        None
    }

    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        panic!("a renewal runs no plain sign-in ({dir:?})");
    }

    fn now(&self) -> i64 {
        0
    }

    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        self.flows
            .borrow_mut()
            .push((home.to_string(), email.to_string(), browser.clone()));
        let account = self.signs_in_as.unwrap_or(email);
        std::fs::write(
            Path::new(home).join(".claude.json"),
            format!(r#"{{"oauthAccount":{{"emailAddress":"{account}"}}}}"#),
        )
        .expect("the flow writes its home");
        if self.obstruct_checkpoint {
            std::fs::write(
                Path::new(home).join(accounts::RENEWAL_CHECKPOINT_FILE),
                b"unrecognized private work",
            )
            .unwrap();
        }
        Ok(self.flow)
    }

    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        self.checked.borrow_mut().push(home.to_string());
        let text = std::fs::read_to_string(Path::new(home).join(".claude.json")).ok();
        let email = text.as_deref().and_then(|t| {
            let doc: Value = aterm_json::from_str(t).ok()?;
            Some(
                doc.get("oauthAccount")?
                    .get("emailAddress")?
                    .as_str()?
                    .to_string(),
            )
        });
        Ok(status_for(email.as_deref()))
    }

    fn ssh(&self, host: &str, argv: &[String]) -> Result<i32, String> {
        self.sshed
            .borrow_mut()
            .push((host.to_string(), argv.to_vec()));
        Ok(self.ssh_exit)
    }

    fn under_ssh(&self) -> bool {
        self.ssh_session
    }

    fn current_exe(&self) -> Option<PathBuf> {
        self.exe.clone()
    }
}

fn renew(labels: &[&str], due: bool, on: Option<&[&str]>) -> AccountsCmd {
    AccountsCmd::Renew {
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        due,
        on: on.map(|hosts| hosts.iter().map(|h| (*h).to_string()).collect()),
    }
}

/// No address in anything this command printed or wrote but the roster.
fn no_address(env: &Env, said: &[&str]) {
    for text in said {
        assert!(
            !text.contains(OWNER) && !text.contains(WORK) && !text.contains("Example Org"),
            "an address or an organization was printed: {text}"
        );
    }
    let store = env.accounts_store();
    for path in [store.ledger_path(), store.state_file(), store.logins_file()] {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !text.contains("@example.com") && !text.contains("Example Org"),
            "{}: {text}",
            path.display()
        );
    }
}

fn logins_of(env: &Env) -> accounts::Logins {
    env.accounts_store().load_logins(env.now).expect("reads").0
}

#[test]
fn the_new_forms_parse_and_their_flags_belong_to_them() {
    let p = |line: &str| parse(&line.split(' ').map(str::to_string).collect::<Vec<_>>());
    assert_eq!(
        p("accounts renew --due").map(|(c, _)| c),
        Ok(Cmd::Accounts(renew(&[], true, None)))
    );
    assert_eq!(
        p("accounts renew main work-2 --on m3,m4:/opt/aterm").map(|(c, _)| c),
        Ok(Cmd::Accounts(renew(
            &["main", "work-2"],
            false,
            Some(&["m3", "m4:/opt/aterm"])
        )))
    );
    assert!(p("accounts renew").is_err(), "nothing to renew");
    assert_eq!(
        p("accounts renew --due --here").map(|(c, _)| c),
        Ok(Cmd::Accounts(renew(&[], true, Some(&[]))))
    );
    assert!(p("accounts renew --here").is_err(), "nothing to renew");
    assert_eq!(
        p(&format!(
            "accounts adopt-default main --email {OWNER} --browser Claude"
        ))
        .map(|(c, _)| c),
        Ok(Cmd::Accounts(AccountsCmd::AdoptDefault {
            label: "main".into(),
            email: OWNER.into(),
            browser: Some("Claude".into()),
        }))
    );
    assert!(p("accounts adopt-default main").is_err(), "needs --email");
    assert_eq!(
        p(&format!("accounts add w2 --email {WORK}")).map(|(c, _)| c),
        Ok(Cmd::Accounts(AccountsCmd::Add {
            label: "w2".into(),
            class: Class::Dedicated,
            email: Some(WORK.into()),
            browser: None,
        }))
    );
    for wrong in [
        format!("accounts login main --email {OWNER}"),
        "accounts --due".to_string(),
        "accounts use main --on m3".to_string(),
        "accounts renew --due --here --on m3".to_string(),
        "accounts --here".to_string(),
        "usage --browser x".to_string(),
    ] {
        assert!(p(&wrong).is_err(), "{wrong}");
    }
}

/// THE HAPPY PATH: the sign-in into a NEW staging home with the row's
/// address as its hint and the account's own profile as its opener, ONE
/// check in that home, the binding repointed there (the login it replaced
/// remembered), the login recorded `ok` with its `renewed` row — and no
/// address in anything printed or written.
#[test]
fn a_renewal_signs_in_to_a_staging_home_checks_it_there_and_repoints_the_binding() {
    let (_tmp, env) = renew_env("renew-happy");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let store = env.accounts_store();
    let staging = store.logins_dir().join("main+1");
    let staging = staging.to_str().expect("utf-8").to_string();
    let opener = store.dir.join("bin").join(crate::harness::open_url::OPENER);
    let browser = if cfg!(unix) {
        RenewBrowser::Profile {
            opener: opener.clone(),
            profile: "Claude main".into(),
        }
    } else {
        RenewBrowser::Default
    };
    assert_eq!(
        *host.flows.borrow(),
        vec![(staging.clone(), OWNER.to_string(), browser)]
    );
    assert_eq!(
        *host.checked.borrow(),
        vec![staging.clone()],
        "checked in that home alone"
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_link(&opener).expect("the opener's link"),
        PathBuf::from("/opt/aterm/bin/aterm")
    );
    let logins = logins_of(&env);
    let main = logins.get("main").expect("bound");
    assert_eq!(main.dir.as_deref(), Some(staging.as_str()));
    assert_eq!(main.was, vec![None], "the default login it replaced");
    assert_eq!(main.subscription.as_deref(), Some("max"));
    let proof = accounts::read_renewal_checkpoint(
        Path::new(&staging),
        &std::fs::canonicalize(&staging).unwrap(),
        env.now,
    )
    .expect("the verified CLI check leaves recovery evidence before binding");
    assert_eq!(proof.label, "main");
    assert_eq!(proof.email, OWNER);
    assert_eq!(proof.checked_at, main.renewed_at.unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(Path::new(&staging).join(accounts::RENEWAL_CHECKPOINT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let (state, _) = store.load(env.now).expect("state");
    assert_eq!(state.account("main").auth, Auth::Ok);
    let kinds: Vec<_> = store
        .tail(10)
        .expect("ledger")
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [accounts::Kind::Auth, accounts::Kind::Renewed]);
    assert!(store.wake_path().exists(), "the waiting decide again");
    assert!(out.contains("main: renewed"), "{out}");
    let opens = if cfg!(unix) {
        "it opens in browser profile 'Claude main'"
    } else {
        "it opens in your default browser"
    };
    assert!(out.contains(opens), "{out}");
    no_address(&env, &[&out, &err]);
    let (code, report, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    assert_eq!(code, exit(0));
    let line = report
        .lines()
        .find(|l| l.starts_with("main "))
        .expect("main's row");
    assert!(line.contains("login: idle, renew by "), "{line}");
    let plain = report
        .lines()
        .find(|l| l.starts_with("plain "))
        .expect("plain's row");
    assert!(
        !plain.contains("login:"),
        "a row with no email is as it was: {plain}"
    );
    let (_, json, _) = acc(AccountsCmd::Report { json: true }, &env, &host);
    let dir = crate::harness::host_path::json(Path::new(&staging));
    assert!(json.contains(&format!("\"dir\":\"{dir}\"")), "{json}");
    assert!(json.contains("\"renews\":true") && json.contains("\"login\":\"idle\""));
    no_address(&env, &[&report, &json]);

    let host = RenewHost::new();
    let (code, ..) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(0));
    let next = store.logins_dir().join("main+2");
    assert_eq!(host.flows.borrow()[0].0, next.to_str().expect("utf-8"));
    assert_eq!(
        logins_of(&env).get("main").expect("bound").was,
        vec![Some(staging), None]
    );
}

/// WHAT A RENEWAL SAYS OF THE SESSIONS ON THE LOGIN IT REPLACED is what the
/// window's supervisor does with them, by this machine's aterm.toml: moved
/// at their next idle turn end with `[harness] enabled`, `logins`, `relaunch` and
/// `resume_limits` on; with one of them off nothing moves them, and the
/// switch is named (the master switch, which leaves the others on in the
/// file, by its own name).
#[test]
fn a_renewal_says_what_moves_its_sessions_and_the_line_to_start_one_by_hand() {
    for (switch, off) in [
        ("", None),
        ("enabled = false", Some("enabled")),
        ("relaunch = false", Some("relaunch")),
        ("resume_limits = false", Some("resume_limits")),
        ("logins = \"off\"", Some("logins")),
    ] {
        let (_tmp, env) = renew_env(&format!("renew-said-{}", off.unwrap_or("on")));
        std::fs::write(
            env.config.as_deref().expect("a config"),
            format!("[harness]\n{switch}\n"),
        )
        .expect("write");
        let host = RenewHost::new();
        let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        let staging = env.accounts_store().logins_dir().join("main+1");
        let moves =
            "main: renewed; its sessions move onto the new login at their next idle turn end";
        match off {
            None => assert!(out.contains(moves), "{out}"),
            Some(off) => {
                assert!(
                    !out.contains("at their next idle turn end"),
                    "{switch}: {out}"
                );
                assert!(
                    out.contains(&format!(
                        "main: renewed; `[harness] {off}` is off, so nothing moves a session \
                         onto the new login"
                    )),
                    "{out}"
                );
            }
        }
        assert!(
            out.contains(&format!(
                "start Claude Code under it by hand with: CLAUDE_SECURESTORAGE_CONFIG_DIR='{}' \
                 claude --resume",
                staging.display()
            )),
            "{out}"
        );
        no_address(&env, &[&out, &err]);
    }
}

/// A STAGING HOME IS NO ROW'S LOGIN DIRECTORY: renewed, `main` is bound to
/// `logins/main+1`, and a label that reads like it — `main.1`, which `add`
/// admits — gets a login directory of its own, empty, never `main`'s live
/// one adopted as "a login made under this name before".
#[test]
fn a_staging_home_is_never_a_rows_login_directory() {
    let (_tmp, env) = renew_env("renew-names");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let store = env.accounts_store();
    let live = logins_of(&env)
        .get("main")
        .and_then(|b| b.dir.clone())
        .expect("bound");
    let add = |label: &str| AccountsCmd::Add {
        label: label.into(),
        class: Class::Dedicated,
        email: None,
        browser: None,
    };
    let (code, out, err) = acc(add("main.1"), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let own = store.logins_dir().join("main.1");
    assert_ne!(own.to_str(), Some(live.as_str()));
    assert!(!err.contains("already held files"), "{err}");
    assert_eq!(std::fs::read_dir(&own).expect("made").count(), 0);
    assert!(
        PathBuf::from(&live).join(".claude.json").exists(),
        "untouched"
    );
    let (code, _, err) = acc(add("main+1"), &env, &host);
    assert_eq!(code, exit(1), "{err}");
    let before = store.logins_dir().join("w9");
    std::fs::create_dir_all(&before).expect("mkdir");
    std::fs::write(before.join(".claude.json"), "{}").expect("write");
    let (code, _, err) = acc(add("w9"), &env, &host);
    assert_eq!(code, exit(0));
    assert!(err.contains("already held files"), "{err}");
}

/// THE WRONG ACCOUNT: the browser signed in another account. Nothing live
/// changes — no binding, no auth word, the staging home gone — and the
/// attempt is recorded by label.
#[test]
fn a_renewal_that_answers_another_account_changes_nothing_live() {
    let (_tmp, env) = renew_env("renew-wrong");
    let mut host = RenewHost::new();
    host.signs_in_as = Some("someone-else@example.com");
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(out.contains("main: REFUSED: wrong account"), "{out}");
    assert!(err.contains("ANOTHER account"), "{err}");
    let store = env.accounts_store();
    assert!(
        !store.logins_dir().join("main+1").exists(),
        "the staging home is gone"
    );
    let logins = logins_of(&env);
    let main = logins.get("main").expect("the attempt is kept");
    assert_eq!(main.dir, None, "nothing repointed");
    assert!(main.wrong_at.is_some());
    let (state, _) = store.load(env.now).expect("state");
    assert_eq!(state.account("main").auth, Auth::Unseen, "no auth word");
    let kinds: Vec<_> = store
        .tail(10)
        .expect("ledger")
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [accounts::Kind::WrongAccount]);
    assert!(!err.contains("someone-else"), "{err}");
    no_address(&env, &[&out, &err]);
    let (_, report, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    assert!(report.contains("last renewal: wrong account"), "{report}");
}

/// A FLOW THAT FAILS (Ctrl-C's 130, a denial's 1) RUNS NOTHING MORE: no
/// check, nothing recorded, the staging home gone.
#[test]
fn a_cancelled_renewal_runs_no_check_and_records_nothing() {
    for flow in [130, 1] {
        let (_tmp, env) = renew_env(&format!("renew-cancel-{flow}"));
        let mut host = RenewHost::new();
        host.flow = flow;
        let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
        assert_eq!(code, exit(1), "{out}{err}");
        assert!(
            host.checked.borrow().is_empty(),
            "no `auth status` after a failed flow"
        );
        assert!(
            err.contains(&format!("exited {flow}; nothing was recorded")),
            "{err}"
        );
        let store = env.accounts_store();
        assert!(!store.logins_dir().join("main+1").exists());
        assert!(!store.logins_file().exists() && !store.state_file().exists());
        assert!(store.tail(10).expect("ledger").is_empty());
    }
}

#[test]
fn a_verified_renewal_keeps_staging_and_records_nothing_when_checkpoint_is_obstructed() {
    let (_tmp, env) = renew_env("renew-checkpoint-obstructed");
    let mut host = RenewHost::new();
    host.obstruct_checkpoint = true;
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(host.flows.borrow().len(), 1);
    assert_eq!(host.checked.borrow().len(), 1);
    let store = env.accounts_store();
    let staging = store.logins_dir().join("main+1");
    assert!(staging.join(".claude.json").is_file());
    assert_eq!(
        std::fs::read(staging.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap(),
        b"unrecognized private work",
    );
    assert!(!store.logins_file().exists() && !store.state_file().exists());
    assert!(store.tail(10).unwrap().is_empty());
    assert!(
        err.contains("completion checkpoint could not be saved"),
        "{err}"
    );
    assert!(err.contains("its home is kept"), "{err}");
    no_address(&env, &[&out, &err]);
}

/// A recording failure is not a rollback: the binding and auth word can
/// already be durable when appending the ledger fails.
#[test]
fn a_renewal_keeps_its_published_home_when_the_ledger_cannot_be_appended() {
    let (_tmp, env) = renew_env("renew-published-ledger-failure");
    let store = env.accounts_store();
    store.ensure().expect("store");
    std::fs::create_dir(store.ledger_path()).expect("block ledger append");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(host.flows.borrow().len(), 1);
    assert_eq!(host.checked.borrow().len(), 1);

    let logins = logins_of(&env);
    let bound = logins.get("main").expect("binding was already published");
    let home = Path::new(bound.dir.as_deref().expect("a bound home"));
    assert_eq!(home, store.logins_dir().join("main+1"));
    let (state, _) = store.load(env.now).expect("state was already published");
    assert_eq!(state.account("main").auth, Auth::Ok);
    assert!(
        home.is_dir(),
        "a durable binding must not point at a deleted home"
    );
    assert!(
        home.join(".claude.json").is_file(),
        "keep the checked login"
    );
    no_address(&env, &[&out, &err]);
}

/// OVER SSH: nothing opens here — `BROWSER` is aterm's opener with no
/// profile, which opens nothing (MEASURED: Claude Code calls its opener under
/// `SSH_CONNECTION` too, `open` where `BROWSER` is unset) — and the line says
/// which profile to open the URL it prints in.
#[test]
fn over_ssh_the_paste_code_flow_runs_and_the_profile_is_named() {
    let (_tmp, env) = renew_env("renew-ssh");
    let mut host = RenewHost::new();
    host.ssh_session = true;
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let opener = env
        .accounts_store()
        .dir
        .join("bin")
        .join(crate::harness::open_url::OPENER);
    assert_eq!(
        host.flows.borrow()[0].2,
        RenewBrowser::Remote {
            // Only Unix makes the opener's link (`opener_link`).
            opener: cfg!(unix).then_some(opener),
        },
        "nothing opens on the machine the owner is not at"
    );
    assert!(
        out.contains(
            "open the URL it prints in Chrome profile 'Claude main' on the machine you are at"
        ),
        "{out}"
    );
    let (_tmp, env) = renew_env("renew-default");
    let host = RenewHost::new();
    let (code, ..) = acc(renew(&["work-2"], false, None), &env, &host);
    assert_eq!(code, exit(0));
    assert_eq!(host.flows.borrow()[0].2, RenewBrowser::Default);
    assert!(!env.accounts_store().dir.join("bin").exists());
}

/// `--due` RENEWS THE DUE ONES ALONE, and nothing due is said, exit 0.
#[test]
fn due_renews_only_what_is_due_and_says_when_nothing_is() {
    let (_tmp, env) = renew_env("renew-due");
    let host = RenewHost::new();
    let (code, out, _) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0));
    assert_eq!(out, "nothing is due on this machine\n");
    assert!(host.flows.borrow().is_empty());
    let mut note = None;
    env.accounts_store()
        .transact_logins(env.now, &mut note, |_, logins| {
            accounts::note_login_notice(logins, "work-2", 2, env.now);
            Ok(Vec::new())
        })
        .expect("recorded");
    let (code, out, err) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let renewed: Vec<_> = host
        .flows
        .borrow()
        .iter()
        .map(|(_, e, _)| e.clone())
        .collect();
    assert_eq!(renewed, [WORK]);
    assert!(out.contains("work-2: renewed"), "{out}");
    no_address(&env, &[&out, &err]);
    let mut quiet = RenewHost::new();
    quiet.tty = false;
    let (code, out, _) = acc(renew(&["main"], false, None), &env, &quiet);
    assert_eq!(code, exit(0));
    assert_eq!(out, "due here: main\n");
    assert!(quiet.flows.borrow().is_empty());

    let (code, out, err) = acc(
        AccountsCmd::Add {
            label: "work-3".into(),
            class: Class::Dedicated,
            email: Some("w3@example.test".into()),
            browser: None,
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            let mut ev = Vec::new();
            for label in ["main", "work-2"] {
                ev.extend(accounts::note_wall(
                    state,
                    label,
                    "seven_day",
                    Some(env.now + 86_400),
                    env.now,
                    env.now,
                ));
            }
            Ok(ev)
        })
        .expect("walls recorded");
    let wall_host = RenewHost::new();
    let (code, out, err) = acc(renew(&[], true, None), &env, &wall_host);
    assert_eq!(code, exit(0), "{out}{err}");
    let wall_renewed: Vec<_> = wall_host
        .flows
        .borrow()
        .iter()
        .map(|(_, e, _)| e.clone())
        .collect();
    assert_eq!(
        wall_renewed,
        ["w3@example.test"],
        "--due selects the un-enrolled successor when all enrolled rows are at a wall"
    );
    assert!(out.contains("work-3: renewed"), "{out}");
    no_address(&env, &[&out, &err]);
}

/// Due selection needs the auth observations. A state this build cannot
/// read must refuse the whole sitting before any local flow or remote run.
#[test]
fn due_renewal_refuses_foreign_state_before_local_or_remote_actions() {
    let (_tmp, env) = renew_env("renew-due-foreign-state");
    let store = env.accounts_store();
    store.ensure().expect("store");
    let foreign = "{\"schema\":999}\n";
    std::fs::write(store.state_file(), foreign).expect("foreign state");
    assert!(
        store.load(env.now).is_err(),
        "the state is not this build's"
    );
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&[], true, Some(&["m3"])), &env, &host);
    assert!(host.flows.borrow().is_empty(), "no local renewal");
    assert!(host.checked.borrow().is_empty(), "no local status check");
    assert!(
        host.sshed.borrow().is_empty(),
        "state uncertainty must not launch remote renewals: {:?}",
        host.sshed.borrow()
    );
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(store.state_file()).unwrap(),
        foreign
    );
    no_address(&env, &[&out, &err]);
}

#[test]
fn uncertain_binding_bytes_refuse_renewal_before_local_or_remote_actions() {
    let (_tmp, env) = renew_env("renew-uncertain-bindings");
    let store = env.accounts_store();
    store.ensure().expect("store");
    let damaged = "{\"schema\":";
    std::fs::write(store.logins_file(), damaged).expect("torn bindings");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&["main"], false, Some(&["m3"])), &env, &host);
    assert!(host.flows.borrow().is_empty(), "no local renewal");
    assert!(host.checked.borrow().is_empty(), "no local status check");
    assert!(host.sshed.borrow().is_empty(), "no remote renewal");
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(store.logins_file()).unwrap(),
        damaged
    );
    no_address(&env, &[&out, &err]);
}

/// THE OTHER MACHINES, in the same sitting: `ssh -t <host> <aterm> harness
/// accounts renew --due --here`, for `--on`'s and, under `--due`, the
/// config's.
#[test]
fn other_machines_are_renewed_over_ssh_in_one_sitting() {
    let (_tmp, env) = renew_env("renew-on");
    let host = RenewHost::new();
    let (code, out, err) = acc(
        renew(&[], true, Some(&["m3", "m4:/opt/aterm"])),
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let argv = |aterm: &str| -> Vec<String> {
        [aterm, "harness", "accounts", "renew", "--due", "--here"]
            .map(str::to_string)
            .to_vec()
    };
    assert_eq!(
        *host.sshed.borrow(),
        vec![
            ("m3".to_string(), argv("aterm")),
            ("m4".to_string(), argv("/opt/aterm"))
        ]
    );
    assert!(
        out.contains("renewing on m3:") && out.contains("renewing on m4:"),
        "{out}"
    );
    std::fs::write(
        env.config.as_ref().expect("a config"),
        "[harness]\nhosts = [\"m5\"]\n",
    )
    .expect("write");
    let host = RenewHost::new();
    let (code, ..) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0));
    assert_eq!(host.sshed.borrow().len(), 1);
    assert_eq!(host.sshed.borrow()[0].0, "m5");
    let remote = host.sshed.borrow()[0].1[2..].to_vec();
    let Ok((Cmd::Accounts(there), _)) = parse(&remote) else {
        panic!("{remote:?} parses");
    };
    let host = RenewHost::new();
    let (code, out, err) = acc(there, &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert!(host.sshed.borrow().is_empty(), "one machine deep: {out}");
    let mut failing = RenewHost::new();
    failing.ssh_exit = 1;
    let (code, _, err) = acc(renew(&[], true, None), &env, &failing);
    assert_eq!(code, exit(1));
    assert!(err.contains("on m5: exited 1"), "{err}");
    std::fs::write(env.config.as_ref().expect("a config"), "").expect("write");
    let host = RenewHost::new();
    let (code, ..) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0));
    assert!(host.sshed.borrow().is_empty(), "no machine named, no ssh");
    for bad in ["-oProxyCommand=x", "m3;rm", "m3:relative/aterm", "m3:/a b"] {
        let host = RenewHost::new();
        let (code, _, err) = acc(renew(&["main"], false, Some(&[bad])), &env, &host);
        assert_eq!(code, exit(1), "{bad}");
        assert!(err.contains("is not a machine ("), "{err}");
        assert!(host.flows.borrow().is_empty() && host.sshed.borrow().is_empty());
    }
}

/// EVERY LINE THAT TELLS THE OWNER TO SIGN A ROW IN NAMES THE VERB THAT
/// ROW TAKES: `renew` for a row with an email (`login` refuses it), `login`
/// for one without — the report's not-enrolled reason, `use`'s refusal of a
/// row not enrolled, and `add`'s of a row already listed.
#[test]
fn each_line_that_signs_a_row_in_names_its_own_verb() {
    let (_tmp, env) = renew_env("renew-verbs");
    let host = RenewHost::new();
    let add = |label: &str, email: Option<&str>| AccountsCmd::Add {
        label: label.into(),
        class: Class::Dedicated,
        email: email.map(str::to_string),
        browser: None,
    };
    let (code, out, err) = acc(add("w3", Some("w3@example.com")), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let (_, report, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    for (label, verb) in [("w3", "renew"), ("plain", "login")] {
        let reason = format!(
            "{label}: not-enrolled (no login made yet: run `aterm harness accounts {verb} \
             {label}`)"
        );
        assert!(report.contains(&reason), "{reason}\n{report}");
        let (code, _, err) = acc(
            AccountsCmd::Use {
                label: label.into(),
            },
            &env,
            &host,
        );
        assert_eq!(code, exit(1));
        assert!(
            err.contains(&format!(
                "aterm harness accounts {verb} {label}`, in a terminal"
            )),
            "{err}"
        );
        let (code, _, err) = acc(add(label, None), &env, &host);
        assert_eq!(code, exit(1));
        assert!(
            err.contains(&format!(
                "`aterm harness accounts {verb} {label}` signs it in"
            )),
            "{err}"
        );
    }
}

/// A ROW WITH AN EMAIL SIGNS IN ONLY THROUGH `renew`: `login` refuses it,
/// running nothing; one with none is refused by `renew`, by name.
#[test]
fn login_and_renew_each_refuse_the_other_ones_rows() {
    let (_tmp, env) = renew_env("renew-refusals");
    let host = RenewHost::new();
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "work-2".into(),
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(1));
    assert!(err.contains("aterm harness accounts renew work-2"), "{err}");
    let (code, _, err) = acc(renew(&["plain"], false, None), &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains("`plain` has no email"), "{err}");
    let (code, _, err) = acc(renew(&["nobody"], false, None), &env, &host);
    assert_eq!(code, exit(1));
    assert!(err.contains("`nobody` is not in the roster"), "{err}");
    assert!(host.flows.borrow().is_empty());
    no_address(&env, &[&err]);
}

/// `adopt-default` LISTS THIS MACHINE'S OWN LOGIN AS ONE RENEWABLE ROW, and
/// refuses a second own login, a listed label and a bad address (unquoted).
#[test]
fn adopt_default_lists_this_machines_own_login_once() {
    let tmp = tmp_dir("adopt");
    let env = env_at(&tmp);
    let adopt = |label: &str, email: &str| AccountsCmd::AdoptDefault {
        label: label.into(),
        email: email.into(),
        browser: Some("Claude m7-a".into()),
    };
    let (code, out, err) = acc_quiet(adopt("m7-a", OWNER), &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.contains("renew it with: aterm harness accounts renew m7-a"),
        "{out}"
    );
    let roster = Roster::load(&env.accounts_roster().expect("a path")).expect("admits");
    assert_eq!(
        roster.rows,
        vec![accounts::Row {
            label: "m7-a".into(),
            email: Some(OWNER.into()),
            browser: Some("Claude m7-a".into()),
            ..accounts::Row::default()
        }]
    );
    let (code, _, err) = acc_quiet(adopt("m7-b", WORK), &env);
    assert_eq!(code, exit(1));
    assert!(err.contains("listed already, as `m7-a`"), "{err}");
    let (code, _, err) = acc_quiet(adopt("m7-a", WORK), &env);
    assert_eq!(code, exit(1));
    assert!(err.contains("already listed"), "{err}");
    let fresh = tmp_dir("adopt-bad");
    let env2 = env_at(&fresh);
    let (code, _, err) = acc_quiet(adopt("m7-a", "not-an-address"), &env2);
    assert_eq!(code, exit(1));
    assert!(err.contains("its email is not one address"), "{err}");
    assert!(!err.contains("not-an-address"), "{err}");
    assert!(!env2.accounts_roster().expect("a path").exists());
    no_address(&env, &[&out, &err]);
    let (code, out, _) = acc_quiet(
        AccountsCmd::Add {
            label: "w2".into(),
            class: Class::Dedicated,
            email: Some(WORK.into()),
            browser: None,
        },
        &env,
    );
    assert_eq!(code, exit(0));
    assert!(
        out.contains("run: aterm harness accounts renew w2"),
        "{out}"
    );
}

/// THE REAL COMMAND BUILDERS, exactly: the sign-in's argv and environment
/// for each browser, and the check run in the staging home by a `/bin/sh`
/// stand-in that answers signed in ONLY there and only with no login
/// variable set — and records each run, so a whole sitting through the real
/// runner can be read back: every `auth status` ran in a staging home this
/// sitting made.
#[test]
fn the_sign_in_and_the_check_are_built_exactly() {
    let envs = |cmd: &aterm_uds::spawnfd::Command| -> BTreeSet<(String, Option<String>)> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    };
    let program = std::ffi::OsStr::new("claude");
    let profile = RenewBrowser::Profile {
        opener: PathBuf::from("/s/bin/aterm-open-url"),
        profile: "Claude main".into(),
    };
    let cmd = renew_command(program, "/s/logins/main+1", OWNER, &profile);
    assert_eq!(
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["auth", "login", "--claudeai", "--email", OWNER]
    );
    let set = |k: &str, v: &str| (k.to_string(), Some(v.to_string()));
    let gone = |k: &str| (k.to_string(), None);
    assert_eq!(
        envs(&cmd),
        BTreeSet::from([
            gone(accounts::LOGIN_ENV),
            set(accounts::HOME_ENV, "/s/logins/main+1"),
            set("BROWSER", "/s/bin/aterm-open-url"),
            set(crate::harness::open_url::PROFILE_ENV, "Claude main"),
        ])
    );
    let remote = renew_command(
        program,
        "/h",
        OWNER,
        &RenewBrowser::Remote {
            opener: Some(PathBuf::from("/s/bin/aterm-open-url")),
        },
    );
    assert!(envs(&remote).contains(&set("BROWSER", "/s/bin/aterm-open-url")));
    assert!(
        envs(&remote).contains(&gone(crate::harness::open_url::PROFILE_ENV)),
        "no profile: the opener opens nothing"
    );
    let bare = renew_command(program, "/h", OWNER, &RenewBrowser::Remote { opener: None });
    assert!(envs(&bare).contains(&gone("BROWSER")));
    let default = renew_command(program, "/h", OWNER, &RenewBrowser::Default);
    assert!(!envs(&default).iter().any(|(k, _)| k == "BROWSER"));
    assert!(envs(&default).contains(&gone(crate::harness::open_url::PROFILE_ENV)));
}

#[cfg(unix)]
#[test]
fn a_whole_sitting_through_a_stand_in_checks_only_in_staging_homes() {
    let (tmp, env) = renew_env("renew-stand-in");
    let program = tmp.path().join("bin").join("claude");
    std::fs::create_dir_all(program.parent().expect("dir")).expect("mkdir");
    aterm_tempfile::stand_in(
        &program,
        &format!(
            "#!/bin/sh\nlog=\"${{0%/*}}/ran\"\necho \"$1 $2 home=$CLAUDE_CONFIG_DIR \
             login=${{{login}+set}}\" >> \"$log\"\nif [ \"$2\" = login ]; then printf '%s' \"$5\" > \
             \"$CLAUDE_CONFIG_DIR/.email\"; echo '{{}}' > \"$CLAUDE_CONFIG_DIR/.claude.json\"; exit 0; \
             fi\nif [ -n \"${{{login}+set}}\" ] || [ ! -f \"$CLAUDE_CONFIG_DIR/.email\" ]; then echo \
             '{{\"loggedIn\":false}}'; exit 1; fi\nprintf \
             '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"%s\",\"subscriptionType\":\"max\"}}' \
             \"$(cat \"$CLAUDE_CONFIG_DIR/.email\")\"\n",
            login = accounts::LOGIN_ENV,
        ),
    )
    .expect("stand-in");
    /// [`AccountsHost`] over the REAL builders with the stand-in's path.
    struct StandIn(PathBuf);
    impl AccountsHost for StandIn {
        fn stdin_is_terminal(&self) -> bool {
            true
        }
        fn auth_status(&self, login: &str) -> Result<Captured, String> {
            panic!("asked under {login:?}");
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no plain sign-in");
        }
        fn now(&self) -> i64 {
            0
        }
        fn renew_flow(&self, home: &str, email: &str, b: &RenewBrowser) -> Result<i32, String> {
            renew_flow_of(self.0.as_os_str(), home, email, b)
        }
        fn status_in_home(&self, home: &str) -> Result<Captured, String> {
            let user = |name: &str| (name == "USER").then(|| OsString::from("someone"));
            status_in_home_within(
                self.0.as_os_str(),
                home,
                &user,
                aterm_spec::wait::MUST_HAPPEN,
            )
        }
    }
    let host = StandIn(program.clone());
    let (code, out, err) = acc(renew(&["main", "work-2"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let ran = std::fs::read_to_string(tmp.path().join("bin").join("ran")).expect("it ran");
    let logins = env.accounts_store().logins_dir();
    let staging: Vec<String> = ["main+1", "work-2+1"]
        .map(|d| logins.join(d).to_str().expect("utf-8").to_string())
        .to_vec();
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(
        lines,
        [
            format!("auth login home={} login=", staging[0]),
            format!("auth status home={} login=", staging[0]),
            format!("auth login home={} login=", staging[1]),
            format!("auth status home={} login=", staging[1]),
        ]
    );
    let bound = logins_of(&env);
    assert_eq!(
        bound.get("main").and_then(|b| b.dir.clone()),
        Some(staging[0].clone())
    );
    assert_eq!(
        bound.get("work-2").and_then(|b| b.dir.clone()),
        Some(staging[1].clone())
    );
    no_address(&env, &[&out, &err]);
}

/// WHAT A RENEWAL SAYS OF THE SESSIONS ON THE OLD LOGIN names the switch
/// that holds them, Claude Code's own among them: the logins are its
/// sessions', and the window's host turns the accounts and logins off with
/// `[harness.claude] enabled = false`, so a renewal promised a move nothing
/// would make.
#[test]
fn a_renewal_names_claude_codes_own_switch_when_it_holds_the_sessions() {
    let read =
        |text: &str| renewed_sessions(&crate::supervise::SupervisorConfig::from_aterm_toml(text).0);
    assert_eq!(
        read(""),
        "its sessions move onto the new login at their next idle turn end"
    );
    assert_eq!(
        read("[harness.claude]\nenabled = false\n"),
        "`[harness.claude] enabled` is off, so nothing moves a session onto the new login"
    );
    assert_eq!(
        read("[harness.codex]\nenabled = false\n"),
        "its sessions move onto the new login at their next idle turn end"
    );
    assert_eq!(
        read("[harness]\nenabled = false\n[harness.claude]\nenabled = false\n"),
        "`[harness] enabled` is off, so nothing moves a session onto the new login"
    );
}

#[cfg(unix)]
#[test]
fn a_mailbox_directory_runs_accounts_renew_without_a_terminal_through_the_opener() {
    let (tmp, env) = renew_env("renew-mailbox-headless");
    let mailbox = env
        .accounts_store()
        .dir
        .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR);
    std::fs::create_dir_all(&mailbox).expect("mkdir mailbox");
    let program = tmp.path().join("bin").join("claude");
    std::fs::create_dir_all(program.parent().expect("dir")).expect("mkdir");
    aterm_tempfile::stand_in(
        &program,
        &format!(
            "#!/bin/sh\nlog=\"${{0%/*}}/ran\"\necho \"$1 $2 browser=${{BROWSER##*/}} \
             mailbox=${{{mailbox_env}+set}} email=${{{email_env}+set}}\" >> \"$log\"\n\
             if [ \"$2\" = login ]; then printf '%s' \"$5\" > \"$CLAUDE_CONFIG_DIR/.email\"; \
             echo '{{}}' > \"$CLAUDE_CONFIG_DIR/.claude.json\"; exit 0; fi\n\
             printf '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"%s\",\"subscriptionType\":\"max\"}}' \
             \"$(cat \"$CLAUDE_CONFIG_DIR/.email\")\"\n",
            mailbox_env = crate::harness::open_url::MAILBOX_ENV,
            email_env = crate::harness::open_url::EMAIL_ENV,
        ),
    )
    .expect("stand-in");
    struct HeadlessMailboxStandIn(PathBuf);
    impl AccountsHost for HeadlessMailboxStandIn {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, login: &str) -> Result<Captured, String> {
            panic!("asked under {login:?}");
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no plain sign-in");
        }
        fn now(&self) -> i64 {
            0
        }
        fn current_exe(&self) -> Option<PathBuf> {
            Some(self.0.clone())
        }
        fn renew_flow(&self, home: &str, email: &str, b: &RenewBrowser) -> Result<i32, String> {
            renew_flow_of(self.0.as_os_str(), home, email, b)
        }
        fn status_in_home(&self, home: &str) -> Result<Captured, String> {
            let user = |name: &str| (name == "USER").then(|| OsString::from("someone"));
            status_in_home_within(
                self.0.as_os_str(),
                home,
                &user,
                aterm_spec::wait::MUST_HAPPEN,
            )
        }
    }
    let host = HeadlessMailboxStandIn(program);
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let ran = std::fs::read_to_string(tmp.path().join("bin").join("ran")).expect("it ran");
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(
        lines,
        [
            "auth login browser=aterm-open-url mailbox=set email=set",
            "auth status browser= mailbox= email=",
        ]
    );
}
