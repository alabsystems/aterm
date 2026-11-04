// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm harness accounts renew` and `adopt-default`.

use std::cell::RefCell;

use super::*;

const OWNER: &str = "owner@example.com";
const WORK: &str = "w2@example.com";

#[cfg(unix)]
struct NativeRenewal {
    program: PathBuf,
    user_home: OsString,
    within: std::time::Duration,
}

#[cfg(unix)]
impl AccountsHost for NativeRenewal {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("a failed renewal must not check a live login")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("a named renewal must not use plain login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn current_exe(&self) -> Option<PathBuf> {
        Some(self.program.clone())
    }
    fn renew_flow(&self, home: &str, email: &str, browser: &RenewBrowser) -> Result<i32, String> {
        self.renew_outcome(home, email, browser)
            .map(|exit| exit.code)
            .map_err(|failure| failure.to_string())
    }
    fn renew_outcome(
        &self,
        home: &str,
        email: &str,
        browser: &RenewBrowser,
    ) -> Result<RenewalExit, RenewalFailure> {
        renew_flow_of(
            self.program.as_os_str(),
            home,
            email,
            browser,
            &|name| match name {
                "HOME" => Some(self.user_home.clone()),
                "USER" => Some("fixture-user".into()),
                _ => None,
            },
            Some(self.within),
        )
    }
    fn status_in_home(&self, _: &str) -> Result<Captured, String> {
        panic!("a failed sign-in must not be considered successful")
    }
}

#[test]
fn a_running_renewal_blocks_readoption_of_its_removed_label() {
    let (_tmp, env) = renew_env("renew-readoption");
    let path = env.accounts_roster().unwrap();
    let row = accounts::Row {
        label: "main".into(),
        email: Some(OWNER.into()),
        ..accounts::Row::default()
    };
    std::fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    let changed_env = env.clone();
    let changed_path = path.clone();
    let mut host = RenewHost::new();
    host.during_flow = Some(Box::new(move || {
        // Model an external edit while the managed provider flow is pending.
        std::fs::write(&changed_path, "").unwrap();
        let (code, out, err) = acc_quiet(
            AccountsCmd::AdoptDefault {
                label: "main".into(),
                email: Some(OWNER.into()),
                identity_email: None,
                browser: None,
            },
            &changed_env,
        );
        assert_eq!(code, exit(1), "{out}{err}");
        assert!(err.contains(accounts::SIGNIN_HELD), "{err}");
        assert_eq!(std::fs::read_to_string(&changed_path).unwrap(), "");
    }));
    let (code, _, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{err}");
    assert_eq!(host.flows.borrow().len(), 1);
    assert!(err.contains("changed"), "{err}");
    assert!(
        env.accounts_store()
            .load_logins(env.now)
            .unwrap()
            .0
            .labels
            .is_empty()
    );
}

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
    let roster = roster_text(work.to_str().expect("utf-8"));
    // Portable renewal semantics use the system browser where no opener is supported.
    // Explicit named-profile admission is covered separately and must never fall back.
    #[cfg(not(unix))]
    let roster = roster.replace("browser = \"Claude main\"\n", "");
    std::fs::write(env.accounts_roster().expect("a config dir"), roster).expect("write");
    (tmp, env)
}

fn cached_profile_only(env: &Env) {
    let browser =
        env.home.as_deref().unwrap().join(
            ".cache/aterm/browser.noindex/chrome-headless-shell-fixture/chrome-headless-shell",
        );
    std::fs::create_dir_all(browser.parent().unwrap()).unwrap();
    std::fs::write(browser, b"inert browser marker; RenewHost runs no program").unwrap();
    let profile = crate::harness::headless_oauth::browser_profile_dir(
        &env.accounts_store().dir,
        Some("main"),
    );
    std::fs::create_dir_all(profile).unwrap();
    std::fs::write(
        env.config.as_deref().unwrap(),
        "[harness]\nusage_tap = false\n",
    )
    .unwrap();
}

struct RenewHost {
    tty: bool,
    ssh_session: bool,
    exe: Option<PathBuf>,
    flow: i32,
    flow_after_first: Option<i32>,
    signs_in_as: Option<&'static str>,
    obstruct_checkpoint: bool,
    during_flow: Option<Box<dyn Fn()>>,
    during_status: Option<Box<dyn Fn()>>,
    remote_exit: i32,
    flows: RefCell<Vec<(String, String, RenewBrowser)>>,
    checked: RefCell<Vec<String>>,
    remotes: RefCell<Vec<String>>,
    /// The mailbox the host's environment names ([`AccountsHost::machine_var`]), if any.
    env_mailbox: Option<PathBuf>,
    env_browser: Option<PathBuf>,
    agent: bool,
}

impl RenewHost {
    fn new() -> RenewHost {
        RenewHost {
            tty: true,
            ssh_session: false,
            exe: Some(std::env::current_exe().expect("the test executable")),
            flow: 0,
            flow_after_first: None,
            signs_in_as: None,
            obstruct_checkpoint: false,
            during_flow: None,
            during_status: None,
            remote_exit: 0,
            flows: RefCell::new(Vec::new()),
            checked: RefCell::new(Vec::new()),
            remotes: RefCell::new(Vec::new()),
            env_mailbox: None,
            env_browser: None,
            agent: false,
        }
    }
}

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
        if let Some(change) = &self.during_flow {
            change();
        }
        Ok(if self.flows.borrow().len() > 1 {
            self.flow_after_first.unwrap_or(self.flow)
        } else {
            self.flow
        })
    }

    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        if let Some(change) = &self.during_status {
            change();
        }
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

    fn renew_remote(&self, host: &str) -> Result<crate::harness::remote_renew::Output, String> {
        self.remotes.borrow_mut().push(host.to_owned());
        Ok(crate::harness::remote_renew::Output {
            code: self.remote_exit,
            stdout: "remote due pass completed\n".into(),
            stderr: "remote observation\n".into(),
        })
    }

    fn under_ssh(&self) -> bool {
        self.ssh_session
    }

    fn current_exe(&self) -> Option<PathBuf> {
        self.exe.clone()
    }

    fn machine_var(&self, name: &str) -> Option<OsString> {
        match name {
            crate::harness::open_url::MAILBOX_ENV => self.env_mailbox.as_ref(),
            crate::harness::headless_oauth::BROWSER_BIN_ENV => self.env_browser.as_ref(),
            "CLAUDECODE" if self.agent => return Some("1".into()),
            _ => None,
        }
        .map(|path| path.clone().into_os_string())
    }
}

fn renew(labels: &[&str], due: bool, on: Option<&[&str]>) -> AccountsCmd {
    AccountsCmd::Renew {
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        due,
        on: on.map(|hosts| hosts.iter().map(|h| (*h).to_string()).collect()),
    }
}

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

#[cfg(unix)]
#[test]
fn renewal_opener_failures_refuse_before_provider_or_binding_changes() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    for fault in [
        "unavailable",
        "missing",
        "directory",
        "nonexec",
        "otherexec",
        "bin",
        "alias",
    ] {
        // Root's access check intentionally permits any executable bit. The owner-only
        // permission regression applies to an ordinary user, as the shipped app runs.
        // SAFETY: getuid has no arguments or pointer preconditions.
        if fault == "otherexec" && unsafe { libc::getuid() } == 0 {
            continue;
        }
        let (tmp, env) = renew_env(&format!("renew-opener-{fault}"));
        let store = env.accounts_store();
        let bin = store.dir.join("bin");
        let link = bin.join(crate::harness::open_url::OPENER);
        let mut host = RenewHost::new();
        host.tty = false;
        host.env_mailbox = Some(tmp.path().to_owned());
        let source = tmp.path().join("private-opener-source");
        let (stage, kind) = match fault {
            "unavailable" => {
                host.exe = None;
                (OpenerStage::Executable, None)
            }
            "missing" => {
                host.exe = Some(source.clone());
                (OpenerStage::Executable, Some(io::ErrorKind::NotFound))
            }
            "directory" => {
                std::fs::create_dir(&source).unwrap();
                host.exe = Some(source.clone());
                (OpenerStage::Executable, Some(io::ErrorKind::InvalidInput))
            }
            "nonexec" | "otherexec" => {
                std::fs::write(&source, b"not executed").unwrap();
                let mode = if fault == "otherexec" { 0o601 } else { 0o600 };
                std::fs::set_permissions(&source, std::fs::Permissions::from_mode(mode)).unwrap();
                host.exe = Some(source.clone());
                (
                    OpenerStage::Executable,
                    Some(io::ErrorKind::PermissionDenied),
                )
            }
            "bin" => (OpenerStage::HelperDirectory, None),
            "alias" => (OpenerStage::Publication, None),
            _ => unreachable!(),
        };
        std::fs::create_dir_all(&store.dir).unwrap();
        if fault == "bin" {
            std::fs::write(&bin, b"retained obstruction").unwrap();
        } else {
            std::fs::create_dir(&bin).unwrap();
            if fault == "alias" {
                std::fs::create_dir(&link).unwrap();
                std::fs::write(link.join("retained"), b"untouched").unwrap();
            } else {
                symlink(std::env::current_exe().unwrap(), &link).unwrap();
            }
        }
        let before_link = std::fs::read_link(&link).ok();
        let paths = [store.state_file(), store.logins_file(), store.ledger_path()];
        let before = paths.each_ref().map(|path| std::fs::read(path).ok());
        let error = opener_link(&store, &host).unwrap_err();
        assert_eq!(error.stage, stage, "{fault}");
        if kind.is_some() || fault == "unavailable" {
            assert_eq!(error.kind, kind, "{fault}");
        }
        let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &host);
        assert_eq!(code, exit(1), "{fault}: {out}{err}");
        assert!(err.contains(&error.to_string()), "{fault}: {err}");
        assert!(!err.contains(source.to_str().unwrap()), "{fault}: {err}");
        assert!(host.flows.borrow().is_empty(), "{fault}");
        assert!(host.checked.borrow().is_empty(), "{fault}");
        assert!(host.remotes.borrow().is_empty(), "{fault}");
        assert_eq!(
            paths.each_ref().map(|path| std::fs::read(path).ok()),
            before,
            "{fault}"
        );
        assert!(!store.logins_dir().exists(), "{fault}");
        assert_eq!(std::fs::read_link(&link).ok(), before_link, "{fault}");
        if fault == "bin" {
            assert_eq!(std::fs::read(&bin).unwrap(), b"retained obstruction");
        } else if fault == "alias" {
            assert_eq!(std::fs::read(link.join("retained")).unwrap(), b"untouched");
        }
    }
}

#[cfg(unix)]
#[test]
fn renewal_ready_opener_runs_the_selected_headless_route() {
    let (tmp, env) = renew_env("renew-opener-ready");
    let mut host = RenewHost::new();
    host.tty = false;
    host.env_mailbox = Some(tmp.path().to_owned());
    let target = std::fs::canonicalize(host.exe.as_ref().unwrap()).unwrap();
    let mut relative = PathBuf::new();
    for component in std::env::current_dir().unwrap().components() {
        if matches!(component, std::path::Component::Normal(_)) {
            relative.push("..");
        }
    }
    relative.push(target.strip_prefix("/").unwrap());
    assert!(!relative.is_absolute());
    host.exe = Some(relative);
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let opener = env
        .accounts_store()
        .dir
        .join("bin")
        .join(crate::harness::open_url::OPENER);
    assert_eq!(std::fs::read_link(&opener).unwrap(), target);
    let calls = host.flows.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, WORK);
    assert_eq!(
        calls[0].2,
        RenewBrowser::Remote {
            opener: Some(opener)
        }
    );
    assert_eq!(host.checked.borrow().as_slice(), [calls[0].0.clone()]);
    assert!(logins_of(&env).get("main").is_none());
}

#[test]
fn renewal_manual_routes_do_not_require_an_unavailable_opener() {
    for ssh in [false, true] {
        let (_tmp, env) = renew_env("renew-opener-manual");
        let mut host = RenewHost::new();
        host.exe = None;
        host.ssh_session = ssh;
        let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &host);
        assert_eq!(code, exit(0), "{out}{err}");
        let calls = host.flows.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].2,
            if ssh {
                RenewBrowser::Remote { opener: None }
            } else {
                RenewBrowser::Default
            }
        );
        assert!(!env.accounts_store().dir.join("bin").exists());
    }
    let (_tmp, env) = renew_env("renew-opener-explicit-profile");
    let path = env.accounts_roster().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    if !text.contains("browser =") {
        std::fs::write(
            &path,
            text.replacen(
                "label = \"main\"",
                "label = \"main\"\nbrowser = \"Claude main\"",
                1,
            ),
        )
        .unwrap();
    }
    let mut host = RenewHost::new();
    host.exe = None;
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(host.flows.borrow().is_empty());
    assert!(host.checked.borrow().is_empty());
}

#[test]
fn the_new_forms_parse_and_their_flags_belong_to_them() {
    let p = |line: &str| parse(&line.split(' ').map(str::to_string).collect::<Vec<_>>());
    assert_eq!(
        p("accounts renew --due").map(|(c, _)| c),
        Ok(Cmd::Accounts(renew(&[], true, None)))
    );
    assert_eq!(
        p("accounts renew main work-2 --on m3,m4:17100").map(|(c, _)| c),
        Ok(Cmd::Accounts(renew(
            &["main", "work-2"],
            false,
            Some(&["m3", "m4:17100"])
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
            email: Some(OWNER.into()),
            identity_email: None,
            browser: Some("Claude".into()),
        }))
    );
    assert!(p("accounts adopt-default main").is_err(), "needs --email");
    assert_eq!(
        p(&format!("accounts add w2 --email {WORK}")).map(|(c, _)| c),
        Ok(Cmd::Accounts(AccountsCmd::Add {
            label: "w2".into(),
            class: Class::Preferred,
            email: Some(WORK.into()),
            identity_email: None,
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

#[test]
fn a_completed_claude_renewal_refuses_changed_policy_or_binding_without_a_checkpoint() {
    for change in ["identity", "removed", "unreadable", "missing", "binding"] {
        let (_tmp, env) = renew_env("renew-changed-policy");
        let roster_path = env.accounts_roster().unwrap();
        let store = env.accounts_store();
        let original = std::fs::read_to_string(&roster_path).unwrap();
        let after_change = std::rc::Rc::new(RefCell::new(None));
        let keep = after_change.clone();
        let changed_store = store.clone();
        let snapshot = |store: &accounts::Store| {
            [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ]
            .map(|path| std::fs::read(path).ok())
        };
        let mut host = RenewHost::new();
        host.during_status = Some(Box::new(move || {
            match change {
                "identity" => std::fs::write(
                    &roster_path,
                    original.replace(OWNER, "changed@example.test"),
                )
                .unwrap(),
                "removed" => std::fs::write(&roster_path, "").unwrap(),
                "unreadable" => std::fs::write(&roster_path, "[[broken").unwrap(),
                "missing" => {
                    std::fs::rename(&roster_path, roster_path.with_extension("kept")).unwrap()
                }
                "binding" => {
                    changed_store
                        .transact_logins(1, &mut None, |state, logins| {
                            Ok(accounts::note_renewed(
                                state,
                                logins,
                                "main",
                                None,
                                at!("/another-login"),
                                None,
                                1,
                            ))
                        })
                        .unwrap();
                }
                _ => unreachable!(),
            }
            *keep.borrow_mut() = Some(snapshot(&changed_store));
        }));
        let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
        assert_eq!(code, exit(1), "{change}: {out}{err}");
        assert_eq!(
            host.flows.borrow().len(),
            1,
            "a policy refusal cannot retry stale policy"
        );
        let staging = PathBuf::from(&host.flows.borrow()[0].0);
        assert!(
            staging.join(".claude.json").is_file(),
            "{change}: keep the completed login"
        );
        assert!(
            !staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists(),
            "{change}"
        );
        assert_eq!(
            snapshot(&store),
            after_change.borrow().clone().unwrap(),
            "{change}: no record after policy changed"
        );
    }
}

#[test]
fn claude_alias_renewal_uses_the_login_address_and_checks_the_provider_identity() {
    for reported in ["canonical@example.test", "alias@example.test"] {
        let (_tmp, env) = renew_env("renew-identity-alias");
        let (code, _, err) = run_line(
            "accounts add alias --email alias@example.test --identity-email canonical@example.test --browser Fixture",
            &env,
        );
        assert_eq!(code, exit(0), "{err}");
        let roster = Roster::load(&env.accounts_roster().unwrap()).unwrap();
        let row = roster.row("alias").unwrap();
        assert_eq!(row.email.as_deref(), Some("alias@example.test"));
        assert_eq!(
            row.expected_identity_email(),
            Some("canonical@example.test")
        );
        let mut host = RenewHost::new();
        host.signs_in_as = Some(reported);
        let (code, out, err) = acc(renew(&["alias"], false, None), &env, &host);
        assert_eq!(host.flows.borrow()[0].1, "alias@example.test");
        let logins = logins_of(&env);
        if reported == "canonical@example.test" {
            assert_eq!(code, exit(0), "{out}{err}");
            let binding = logins.get("alias").unwrap();
            let staging = Path::new(binding.dir.as_deref().unwrap());
            let proof = accounts::read_renewal_checkpoint(
                staging,
                &std::fs::canonicalize(staging).unwrap(),
                env.now,
            )
            .unwrap();
            assert_eq!(proof.email, reported);
        } else {
            assert_eq!(code, exit(1), "{out}{err}");
            assert!(
                logins
                    .get("alias")
                    .is_some_and(|binding| binding.dir.is_none())
            );
            assert!(out.contains("REFUSED: wrong account"), "{out}");
        }
    }
}

#[test]
fn a_renewal_signs_in_to_a_staging_home_checks_it_there_and_repoints_the_binding() {
    let (_tmp, env) = renew_env("renew-happy");
    let host = RenewHost::new();
    let parent = logins_of(&env);
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
        std::fs::canonicalize(host.exe.as_ref().unwrap()).unwrap()
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
    assert!(proof.parent.matches(&parent, "main"));
    assert!(!proof.parent.matches(&logins, "main"));
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
        class: Class::Preferred,
        email: None,
        identity_email: None,
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

#[test]
fn a_cancelled_renewal_runs_no_check_and_records_nothing() {
    for (flow, cached) in [
        (130, false),
        (1, false),
        (130, true),
        (137, true),
        (143, true),
    ] {
        if cached && !cfg!(unix) {
            continue;
        }
        let (_tmp, env) = renew_env(&format!("renew-cancel-{flow}-{cached}"));
        if cached {
            cached_profile_only(&env);
        }
        let mut host = RenewHost::new();
        host.flow = flow;
        host.flow_after_first = Some(0);
        let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
        assert_eq!(code, exit(1), "{out}{err}");
        assert_eq!(host.flows.borrow().len(), 1);
        if cached {
            assert!(matches!(
                &host.flows.borrow()[0].2,
                RenewBrowser::Remote { .. }
            ));
        }
        assert!(
            host.checked.borrow().is_empty(),
            "no `auth status` after a failed flow"
        );
        let store = env.accounts_store();
        assert!(!store.logins_dir().join("main+1").exists());
        assert!(!store.logins_file().exists() && !store.state_file().exists());
        assert!(store.tail(10).expect("ledger").is_empty());
    }
}

#[cfg(unix)]
#[test]
fn a_replacement_login_stops_a_stale_manual_authentication_fallback() {
    let (_tmp, env) = renew_env("renew-stale-fallback");
    cached_profile_only(&env);
    let store = env.accounts_store();
    let old = store.login_dir("main").unwrap();
    let old_text = old.to_str().unwrap().to_owned();
    store
        .transact_logins(env.now, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state, logins, "main", None, &old_text, None, env.now,
            ))
        })
        .unwrap();
    let after = std::rc::Rc::new(RefCell::new(None));
    let changed_after = std::rc::Rc::clone(&after);
    let changed_store = store.clone();
    let now = env.now;
    let mut host = RenewHost::new();
    host.flow = 1;
    host.flow_after_first = Some(0);
    host.during_flow = Some(Box::new(move || {
        changed_store
            .transact_logins(now, &mut None, |state, logins| {
                let was = logins.get("main").unwrap().was.clone();
                let events =
                    accounts::note_renewed(state, logins, "main", None, &old_text, None, now);
                logins.labels.get_mut("main").unwrap().was = was;
                Ok(events)
            })
            .unwrap();
        *changed_after.borrow_mut() = Some(
            [
                changed_store.state_file(),
                changed_store.logins_file(),
                changed_store.ledger_path(),
                changed_store.wake_path(),
            ]
            .map(|path| std::fs::read(path).ok()),
        );
    }));
    let (code, _, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{err}");
    assert_eq!(host.flows.borrow().len(), 1, "no stale fallback");
    assert_eq!(
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ]
        .map(|path| std::fs::read(path).ok()),
        after.borrow().clone().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn a_cached_profile_authentication_failure_falls_back_once_to_the_interactive_browser() {
    let (_tmp, env) = renew_env("renew-headless-auth-fallback");
    cached_profile_only(&env);
    let mut host = RenewHost::new();
    host.flow = 1;
    host.flow_after_first = Some(0);
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let flows = host.flows.borrow();
    assert_eq!(flows.len(), 2);
    assert!(matches!(&flows[0].2, RenewBrowser::Remote { .. }));
    assert!(!matches!(&flows[1].2, RenewBrowser::Remote { .. }));
    assert_eq!(*host.checked.borrow(), [flows[1].0.clone()]);
    let store = env.accounts_store();
    assert_eq!(
        logins_of(&env).get("main").unwrap().dir.as_deref(),
        Some(flows[1].0.as_str()),
    );
    assert!(
        accounts::read_renewal_checkpoint(
            Path::new(&flows[1].0),
            &std::fs::canonicalize(&flows[1].0).unwrap(),
            env.now,
        )
        .is_some()
    );
    assert_eq!(
        store
            .tail(10)
            .unwrap()
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        [accounts::Kind::Auth, accounts::Kind::Renewed],
    );
    no_address(&env, &[&out, &err]);
}

#[test]
fn a_verified_renewal_keeps_staging_and_records_nothing_when_checkpoint_is_obstructed() {
    let (_tmp, env) = renew_env("renew-checkpoint-obstructed");
    #[cfg(unix)]
    cached_profile_only(&env);
    let mut host = RenewHost::new();
    host.obstruct_checkpoint = true;
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(host.flows.borrow().len(), 1);
    assert_eq!(
        matches!(&host.flows.borrow()[0].2, RenewBrowser::Remote { .. }),
        cfg!(unix)
    );
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
    assert!(!store.logins_dir().join("main+2").exists());
    no_address(&env, &[&out, &err]);
}

#[test]
fn a_renewal_does_not_start_when_the_ledger_is_unreadable() {
    let (_tmp, env) = renew_env("renew-published-ledger-failure");
    #[cfg(unix)]
    cached_profile_only(&env);
    let store = env.accounts_store();
    store.ensure().expect("store");
    store
        .save(&accounts::State::default())
        .expect("known prior state");
    std::fs::create_dir(store.ledger_path()).expect("block ledger append");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(host.flows.borrow().is_empty());
    assert!(host.checked.borrow().is_empty());
    assert!(logins_of(&env).get("main").is_none());
    assert!(!store.load(env.now).unwrap().0.accounts.contains_key("main"));
    assert!(!store.logins_dir().join("main+1").exists());
    assert!(store.ledger_path().is_dir());
    std::fs::rename(
        store.ledger_path(),
        store.dir.join("retained-ledger-obstruction"),
    )
    .unwrap();
    let (code, out, err) = acc(renew(&["main"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(host.flows.borrow().len(), 1);
    assert_eq!(
        matches!(&host.flows.borrow()[0].2, RenewBrowser::Remote { .. }),
        cfg!(unix)
    );
    assert_eq!(host.checked.borrow().len(), 1);

    let logins = logins_of(&env);
    let bound = logins
        .get("main")
        .expect("binding published after admission");
    let home = Path::new(bound.dir.as_deref().expect("a bound home"));
    assert_eq!(home, store.logins_dir().join("main+1"));
    let (state, _) = store
        .load(env.now)
        .expect("state published after admission");
    assert_eq!(state.account("main").auth, Auth::Ok);
    assert!(
        home.is_dir(),
        "a durable binding must not point at a deleted home"
    );
    assert!(
        home.join(".claude.json").is_file(),
        "keep the checked login"
    );
    assert!(
        accounts::read_renewal_checkpoint(home, &std::fs::canonicalize(home).unwrap(), env.now,)
            .is_some()
    );
    assert!(!store.logins_dir().join("main+2").exists());
    no_address(&env, &[&out, &err]);
}

#[test]
fn over_ssh_the_paste_code_flow_runs_and_the_profile_is_named() {
    let (_tmp, env) = renew_env("renew-ssh");
    #[cfg(not(unix))]
    {
        let path = env.accounts_roster().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replacen(
                "label = \"main\"",
                "label = \"main\"\nbrowser = \"Claude main\"",
                1,
            ),
        )
        .unwrap();
    }
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

/// `--due` runs what a session needs now, never a row the upkeep signs in by itself before any
/// session needs it: a row with an email and no login, and nothing walled or due, is not due.
#[test]
fn due_leaves_a_row_no_session_needs_to_the_upkeep() {
    let (_tmp, env) = renew_env("renew-due-unneeded");
    let host = RenewHost::new();
    let (code, out, err) = acc(
        AccountsCmd::Add {
            label: "w3".into(),
            class: Class::Preferred,
            email: Some("w3@example.test".into()),
            identity_email: None,
            browser: None,
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let (code, out, err) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(out, "nothing is due on this machine\n");
    assert!(host.flows.borrow().is_empty(), "no sign-in started");
}

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
    assert_eq!(code, exit(1), "a named row left as it was is no renewal");
    assert_eq!(out, "due here: main\n");
    assert!(quiet.flows.borrow().is_empty());

    let (code, out, err) = acc(
        AccountsCmd::Add {
            label: "work-3".into(),
            class: Class::Preferred,
            email: Some("w3@example.test".into()),
            identity_email: None,
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
    env.accounts_store()
        .transact_logins(env.now, &mut note, |_, logins| {
            accounts::note_login_notice(logins, "main", 2, env.now);
            Ok(Vec::new())
        })
        .expect("walled row login notice recorded");
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
        host.remotes.borrow().is_empty(),
        "state uncertainty must not launch remote renewals: {:?}",
        host.remotes.borrow()
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
    assert!(host.remotes.borrow().is_empty(), "no remote renewal");
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(store.logins_file()).unwrap(),
        damaged
    );
    no_address(&env, &[&out, &err]);
}

#[test]
#[cfg(unix)]
fn renewal_accepts_a_native_fleet_endpoint_with_a_port() {
    let (_tmp, env) = renew_env("renew-native-endpoint");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&[], true, Some(&["192.0.2.19:17100"])), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert!(out.contains("renewing on 192.0.2.19:17100:"), "{out}");
}

#[test]
#[cfg(unix)]
fn other_machines_are_renewed_through_native_fleet_in_one_sitting() {
    let (_tmp, env) = renew_env("renew-on");
    let host = RenewHost::new();
    let (code, out, err) = acc(renew(&[], true, Some(&["m3", "m4:17100"])), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(*host.remotes.borrow(), ["m3", "m4:17100"]);
    assert!(
        out.contains("renewing on m3:") && out.contains("renewing on m4:17100:"),
        "{out}"
    );
    assert_eq!(out.matches("remote due pass completed").count(), 2);
    assert_eq!(err.matches("remote observation").count(), 2);
    std::fs::write(
        env.config.as_ref().expect("a config"),
        "[harness]\nhosts = [\"m5\"]\n",
    )
    .expect("write");
    let host = RenewHost::new();
    let (code, ..) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0));
    assert_eq!(*host.remotes.borrow(), ["m5"]);
    let remote = ["accounts", "renew", "--due", "--here"]
        .map(str::to_owned)
        .to_vec();
    let Ok((Cmd::Accounts(there), _)) = parse(&remote) else {
        panic!("{remote:?} parses");
    };
    let host = RenewHost::new();
    let (code, out, err) = acc(there, &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert!(host.remotes.borrow().is_empty(), "one machine deep: {out}");
    let mut failing = RenewHost::new();
    failing.remote_exit = 1;
    let (code, _, err) = acc(renew(&[], true, None), &env, &failing);
    assert_eq!(code, exit(1));
    assert!(err.contains("on m5: exited 1"), "{err}");
    std::fs::write(env.config.as_ref().expect("a config"), "").expect("write");
    let host = RenewHost::new();
    let (code, ..) = acc(renew(&[], true, None), &env, &host);
    assert_eq!(code, exit(0));
    assert!(
        host.remotes.borrow().is_empty(),
        "no machine named, no remote call"
    );
    for bad in [
        "-oProxyCommand=x",
        "m3;rm",
        "m3:relative/aterm",
        "m3:/a b",
        "user@m3",
        "m3:/opt/aterm",
        "m3:0",
        "m3:65536",
    ] {
        let host = RenewHost::new();
        let (code, _, err) = acc(renew(&["main"], false, Some(&[bad])), &env, &host);
        assert_eq!(code, exit(1), "{bad}");
        assert!(err.contains("is not a native fleet host ("), "{err}");
        assert!(host.flows.borrow().is_empty() && host.remotes.borrow().is_empty());
    }
}

#[test]
#[cfg(unix)]
fn local_interactive_rows_do_not_prevent_a_native_remote_pass() {
    let (_tmp, env) = renew_env("renew-remote-without-tty");
    let mut note = None;
    env.accounts_store()
        .transact_logins(env.now, &mut note, |_, logins| {
            accounts::note_login_notice(logins, "work-2", 2, env.now);
            Ok(Vec::new())
        })
        .expect("make the local interactive account due");
    for (labels, wanted_code) in [(&[][..], exit(0)), (&["main"][..], exit(1))] {
        let mut host = RenewHost::new();
        host.tty = false;
        let (code, out, err) = acc(renew(labels, true, Some(&["m3"])), &env, &host);
        assert_eq!(code, wanted_code, "{out}{err}");
        assert!(host.flows.borrow().is_empty());
        assert_eq!(*host.remotes.borrow(), ["m3"]);
        assert!(out.contains("renewing on m3:"), "{out}");
        assert!(err.contains("stdin is not a terminal"), "{err}");
    }
}

#[test]
fn each_line_that_signs_a_row_in_names_its_own_verb() {
    let (_tmp, env) = renew_env("renew-verbs");
    let host = RenewHost::new();
    let add = |label: &str, email: Option<&str>| AccountsCmd::Add {
        label: label.into(),
        class: Class::Preferred,
        email: email.map(str::to_string),
        identity_email: None,
        browser: None,
    };
    let (code, out, err) = acc(add("w3", Some("w3@example.com")), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let (_, report, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    for (label, verb, reason) in [
        (
            "w3",
            "renew",
            "w3: not-enrolled (no login made yet: aterm signs it in by itself; `aterm harness \
             accounts renew w3` runs that now)",
        ),
        (
            "plain",
            "login",
            "plain: not-enrolled (no login made yet: run `aterm harness accounts login plain`)",
        ),
    ] {
        assert!(report.contains(reason), "{reason}\n{report}");
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

/// A row's page stopped its sign-in: no later automatic run passes that, so the report names the
/// person's own sign-in instead of leaving the row to aterm, in the verb such a row takes.
/// NEGATIVE CONTROL: the same row with `Failed` is left to aterm, naming no step.
#[test]
fn a_page_that_stopped_a_sign_in_names_the_persons_renewal() {
    let (_tmp, env) = renew_env("renew-blocked");
    let host = RenewHost::new();
    let (code, out, err) = acc(
        AccountsCmd::Add {
            label: "w3".into(),
            class: Class::Preferred,
            email: Some("w3@example.com".into()),
            identity_email: None,
            browser: None,
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let at = host.now() - 60;
    let store = env.accounts_store();
    let record = |outcome| {
        store
            .transact_logins(at, &mut None, |_, logins| {
                Ok(accounts::note_sign_in_attempt(
                    logins, "w3", outcome, "mailbox", at,
                ))
            })
            .expect("recorded");
    };
    let step = "sign-in needed: Run aterm harness accounts renew w3 --cli claude in a terminal";
    record(accounts::AttemptOutcome::Failed);
    let (_, left, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    assert!(!left.contains(step), "CONTROL: {left}");
    record(accounts::AttemptOutcome::Blocked);
    let (_, report, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    assert!(report.contains(step), "{report}");
    assert!(report.contains("sign-in: error blocked since"), "{report}");
    assert!(
        report.contains(
            "w3: not-enrolled (no login made yet: a page stopped aterm's sign-in: run `aterm \
             harness accounts renew w3` in a terminal"
        ),
        "{report}"
    );
    assert!(
        !report.contains("`aterm harness accounts renew w3` runs that now"),
        "{report}"
    );
    assert!(!report.contains("accounts login w3"), "{report}");
}

/// The step that report names does what it says: a person at this Mac's terminal signs in a row
/// its page stopped in that row's own browser profile, or with none named in their default
/// browser, since another headless run would meet that page again and a default browser signed in
/// to another account answers `wrong-account` every time. NEGATIVE CONTROLS: the same row with
/// `Failed` goes headless, and a `Blocked` one goes headless too with nobody at a terminal, over
/// SSH, or at an agent's terminal, since no window may open on this Mac for any of them.
#[cfg(unix)]
#[test]
fn a_person_signs_a_row_its_page_stopped_in_its_own_browser() {
    use accounts::AttemptOutcome::{Blocked, Failed};
    #[derive(Debug, PartialEq, Eq)]
    enum Opens {
        OwnProfile,
        Default,
        Headless,
    }
    let own = if cfg!(unix) {
        Opens::OwnProfile
    } else {
        Opens::Default
    };
    for (tag, outcome, tty, ssh, agent, named, opens) in [
        ("failed", Failed, true, false, false, true, Opens::Headless),
        ("stopped", Blocked, true, false, false, true, own),
        (
            "unnamed",
            Blocked,
            true,
            false,
            false,
            false,
            Opens::Default,
        ),
        (
            "nobody",
            Blocked,
            false,
            false,
            false,
            true,
            Opens::Headless,
        ),
        ("ssh", Blocked, true, true, false, true, Opens::Headless),
        ("agent", Blocked, true, false, true, true, Opens::Headless),
    ] {
        let (_tmp, env) = renew_env(&format!("renew-page-{tag}"));
        cached_profile_only(&env);
        if !named {
            let path = env.accounts_roster().expect("a config dir");
            let text = std::fs::read_to_string(&path).expect("the roster");
            let lines: Vec<_> = text
                .lines()
                .filter(|line| !line.starts_with("browser"))
                .collect();
            std::fs::write(&path, lines.join("\n")).expect("the roster");
        }
        let at = env.now - 60;
        env.accounts_store()
            .transact_logins(at, &mut None, |_, logins| {
                Ok(accounts::note_sign_in_attempt(
                    logins, "main", outcome, "mailbox", at,
                ))
            })
            .expect("recorded");
        let mut host = RenewHost::new();
        host.tty = tty;
        host.ssh_session = ssh;
        host.agent = agent;
        let (_, out, err) = acc(renew(&["main"], false, None), &env, &host);
        let flows = host.flows.borrow();
        let first = flows.first().unwrap_or_else(|| panic!("{tag}: {out}{err}"));
        let opened = match first.2 {
            RenewBrowser::OwnProfile { .. } => Opens::OwnProfile,
            RenewBrowser::Default => Opens::Default,
            RenewBrowser::Remote { .. } => Opens::Headless,
            RenewBrowser::Profile { .. } => panic!("{tag}: {:?}", first.2),
        };
        assert_eq!(opened, opens, "{tag}: {:?}", first.2);
        assert_eq!(flows.len(), 1, "{tag}: one run");
    }
}

/// An agent's terminal opens no browser on this Mac: a row with no headless route is left to a
/// person, and a headless run that failed gets no second, interactive one. CONTROL: a person at
/// the same terminal gets both.
#[test]
fn an_agents_terminal_opens_no_browser_for_a_row() {
    for (agent, code, flows) in [(false, 0, 1), (true, 1, 0)] {
        let (_tmp, env) = renew_env("renew-agent-no-route");
        let mut host = RenewHost::new();
        host.agent = agent;
        let (got, out, err) = acc(renew(&["main"], false, None), &env, &host);
        assert_eq!(got, exit(code), "{out}{err}");
        assert_eq!(host.flows.borrow().len(), flows, "{out}{err}");
    }
    #[cfg(unix)]
    for (agent, flows) in [(false, 2), (true, 1)] {
        let (_tmp, env) = renew_env("renew-agent-fallback");
        cached_profile_only(&env);
        let mut host = RenewHost::new();
        host.agent = agent;
        host.flow = 1;
        host.flow_after_first = Some(0);
        let (_, out, err) = acc(renew(&["main"], false, None), &env, &host);
        assert_eq!(host.flows.borrow().len(), flows, "{out}{err}");
    }
}

#[test]
fn the_report_shows_a_rows_failed_automatic_sign_in_until_a_login_is_made() {
    let (_tmp, env) = renew_env("renew-attempt");
    let host = RenewHost::new();
    let (code, out, err) = acc(
        AccountsCmd::Add {
            label: "w3".into(),
            class: Class::Preferred,
            email: Some("w3@example.com".into()),
            identity_email: None,
            browser: None,
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{out}{err}");
    let routes = "profile(none) mailbox(none configured) vault(not a member) \
                  headless-browser(none installed) hosts(none configured)";
    let at = host.now() - 60;
    let store = env.accounts_store();
    for label in ["w3", "main"] {
        store
            .transact_logins(at, &mut None, |_, logins| {
                Ok(accounts::note_sign_in_attempt(
                    logins,
                    label,
                    accounts::AttemptOutcome::NoRoute,
                    routes,
                    at,
                ))
            })
            .expect("recorded");
    }
    let (code, report, _) = acc(AccountsCmd::Report { json: false }, &env, &host);
    assert_eq!(code, exit(0));
    let line = format!(
        "  sign-in: error no-route since {}: tried {routes}",
        rfc3339_utc(at)
    );
    assert_eq!(
        report.matches(&line).count(),
        1,
        "only the row still without a login says it: {report}"
    );
    let (_, json, _) = acc(AccountsCmd::Report { json: true }, &env, &host);
    let doc: Value = aterm_json::from_str(&json).expect("json");
    let attempt_of = |label: &str| {
        doc["accounts"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["label"] == Value::from(label))
            .map(|row| row["sign_in_attempt"].clone())
            .expect("the row")
    };
    let w3 = attempt_of("w3");
    assert_eq!(w3["outcome"], Value::from("no-route"), "{json}");
    assert_eq!(w3["routes"], Value::from(routes));
    assert_eq!(w3["since"], Value::from(at));
    assert_eq!(attempt_of("work-2"), Value::Null, "never tried");
    no_address(&env, &[&report, &json]);
}

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

#[test]
fn adopt_default_lists_this_machines_own_login_once() {
    let tmp = tmp_dir("adopt");
    let env = env_at(&tmp);
    let adopt = |label: &str, email: &str| AccountsCmd::AdoptDefault {
        label: label.into(),
        email: Some(email.into()),
        identity_email: None,
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
            class: Class::Preferred,
            email: Some(WORK.into()),
            identity_email: None,
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
            gone(crate::harness::open_url::INPUT_ENV),
            set(accounts::HOME_ENV, "/s/logins/main+1"),
            gone(crate::harness::open_url::NO_HEADLESS_ENV),
            set("BROWSER", "/s/bin/aterm-open-url"),
            set(crate::harness::open_url::PROFILE_ENV, "Claude main"),
        ])
    );
    let own = renew_command(
        program,
        "/h",
        OWNER,
        &RenewBrowser::OwnProfile {
            opener: PathBuf::from("/s/bin/aterm-open-url"),
            profile: "Claude main".into(),
        },
    );
    assert!(envs(&own).contains(&set(crate::harness::open_url::PROFILE_ENV, "Claude main")));
    assert!(
        envs(&own).contains(&set(crate::harness::open_url::NO_HEADLESS_ENV, "1")),
        "a row its page stopped opens with no headless try first"
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
    for command in [&remote, &bare, &default] {
        assert!(envs(command).contains(&gone(crate::harness::open_url::INPUT_ENV)));
    }
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
    /// The stand-in claude and the scratch home its renewal reads as `$HOME`.
    struct StandIn(PathBuf, OsString);
    impl AccountsHost for StandIn {
        fn stdin_is_terminal(&self) -> bool {
            true
        }
        fn current_exe(&self) -> Option<PathBuf> {
            Some(std::env::current_exe().expect("the test executable"))
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
            let user_home = self.1.clone();
            renew_flow_of(
                self.0.as_os_str(),
                home,
                email,
                b,
                &move |name| (name == "HOME").then(|| user_home.clone()),
                None,
            )
            .map(|exit| exit.code)
            .map_err(|failure| failure.to_string())
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
    let host = StandIn(
        program.clone(),
        env.home.clone().expect("a scratch home").into_os_string(),
    );
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
             mailbox=${{{mailbox_env}+set}} email=${{{email_env}+set}} webmail=${{{webmail_env}+set}}\" >> \"$log\"\n\
             if [ \"$2\" = login ]; then printf '%s' \"$5\" > \"$CLAUDE_CONFIG_DIR/.email\"; \
             echo '{{}}' > \"$CLAUDE_CONFIG_DIR/.claude.json\"; exit 0; fi\n\
             printf '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"%s\",\"subscriptionType\":\"max\"}}' \
             \"$(cat \"$CLAUDE_CONFIG_DIR/.email\")\"\n",
            mailbox_env = crate::harness::open_url::MAILBOX_ENV,
            email_env = crate::harness::open_url::EMAIL_ENV,
            webmail_env = crate::harness::headless_oauth::WEBMAIL_URL_ENV,
        ),
    )
    .expect("stand-in");
    /// The stand-in claude and the scratch home its renewal reads as `$HOME`.
    struct HeadlessMailboxStandIn(PathBuf, OsString);
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
            let user_home = self.1.clone();
            renew_flow_of(
                self.0.as_os_str(),
                home,
                email,
                b,
                &move |name| match name {
                    "HOME" => Some(user_home.clone()),
                    "USER" => Some("fixture-user".into()),
                    crate::harness::headless_oauth::WEBMAIL_URL_ENV => {
                        Some("https://mailbox.example.test".into())
                    }
                    _ => None,
                },
                Some(aterm_spec::wait::MUST_HAPPEN),
            )
            .map(|exit| exit.code)
            .map_err(|failure| failure.to_string())
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
    let host = HeadlessMailboxStandIn(
        program,
        env.home.clone().expect("a scratch home").into_os_string(),
    );
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    let ran = std::fs::read_to_string(tmp.path().join("bin").join("ran")).expect("it ran");
    let lines: Vec<&str> = ran.lines().collect();
    assert_eq!(
        lines,
        [
            "auth login browser=aterm-open-url mailbox=set email=set webmail=set",
            "auth status browser= mailbox= email= webmail=",
        ]
    );
}

#[cfg(unix)]
#[test]
fn an_unattended_renewal_timeout_releases_the_machine_signin_lock() {
    let (tmp, env) = renew_env("renew-unattended-timeout");
    let store = env.accounts_store();
    std::fs::create_dir_all(
        store
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .unwrap();
    let program = tmp.path().join("bin").join("claude");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    aterm_tempfile::stand_in(
        &program,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"${0%/*}/pid\"\nexec /bin/sleep 3600\n",
    )
    .unwrap();
    let pid_file = program.parent().unwrap().join("pid");
    let before = logins_of(&env).get("work-2").and_then(|b| b.dir.clone());

    let worker_env = env.clone();
    let host = NativeRenewal {
        program,
        user_home: env.home.clone().unwrap().into_os_string(),
        within: std::time::Duration::from_millis(100),
    };
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let answer = acc(renew(&["work-2"], false, None), &worker_env, &host);
        let _ = done.send(answer);
    });
    // The short interval is the configured behavior under test. This separate
    // one-minute wait detects a return to an unbounded child wait.
    let answer = result.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let hung = answer.is_err();
    let answer = answer.unwrap_or_else(|_| {
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        // The fixture wrote its own PID before exec and owns this process group.
        aterm_uds::process::kill_group_until_empty(pid).unwrap();
        result.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap()
    });
    worker.join().unwrap();
    assert!(
        !hung,
        "an unattended renewal kept the machine sign-in lock indefinitely"
    );
    assert_ne!(answer.0, exit(0));
    assert!(
        store.signin_now().unwrap().is_some(),
        "the next account can renew"
    );
    assert_eq!(
        logins_of(&env).get("work-2").and_then(|b| b.dir.clone()),
        before
    );
}

#[cfg(unix)]
#[test]
fn an_unattended_nonzero_exit_reports_its_browser_cause_without_changing_retries() {
    for (code, expected) in [(1, Renewal::AuthFailed), (130, Renewal::Failed)] {
        let (tmp, env) = renew_env("renew-browser-exit");
        let program = tmp.path().join("provider");
        aterm_tempfile::stand_in(
            &program,
            &format!(
                "#!/bin/sh\nprintf 'bot-check claude.ai\\n' > \"$ATERM_OPEN_URL_OUTCOME\"\nexit {code}\n"
            ),
        )
        .unwrap();
        let host = NativeRenewal {
            program,
            user_home: env.home.clone().unwrap().into_os_string(),
            within: aterm_spec::wait::MUST_HAPPEN,
        };
        let store = env.accounts_store();
        let roster_path = env.accounts_roster().unwrap();
        let roster = Roster::load(&roster_path).unwrap();
        let row = roster.row("work-2").unwrap();
        let logins = logins_of(&env);
        let before = logins.get("work-2").cloned();
        let signin = store.signin_now().unwrap().unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = renew_one(
            &env,
            &host,
            &store,
            &signin,
            accounts::RenewalPolicy {
                roster_path: &roster_path,
                roster: &roster,
                row,
                logins: &logins,
            },
            row.dir.as_deref(),
            &RenewBrowser::Remote { opener: None },
            &mut out,
            &mut err,
        );
        let err = String::from_utf8(err).unwrap();
        assert_eq!(result, expected, "{err}");
        assert_eq!(result.can_retry_authentication(), code == 1, "{err}");
        assert!(err.contains(&format!("exited {code}")), "{err}");
        assert!(err.contains("Cloudflare"), "{err}");
        assert!(err.contains("claude.ai"), "{err}");
        assert_eq!(logins_of(&env).get("work-2").cloned(), before);
    }
}

#[test]
fn an_unattended_renewal_keeps_the_safe_browser_outcome_after_capture_fails() {
    use crate::harness::open_url::{Met, MetWord, OUTCOME_ENV};

    let tmp = aterm_tempfile::tempdir().unwrap();
    let stale = tmp.path().join("stale-outcome");
    std::fs::write(&stale, "block-page stale.example\n").unwrap();
    let reason = "it ran past its 300000 ms deadline";
    for (text, expected) in [
        (
            "bot-check claude.ai\n",
            Some(Met::new(MetWord::BotCheck, Some("claude.ai"))),
        ),
        (
            "bot-check https://claude.ai/login?code=private-code\n",
            None,
        ),
        (
            "browser-start\n",
            Some(Met::new(MetWord::BrowserStart, None)),
        ),
        ("browser-start private-code\n", None),
        (
            "browser-control\n",
            Some(Met::new(MetWord::BrowserControl, None)),
        ),
        ("browser-control private-code\n", None),
    ] {
        let mut cmd = aterm_uds::spawnfd::Command::new("unused-provider");
        cmd.env(OUTCOME_ENV, &stale);
        let mut actual_path = None;
        let failure = capture_unattended_renewal_with(cmd, |cmd| {
            let path = cmd
                .get_envs()
                .find(|(key, _)| *key == OUTCOME_ENV)
                .and_then(|(_, value)| value)
                .map(PathBuf::from)
                .expect("this attempt has its own outcome file");
            assert_ne!(path, stale);
            std::fs::write(&path, text).unwrap();
            actual_path = Some(path);
            Err(reason.to_owned())
        })
        .unwrap_err();
        assert_eq!(failure.met, expected);
        let diagnostic = failure.to_string();
        assert!(diagnostic.contains(reason), "{diagnostic}");
        assert!(!diagnostic.contains("private-code"), "{diagnostic}");
        if let Some(expected) = expected {
            assert!(diagnostic.contains(&expected.words()), "{diagnostic}");
        } else {
            assert_eq!(diagnostic, reason);
        }
        assert!(
            !actual_path.unwrap().exists(),
            "the private run is cleaned up"
        );
    }
    assert_eq!(
        std::fs::read_to_string(stale).unwrap(),
        "block-page stale.example\n"
    );
}

#[cfg(unix)]
#[test]
fn an_unattended_provider_exit_keeps_its_browser_outcome() {
    use crate::harness::open_url::{Met, MetWord};

    let tmp = aterm_tempfile::tempdir().unwrap();
    let program = tmp.path().join("provider");
    aterm_tempfile::stand_in(
        &program,
        "#!/bin/sh\nprintf 'block-page claude.ai\\n' > \"$ATERM_OPEN_URL_OUTCOME\"\nexit 1\n",
    )
    .unwrap();
    let cmd = aterm_uds::spawnfd::Command::new(&program);
    let (ran, met) = capture_unattended_renewal(cmd, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert_eq!(ran.code, Some(1));
    assert_eq!(met, Some(Met::new(MetWord::BlockPage, Some("claude.ai"))));
}

/// `accounts renew`'s choice of rows reads the machine through its host alone: a row whose
/// mailbox only the host's environment names renews with no terminal. CONTROL: under a host
/// whose environment names none, the same row is only reported due and nothing runs.
#[cfg(unix)]
#[test]
fn renews_choice_of_rows_reads_the_environment_through_its_host() {
    let (tmp, env) = renew_env("renew-host-env");
    let mailbox = tmp.path().join("mail-the-host-names");
    std::fs::create_dir_all(&mailbox).expect("mailbox");
    let mut quiet = RenewHost::new();
    quiet.tty = false;
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &quiet);
    assert_eq!(code, exit(1), "{out}{err}");
    assert_eq!(out, "due here: work-2\n", "CONTROL");
    assert!(quiet.flows.borrow().is_empty());

    let mut named = RenewHost::new();
    named.tty = false;
    named.env_mailbox = Some(mailbox);
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &named);
    assert_eq!(code, exit(0), "{out}{err}");
    let renewed: Vec<_> = named
        .flows
        .borrow()
        .iter()
        .map(|(_, e, _)| e.clone())
        .collect();
    assert_eq!(renewed, [WORK], "{out}{err}");
}

#[cfg(unix)]
#[test]
fn renews_profile_route_uses_only_the_browser_its_host_names() {
    let (tmp, env) = renew_env("renew-host-browser");
    let store = env.accounts_store();
    let profile = crate::harness::headless_oauth::resolve_browser_profile_dir(
        &store.dir,
        Some("work-2"),
        Some(WORK),
    );
    std::fs::create_dir_all(&profile).unwrap();
    let browser = tmp.path().join("fixture-browser");
    std::fs::write(&browser, "fixture; never executed").unwrap();
    let mut quiet = RenewHost::new();
    quiet.tty = false;
    assert!(!quiet.system_browsers());
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &quiet);
    assert_eq!(code, exit(1), "{out}{err}");
    assert!(quiet.flows.borrow().is_empty());
    let mut named = RenewHost::new();
    named.tty = false;
    named.env_browser = Some(browser);
    let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &named);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(named.flows.borrow().len(), 1);
}

#[cfg(unix)]
#[test]
fn a_repaired_browser_reopens_noninteractive_renewal_without_proving_its_identity() {
    for (repaired, reported) in [(false, WORK), (true, WORK), (true, OWNER)] {
        let (tmp, env) = renew_env("renew-repaired-browser");
        let store = env.accounts_store();
        let profile = crate::harness::headless_oauth::resolve_browser_profile_dir(
            &store.dir,
            Some("work-2"),
            Some(WORK),
        );
        std::fs::create_dir_all(profile).unwrap();
        let browser = tmp.path().join("fixture-browser");
        std::fs::write(&browser, "fixture; never executed").unwrap();
        let wrong_at = env.now - 2;
        store
            .transact_logins(env.now, &mut None, |state, logins| {
                let mut events =
                    accounts::note_auth(state, "work-2", accounts::Auth::SignedOut, wrong_at);
                events.extend(accounts::note_wrong_account(logins, "work-2", wrong_at));
                if repaired {
                    events.push(
                        accounts::Event::new(env.now - 1, accounts::Kind::SignIn, "work-2")
                            .because("claude.ai window done session-kept"),
                    );
                }
                Ok(events)
            })
            .unwrap();
        let mut host = RenewHost::new();
        host.tty = false;
        host.env_browser = Some(browser);
        host.signs_in_as = Some(reported);
        let before_check = store.clone();
        let now = env.now;
        host.during_flow = Some(Box::new(move || {
            assert_eq!(
                before_check.load(now).unwrap().0.account("work-2").auth,
                accounts::Auth::SignedOut
            );
            assert_eq!(
                before_check
                    .load_logins(now)
                    .unwrap()
                    .0
                    .get("work-2")
                    .unwrap()
                    .wrong_at,
                Some(wrong_at)
            );
        }));
        let (code, out, err) = acc(renew(&["work-2"], false, None), &env, &host);
        assert_eq!(
            host.flows.borrow().len(),
            usize::from(repaired),
            "{out}{err}"
        );
        assert_eq!(
            host.checked.borrow().len(),
            usize::from(repaired),
            "{out}{err}"
        );
        let accepted = repaired && reported == WORK;
        assert_eq!(code, exit(u8::from(!accepted)), "{out}{err}");
        let auth = store.load(env.now).unwrap().0.account("work-2").auth;
        assert_eq!(
            auth,
            if accepted {
                accounts::Auth::Ok
            } else {
                accounts::Auth::SignedOut
            }
        );
        let logins = logins_of(&env);
        assert_eq!(logins.get("work-2").unwrap().wrong_at.is_none(), accepted);
        if repaired {
            assert!(matches!(
                host.flows.borrow()[0].2,
                RenewBrowser::Remote { .. }
            ));
        }
    }
}
